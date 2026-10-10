//! Process execution for operations (lmts `server/exec.ts`). Arguments are always passed
//! as an array, never through a shell, so metacharacter injection is impossible by
//! construction. Synchronous: no async runtime.

use std::collections::HashMap;
use std::fmt;
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

/// Captured output of a finished command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecResult {
    pub stdout: String,
    pub stderr: String,
    /// Non-zero only when `allow_non_zero_exit` was set (otherwise it is an error).
    pub exit_code: i32,
}

/// Why a command did not produce an [`ExecResult`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecError {
    /// The process could not be started (e.g. the binary does not exist).
    Spawn(String),
    /// The deadline passed; the process was killed.
    Timeout(String),
    /// The cancel flag was raised; the process was killed.
    Cancelled(String),
    /// The command exited non-zero and that was not allowed.
    NonZeroExit {
        exit_code: i32,
        stdout: String,
        stderr: String,
    },
}

impl fmt::Display for ExecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ExecError::Spawn(m) | ExecError::Timeout(m) | ExecError::Cancelled(m) => f.write_str(m),
            ExecError::NonZeroExit {
                exit_code,
                stdout,
                stderr,
            } => {
                write!(f, "Exit Code: {exit_code}\nStderr: {stderr}\nStdout: {stdout}")
            }
        }
    }
}

impl std::error::Error for ExecError {}

/// Options for [`spawn_command`].
#[derive(Debug, Clone, Copy)]
pub struct ExecOptions<'a> {
    /// Deadline (default 60 s).
    pub timeout: Duration,
    /// Return a non-zero exit as a result instead of an error.
    pub allow_non_zero_exit: bool,
    /// Optional cancellation flag, polled while the process runs.
    pub cancel: Option<&'a AtomicBool>,
}

impl Default for ExecOptions<'_> {
    fn default() -> Self {
        ExecOptions {
            timeout: Duration::from_secs(60),
            allow_non_zero_exit: false,
            cancel: None,
        }
    }
}

fn describe(command: &str, args: &[String]) -> String {
    let mut s = command.to_string();
    for a in args {
        s.push(' ');
        s.push_str(a);
    }
    s
}

fn exit_code_of(status: std::process::ExitStatus) -> i32 {
    if let Some(code) = status.code() {
        return code;
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(sig) = status.signal() {
            return 128 + sig;
        }
    }
    1
}

/// Run `command args...` in `cwd` with exactly the environment `env` (it replaces the
/// parent's), stdin closed and stdout/stderr captured as UTF-8 (lossy).
pub fn spawn_command(
    command: &str,
    args: &[String],
    cwd: &Path,
    env: &HashMap<String, String>,
    opts: ExecOptions<'_>,
) -> Result<ExecResult, ExecError> {
    let what = describe(command, args);
    if opts.cancel.is_some_and(|c| c.load(Ordering::SeqCst)) {
        return Err(ExecError::Cancelled(format!(
            "Command cancelled before execution: {what}"
        )));
    }
    let mut child = Command::new(command)
        .args(args)
        .current_dir(cwd)
        .env_clear()
        .envs(env)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| ExecError::Spawn(format!("Failed to start {command}: {e}")))?;

    let drain = |stream: Option<Box<dyn Read + Send>>| {
        thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut s) = stream {
                let _ = s.read_to_end(&mut buf);
            }
            String::from_utf8_lossy(&buf).into_owned()
        })
    };
    let out = drain(child.stdout.take().map(|s| Box::new(s) as Box<dyn Read + Send>));
    let err = drain(child.stderr.take().map(|s| Box::new(s) as Box<dyn Read + Send>));

    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(e) => return Err(ExecError::Spawn(format!("Failed to wait for {command}: {e}"))),
        }
        if opts.cancel.is_some_and(|c| c.load(Ordering::SeqCst)) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(ExecError::Cancelled(format!("Command cancelled: {what}")));
        }
        if started.elapsed() >= opts.timeout {
            let _ = child.kill();
            let _ = child.wait();
            return Err(ExecError::Timeout(format!(
                "Command timed out after {}s: {what}",
                crate::js::number_to_string(opts.timeout.as_secs_f64())
            )));
        }
        thread::sleep(Duration::from_millis(5));
    };
    let stdout = out.join().unwrap_or_default();
    let stderr = err.join().unwrap_or_default();
    let exit_code = exit_code_of(status);
    if exit_code != 0 && !opts.allow_non_zero_exit {
        return Err(ExecError::NonZeroExit {
            exit_code,
            stdout,
            stderr,
        });
    }
    Ok(ExecResult {
        stdout,
        stderr,
        exit_code,
    })
}
