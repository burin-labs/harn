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
//! gracefully terminate their child process tree/group (SIGTERM, then SIGKILL
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
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

/// Private environment marker inherited by subprocess descendants. Cleanup uses
/// this token to rediscover escaped descendants that have reparented or moved to
/// a different process group before the parent-edge scan runs.
pub const PROCESS_CLEANUP_TOKEN_ENV: &str = "HARN_PROCESS_CLEANUP_TOKEN";

/// Marker shared by every process in one externally supervised lifetime.
///
/// Individual process operations keep using [`PROCESS_CLEANUP_TOKEN_ENV`] so
/// cancelling one operation does not terminate its siblings. A native
/// owner-death guardian sets this second marker on its payload; nested process
/// boundaries preserve it even when they otherwise replace the environment.
/// The guardian can therefore find detached grandchildren after the immediate
/// payload or an intermediate shell has exited.
pub const PROCESS_OWNER_TOKEN_ENV: &str = "HARN_INTERNAL_PROCESS_OWNER_TOKEN";

/// How long a subprocess gets to exit after SIGTERM before the whole
/// process group is SIGKILLed. Deliberately longer than the interpreter's
/// 250ms async-op cancel grace (`CANCEL_GRACE_ASYNC_OP`): child processes
/// often need to flush buffers / remove lock files on SIGTERM.
pub const SUBPROCESS_TERM_GRACE: Duration = Duration::from_secs(2);
#[cfg(unix)]
const SUBPROCESS_KILL_SETTLE: Duration = Duration::from_millis(250);

pub fn new_process_cleanup_token() -> String {
    format!("harn-cleanup-{}", uuid::Uuid::now_v7().simple())
}

fn owner_process_group_journal(token: &str) -> std::path::PathBuf {
    let digest = blake3::hash(token.as_bytes()).to_hex();
    std::env::temp_dir().join(format!("harn-process-owner-{digest}.groups"))
}

/// Create the owner journal before untrusted payload code can observe its token.
pub fn initialize_process_owner_group_journal(token: &str) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;

        std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(owner_process_group_journal(token))
            .map(|_| ())
    }
    #[cfg(not(unix))]
    {
        let _ = token;
        Ok(())
    }
}

/// Persist the process group created for `pid` in the current owner lifetime.
///
/// The native guardian reads this append-only journal after abrupt owner death,
/// when in-memory cleanup registrations no longer exist. Process groups are the
/// portable baseline; Linux additionally uses the inherited token to find
/// descendants that deliberately escape their original group.
pub fn record_current_process_owner_group(pid: u32) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;

        let Ok(token) = std::env::var(PROCESS_OWNER_TOKEN_ENV) else {
            return Ok(());
        };
        let observed_pgid = unsafe { libc::getpgid(pid as i32) };
        let pgid = u32::try_from(observed_pgid).unwrap_or(pid);
        let mut journal = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(owner_process_group_journal(&token))?;
        journal.write_all(format!("{pgid}\n").as_bytes())
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        Ok(())
    }
}

/// Record a Tokio child's owner group or terminate it before returning error.
pub async fn record_tokio_process_owner_group(
    child: &mut tokio::process::Child,
    cleanup_token: &str,
) -> std::io::Result<()> {
    let Some(pid) = child.id() else {
        return Ok(());
    };
    if let Err(error) = record_current_process_owner_group(pid) {
        let _ = signal_pid_tree_group_and_token_with_report(pid, Some(cleanup_token), 9);
        let _ = child.start_kill();
        let _ = child.wait().await;
        return Err(error);
    }
    Ok(())
}

#[cfg(unix)]
fn owner_process_groups(token: &str) -> Vec<u32> {
    let Ok(contents) = std::fs::read_to_string(owner_process_group_journal(token)) else {
        return Vec::new();
    };
    let mut groups = contents
        .lines()
        .filter_map(|line| line.parse::<u32>().ok())
        .collect::<Vec<_>>();
    groups.sort_unstable();
    groups.dedup();
    groups
}

/// Remove the current owner lifetime's process-group journal.
pub fn remove_process_owner_group_journal(token: &str) {
    let _ = std::fs::remove_file(owner_process_group_journal(token));
}

/// Preserve an inherited native owner-lifetime marker across an environment
/// replacement on `command`.
pub fn preserve_process_owner_token(command: &mut std::process::Command) {
    if let Some(token) = std::env::var_os(PROCESS_OWNER_TOKEN_ENV).filter(|token| !token.is_empty())
    {
        command.env(PROCESS_OWNER_TOKEN_ENV, token);
    }
}

