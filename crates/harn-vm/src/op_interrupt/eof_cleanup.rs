//! Private pipe EOF ends capture, not necessarily the root process. Preserve
//! its exit status while removing helpers, including already orphaned peers.

use super::{
    cleanup_token_processes, command_name, descendant_processes, signal_pid_preserving_group,
    ActiveProcessCleanupGuard, ProcessCleanupChild, ProcessCleanupReport, ACTIVE_PROCESS_CLEANUPS,
};
use std::collections::BTreeMap;
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System};

impl ActiveProcessCleanupGuard {
    /// Call before reaping the root: its reserved PID also reserves the owned
    /// process-group identity. Never signal that root before its normal exit.
    pub fn terminate_descendants(&self) -> ProcessCleanupReport {
        let entry = ACTIVE_PROCESS_CLEANUPS
            .lock()
            .expect("active process cleanup registry poisoned")
            .get(&self.id)
            .cloned();
        let Some(entry) = entry else {
            return ProcessCleanupReport::default();
        };
        let Some(pid) = entry.pid else {
            return ProcessCleanupReport::default();
        };
        let mut children = descendant_processes(pid)
            .into_iter()
            .chain(cleanup_token_processes(&entry.cleanup_token))
            .map(|child| (child.pid, child))
            .collect::<BTreeMap<_, _>>();
        let mut system = System::new();
        system.refresh_processes_specifics(
            ProcessesToUpdate::All,
            false,
            ProcessRefreshKind::nothing(),
        );
        for (peer, process) in system.processes() {
            let peer = peer.as_u32();
            if peer != pid && unsafe { libc::getpgid(peer as i32) } == pid as i32 {
                children.entry(peer).or_insert_with(|| {
                    ProcessCleanupChild::new(
                        peer,
                        process.parent().map(|parent| parent.as_u32()),
                        1,
                        command_name(process.cmd()),
                    )
                });
            }
        }
        children.remove(&pid);
        let mut report = ProcessCleanupReport::for_signal(Some(pid), 9);
        for child in children.into_values() {
            signal_pid_preserving_group(child.pid, pid as i32, 9);
            report.merge_child(child.with_signal(9));
        }
        report.refresh_survivor_status();
        report
    }
}
