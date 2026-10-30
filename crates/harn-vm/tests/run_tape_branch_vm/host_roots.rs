use super::*;

#[test]
fn an_explicit_session_root_outside_the_recording_is_refused_in_the_prefix() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let dirs = dirs();
    let hook_marker = dirs.markers.join("hook-marker");
    let exec_marker = dirs.markers.join("exec-marker");
    let elsewhere = dirs.workspace.parent().unwrap().join("elsewhere");
    let source = rooted_reset_script(&hook_marker, &exec_marker, &elsewhere);
    record(&source, &dirs.tapes, &dirs.workspace);
    assert_eq!(durable_resets(&elsewhere), 2, "one reset per turn");
    let Branched { output, report, .. } = branch(&source, &dirs.tapes, &dirs.workspace, 2);
    assert!(output.is_err(), "the refused write fails the run");
    let divergence = report.divergence.expect("the prefix diverged");
    assert_eq!(
        divergence.what, "durable state outside the branch",
        "{divergence:?}"
    );
    assert!(
        divergence.detail.contains("session store"),
        "{divergence:?}"
    );
    assert_eq!(
        durable_resets(&elsewhere),
        2,
        "the branch wrote outside itself"
    );
}

#[test]
fn a_session_root_the_host_declares_per_run_lands_in_the_branch() {
    let _serial = SERIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let dirs = dirs();
    let hook_marker = dirs.markers.join("hook-marker");
    let exec_marker = dirs.markers.join("exec-marker");
    let home = |name: &str| {
        dirs.workspace
            .parent()
            .unwrap()
            .join(name)
            .join("agent-context")
    };
    let (record_home, branch_home) = (home("record-home"), home("branch-home"));
    record_with_roots(
        &rooted_reset_script(&hook_marker, &exec_marker, &record_home),
        &dirs.tapes,
        &dirs.workspace,
        vec![record_home.clone()],
    );
    assert_eq!(
        durable_resets(&record_home),
        2,
        "the recording wrote one reset per turn"
    );
    let tape = EventTape::load(&dirs.tapes.join(TAPE_FILE_NAME)).unwrap();
    assert!(tape
        .header
        .run
        .unwrap()
        .state_roots
        .contains(&record_home.canonicalize().unwrap()));
    remove_marker(&hook_marker, &dirs.tapes);
    remove_marker(&exec_marker, &dirs.tapes);

    let branched = branch_with_roots(
        &rooted_reset_script(&hook_marker, &exec_marker, &branch_home),
        &dirs.tapes,
        &dirs.workspace,
        2,
        vec![branch_home.clone()],
    );
    branched.output.expect("the branched run succeeds");
    assert_eq!(branched.report.divergence, None);
    assert_eq!(branched.report.served_calls, 2);
    assert_eq!(
        durable_resets(&branch_home),
        0,
        "the host home must stay untouched"
    );
    assert_eq!(
        durable_resets(&branched.state),
        2,
        "the branch owns both resets"
    );
    assert_eq!(
        durable_resets(&record_home),
        2,
        "the recording's state must stay untouched"
    );
}
