//! Interrupt, deadline, and signal handling for `harn run`.
//!
//! A run can be cut short two ways: a wall-clock deadline (`--timeout`) or an
//! OS signal (SIGINT/SIGTERM/SIGHUP). Both funnel through the same
//! [`RunInterruptTokens`] so the VM and any in-flight subprocesses are asked to
//! unwind once, with a hard-exit backstop if the grace period elapses. Split
//! out of the parent module so this cross-cutting shutdown concern lives in one
//! auditable unit rather than interleaved with the run driver.

use std::process;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::RunInterruptTokens;

const FIRST_SIGNAL_MESSAGE: &str =
    "[harn] signal received, interrupting VM (give it a moment to unwind in-flight async ops; Ctrl-C again to force-exit)...";
const RUN_TIMEOUT_MESSAGE: &str =
    "[harn] run timeout reached, interrupting VM and in-flight subprocesses...";
const RUN_TIMEOUT_HARD_EXIT_MESSAGE: &str = "[harn] run timeout grace elapsed, terminating";
const SIGTERM: i32 = 15;
const SIGKILL: i32 = 9;

pub(super) struct RunDeadlineGuard {
    finished: Arc<AtomicBool>,
    timed_out: Arc<AtomicBool>,
}

impl RunDeadlineGuard {
    pub(super) fn finish(&self) {
        self.finished.store(true, Ordering::SeqCst);
    }

    pub(super) fn timed_out(&self) -> bool {
        self.timed_out.load(Ordering::SeqCst)
    }
}

impl Drop for RunDeadlineGuard {
    fn drop(&mut self) {
        self.finish();
    }
}

pub(super) fn start_run_deadline_watchdog(
    timeout: Duration,
    tokens: RunInterruptTokens,
) -> RunDeadlineGuard {
    let finished = Arc::new(AtomicBool::new(false));
    let timed_out = Arc::new(AtomicBool::new(false));
    let task_finished = Arc::clone(&finished);
    let task_timed_out = Arc::clone(&timed_out);
    tokio::spawn(async move {
        tokio::time::sleep(timeout).await;
        if task_finished.load(Ordering::SeqCst) {
            return;
        }
        task_timed_out.store(true, Ordering::SeqCst);
        request_vm_interrupt(&tokens, "timeout");
        eprintln!("{RUN_TIMEOUT_MESSAGE}");
        tokio::time::sleep(harn_vm::op_interrupt::SUBPROCESS_TERM_GRACE).await;
        if !task_finished.load(Ordering::SeqCst) {
            signal_run_process_cleanups(&tokens, SIGKILL);
            eprintln!("{RUN_TIMEOUT_HARD_EXIT_MESSAGE}");
            process::exit(124);
        }
    });
    RunDeadlineGuard {
        finished,
        timed_out,
    }
}

pub(super) fn install_signal_shutdown_handler() -> RunInterruptTokens {
    let tokens = RunInterruptTokens {
        cancel_token: Arc::new(AtomicBool::new(false)),
        signal_token: Arc::new(Mutex::new(None)),
    };
    let tokens_clone = tokens.clone();
    tokio::spawn(async move {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{signal, SignalKind};
            // Containers without controlling-tty access can refuse signal()
            // registration. In that case, degrade to no-signal mode rather
            // than crashing the `harn run` script — the script still runs,
            // it just can't be interrupted cleanly.
            let sigterm_stream = signal(SignalKind::terminate()).ok();
            let sigint_stream = signal(SignalKind::interrupt()).ok();
            let sighup_stream = signal(SignalKind::hangup()).ok();
            let (mut sigterm, mut sigint, mut sighup) =
                match (sigterm_stream, sigint_stream, sighup_stream) {
                    (Some(t), Some(i), Some(h)) => (t, i, h),
                    _ => {
                        eprintln!(
                            "[harn] signal handlers unavailable in this environment; \
                             continuing without graceful-shutdown interception"
                        );
                        return;
                    }
                };
            let mut seen_signal = false;
            loop {
                let signal_name = tokio::select! {
                    _ = sigterm.recv() => "SIGTERM",
                    _ = sigint.recv() => "SIGINT",
                    _ = sighup.recv() => "SIGHUP",
                };
                if seen_signal {
                    eprintln!("[harn] second signal received, terminating");
                    process::exit(124);
                }
                seen_signal = true;
                request_vm_interrupt(&tokens_clone, signal_name);
                eprintln!("{FIRST_SIGNAL_MESSAGE}");
            }
        }
        #[cfg(not(unix))]
        {
            let mut seen_signal = false;
            loop {
                let _ = tokio::signal::ctrl_c().await;
                if seen_signal {
                    eprintln!("[harn] second signal received, terminating");
                    process::exit(124);
                }
                seen_signal = true;
                request_vm_interrupt(&tokens_clone, "SIGINT");
                eprintln!("{FIRST_SIGNAL_MESSAGE}");
            }
        }
    });
    tokens
}

fn request_vm_interrupt(tokens: &RunInterruptTokens, signal_name: &str) {
    if let Ok(mut signal) = tokens.signal_token.lock() {
        *signal = Some(signal_name.to_string());
    }
    tokens.cancel_token.store(true, Ordering::SeqCst);
    signal_run_process_cleanups(tokens, SIGTERM);
}

fn signal_run_process_cleanups(tokens: &RunInterruptTokens, signal: i32) {
    let mut report = harn_vm::op_interrupt::signal_active_process_cleanups_for_cancel_token(
        signal,
        &tokens.cancel_token,
    );
    // Older/integration-created entries may not have an owner token. Keep the
    // fail-closed fallback for the standalone `harn run` process, while the
    // token path above prevents scoped VM runs from becoming process-global.
    if report.root_pid.is_none() && report.children.is_empty() {
        report = harn_vm::op_interrupt::signal_ownerless_active_process_cleanups(signal);
    }
    drop(report);
}