/// Return live processes carrying `token` as an operation or owner marker.
///
/// The current process is excluded. This audit deliberately does not consult
/// the append-only process-group journal: exited groups can be reused by
/// unrelated processes during a large suite. The journal is safe only for the
/// guardian's abrupt-owner-death path, where reclaiming the whole lifetime
/// takes precedence over a normal terminal report.
pub fn process_owner_survivors(token: &str) -> Vec<ProcessCleanupChild> {
    #[cfg(unix)]
    {
        let mut survivors = cleanup_token_processes(token)
            .into_iter()
            .filter(|child| child.pid != std::process::id())
            .collect::<Vec<_>>();
        survivors.sort_by_key(|child| child.pid);
        survivors
    }
    #[cfg(not(unix))]
    {
        let _ = token;
        Vec::new()
    }
}

/// Structural evidence collected when Harn kills a child process tree.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProcessCleanupReport {
    pub root_pid: Option<u32>,
    pub attempted_signals: Vec<i32>,
    pub children: Vec<ProcessCleanupChild>,
}

impl ProcessCleanupReport {
    pub fn for_signal(root_pid: Option<u32>, signal: i32) -> Self {
        Self {
            root_pid,
            attempted_signals: vec![signal],
            children: Vec::new(),
        }
    }

    pub fn merge(&mut self, other: Self) {
        if self.root_pid.is_none() {
            self.root_pid = other.root_pid;
        }
        for signal in other.attempted_signals {
            push_unique(&mut self.attempted_signals, signal);
        }
        for child in other.children {
            self.merge_child(child);
        }
    }

    pub fn refresh_survivor_status(&mut self) {
        #[cfg(unix)]
        {
            for child in &mut self.children {
                child.alive_after_cleanup = Some(process_exists(child.pid));
            }
        }
    }

    fn merge_child(&mut self, child: ProcessCleanupChild) {
        if let Some(existing) = self
            .children
            .iter_mut()
            .find(|entry| entry.pid == child.pid)
        {
            for signal in child.signals {
                push_unique(&mut existing.signals, signal);
            }
            if existing.command_name.is_none() {
                existing.command_name = child.command_name;
            }
            if child.alive_after_cleanup.is_some() {
                existing.alive_after_cleanup = child.alive_after_cleanup;
            }
            return;
        }
        self.children.push(child);
        self.children
            .sort_by(|left, right| left.depth.cmp(&right.depth).then(left.pid.cmp(&right.pid)));
    }
}

/// A descendant process Harn targeted during cleanup.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessCleanupChild {
    pub pid: u32,
    pub parent_pid: Option<u32>,
    pub depth: u32,
    pub command_name: Option<String>,
    pub signals: Vec<i32>,
    pub alive_after_cleanup: Option<bool>,
}

impl ProcessCleanupChild {
    pub fn new(
        pid: u32,
        parent_pid: Option<u32>,
        depth: u32,
        command_name: Option<String>,
    ) -> Self {
        Self {
            pid,
            parent_pid,
            depth,
            command_name,
            signals: Vec::new(),
            alive_after_cleanup: None,
        }
    }

    #[cfg(unix)]
    fn with_signal(mut self, signal: i32) -> Self {
        push_unique(&mut self.signals, signal);
        self
    }
}

fn push_unique<T: Copy + Eq>(values: &mut Vec<T>, value: T) {
    if !values.contains(&value) {
        values.push(value);
    }
}

#[derive(Clone, Default)]
struct OpInterrupt {
    cancel: Option<Arc<AtomicBool>>,
    deadline: Option<Instant>,
}

thread_local! {
    static CURRENT: RefCell<Option<OpInterrupt>> = const { RefCell::new(None) };
}

#[derive(Clone, Debug)]
struct ActiveProcessCleanup {
    pid: Option<u32>,
    cleanup_token: String,
    owner_cancel_token: Option<Arc<AtomicBool>>,
}

static ACTIVE_PROCESS_CLEANUP_ID: AtomicU64 = AtomicU64::new(1);
static ACTIVE_PROCESS_CLEANUPS: LazyLock<Mutex<BTreeMap<u64, ActiveProcessCleanup>>> =
    LazyLock::new(|| Mutex::new(BTreeMap::new()));

/// Registration guard for an asynchronously waited child process. The VM's
/// sync process paths poll [`requested`] directly, but Tokio wait paths can be
/// parked inside `wait_with_output()`/`child.wait()` and need an out-of-band
/// cleanup hook when `harn run` is interrupted or reaches its run deadline.
pub struct ActiveProcessCleanupGuard {
    id: u64,
}

impl Drop for ActiveProcessCleanupGuard {
    fn drop(&mut self) {
        unregister_active_process_cleanup(self.id);
    }
}

pub fn register_active_process_cleanup(
    pid: Option<u32>,
    cleanup_token: &str,
    owner_cancel_token: Option<Arc<AtomicBool>>,
) -> ActiveProcessCleanupGuard {
    let id = ACTIVE_PROCESS_CLEANUP_ID.fetch_add(1, Ordering::SeqCst);
    ACTIVE_PROCESS_CLEANUPS
        .lock()
        .expect("active process cleanup registry poisoned")
        .insert(
            id,
            ActiveProcessCleanup {
                pid,
                cleanup_token: cleanup_token.to_string(),
                owner_cancel_token,
            },
        );
    ActiveProcessCleanupGuard { id }
}

