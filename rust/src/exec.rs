//! One helper that runs a CLI with captured output: killed (with its whole
//! process group) after a timeout or when a [`CancelToken`] fires, so a wedged
//! `nono` or `herdr` can neither hang an action nor outlive a keypress.

use std::ffi::OsStr;
use std::io::Read;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::context::Env;

/// How much of each stream is kept; more is read and dropped.
pub const MAX_OUTPUT_BYTES: usize = 16 * 1024 * 1024;

/// How long to wait for the output pipes to close after the child is gone,
/// in case a grandchild that escaped the process group still holds them.
const PIPE_GRACE: Duration = Duration::from_millis(1000);

/// A flag a caller sets to abandon a running command.
#[derive(Debug, Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    #[allow(dead_code)]
    pub fn new() -> Self {
        Self::default()
    }

    /// Abandons the commands that run under this token (the overlay cancels on a keypress).
    #[allow(dead_code)]
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// The result of a command that ran to completion.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CliOutput {
    /// The exit code, `None` when a signal ended the process.
    pub status: Option<i32>,
    pub signal: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl CliOutput {
    /// stdout followed by stderr, as the failure messages show them.
    pub fn output(&self) -> String {
        format!("{}{}", self.stdout, self.stderr)
    }

    #[cfg(test)]
    pub fn success(&self) -> bool {
        self.status == Some(0)
    }
}

/// Why a command did not complete. The timed-out and cancelled variants carry
/// whatever output arrived before the kill.
#[derive(Debug)]
pub enum ExecError {
    Spawn(std::io::Error),
    TimedOut(CliOutput),
    Cancelled(CliOutput),
}

/// Options of [`run_cli`].
pub struct RunOptions<'a> {
    /// The whole environment of the child; `None` inherits this process's.
    pub env: Option<&'a Env>,
    pub cwd: Option<&'a Path>,
    pub timeout: Duration,
    pub cancel: Option<&'a CancelToken>,
}

#[cfg(test)]
impl RunOptions<'_> {
    pub fn new(timeout: Duration) -> RunOptions<'static> {
        RunOptions {
            env: None,
            cwd: None,
            timeout,
            cancel: None,
        }
    }
}

/// A stream drained on its own thread into a shared buffer.
struct Drain {
    buffer: Arc<Mutex<Vec<u8>>>,
    done: Arc<AtomicBool>,
}

fn drain(mut stream: impl Read + Send + 'static) -> Drain {
    let buffer = Arc::new(Mutex::new(Vec::new()));
    let done = Arc::new(AtomicBool::new(false));
    let (shared, finished) = (Arc::clone(&buffer), Arc::clone(&done));
    std::thread::spawn(move || {
        let mut chunk = [0u8; 8192];
        while let Ok(read) = stream.read(&mut chunk) {
            if read == 0 {
                break;
            }
            let mut kept = shared
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let room = MAX_OUTPUT_BYTES.saturating_sub(kept.len());
            kept.extend_from_slice(&chunk[..read.min(room)]);
        }
        finished.store(true, Ordering::SeqCst);
    });
    Drain { buffer, done }
}

impl Drain {
    fn text(&self) -> String {
        let bytes = self
            .buffer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        String::from_utf8_lossy(&bytes).into_owned()
    }
}

fn kill_group(pid: u32) {
    if let Ok(pid) = libc::pid_t::try_from(pid) {
        // SAFETY: SIGKILL to the child's own process group, which `process_group(0)` created.
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
            libc::kill(pid, libc::SIGKILL);
        }
    }
}

/// Spawns the command, retrying a few times while the program is "text file
/// busy": another thread of this process still has the script open for writing
/// (it forked before closing it), which clears as soon as that child execs.
fn spawn_retrying(command: &mut Command) -> std::io::Result<std::process::Child> {
    let mut attempts = 0;
    loop {
        match command.spawn() {
            Err(error) if error.raw_os_error() == Some(libc::ETXTBSY) && attempts < 20 => {
                attempts += 1;
                std::thread::sleep(Duration::from_millis(10));
            }
            other => return other,
        }
    }
}

