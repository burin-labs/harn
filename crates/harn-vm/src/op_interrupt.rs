//! Cooperative interrupt observation for blocking sync builtins.
//!
//! Sync builtins (including every subprocess-spawning path: the hostlib
//! `run_command` tool family and the VM-side `process.exec`/`exec_opts`
//! builtins) execute inline on the VM's async task. While one of them
//! blocks — typically waiting on a child process — the interpreter's
//! `tokio::select!` cancel/deadline race in
//! `vm/execution.rs::execute_op_with_scope_interrupts` cannot run: the op
//! future never yields, so scope cancellation, `deadline` expiry, and host
//! aborts used to wait for the child to exit on its own (orphaning it on
//! task abort / VM drop).
//!
//! This module closes that gap cooperatively. Before invoking a sync
//! builtin, the VM installs the *currently armed* interrupt sources — its
//! host cancel token (`Arc<AtomicBool>`) and the innermost deadline — into
//! a thread-local via [`install`]. Blocking wait loops poll [`requested`]
//! (they already poll `try_wait` every ~20ms) and, when it fires,
//! gracefully terminate their child process group (SIGTERM, then SIGKILL
//! after [`SUBPROCESS_TERM_GRACE`]) and return. The VM then surfaces the
//! ordinary cancellation / deadline error at the next op boundary.
//!
//! Trigger coverage:
//! - **Scope / `parallel` cancellation and VM drop**: spawned-task child
//!   VMs share the `Arc<AtomicBool>` stored in their `VmTaskHandle`;
//!   `Vm::cancel_spawned_tasks` (also called from `Drop for Vm`) sets it,
//!   which the blocked wait loop observes.
//! - **Host abort**: hosts cancel a VM by setting its cancel token — same
//!   observation path.
//! - **`deadline` expiry**: the deadline `Instant` is captured when the
//!   builtin starts; the wait loop compares against `Instant::now()`.

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How long a subprocess gets to exit after SIGTERM before the whole
/// process group is SIGKILLed. Deliberately longer than the interpreter's
/// 250ms async-op cancel grace (`CANCEL_GRACE_ASYNC_OP`): child processes
/// often need to flush buffers / remove lock files on SIGTERM.
pub const SUBPROCESS_TERM_GRACE: Duration = Duration::from_secs(2);

#[derive(Clone, Default)]
struct OpInterrupt {
    cancel: Option<Arc<AtomicBool>>,
    deadline: Option<Instant>,
}

thread_local! {
    static CURRENT: RefCell<Option<OpInterrupt>> = const { RefCell::new(None) };
}

/// Guard returned by [`install`]. Restores the previously installed
/// interrupt context on drop so nested builtin dispatch (child VMs running
/// on the same thread) composes correctly.
pub struct OpInterruptGuard {
    // Outer Option = "guard owes a restore"; inner Option is the previous
    // thread-local slot value (which can itself be None).
    #[allow(clippy::option_option)]
    prev: Option<Option<OpInterrupt>>,
}

impl Drop for OpInterruptGuard {
    fn drop(&mut self) {
        if let Some(prev) = self.prev.take() {
            CURRENT.with(|slot| *slot.borrow_mut() = prev);
        }
    }
}

/// Install the interrupt sources a blocking builtin on this thread should
/// observe: an optional cooperative cancel token and an optional deadline.
/// The VM calls this around sync builtin dispatch; tests use it to simulate
/// scope cancellation without booting a full interpreter.
pub fn install(cancel: Option<Arc<AtomicBool>>, deadline: Option<Instant>) -> OpInterruptGuard {
    let prev = CURRENT.with(|slot| slot.borrow_mut().replace(OpInterrupt { cancel, deadline }));
    OpInterruptGuard { prev: Some(prev) }
}

/// Returns `true` when the interrupt context installed on this thread has
/// fired: the cancel token is set, or the deadline has passed. Cheap enough
/// to call from a ~20ms poll loop. Returns `false` when nothing is armed.
pub fn requested() -> bool {
    CURRENT.with(|slot| {
        let ctx = slot.borrow();
        let Some(ctx) = ctx.as_ref() else {
            return false;
        };
        if ctx
            .cancel
            .as_ref()
            .is_some_and(|token| token.load(Ordering::SeqCst))
        {
            return true;
        }
        ctx.deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
    })
}

/// Put the child in its own process group (`setpgid(0, 0)`) so a later
/// group signal reaps grandchildren too. No-op on non-Unix targets — group
/// semantics are Unix-first; Windows callers fall back to killing the
/// direct child handle (`TerminateProcess` via `Child::kill`).
pub fn configure_kill_group(command: &mut std::process::Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    #[cfg(not(unix))]
    {
        let _ = command;
    }
}