fn unregister_active_process_cleanup(id: u64) {
    ACTIVE_PROCESS_CLEANUPS
        .lock()
        .expect("active process cleanup registry poisoned")
        .remove(&id);
}

/// Signal every actively registered async child process tree. Prefer
/// [`signal_active_process_cleanups_for_cancel_token`] or
/// [`signal_ownerless_active_process_cleanups`] when the caller can avoid a
/// process-global sweep.
pub fn signal_active_process_cleanups(signal: i32) -> ProcessCleanupReport {
    signal_active_process_cleanups_matching(signal, |_| true)
}

pub fn signal_ownerless_active_process_cleanups(signal: i32) -> ProcessCleanupReport {
    signal_active_process_cleanups_matching(signal, |entry| entry.owner_cancel_token.is_none())
}

pub fn signal_active_process_cleanups_for_cancel_token(
    signal: i32,
    cancel_token: &Arc<AtomicBool>,
) -> ProcessCleanupReport {
    signal_active_process_cleanups_matching(signal, |entry| {
        entry
            .owner_cancel_token
            .as_ref()
            .is_some_and(|owner| Arc::ptr_eq(owner, cancel_token))
    })
}

#[cfg(test)]
fn active_cleanup_tokens_for_cancel_token_for_test(cancel_token: &Arc<AtomicBool>) -> Vec<String> {
    ACTIVE_PROCESS_CLEANUPS
        .lock()
        .expect("active process cleanup registry poisoned")
        .values()
        .filter(|entry| {
            entry
                .owner_cancel_token
                .as_ref()
                .is_some_and(|owner| Arc::ptr_eq(owner, cancel_token))
        })
        .map(|entry| entry.cleanup_token.clone())
        .collect()
}

#[cfg(test)]
fn ownerless_active_cleanup_tokens_for_test() -> Vec<String> {
    ACTIVE_PROCESS_CLEANUPS
        .lock()
        .expect("active process cleanup registry poisoned")
        .values()
        .filter(|entry| entry.owner_cancel_token.is_none())
        .map(|entry| entry.cleanup_token.clone())
        .collect()
}

