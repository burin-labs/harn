//! Real-process coverage for the subprocess stop grace: what a child gets to
//! finish between SIGTERM and the forced kill of its process group.
//!
//! Spawns through the same real `run_command` path as `process_tools_e2e`
//! and reuses its spawn helpers.

#![cfg(unix)]

use std::io::Read;
use std::sync::Arc;

use super::process_tools_e2e::{call, vlist};
use super::support::*;

#[test]
fn real_run_command_child_finishes_its_cleanup_inside_the_grace() {
    // A child that needs a moment after SIGTERM must complete that work before
    // the forced kill. 400 ms stands in for removing a lock or flushing a file.
    let temp = tempfile::tempdir().expect("ready fifo tempdir");
    let ready_path = temp.path().join("ready.fifo");
    let cleaned_path = temp.path().join("cleaned");
    let status = std::process::Command::new("mkfifo")
        .arg(&ready_path)
        .status()
        .expect("spawn mkfifo");
    assert!(status.success(), "mkfifo failed: {status}");
    let mut ready_fifo = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&ready_path)
        .expect("open ready fifo");

    let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let worker_cancel = Arc::clone(&cancel);
    let ready_arg = shell_words::quote(&ready_path.to_string_lossy()).into_owned();
    let cleaned_arg = shell_words::quote(&cleaned_path.to_string_lossy()).into_owned();
    let worker = std::thread::spawn(move || {
        let _guard = harn_vm::op_interrupt::install(Some(worker_cancel), None);
        let mut req = dict_map();
        req.insert(
            "argv".into(),
            vlist(vec![
                vm_string("sh"),
                vm_string("-c"),
                vm_string(&format!(
                    "trap 'sleep 0.4; printf done > {cleaned_arg}; exit 0' TERM; \
                     printf ready > {ready_arg}; while :; do sleep 0.05; done"
                )),
            ]),
        );
        require_dict(call("hostlib_tools_run_command", req).unwrap())
    });

    let mut marker = [0_u8; 5];
    ready_fifo
        .read_exact(&mut marker)
        .expect("cleanup child did not signal readiness");
    cancel.store(true, std::sync::atomic::Ordering::SeqCst);
    let resp = worker
        .join()
        .expect("interruptible command thread panicked");

    assert_eq!(
        std::fs::read_to_string(&cleaned_path).ok().as_deref(),
        Some("done"),
        "the child's cleanup did not finish before the forced kill: {resp:?}"
    );
}
