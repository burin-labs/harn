//! Private pipe EOF ends capture, not necessarily the root process. Preserve
//! its exit status while removing helpers, including already orphaned peers.

use super::{
    command_name, signal_pid_preserving_group, token_census, ActiveProcessCleanupGuard,
    ProcessCleanupChild, ProcessCleanupReport, ProcessTable, ACTIVE_PROCESS_CLEANUPS,
};
use std::collections::BTreeMap;

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
        // One table answers all three questions: descendants, the token
        // cohort, and peers still in the root's process group.
        let mut children = BTreeMap::new();
        if let Some(table) = ProcessTable::read(Some(&entry.cleanup_token)) {
            children.extend(
                table
                    .descendants(pid)
                    .into_iter()
                    .chain(token_census(&table, &entry.cleanup_token).0)
                    .map(|child| (child.pid, child)),
            );
            for (peer, process) in table.0.processes() {
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
