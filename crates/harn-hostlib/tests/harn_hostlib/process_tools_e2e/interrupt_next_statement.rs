use super::*;

#[test]
fn real_run_command_interrupt_never_starts_the_shells_next_statement() {
    // A shell whose foreground child dies moves straight to its next statement,
    // so the shell has to be signalled before that child is.
    let temp = tempfile::tempdir().expect("marker tempdir");
    for trial in 0..5 {
        let started = temp.path().join(format!("started-{trial}"));
        let next = temp.path().join(format!("next-{trial}"));
        let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancel);
        let script = format!(
            "printf x > {started}; sleep 30; printf x > {next}",
            started = shell_words::quote(&started.to_string_lossy()),
            next = shell_words::quote(&next.to_string_lossy()),
        );
        let worker = std::thread::spawn(move || {
            let _guard = harn_vm::op_interrupt::install(Some(worker_cancel), None);
            let mut req = dict_map();
            req.insert(
                "argv".into(),
                vlist(vec![vm_string("sh"), vm_string("-c"), vm_string(&script)]),
            );
            require_dict(call("hostlib_tools_run_command", req).unwrap())
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while !started.exists() {
            assert!(
                std::time::Instant::now() < deadline,
                "command never started"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        // Long enough for the shell to be waiting on its foreground `sleep`.
        std::thread::sleep(std::time::Duration::from_millis(300));
        cancel.store(true, std::sync::atomic::Ordering::SeqCst);
        let resp = worker
            .join()
            .expect("interruptible command thread panicked");
        assert_eq!(require_str(&resp, "status"), "killed");
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert!(
            !next.exists(),
            "trial {trial}: the shell ran its next statement after the interrupt"
        );
    }
}
