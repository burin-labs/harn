use super::*;

#[test]
fn universal_catastrophic_reason_blocks_full_floor() {
    let root = vec![ROOT.to_string()];
    let cwd = Path::new(ROOT);
    let s = |parts: &[&str]| parts.iter().map(|p| p.to_string()).collect::<Vec<_>>();
    assert!(universal_catastrophic_reason("rm", &s(&["-rf", "/"]), &root, cwd).is_some());
    assert!(universal_catastrophic_reason("mkfs.ext4", &s(&["/dev/sda"]), &root, cwd).is_some());
    assert!(
        universal_catastrophic_reason("dd", &s(&["of=/dev/sda", "if=/dev/zero"]), &root, cwd)
            .is_some()
    );
    // Fork bomb through the canonical sh -c argv wrapper.
    assert!(
        universal_catastrophic_reason("sh", &s(&["-c", ":(){ :|:& };:"]), &root, cwd).is_some()
    );
    assert!(universal_catastrophic_reason("chmod", &s(&["-R", "000", "."]), &root, cwd).is_some());
    assert!(universal_catastrophic_reason("git", &s(&["reset", "--hard"]), &root, cwd).is_some());
    assert!(universal_catastrophic_reason("git", &s(&["clean", "-fdx"]), &root, cwd).is_some());
    assert!(universal_catastrophic_reason(
        "git",
        &s(&["push", "--force-with-lease=main:abc123", "origin", "HEAD"]),
        &root,
        cwd,
    )
    .is_some());
    assert!(
        universal_catastrophic_reason("sh", &s(&["-c", "git reset --hard"]), &root, cwd).is_some()
    );
    // Benign commands never fire.
    assert!(universal_catastrophic_reason("ls", &s(&["-la"]), &root, cwd).is_none());
    assert!(universal_catastrophic_reason("rm", &s(&["-rf", "build"]), &root, cwd).is_none());
    assert!(universal_catastrophic_reason("git", &s(&["status"]), &root, cwd).is_none());
    assert!(
        universal_catastrophic_reason("git", &s(&["push", "origin", "HEAD"]), &root, cwd).is_none()
    );
    let cmake_setup = universal_catastrophic_reason(
        "sh",
        &s(&["-c", "rm -rf build/burin-eval-setup && if command -v ninja >/dev/null 2>&1; then cmake -S . -B build/burin-eval-setup -G Ninja; else cmake -S . -B build/burin-eval-setup; fi"]),
        &root,
        cwd,
    );
    assert!(cmake_setup.is_none(), "unexpected block: {cmake_setup:?}");
}

#[tokio::test]
async fn policy_present_floor_blocks_full_set_including_workflow() {
    // With a policy on the stack the same floor applies before approval.
    clear_command_policies();
    push_command_policy(CommandPolicy::default());
    assert_floor_blocked(
        &preflight_argv(&[
            "git",
            "push",
            "--force-with-lease=main:abc123",
            "origin",
            "HEAD",
        ])
        .await,
    );
    assert_floor_blocked(&preflight_argv(&["git", "reset", "--hard"]).await);
    assert_floor_blocked(&preflight_shell("rm -rf /").await);
    assert_proceed(&preflight_argv(&["ls", "-la"]).await);
    clear_command_policies();
}

#[tokio::test]
async fn reviewed_lease_push_still_obeys_explicit_git_force_push_denial() {
    clear_command_policies();
    let mut policy = CommandPolicy::default();
    policy.deny_labels.insert("git_force_push".to_string());
    push_command_policy(policy);

    let preflight = run_command_policy_preflight_with_origin(
        None,
        &argv_params(&[
            "git",
            "push",
            "--force-with-lease=main:abc123",
            "origin",
            "HEAD:main",
        ]),
        JsonValue::Null,
        CommandDispatchOrigin::ReviewedGitPushWithLease,
    )
    .await
    .expect("preflight succeeds");

    match preflight {
        CommandPolicyPreflight::Blocked { decisions, .. } => assert!(
            decisions
                .iter()
                .any(|decision| decision.source == "deny_labels"),
            "expected an explicit deny_labels decision, got {decisions:?}"
        ),
        CommandPolicyPreflight::Proceed { .. } => {
            panic!("explicit git_force_push policy must override the reviewed origin")
        }
    }
    clear_command_policies();
}
