//! Cancellable subprocess execution.
//!
//! The analyzer and Demucs run as blocking child processes that spawn their own
//! grandchildren (uv → python → torch). To stop one cleanly we launch it in its
//! own process group (`setpgid(0,0)`) and kill the whole group with `killpg`, so
//! nothing is orphaned and the GPU/VRAM it held is freed. `CancelToken` is the
//! shared kill switch: the worker thread runs the command through
//! `run_cancellable`, another thread flips the token.

use std::process::{Command, Output};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// Shared kill switch for one cancellable subprocess run. Clone it freely; every
/// clone points at the same state.
#[derive(Clone, Default)]
pub struct CancelToken(Arc<Inner>);

#[derive(Default)]
struct Inner {
    cancelled: AtomicBool,
    /// The running child's process-group id, set once spawned and cleared when
    /// it exits. `None` before spawn and after wait.
    pgid: Mutex<Option<i32>>,
}

/// What a cancellable run produced.
pub enum Outcome {
    /// Ran to completion (the exit status may still be a failure).
    Done(Output),
    /// The token was cancelled before spawn, or the child was killed mid-run.
    Cancelled,
    /// The process could not be spawned or waited on.
    Err(std::io::Error),
}

impl CancelToken {
    pub fn is_cancelled(&self) -> bool {
        self.0.cancelled.load(Ordering::SeqCst)
    }

    /// Request cancellation: set the flag and, if a child is currently running,
    /// SIGKILL its whole process group. Safe to call any number of times and
    /// before/after the run; a kill against an already-gone group is a no-op.
    pub fn cancel(&self) {
        self.0.cancelled.store(true, Ordering::SeqCst);
        #[cfg(unix)]
        if let Some(pgid) = *self.0.pgid.lock().unwrap() {
            // SAFETY: killpg on a pgid we created as a group leader; ESRCH when
            // the group already exited is ignored.
            unsafe {
                libc::killpg(pgid, libc::SIGKILL);
            }
        }
    }
}

/// Run `cmd` to completion in its own process group, honoring `token`. stdout
/// and stderr are captured (as `Command::output` does).
#[cfg(unix)]
pub fn run_cancellable(mut cmd: Command, token: &CancelToken) -> Outcome {
    use std::os::unix::process::CommandExt;

    if token.is_cancelled() {
        return Outcome::Cancelled;
    }
    // Own process group so a cancel can killpg the whole tree, plus the existing
    // die-with-parent arming so an abrupt dredge exit still reaps the child.
    // SAFETY: only async-signal-safe calls run in the child between fork and exec.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setpgid(0, 0) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    crate::stems::die_with_parent(&mut cmd);
    // No tty stdin: the child runs in a background process group, and any
    // grandchild that reads the terminal (torchcodec's ffmpeg does) would get
    // SIGTTIN and suspend the whole job when dredge was launched from a shell.
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    let child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return Outcome::Err(e),
    };
    // The child is its own group leader, so its pid is the process-group id.
    let pgid = child.id() as i32;
    *token.0.pgid.lock().unwrap() = Some(pgid);
    // Race guard: a cancel that landed between the pre-spawn check and here still
    // has to kill the child we just started (its pgid was `None` when cancel ran).
    if token.is_cancelled() {
        unsafe {
            libc::killpg(pgid, libc::SIGKILL);
        }
    }

    let waited = child.wait_with_output();
    *token.0.pgid.lock().unwrap() = None;
    match waited {
        Ok(out) if !token.is_cancelled() => Outcome::Done(out),
        Ok(_) => Outcome::Cancelled,
        Err(_) if token.is_cancelled() => Outcome::Cancelled,
        Err(e) => Outcome::Err(e),
    }
}

/// Non-unix fallback: no process-group control, cancellation degrades to a
/// pre-spawn check only. dredge targets Linux, so this exists for completeness.
#[cfg(not(unix))]
pub fn run_cancellable(mut cmd: Command, token: &CancelToken) -> Outcome {
    if token.is_cancelled() {
        return Outcome::Cancelled;
    }
    match cmd.output() {
        Ok(out) => Outcome::Done(out),
        Err(e) => Outcome::Err(e),
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    fn sh(script: &str) -> Command {
        let mut c = Command::new("/bin/sh");
        c.arg("-c").arg(script);
        c
    }

    #[test]
    fn done_captures_output() {
        let token = CancelToken::default();
        match run_cancellable(sh("printf hello"), &token) {
            Outcome::Done(out) => assert_eq!(out.stdout, b"hello"),
            _ => panic!("expected Done"),
        }
        assert!(!token.is_cancelled());
    }

    #[test]
    fn cancel_before_spawn_never_runs() {
        // A sentinel file the script would touch if it ever executed.
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("ran");
        let token = CancelToken::default();
        token.cancel();
        let script = format!("touch {}", marker.display());
        assert!(matches!(
            run_cancellable(sh(&script), &token),
            Outcome::Cancelled
        ));
        std::thread::sleep(Duration::from_millis(50));
        assert!(!marker.exists(), "command must not have run");
    }

    #[test]
    fn cancel_mid_run_kills_the_process() {
        let token = CancelToken::default();
        let done = Arc::new(AtomicBool::new(false));
        let started = Instant::now();
        let t = {
            let token = token.clone();
            let done = done.clone();
            std::thread::spawn(move || {
                // Would run for 30 s if not killed.
                let outcome = run_cancellable(sh("sleep 30"), &token);
                done.store(true, Ordering::SeqCst);
                matches!(outcome, Outcome::Cancelled)
            })
        };
        // Let it spawn, then cancel.
        std::thread::sleep(Duration::from_millis(150));
        token.cancel();
        let was_cancelled = t.join().unwrap();
        assert!(was_cancelled, "outcome should be Cancelled");
        assert!(done.load(Ordering::SeqCst));
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "kill should be prompt"
        );
    }
}
