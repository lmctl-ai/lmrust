//! Self-exec of the legacy CLI: the only place the bridge spawns a process. No shell, the
//! parent's environment plus [`super::CHILD_ENV`], stdin piped or null, stdout/stderr read on
//! threads with byte caps (the pipes are drained to the end so the child never blocks), an
//! optional deadline that kills the child.

use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

/// Limits of one run.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Limits {
    pub max_stdout: usize,
    pub max_stderr: usize,
    pub timeout: Option<Duration>,
}

/// What one legacy run produced.
#[derive(Debug)]
pub(crate) struct ChildRun {
    /// `None` when the child was killed by a signal (or by the deadline).
    pub exit_code: Option<i32>,
    /// The signal that ended the child (Unix).
    pub signal: Option<i32>,
    pub stdout: Vec<u8>,
    /// More than `max_stdout` bytes were written.
    pub stdout_overflow: bool,
    pub stderr: String,
    pub timed_out: bool,
}

/// Why a run could not start.
#[derive(Debug, PartialEq)]
pub(crate) enum SpawnError {
    /// The params cannot be process arguments (NUL byte, argument list too long).
    Input(String),
    Internal(String),
}

fn spawn_error(exe: &Path, error: std::io::Error) -> SpawnError {
    // E2BIG ("Argument list too long") is 7 on Linux and macOS.
    if error.kind() == std::io::ErrorKind::InvalidInput || error.raw_os_error() == Some(7) {
        SpawnError::Input(format!("params cannot be passed to {}: {error}", exe.display()))
    } else {
        SpawnError::Internal(format!("cannot start {}: {error}", exe.display()))
    }
}

fn capped_reader<R: Read + Send + 'static>(mut pipe: R, cap: usize) -> thread::JoinHandle<(Vec<u8>, bool)> {
    thread::spawn(move || {
        let mut kept = Vec::new();
        let mut overflow = false;
        let mut buf = [0u8; 8192];
        loop {
            match pipe.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let room = cap.saturating_sub(kept.len());
                    if n > room {
                        overflow = true;
                    }
                    kept.extend_from_slice(&buf[..n.min(room)]);
                }
            }
        }
        (kept, overflow)
    })
}

#[cfg(unix)]
fn signal_of(status: &std::process::ExitStatus) -> Option<i32> {
    use std::os::unix::process::ExitStatusExt;
    status.signal()
}

#[cfg(not(unix))]
fn signal_of(_: &std::process::ExitStatus) -> Option<i32> {
    None
}

/// Run `exe argv...` and collect its output.
pub(crate) fn run(exe: &Path, argv: &[String], stdin: Option<String>, limits: Limits) -> Result<ChildRun, SpawnError> {
    let mut child = Command::new(exe)
        .args(argv)
        .env(super::CHILD_ENV, "1")
        .stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| spawn_error(exe, e))?;
    let writer = match (stdin, child.stdin.take()) {
        // Written on a thread: a child that prints before reading must not deadlock us. A closed
        // pipe (the child stopped reading) shows in its result instead.
        (Some(text), Some(mut pipe)) => Some(thread::spawn(move || {
            let _ = pipe.write_all(text.as_bytes());
        })),
        _ => None,
    };
    let out = capped_reader(child.stdout.take().expect("piped stdout"), limits.max_stdout);
    let err = capped_reader(child.stderr.take().expect("piped stderr"), limits.max_stderr);
    let started = Instant::now();
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(e) => return Err(SpawnError::Internal(format!("{} did not finish: {e}", exe.display()))),
        }
        if limits.timeout.is_some_and(|t| started.elapsed() >= t) {
            timed_out = true;
            let _ = child.kill();
            match child.wait() {
                Ok(status) => break status,
                Err(e) => return Err(SpawnError::Internal(format!("{} did not finish: {e}", exe.display()))),
            }
        }
        thread::sleep(Duration::from_millis(5));
    };
    let (stdout, stdout_overflow) = out.join().unwrap_or_default();
    let (stderr, _) = err.join().unwrap_or_default();
    if let Some(writer) = writer {
        let _ = writer.join();
    }
    Ok(ChildRun {
        exit_code: if timed_out { None } else { status.code() },
        signal: signal_of(&status),
        stdout,
        stdout_overflow,
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
        timed_out,
    })
}