/// Runs `bin args...` with stdin closed and stdout and stderr captured.
pub fn run_cli<S: AsRef<OsStr>>(
    bin: &str,
    args: &[S],
    options: &RunOptions,
) -> Result<CliOutput, ExecError> {
    if options.cancel.is_some_and(CancelToken::is_cancelled) {
        return Err(ExecError::Cancelled(CliOutput::default()));
    }
    let mut command = Command::new(bin);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    if let Some(env) = options.env {
        command.env_clear().envs(env);
    }
    if let Some(cwd) = options.cwd {
        command.current_dir(cwd);
    }
    let mut child = spawn_retrying(&mut command).map_err(ExecError::Spawn)?;
    let stdout = drain(child.stdout.take().expect("piped stdout"));
    let stderr = drain(child.stderr.take().expect("piped stderr"));
    let started = Instant::now();
    let mut interrupted: Option<bool> = None;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(error) => {
                kill_group(child.id());
                let _ = child.wait();
                return Err(ExecError::Spawn(error));
            }
        }
        let cancelled = options.cancel.is_some_and(CancelToken::is_cancelled);
        if cancelled || started.elapsed() >= options.timeout {
            interrupted = Some(cancelled);
            kill_group(child.id());
            match child.wait() {
                Ok(status) => break status,
                Err(error) => return Err(ExecError::Spawn(error)),
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    // The group is gone, so the pipes close at once unless something escaped it.
    let grace = Instant::now();
    while grace.elapsed() < PIPE_GRACE
        && !(stdout.done.load(Ordering::SeqCst) && stderr.done.load(Ordering::SeqCst))
    {
        std::thread::sleep(Duration::from_millis(2));
    }
    let output = CliOutput {
        status: status.code(),
        signal: status.signal(),
        stdout: stdout.text(),
        stderr: stderr.text(),
    };
    match interrupted {
        Some(true) => Err(ExecError::Cancelled(output)),
        Some(false) => Err(ExecError::TimedOut(output)),
        None => Ok(output),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shell(script: &str, options: &RunOptions) -> Result<CliOutput, ExecError> {
        run_cli("sh", &["-c", script], options)
    }

    #[test]
    fn captures_both_streams_and_the_exit_code() {
        let output = shell(
            "printf out; printf err >&2; exit 3",
            &RunOptions::new(Duration::from_secs(10)),
        )
        .unwrap();
        assert_eq!((output.status, output.signal), (Some(3), None));
        assert_eq!(
            (output.stdout.as_str(), output.stderr.as_str()),
            ("out", "err")
        );
        assert_eq!(output.output(), "outerr");
        assert!(!output.success());
        assert!(shell("true", &RunOptions::new(Duration::from_secs(10)))
            .unwrap()
            .success());
    }

    #[test]
    fn a_script_that_is_briefly_open_for_writing_is_retried_instead_of_failing() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("tool");
        std::fs::write(&script, "#!/bin/sh\nprintf ok\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        // While a write handle is open, executing the file fails with ETXTBSY.
        let handle = std::fs::OpenOptions::new()
            .append(true)
            .open(&script)
            .unwrap();
        let closer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(60));
            drop(handle);
        });
        let output = run_cli(
            script.to_str().unwrap(),
            &[] as &[&str],
            &RunOptions::new(Duration::from_secs(10)),
        )
        .unwrap();
        closer.join().unwrap();
        assert_eq!(output.stdout, "ok");
    }

    #[test]
    fn a_missing_program_is_a_spawn_error() {
        let result = run_cli(
            "/definitely/not/a/program",
            &["x"],
            &RunOptions::new(Duration::from_secs(1)),
        );
        assert!(
            matches!(result, Err(ExecError::Spawn(error)) if error.kind() == std::io::ErrorKind::NotFound)
        );
    }

    #[test]
    fn a_timeout_kills_the_whole_process_group_and_keeps_the_partial_output() {
        let started = Instant::now();
        // The inner sleep would hold the pipes open for a minute if only the shell were killed.
        let result = shell(
            "echo early; sleep 60 & sleep 60",
            &RunOptions::new(Duration::from_millis(300)),
        );
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "returned after {:?}",
            started.elapsed()
        );
        match result {
            Err(ExecError::TimedOut(output)) => assert_eq!(output.stdout, "early\n"),
            other => panic!("expected a timeout, got {other:?}"),
        }
    }

    #[test]
    fn cancelling_stops_a_running_command() {
        let token = CancelToken::new();
        let trigger = token.clone();
        let canceller = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(150));
            trigger.cancel();
        });
        let options = RunOptions {
            env: None,
            cwd: None,
            timeout: Duration::from_secs(30),
            cancel: Some(&token),
        };
        let started = Instant::now();
        assert!(matches!(
            shell("sleep 30", &options),
            Err(ExecError::Cancelled(_))
        ));
        assert!(started.elapsed() < Duration::from_secs(5));
        canceller.join().unwrap();
        assert!(
            matches!(shell("true", &options), Err(ExecError::Cancelled(_))),
            "already cancelled before the start"
        );
    }

    #[test]
    fn env_and_cwd_are_passed_through() {
        let dir = tempfile::tempdir().unwrap();
        let env = Env::from([
            ("PATH".to_string(), std::env::var("PATH").unwrap()),
            ("ONLY_THIS".to_string(), "yes".to_string()),
        ]);
        let options = RunOptions {
            env: Some(&env),
            cwd: Some(dir.path()),
            timeout: Duration::from_secs(10),
            cancel: None,
        };
        let output = shell(
            "printf '%s %s %s' \"$ONLY_THIS\" \"${HOME-unset}\" \"$(pwd -P)\"",
            &options,
        )
        .unwrap();
        let expected_dir = std::fs::canonicalize(dir.path()).unwrap();
        assert_eq!(
            output.stdout,
            format!("yes unset {}", expected_dir.display())
        );
    }

    #[test]
    fn large_output_is_drained_without_deadlock_and_invalid_utf8_is_replaced() {
        let output = shell(
            "head -c 3000000 /dev/zero | tr '\\0' 'a'; printf '\\377'",
            &RunOptions::new(Duration::from_secs(20)),
        )
        .unwrap();
        assert_eq!(output.stdout.len(), 3_000_000 + '\u{fffd}'.len_utf8());
        assert!(output.stdout.ends_with('\u{fffd}'));
    }
}
