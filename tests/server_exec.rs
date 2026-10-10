//! Port of lmts `tests/server-exec.test.ts` (`spawnCommand`).
//!
//! lmts runs `process.execPath -e <js>` under both its Bun and Node adapters; lmrust has a
//! single synchronous implementation, so the behaviour behind every adapter test (stdout /
//! stderr capture, exit codes, `allowNonZeroExit`, timeout kill, cancellation before and
//! during execution, spawn failure, no shell interpretation) is ported once with `/bin/sh`.
//! Cancellation uses the `AtomicBool` flag instead of an `AbortSignal`.
#![cfg(unix)]

use lmrust::server::{spawn_command, ExecError, ExecOptions};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

fn sh_env() -> HashMap<String, String> {
    [("PATH".to_string(), "/usr/bin:/bin".to_string())]
        .into_iter()
        .collect()
}

fn cwd() -> PathBuf {
    std::env::current_dir().unwrap()
}

fn opts(timeout_ms: u64) -> ExecOptions<'static> {
    ExecOptions {
        timeout: Duration::from_millis(timeout_ms),
        ..ExecOptions::default()
    }
}

fn sh(script: &str, o: ExecOptions<'_>) -> Result<lmrust::server::ExecResult, ExecError> {
    spawn_command("/bin/sh", &["-c".to_string(), script.to_string()], &cwd(), &sh_env(), o)
}

struct TempDir(PathBuf);