fn signal_active_process_cleanups_matching(
    signal: i32,
    matches_entry: impl Fn(&ActiveProcessCleanup) -> bool,
) -> ProcessCleanupReport {
    let entries = ACTIVE_PROCESS_CLEANUPS
        .lock()
        .expect("active process cleanup registry poisoned")
        .values()
        .filter(|entry| matches_entry(entry))
        .cloned()
        .collect::<Vec<_>>();
    let mut report = ProcessCleanupReport::default();
    for entry in entries {
        if let Some(pid) = entry.pid {
            report.merge(signal_pid_tree_group_and_token_with_report(
                pid,
                Some(&entry.cleanup_token),
                signal,
            ));
        }
    }
    report
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

/// Returns `true` when an interrupt context is installed on this thread.
///
/// This is separate from [`requested`] so blocking operations can decide
/// whether to use a short heartbeat poll or a true indefinite wait.
pub fn installed() -> bool {
    CURRENT.with(|slot| slot.borrow().is_some())
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

/// Put the child in its own session (`setsid()`), which also makes it the
/// leader of a fresh process group. A session boundary is stronger than a
/// bare `setpgid(0, 0)`: descendants cannot move back into Harn's session and
/// accidentally deliver a tool-owned group signal to the parent VM. Group
/// cleanup still reaches ordinary grandchildren because the child remains its
/// new process-group leader.
///
/// No-op on non-Unix targets; Windows callers use Job Objects or fall back to
/// killing the direct child handle (`TerminateProcess` via `Child::kill`).
pub fn configure_kill_group(command: &mut std::process::Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: `pre_exec` runs after fork and before exec. `setsid(2)` is an
        // async-signal-safe syscall and touches no Rust-owned memory. A freshly
        // forked child cannot already lead the parent's active process group,
        // so `setsid` is the deterministic containment boundary we require.
        unsafe {
            command.pre_exec(start_kill_session);
        }
    }
    #[cfg(not(unix))]
    {
        let _ = command;
    }
}

/// Tokio-process variant of [`configure_kill_group`]. Tokio's command wrapper
/// does not flow through `std::process::Command`, so async spawn paths must opt
/// in separately before they rely on session/tree cleanup.
pub fn configure_tokio_kill_group(command: &mut tokio::process::Command) {
    #[cfg(unix)]
    {
        // SAFETY: see `configure_kill_group`; Tokio forwards this hook to the
        // underlying `std::process::Command` pre-exec path.
        unsafe {
            command.pre_exec(start_kill_session);
        }
    }
    #[cfg(not(unix))]
    {
        let _ = command;
    }
}

#[cfg(unix)]
fn start_kill_session() -> std::io::Result<()> {
    if unsafe { libc::setsid() } == -1 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
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

/// Signal a pid, its process group, and every descendant process visible in
/// the system process table. Descendants are signalled deepest-first so a
/// child that escaped into its own process group (for example via `setsid`)
/// cannot survive a timeout merely because it left the original group.
pub fn signal_pid_tree_and_group(pid: u32, signal: i32) {
    let _ = signal_pid_tree_and_group_with_report(pid, signal);
}

/// Signal a pid, its process group, and visible descendants, returning the
/// structural targets observed before signaling.
pub fn signal_pid_tree_and_group_with_report(pid: u32, signal: i32) -> ProcessCleanupReport {
    signal_pid_tree_group_and_token_with_report(pid, None, signal)
}

/// Signal a pid, its process group, visible descendants, and any same-token
/// process that inherited Harn's cleanup marker. The token path closes the
/// reparented-descendant hole in pure parent-edge scanning: a child can `setsid`
/// and outlive its direct parent, but it keeps the inherited environment unless
/// it deliberately scrubs it.
pub fn signal_pid_tree_group_and_token_with_report(
    pid: u32,
    cleanup_token: Option<&str>,
    signal: i32,
) -> ProcessCleanupReport {
    #[cfg(unix)]
    {
        let mut report = ProcessCleanupReport::for_signal(Some(pid), signal);
        for child in descendant_processes(pid) {
            signal_pid_and_group(child.pid, signal);
            report.merge_child(child.with_signal(signal));
        }
        if let Some(cleanup_token) = cleanup_token.filter(|token| !token.is_empty()) {
            for child in cleanup_token_processes(cleanup_token) {
                if child.pid == pid {
                    continue;
                }
                signal_pid_and_group(child.pid, signal);
                report.merge_child(child.with_signal(signal));
            }
        }
        signal_pid_and_group(pid, signal);
        if signal == 9 {
            wait_for_report_children_to_exit(&report, SUBPROCESS_KILL_SETTLE);
        }
        report.refresh_survivor_status();
        report
    }
    #[cfg(not(unix))]
    {
        let _ = cleanup_token;
        ProcessCleanupReport::for_signal(Some(pid), signal)
    }
}

/// Terminate a pid, its process group, its visible descendants and its
/// cleanup-token cohort with escalation: SIGTERM, up to
/// [`SUBPROCESS_TERM_GRACE`] of grace, then SIGKILL.
///
/// This is the pid-addressed twin of
/// [`terminate_child_group_with_cleanup_token_report`], for the callers that
/// hold a pid and a cleanup token rather than a `std::process::Child`: a
/// background command handle reclaimed when its agent session ends has long
/// since handed its `Child` to a waiter thread. Those callers previously sent
/// SIGKILL directly, so a child never got the chance to flush, remove a lock
/// file, or shut a socket down. The grace period is the same constant the
/// child-handle path uses, because there is one escalation policy and it lives
/// here.
///
/// The SIGKILL sweep is unconditional rather than survivor-gated. It re-scans
/// the tree, so it costs a pass over already-dead pids and closes the case
/// where the named root exits on SIGTERM while a SIGTERM-immune descendant
/// keeps running: a survivor check on the root alone would read that as a
/// clean termination.
pub fn terminate_pid_tree_group_and_token_with_report(
    pid: u32,
    cleanup_token: Option<&str>,
) -> ProcessCleanupReport {
    #[cfg(unix)]
    {
        const SIGTERM: i32 = 15;
        let mut report = signal_pid_tree_group_and_token_with_report(pid, cleanup_token, SIGTERM);
        wait_for_pid_and_report_children_to_exit(&report, pid, SUBPROCESS_TERM_GRACE);
        report.merge(signal_pid_tree_group_and_token_with_report(
            pid,
            cleanup_token,
            9,
        ));
        report.refresh_survivor_status();
        report
    }
    #[cfg(not(unix))]
    {
        signal_pid_tree_group_and_token_with_report(pid, cleanup_token, 9)
    }
}

/// Signal a process tree and cleanup-token cohort without signaling the
/// `preserved_pgid`.
///
/// Owner-death guardians use this to kill every worker in their own group,
/// reap adopted descendants, and only then terminate the now-empty group
/// leader. Processes that escaped into another group still receive a group
/// signal.
#[cfg(unix)]
pub fn signal_pid_tree_and_token_preserving_group_with_report(
    pid: u32,
    cleanup_token: Option<&str>,
    preserved_pgid: u32,
    signal: i32,
) -> ProcessCleanupReport {
    let preserved_pgid = preserved_pgid as i32;
    let mut report = ProcessCleanupReport::for_signal(Some(pid), signal);
    for child in descendant_processes(pid) {
        signal_pid_preserving_group(child.pid, preserved_pgid, signal);
        report.merge_child(child.with_signal(signal));
    }
    if let Some(cleanup_token) = cleanup_token.filter(|token| !token.is_empty()) {
        for child in cleanup_token_processes(cleanup_token) {
            if child.pid == pid {
                continue;
            }
            signal_pid_preserving_group(child.pid, preserved_pgid, signal);
            report.merge_child(child.with_signal(signal));
        }
        for pgid in owner_process_groups(cleanup_token) {
            if pgid != preserved_pgid as u32 {
                unsafe {
                    libc::kill(-(pgid as i32), signal);
                }
            }
        }
    }
    signal_pid_preserving_group(pid, preserved_pgid, signal);
    report
}

#[cfg(unix)]
fn signal_pid_preserving_group(pid: u32, preserved_pgid: i32, signal: i32) {
    let pid = pid as i32;
    let pgid = unsafe { libc::getpgid(pid) };
    unsafe {
        if pgid > 0 && pgid != preserved_pgid {
            libc::kill(-pgid, signal);
        }
        libc::kill(pid, signal);
    }
}

#[cfg(unix)]
fn descendant_processes(root: u32) -> Vec<ProcessCleanupChild> {
    use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System};

    let mut sys = System::new();
    sys.refresh_processes_specifics(
        ProcessesToUpdate::All,
        false,
        ProcessRefreshKind::everything(),
    );
    let rows = sys
        .processes()
        .iter()
        .filter_map(|(pid, process)| {
            Some((
                pid.as_u32(),
                process.parent()?.as_u32(),
                command_name(process.cmd()),
            ))
        })
        .collect::<Vec<_>>();
    descendant_processes_from_parent_edges(root, &rows)
}

#[cfg(unix)]
fn cleanup_token_processes(token: &str) -> Vec<ProcessCleanupChild> {
    use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

    let mut sys = System::new();
    sys.refresh_processes_specifics(
        ProcessesToUpdate::All,
        false,
        ProcessRefreshKind::nothing()
            .with_environ(UpdateKind::Always)
            .with_cmd(UpdateKind::Always),
    );
    let mut children = sys
        .processes()
        .iter()
        .filter(|(_, process)| {
            process_status_can_execute(process.status())
                && process_has_cleanup_token(process.environ(), token)
        })
        .map(|(pid, process)| {
            ProcessCleanupChild::new(
                pid.as_u32(),
                process.parent().map(|parent| parent.as_u32()),
                1,
                command_name(process.cmd()),
            )
        })
        .collect::<Vec<_>>();
    children.sort_by_key(|child| child.pid);
    children
}

#[cfg(unix)]
fn process_status_can_execute(status: sysinfo::ProcessStatus) -> bool {
    status != sysinfo::ProcessStatus::Zombie
}

#[cfg(unix)]
fn process_has_cleanup_token(environ: &[std::ffi::OsString], token: &str) -> bool {
    let cleanup = format!("{PROCESS_CLEANUP_TOKEN_ENV}={token}");
    let owner = format!("{PROCESS_OWNER_TOKEN_ENV}={token}");
    environ
        .iter()
        .any(|entry| matches!(entry.to_string_lossy().as_ref(), value if value == cleanup || value == owner))
}

#[cfg(all(unix, test))]
fn descendant_pids_from_parent_edges(root: u32, edges: &[(u32, u32)]) -> Vec<u32> {
    let rows = edges
        .iter()
        .map(|(pid, parent)| (*pid, *parent, None))
        .collect::<Vec<_>>();
    descendant_processes_from_parent_edges(root, &rows)
        .into_iter()
        .map(|child| child.pid)
        .collect()
}

#[cfg(unix)]
fn descendant_processes_from_parent_edges(
    root: u32,
    rows: &[(u32, u32, Option<String>)],
) -> Vec<ProcessCleanupChild> {
    use std::collections::{HashMap, HashSet};

    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    let mut metadata: HashMap<u32, (u32, Option<String>)> = HashMap::new();
    for (pid, parent, command) in rows {
        metadata.insert(*pid, (*parent, command.clone()));
        children.entry(*parent).or_default().push(*pid);
    }

    let mut seen = HashSet::new();
    let mut stack = vec![(root, 0usize)];
    let mut descendants = Vec::new();
    while let Some((pid, depth)) = stack.pop() {
        if !seen.insert(pid) {
            continue;
        }
        if pid != root {
            descendants.push((pid, depth));
        }
        if let Some(kids) = children.get(&pid) {
            for &child in kids {
                stack.push((child, depth + 1));
            }
        }
    }

    descendants.sort_by(|(left_pid, left_depth), (right_pid, right_depth)| {
        right_depth
            .cmp(left_depth)
            .then_with(|| left_pid.cmp(right_pid))
    });
    descendants
        .into_iter()
        .map(|(pid, depth)| {
            let (parent_pid, command) = metadata.get(&pid).cloned().unwrap_or((root, None));
            ProcessCleanupChild::new(pid, Some(parent_pid), depth as u32, command)
        })
        .collect()
}

#[cfg(unix)]
fn command_name(command: &[std::ffi::OsString]) -> Option<String> {
    if command.is_empty() {
        return None;
    }
    std::path::Path::new(&command[0])
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
}

#[cfg(unix)]
fn process_exists(pid: u32) -> bool {
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

#[cfg(unix)]
fn wait_for_report_children_to_exit(report: &ProcessCleanupReport, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if report
            .children
            .iter()
            .all(|child| !process_exists(child.pid))
        {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Like [`wait_for_report_children_to_exit`], but also waits on the root pid.
///
/// The escalation path names the root explicitly rather than trusting the
/// report to contain it: the report's `children` are the *descendants* the
/// scan found, so a root with no children would otherwise satisfy the
/// all-gone test immediately and collapse the grace period to nothing.
#[cfg(unix)]
fn wait_for_pid_and_report_children_to_exit(
    report: &ProcessCleanupReport,
    pid: u32,
    timeout: Duration,
) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if !process_exists(pid)
            && report
                .children
                .iter()
                .all(|child| !process_exists(child.pid))
        {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// How an interruptible child wait ended.
pub enum ChildWait {
    /// The child exited on its own.
    Exited(std::process::ExitStatus),
    /// The caller-supplied timeout elapsed; the child tree/group was killed.
    TimedOut(ProcessCleanupReport),
    /// [`requested`] fired; the child tree/group was SIGTERMed and, after
    /// [`SUBPROCESS_TERM_GRACE`], SIGKILLed. Carries the reaped status when
    /// the OS reported one.
    Interrupted(Option<std::process::ExitStatus>, ProcessCleanupReport),
}

/// Wait for `child` while polling [`requested`] and the optional timeout.
///
/// Used by the VM-side `process.*` builtins (`exec`, `shell`, `exec_opts`,
/// `harness.process.run`). The hostlib `run_command` family implements the same
/// protocol inside its `ProcessSpawner` abstraction. Callers should have
/// spawned the child with [`configure_kill_group`] so group signals reach
/// ordinary grandchildren; escaped descendants are reaped by process-tree
/// scanning on Unix.
pub fn wait_child_interruptible(
    child: &mut std::process::Child,
    timeout: Option<Duration>,
) -> std::io::Result<ChildWait> {
    wait_child_interruptible_with_cleanup_token(child, timeout, None)
}

pub fn wait_child_interruptible_with_cleanup_token(
    child: &mut std::process::Child,
    timeout: Option<Duration>,
    cleanup_token: Option<&str>,
) -> std::io::Result<ChildWait> {
    let deadline = timeout.map(|limit| Instant::now() + limit);
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(ChildWait::Exited(status));
        }
        if requested() {
            let (status, report) =
                terminate_child_group_with_cleanup_token_report(child, cleanup_token);
            return Ok(ChildWait::Interrupted(status, report));
        }
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            // Timeout keeps its historical semantics: immediate SIGKILL.
            let mut report = child_pid(child)
                .map(|pid| signal_pid_tree_group_and_token_with_report(pid, cleanup_token, 9))
                .unwrap_or_default();
            let _ = child.kill();
            let _ = child.wait();
            report.refresh_survivor_status();
            return Ok(ChildWait::TimedOut(report));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Gracefully terminate `child` and its process tree/group: SIGTERM, wait up to
/// [`SUBPROCESS_TERM_GRACE`], then SIGKILL. Reaps the child and returns its
/// exit status when available. On non-Unix targets this is a best-effort
/// direct `Child::kill` (`TerminateProcess`), which does not reach
/// grandchildren.
pub fn terminate_child_group(child: &mut std::process::Child) -> Option<std::process::ExitStatus> {
    terminate_child_group_with_report(child).0
}

/// Like [`terminate_child_group`], but also returns a structural cleanup
/// report describing descendants observed and signalled.
pub fn terminate_child_group_with_report(
    child: &mut std::process::Child,
) -> (Option<std::process::ExitStatus>, ProcessCleanupReport) {
    terminate_child_group_with_cleanup_token_report(child, None)
}

pub fn terminate_child_group_with_cleanup_token_report(
    child: &mut std::process::Child,
    cleanup_token: Option<&str>,
) -> (Option<std::process::ExitStatus>, ProcessCleanupReport) {
    let mut report = child_pid(child)
        .map(|pid| ProcessCleanupReport::for_signal(Some(pid), 15))
        .unwrap_or_default();
    #[cfg(not(unix))]
    let _ = cleanup_token;
    #[cfg(unix)]
    {
        if let Some(pid) = child_pid(child) {
            const SIGTERM: i32 = 15;
            report = signal_pid_tree_group_and_token_with_report(pid, cleanup_token, SIGTERM);
            let grace_deadline = Instant::now() + SUBPROCESS_TERM_GRACE;
            loop {
                match child.try_wait() {
                    Ok(Some(status)) => {
                        // The direct child is gone, but SIGTERM-immune
                        // descendants may linger — sweep the group.
                        report.merge(signal_pid_tree_group_and_token_with_report(
                            pid,
                            cleanup_token,
                            9,
                        ));
                        report.refresh_survivor_status();
                        return (Some(status), report);
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
            report.merge(signal_pid_tree_group_and_token_with_report(
                pid,
                cleanup_token,
                9,
            ));
        }
    }
    let _ = child.kill();
    let status = child.wait().ok();
    report.refresh_survivor_status();
    (status, report)
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
                    signal_pid_tree_and_group(child_pid, SIGTERM);
                    if let Ok(buf) = rx.recv_timeout(SUBPROCESS_TERM_GRACE) {
                        signal_pid_tree_and_group(child_pid, 9);
                        return buf;
                    }
                    signal_pid_tree_and_group(child_pid, 9);
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
    let cleanup_token = new_process_cleanup_token();
    command.env(PROCESS_CLEANUP_TOKEN_ENV, &cleanup_token);
    let mut child = command.spawn()?;
    let pid = child.id();
    let rx_out = child.stdout.take().map(spawn_pipe_drain);
    let rx_err = child.stderr.take().map(spawn_pipe_drain);

    let (status, killed) = match wait_child_interruptible_with_cleanup_token(
        &mut child,
        None,
        Some(&cleanup_token),
    )? {
        ChildWait::Exited(status) => (status, false),
        // No timeout is armed here, but keep the arm total.
        ChildWait::TimedOut(_) => (std::process::ExitStatus::default(), true),
        ChildWait::Interrupted(status, _) => (status.unwrap_or_default(), true),
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
    fn installed_tracks_guard_lifetime() {
        assert!(!installed());
        let guard = install(None, None);
        assert!(installed());
        drop(guard);
        assert!(!installed());
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

    #[test]
    fn active_cleanup_owner_scopes_are_disjoint() {
        let owner = Arc::new(AtomicBool::new(false));
        let _owned =
            register_active_process_cleanup(None, "owned-scope-test", Some(Arc::clone(&owner)));
        let _ownerless = register_active_process_cleanup(None, "ownerless-scope-test", None);

        assert_eq!(
            active_cleanup_tokens_for_cancel_token_for_test(&owner),
            vec!["owned-scope-test".to_string()]
        );
        assert!(
            ownerless_active_cleanup_tokens_for_test()
                .iter()
                .any(|token| token == "ownerless-scope-test"),
            "explicit ownerless fallback should remain separately discoverable"
        );
    }

    #[test]
    fn active_cleanup_guard_unregisters_on_drop() {
        let owner = Arc::new(AtomicBool::new(false));
        let token = "guard-lifetime-test";
        let guard = register_active_process_cleanup(None, token, Some(Arc::clone(&owner)));

        assert!(
            active_cleanup_tokens_for_cancel_token_for_test(&owner)
                .iter()
                .any(|entry| entry == token),
            "active cleanup must remain registered while its guard is alive"
        );

        drop(guard);

        assert!(
            !active_cleanup_tokens_for_cancel_token_for_test(&owner)
                .iter()
                .any(|entry| entry == token),
            "dropping the guard must unregister the cleanup token"
        );
    }

    #[cfg(unix)]
    #[test]
    fn descendant_pids_from_parent_edges_returns_deepest_first_tree_only() {
        let edges = [
            (20, 10),
            (30, 20),
            (40, 20),
            (50, 30),
            (60, 99),
            (70, 60),
            // A malformed process table cycle should not hang traversal.
            (80, 90),
            (90, 80),
        ];

        assert_eq!(
            descendant_pids_from_parent_edges(10, &edges),
            vec![50, 30, 40, 20]
        );
        assert_eq!(descendant_pids_from_parent_edges(99, &edges), vec![70, 60]);
        assert_eq!(
            descendant_pids_from_parent_edges(123, &edges),
            Vec::<u32>::new()
        );
    }

    #[cfg(unix)]
    #[test]
    fn descendant_processes_preserve_metadata_and_depth_order() {
        let rows = [
            (20, 10, Some("worker".to_string())),
            (30, 20, Some("grandchild".to_string())),
            (40, 20, None),
            (50, 30, Some("leaf".to_string())),
        ];

        let descendants = descendant_processes_from_parent_edges(10, &rows);
        let pids = descendants
            .iter()
            .map(|child| {
                (
                    child.pid,
                    child.parent_pid,
                    child.depth,
                    child.command_name.as_deref(),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            pids,
            vec![
                (50, Some(30), 3, Some("leaf")),
                (30, Some(20), 2, Some("grandchild")),
                (40, Some(20), 2, None),
                (20, Some(10), 1, Some("worker")),
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn command_name_keeps_only_argv0_basename() {
        let command = vec![
            std::ffi::OsString::from("/usr/local/bin/tool"),
            std::ffi::OsString::from("--api-key"),
            std::ffi::OsString::from("secret-value"),
            std::ffi::OsString::from("plain"),
        ];

        assert_eq!(command_name(&command).as_deref(), Some("tool"));
        assert_eq!(command_name(&[]).as_deref(), None);
    }

    #[cfg(unix)]
    #[test]
    fn process_has_cleanup_token_requires_exact_marker_entry() {
        let token = "tok-123";
        let env = vec![
            std::ffi::OsString::from("PATH=/usr/bin"),
            std::ffi::OsString::from(format!("{PROCESS_CLEANUP_TOKEN_ENV}={token}")),
        ];
        assert!(process_has_cleanup_token(&env, token));
        assert!(!process_has_cleanup_token(&env, "tok"));
        assert!(!process_has_cleanup_token(
            &[std::ffi::OsString::from("OTHER=tok-123")],
            token
        ));
    }

    #[cfg(unix)]
    #[test]
    fn process_has_cleanup_token_accepts_owner_lifetime_marker() {
        let token = "owner-123";
        let env = vec![std::ffi::OsString::from(format!(
            "{PROCESS_OWNER_TOKEN_ENV}={token}"
        ))];
        assert!(process_has_cleanup_token(&env, token));
        assert!(!process_has_cleanup_token(&env, "owner"));
    }

    #[cfg(unix)]
    #[test]
    fn zombie_processes_are_not_lifetime_survivors() {
        assert!(!process_status_can_execute(sysinfo::ProcessStatus::Zombie));
        assert!(process_status_can_execute(sysinfo::ProcessStatus::Sleep));
        assert!(process_status_can_execute(sysinfo::ProcessStatus::Dead));
    }

    #[cfg(unix)]
    #[test]
    fn owner_journal_initialization_refuses_preexisting_symlink() {
        let token = new_process_cleanup_token();
        let journal = owner_process_group_journal(&token);
        let target = tempfile::NamedTempFile::new().expect("create journal symlink target");
        std::os::unix::fs::symlink(target.path(), &journal).expect("create owner journal symlink");
        initialize_process_owner_group_journal(&token)
            .expect_err("preexisting journal symlink must fail closed");
        std::fs::remove_file(journal).expect("remove owner journal symlink");
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
        assert!(matches!(outcome, ChildWait::Interrupted(_, _)));
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

    /// Both halves of the escalation, on one pid each.
    ///
    /// A dead pid on its own cannot tell TERM -> grace -> KILL apart from an
    /// immediate KILL: both leave the same corpse. So the polite process
    /// writes a marker from inside its SIGTERM handler, and that file is the
    /// only evidence that the first signal was ever sent. Reverting
    /// `terminate_pid_tree_group_and_token_with_report` to a bare SIGKILL
    /// leaves the marker absent; reverting it to a bare SIGTERM leaves the
    /// immune pid alive.
    #[cfg(unix)]
    #[test]
    fn escalating_terminate_kills_a_term_immune_child_and_asks_a_polite_one_first() {
        use std::process::{Command, Stdio};

        let dir = tempfile::tempdir().expect("temp dir");
        let marker = dir.path().join("term-received.marker");

        let mut immune = Command::new("sh")
            .arg("-c")
            .arg("trap '' TERM; while true; do sleep 0.05; done")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn term-immune child");
        let mut polite = Command::new("sh")
            .arg("-c")
            .arg(format!(
                "trap 'printf TERM > {}; exit 0' TERM; while true; do sleep 0.05; done",
                marker.display()
            ))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn polite child");

        let immune_pid = immune.id();
        let polite_pid = polite.id();

        // Liveness first: a child that never started would make every clause
        // below pass for the wrong reason.
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(5)
            && !(process_exists(immune_pid) && process_exists(polite_pid))
        {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            process_exists(immune_pid) && process_exists(polite_pid),
            "both probe children must be running before the terminate"
        );
        assert!(
            !marker.exists(),
            "the SIGTERM marker must not exist before the terminate"
        );

        let immune_report = terminate_pid_tree_group_and_token_with_report(immune_pid, None);
        let polite_report = terminate_pid_tree_group_and_token_with_report(polite_pid, None);

        let _ = immune.wait();
        let _ = polite.wait();

        assert!(
            !process_exists(immune_pid),
            "a child that ignores SIGTERM must still be gone: {immune_report:?}"
        );
        assert!(
            !process_exists(polite_pid),
            "the polite child must be gone: {polite_report:?}"
        );
        assert_eq!(
            std::fs::read_to_string(&marker).unwrap_or_default(),
            "TERM",
            "the polite child must have handled SIGTERM before anything killed it"
        );
        assert!(
            polite_report.attempted_signals.contains(&15),
            "the escalation must record the SIGTERM it sent: {polite_report:?}"
        );
    }
}