/// Signal a pid and its process group. No-op on non-Unix targets.
pub fn signal_pid_and_group(pid: u32, signal: i32) {
    #[cfg(unix)]
    {
        // SAFETY: kill(2) takes a pid_t (i32 on all Unix targets) and a
        // signal number; calling it with any valid signal is well-defined.
        extern "C" {
            fn kill(pid: i32, sig: i32) -> i32;
        }
        unsafe {
            kill(-(pid as i32), signal);
            kill(pid as i32, signal);
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (pid, signal);
    }
}

/// How an interruptible child wait ended.
pub enum ChildWait {
    /// The child exited on its own.
    Exited(std::process::ExitStatus),
    /// The caller-supplied timeout elapsed; the child (group) was killed.
    TimedOut,
    /// [`requested`] fired; the child group was SIGTERMed and, after
    /// [`SUBPROCESS_TERM_GRACE`], SIGKILLed. Carries the reaped status when
    /// the OS reported one.
    Interrupted(Option<std::process::ExitStatus>),
}

/// Wait for `child` while polling [`requested`] and the optional timeout.
///
/// Used by the VM-side `process.*` builtins (`exec`, `shell`, `exec_opts`,
/// `spawn_captured`). The hostlib `run_command` family implements the same
/// protocol inside its `ProcessSpawner` abstraction. Callers should have
/// spawned the child with [`configure_kill_group`] so the group signals
/// reach grandchildren.
pub fn wait_child_interruptible(
    child: &mut std::process::Child,
    timeout: Option<Duration>,
) -> std::io::Result<ChildWait> {
    let deadline = timeout.map(|limit| Instant::now() + limit);
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(ChildWait::Exited(status));
        }
        if requested() {
            let status = terminate_child_group(child);
            return Ok(ChildWait::Interrupted(status));
        }
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            // Timeout keeps its historical semantics: immediate SIGKILL.
            if let Some(pid) = child_pid(child) {
                signal_pid_and_group(pid, 9);
            }
            let _ = child.kill();
            let _ = child.wait();
            return Ok(ChildWait::TimedOut);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Gracefully terminate `child` and its process group: SIGTERM, wait up to
/// [`SUBPROCESS_TERM_GRACE`], then SIGKILL. Reaps the child and returns its
/// exit status when available. On non-Unix targets this is a best-effort
/// direct `Child::kill` (`TerminateProcess`), which does not reach
/// grandchildren.
pub fn terminate_child_group(child: &mut std::process::Child) -> Option<std::process::ExitStatus> {
    #[cfg(unix)]
    {
        if let Some(pid) = child_pid(child) {
            const SIGTERM: i32 = 15;
            signal_pid_and_group(pid, SIGTERM);
            let grace_deadline = Instant::now() + SUBPROCESS_TERM_GRACE;
            loop {
                match child.try_wait() {
                    Ok(Some(status)) => {
                        // The direct child is gone, but SIGTERM-immune
                        // descendants may linger — sweep the group.
                        signal_pid_and_group(pid, 9);
                        return Some(status);
                    }
                    Ok(None) => {
                        if Instant::now() >= grace_deadline {
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(20));
                    }
                    Err(_) => break,
                }
            }
            signal_pid_and_group(pid, 9);
        }
    }
    let _ = child.kill();
    child.wait().ok()
}

fn child_pid(child: &std::process::Child) -> Option<u32> {
    let pid = child.id();
    (pid > 0).then_some(pid)
}

/// Collect one captured pipe from a drain thread that sends the full buffer
/// on EOF.
///
/// `killed == true` (the child group was already signalled) keeps a 100ms
/// best-effort window for partial output. Otherwise wait for EOF like
/// `Command::output` would — but keep observing [`requested`], because a
/// lingering grandchild that inherited the pipe can hold it open long after
/// the direct child exited; on interrupt the group gets the same SIGTERM →
/// grace → SIGKILL treatment.
pub(crate) fn drain_captured_pipe(
    rx: &std::sync::mpsc::Receiver<Vec<u8>>,
    killed: bool,
    child_pid: u32,
) -> Vec<u8> {
    use std::sync::mpsc::RecvTimeoutError;
    if killed {
        return rx
            .recv_timeout(Duration::from_millis(100))
            .unwrap_or_default();
    }
    loop {
        match rx.recv_timeout(Duration::from_millis(20)) {
            Ok(buf) => return buf,
            Err(RecvTimeoutError::Disconnected) => return Vec::new(),
            Err(RecvTimeoutError::Timeout) => {
                if requested() {
                    const SIGTERM: i32 = 15;
                    signal_pid_and_group(child_pid, SIGTERM);
                    if let Ok(buf) = rx.recv_timeout(SUBPROCESS_TERM_GRACE) {
                        signal_pid_and_group(child_pid, 9);
                        return buf;
                    }
                    signal_pid_and_group(child_pid, 9);
                    return rx
                        .recv_timeout(Duration::from_millis(100))
                        .unwrap_or_default();
                }
            }
        }
    }
}

/// Spawn a drain thread that reads `reader` to EOF and sends the buffer.
pub(crate) fn spawn_pipe_drain<R: std::io::Read + Send + 'static>(
    mut reader: R,
) -> std::sync::mpsc::Receiver<Vec<u8>> {
    let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = reader.read_to_end(&mut buf);
        let _ = tx.send(buf);
    });
    rx
}

