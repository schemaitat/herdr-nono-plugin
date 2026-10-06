//! Signal handling for a bridge that supervises an interactive child. The
//! terminal sends Ctrl-C and Ctrl-\ to the whole foreground process group, so
//! the child already gets them; the bridge must survive them to record how the
//! child ended. SIGTERM and SIGHUP are recorded and forwarded by the waiting
//! loop. Handlers are functions, not `SIG_IGN`: an ignored signal would be
//! inherited by a program started later, a handled one is reset by exec.

use std::sync::atomic::{AtomicI32, Ordering};

static PENDING: AtomicI32 = AtomicI32::new(0);

extern "C" fn swallow(_signal: libc::c_int) {}

extern "C" fn record(signal: libc::c_int) {
    PENDING.store(signal, Ordering::SeqCst);
}

fn install(signal: libc::c_int, handler: extern "C" fn(libc::c_int)) {
    // SAFETY: the handlers only touch an atomic or do nothing, which is async-signal-safe.
    unsafe {
        libc::signal(signal, handler as libc::sighandler_t);
    }
}

/// While alive, Ctrl-C and Ctrl-\ do not end this process; with `forward`, SIGTERM
/// and SIGHUP are recorded for [`take_pending`] instead of ending it.
pub struct SignalGuard {
    forward: bool,
}

impl SignalGuard {
    pub fn install(forward: bool) -> SignalGuard {
        PENDING.store(0, Ordering::SeqCst);
        install(libc::SIGINT, swallow);
        install(libc::SIGQUIT, swallow);
        if forward {
            install(libc::SIGTERM, record);
            install(libc::SIGHUP, record);
        }
        SignalGuard { forward }
    }
}

impl Drop for SignalGuard {
    fn drop(&mut self) {
        let mut restore = vec![libc::SIGINT, libc::SIGQUIT];
        if self.forward {
            restore.extend([libc::SIGTERM, libc::SIGHUP]);
        }
        for signal in restore {
            // SAFETY: restoring the default disposition.
            unsafe {
                libc::signal(signal, libc::SIG_DFL);
            }
        }
    }
}

/// The SIGTERM or SIGHUP received since the last call, if any.
pub fn take_pending() -> Option<i32> {
    match PENDING.swap(0, Ordering::SeqCst) {
        0 => None,
        signal => Some(signal),
    }
}

/// Sends `signal` to a process; false when it is gone.
pub fn send_signal(pid: u32, signal: i32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    // SAFETY: plain kill(2) on a pid we spawned.
    unsafe { libc::kill(pid, signal) == 0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_recorded_signal_is_taken_once_and_the_guard_swallows_interrupts() {
        let _guard = SignalGuard::install(true);
        assert_eq!(take_pending(), None);
        // SAFETY: raising a signal whose handler is installed above.
        unsafe {
            libc::raise(libc::SIGTERM);
        }
        assert_eq!(take_pending(), Some(libc::SIGTERM));
        assert_eq!(take_pending(), None);
        // SAFETY: SIGINT is swallowed while the guard lives, so this does not end the test process.
        unsafe {
            libc::raise(libc::SIGINT);
        }
        assert_eq!(take_pending(), None, "interrupts are not forwarded");
    }

    #[test]
    fn send_signal_reports_a_missing_process() {
        assert!(!send_signal(2_147_483_647, 0));
        assert!(send_signal(std::process::id(), 0));
    }
}
