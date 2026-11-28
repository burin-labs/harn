use super::*;

#[test]
fn guardian_custody_proves_zero_only_for_complete_unmixed_cleanup() {
    let mut report = ProcessCleanupReport {
        census: ProcessCleanupCensus {
            control_read: true,
            scanned_processes: 100,
            unreadable_processes: 20,
        },
        ..ProcessCleanupReport::default()
    };
    assert_eq!(report.survivor_count(), None);
    report.guardian_custody_complete = true;
    assert!(!report.census_complete());
    assert_eq!(report.survivor_count(), Some(0));
    report.omitted_children = 1;
    assert_eq!(report.survivor_count(), None);
    report.omitted_children = 0;
    report
        .children
        .push(ProcessCleanupChild::new(42, None, 1, None));
    assert_eq!(report.survivor_count(), Some(1));
    report.children.clear();
    report.merge(ProcessCleanupReport::for_signal(Some(43), 9));
    assert_eq!(report.survivor_count(), None);
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
    let mut child = super::probe_tests::start_process_probe("tree", true, None);
    let pgid = child.id();

    let cancel = Arc::new(AtomicBool::new(true));
    let _guard = install(Some(cancel), None);
    let started = Instant::now();
    let outcome = wait_child_interruptible(&mut child, None).expect("wait");
    let ChildWait::Interrupted(_, report) = outcome else {
        panic!("expected interrupt, got a different child wait outcome");
    };
    assert!(
        !report.children.is_empty(),
        "probe must reach a live descendant: {report:?}"
    );
    assert!(started.elapsed() < Duration::from_secs(10));

    // kill(-pgid, 0) fails with ESRCH once every member is gone.
    let group_gone = || {
        (unsafe { libc::kill(-(pgid as i32), 0) }) != 0
            && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
    };
    let deadline = Instant::now() + harn_clock::test_support::HANG_CEILING;
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
    let dir = tempfile::tempdir().expect("temp dir");
    let marker = dir.path().join("term-received.marker");
    let mut immune = super::probe_tests::start_process_probe("immune", true, None);
    let mut polite = super::probe_tests::start_process_probe("polite", true, Some(&marker));

    let immune_pid = immune.id();
    let polite_pid = polite.id();

    // Liveness first: a child that never started would make every clause
    // below pass for the wrong reason.
    assert!(
        process_exists(immune_pid) && process_exists(polite_pid),
        "both probe children must be running before the terminate"
    );
    assert_eq!(
        std::fs::read(&marker).unwrap(),
        b"",
        "no SIGTERM handled before termination"
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