/// Interrupt-aware replacement for `Command::output()`: the child runs in
/// its own kill group, stdout/stderr are captured in full, stdin is closed,
/// and the wait polls [`requested`]. When an interrupt fires the whole
/// group is gracefully terminated and the (signal-terminated) status is
/// returned — the interpreter surfaces the pending cancellation / deadline
/// error at the next op boundary.
pub fn capture_output_interruptible(
    command: &mut std::process::Command,
) -> std::io::Result<std::process::Output> {
    use std::process::Stdio;
    command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null());
    configure_kill_group(command);
    let mut child = command.spawn()?;
    let pid = child.id();
    let rx_out = child.stdout.take().map(spawn_pipe_drain);
    let rx_err = child.stderr.take().map(spawn_pipe_drain);

    let (status, killed) = match wait_child_interruptible(&mut child, None)? {
        ChildWait::Exited(status) => (status, false),
        // No timeout is armed here, but keep the arm total.
        ChildWait::TimedOut => (std::process::ExitStatus::default(), true),
        ChildWait::Interrupted(status) => (status.unwrap_or_default(), true),
    };
    let stdout = rx_out
        .map(|rx| drain_captured_pipe(&rx, killed, pid))
        .unwrap_or_default();
    let stderr = rx_err
        .map(|rx| drain_captured_pipe(&rx, killed, pid))
        .unwrap_or_default();
    Ok(std::process::Output {
        status,
        stdout,
        stderr,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requested_is_false_without_context() {
        assert!(!requested());
    }

    #[test]
    fn cancel_token_trips_requested_and_guard_restores() {
        let token = Arc::new(AtomicBool::new(false));
        let guard = install(Some(token.clone()), None);
        assert!(!requested());
        token.store(true, Ordering::SeqCst);
        assert!(requested());
        drop(guard);
        assert!(!requested());
    }

    #[test]
    fn deadline_trips_requested() {
        let expired = Instant::now()
            .checked_sub(Duration::from_millis(1))
            .expect("monotonic clock supports a 1ms test lookback");
        let _guard = install(None, Some(expired));
        assert!(requested());
    }

    #[test]
    fn nested_installs_restore_in_order() {
        let outer_token = Arc::new(AtomicBool::new(true));
        let _outer = install(Some(outer_token), None);
        assert!(requested());
        {
            let _inner = install(None, None);
            assert!(!requested());
        }
        assert!(requested());
    }

    #[cfg(unix)]
    #[test]
    fn interrupted_wait_kills_process_group() {
        // Child spawns a grandchild; the whole group must die on interrupt.
        let mut command = std::process::Command::new("sh");
        command.args(["-c", "sleep 30 & wait"]);
        configure_kill_group(&mut command);
        let mut child = command.spawn().expect("spawn sh");
        let pgid = child.id();

        let cancel = Arc::new(AtomicBool::new(true));
        let _guard = install(Some(cancel), None);
        let started = Instant::now();
        let outcome = wait_child_interruptible(&mut child, None).expect("wait");
        assert!(matches!(outcome, ChildWait::Interrupted(_)));
        assert!(started.elapsed() < Duration::from_secs(10));

        // kill(-pgid, 0) fails with ESRCH once every member is gone.
        extern "C" {
            fn kill(pid: i32, sig: i32) -> i32;
        }
        let group_gone = || unsafe { kill(-(pgid as i32), 0) } != 0;
        let deadline = Instant::now() + Duration::from_secs(5);
        while !group_gone() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(group_gone(), "process group {pgid} survived interrupt");
    }
}
