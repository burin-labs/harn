//! Fail-closed process-lifetime audit for conformance.

use std::io::Write;

pub(super) fn surviving_helpers_error() -> Option<String> {
    if harn_vm::op_interrupt::current_process_owner_survivors().is_empty() {
        return None;
    }
    let _ = harn_vm::op_interrupt::kill_current_process_owner_survivors();
    let survivors = harn_vm::op_interrupt::current_process_owner_survivors();
    if survivors.is_empty() {
        return None;
    }
    let summary = survivors
        .iter()
        .map(|process| {
            #[cfg(unix)]
            let process_group = unsafe { libc::getpgid(process.pid as i32) };
            #[cfg(not(unix))]
            let process_group = -1;
            format!(
                "pid={} parent={} pgid={} command={}",
                process.pid,
                process
                    .parent_pid
                    .map_or_else(|| "<unknown>".to_string(), |pid| pid.to_string()),
                process_group,
                process.command_name.as_deref().unwrap_or("<unknown>")
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    #[cfg(unix)]
    let current_group = unsafe { libc::getpgrp() };
    #[cfg(not(unix))]
    let current_group = -1;
    eprintln!(
        "conformance helper lifetime audit found survivors: runner_pid={} runner_pgid={current_group}; {summary}",
        std::process::id()
    );
    let _ = std::io::stderr().flush();
    Some(format!(
        "conformance suite left helper processes alive after forced cleanup: {summary}"
    ))
}