impl TempDir {
    fn new(prefix: &str) -> Self {
        use std::sync::atomic::AtomicUsize;
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "{prefix}{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        TempDir(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn exec_options_default_is_60s_and_disallows_non_zero_exit() {
    let d = ExecOptions::default();
    assert_eq!(d.timeout, Duration::from_secs(60));
    assert!(!d.allow_non_zero_exit);
    assert!(d.cancel.is_none());
}

// 'spawnCommand and spawnWithBun success' / 'spawnCommand with Node adapter' (1)
#[test]
fn spawn_command_success_captures_stdout_and_stderr() {
    let res = sh("echo 'hello world'; printf info >&2", opts(5000)).unwrap();
    assert_eq!(res.stdout.trim(), "hello world");
    assert_eq!(res.stderr.trim(), "info");
    assert_eq!(res.exit_code, 0);

    let direct = sh("echo 'direct'", ExecOptions::default()).unwrap();
    assert_eq!(direct.stdout, "direct\n");
}

// 'spawnCommand non-zero exit handling' (1) / Node adapter (3)
#[test]
fn spawn_command_non_zero_exit_disallowed_is_an_error() {
    let err = sh("printf 'fatal error' >&2; exit 42", opts(5000)).unwrap_err();
    match &err {
        ExecError::NonZeroExit { exit_code, stderr, .. } => {
            assert_eq!(*exit_code, 42);
            assert_eq!(stderr, "fatal error");
        }
        other => panic!("expected NonZeroExit, got {other:?}"),
    }
    let message = err.to_string();
    assert!(message.contains("Exit Code: 42"), "{message}");
    assert!(message.contains("fatal error"), "{message}");
}

// 'spawnCommand non-zero exit handling' (2) / Node adapter (2)
#[test]
fn spawn_command_non_zero_exit_allowed_returns_result() {
    let res = sh(
        "printf 'soft error' >&2; exit 7",
        ExecOptions {
            allow_non_zero_exit: true,
            ..opts(5000)
        },
    )
    .unwrap();
    assert_eq!(res.exit_code, 7);
    assert_eq!(res.stderr.trim(), "soft error");
}

// 'spawnCommand timeout' / Node adapter (6)
#[test]
fn spawn_command_timeout_kills_the_process() {
    let started = Instant::now();
    let err = sh("exec sleep 5", opts(100)).unwrap_err();
    assert!(started.elapsed() < Duration::from_secs(4), "not killed in time");
    match &err {
        ExecError::Timeout(_) => {}
        other => panic!("expected Timeout, got {other:?}"),
    }
    let message = err.to_string();
    assert!(message.contains("timed out after 0.1s"), "{message}");
    assert!(message.contains("Command timed out"), "{message}");
}

// 'spawnCommand signal already aborted' / Node adapter (4)
#[test]
fn spawn_command_cancel_flag_already_set() {
    let cancel = AtomicBool::new(true);
    let marker = TempDir::new("lmrust-exec-precancel-");
    let file = marker.0.join("ran");
    let script = format!("touch '{}'", file.display());
    let err = sh(
        &script,
        ExecOptions {
            cancel: Some(&cancel),
            ..opts(5000)
        },
    )
    .unwrap_err();
    assert!(matches!(err, ExecError::Cancelled(_)), "{err:?}");
    assert!(err.to_string().contains("Command cancelled before execution"));
    assert!(!file.exists(), "command must not run");
}

// 'spawnCommand signal aborted during execution' / Node adapter (5)
#[test]
fn spawn_command_cancel_flag_set_during_execution() {
    let cancel = AtomicBool::new(false);
    let started = Instant::now();
    let result = thread::scope(|s| {
        s.spawn(|| {
            thread::sleep(Duration::from_millis(50));
            cancel.store(true, Ordering::SeqCst);
        });
        sh(
            "exec sleep 3",
            ExecOptions {
                cancel: Some(&cancel),
                ..opts(5000)
            },
        )
    });
    let err = result.unwrap_err();
    assert!(started.elapsed() < Duration::from_millis(2500), "not killed in time");
    assert!(matches!(err, ExecError::Cancelled(_)), "{err:?}");
    assert!(err.to_string().contains("Command cancelled"));
}

#[test]
fn spawn_command_never_interprets_argv_through_a_shell() {
    let tmp = TempDir::new("lmrust-exec-argv-");
    let injected = tmp.0.join("injected");
    let test_args: Vec<String> = vec![
        "; echo pwned;".into(),
        "$(whoami)".into(),
        "| cat".into(),
        format!("> {}", injected.display()),
        "*.txt".into(),
        "hello world with spaces".into(),
    ];
    // printf receives every argument verbatim (no shell between us and it).
    let mut args = vec!["%s\\n".to_string()];
    args.extend(test_args.iter().cloned());
    let res = spawn_command("/usr/bin/printf", &args, &tmp.0, &sh_env(), opts(5000)).unwrap();
    let received: Vec<&str> = res.stdout.lines().collect();
    assert_eq!(received, test_args.iter().map(String::as_str).collect::<Vec<_>>());
    assert!(!injected.exists());

    // Same via a script reading "$@": the arguments arrive as data.
    let mut args = vec![
        "-c".to_string(),
        "for a in \"$@\"; do printf '%s\\n' \"$a\"; done".to_string(),
        "sh".to_string(),
    ];
    args.extend(test_args.iter().cloned());
    let res = spawn_command("/bin/sh", &args, &tmp.0, &sh_env(), opts(5000)).unwrap();
    assert_eq!(
        res.stdout.lines().collect::<Vec<_>>(),
        test_args.iter().map(String::as_str).collect::<Vec<_>>()
    );
    assert!(!injected.exists());
}

// Node adapter (7): command not found (ENOENT)
#[test]
fn spawn_command_missing_binary_is_a_spawn_error() {
    let err = spawn_command("non_existent_binary_12345_xyz", &[], &cwd(), &sh_env(), opts(5000)).unwrap_err();
    match &err {
        ExecError::Spawn(m) => assert!(m.contains("non_existent_binary_12345_xyz"), "{m}"),
        other => panic!("expected Spawn, got {other:?}"),
    }
}

#[test]
fn spawn_command_env_replaces_the_parent_environment() {
    let env: HashMap<String, String> = [("LMRUST_ONLY_VAR".to_string(), "value 1".to_string())]
        .into_iter()
        .collect();
    let res = spawn_command("/usr/bin/env", &[], &cwd(), &env, opts(5000)).unwrap();
    assert_eq!(res.stdout, "LMRUST_ONLY_VAR=value 1\n");

    let empty = spawn_command("/usr/bin/env", &[], &cwd(), &HashMap::new(), opts(5000)).unwrap();
    assert_eq!(empty.stdout, "");
}

#[test]
fn spawn_command_runs_in_cwd() {
    let tmp = TempDir::new("lmrust-exec-cwd-");
    let res = spawn_command(
        "/bin/sh",
        &["-c".into(), "pwd -P".into()],
        &tmp.0,
        &sh_env(),
        opts(5000),
    )
    .unwrap();
    let expected = std::fs::canonicalize(&tmp.0).unwrap();
    assert_eq!(Path::new(res.stdout.trim()), expected.as_path());
}

#[test]
fn spawn_command_stdin_is_closed() {
    // `cat` with stdin closed (null) ends immediately with empty output.
    let res = spawn_command("/bin/cat", &[], &cwd(), &sh_env(), opts(5000)).unwrap();
    assert_eq!(res.stdout, "");
    assert_eq!(res.exit_code, 0);
}

#[test]
fn spawn_command_large_output_is_fully_captured() {
    // More than a pipe buffer on both streams: drained concurrently, no deadlock.
    let res = sh(
        "i=0; while [ $i -lt 20000 ]; do echo xxxxxxxxxxxxxxxxxxxxxxxxxxxxxx; echo yyyyyyyyyy >&2; i=$((i+1)); done",
        opts(30000),
    )
    .unwrap();
    assert_eq!(res.stdout.lines().count(), 20000);
    assert_eq!(res.stderr.lines().count(), 20000);
}

// lmrust-specific (no lmts test): a signal death reports 128 + signal. lmts's Node adapter
// reports `exitCode ?? 0` (0) here; its Bun adapter reports whatever `proc.exited` yields.
#[test]
fn spawn_command_signal_exit_maps_to_128_plus_signal() {
    let res = sh(
        "kill -TERM $$",
        ExecOptions {
            allow_non_zero_exit: true,
            ..opts(5000)
        },
    )
    .unwrap();
    assert_eq!(res.exit_code, 128 + 15);
}

#[test]
fn spawn_command_invalid_utf8_is_decoded_lossily() {
    let res = sh("printf 'a\\377b'", opts(5000)).unwrap();
    assert_eq!(res.stdout, "a\u{FFFD}b");
}
