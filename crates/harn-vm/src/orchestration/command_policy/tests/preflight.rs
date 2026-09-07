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

/// Both directions of the non-Git floor decision, in one test.
///
/// The floor exists to make erasing reviewed project state impossible. Without
/// Git there is nothing to point at, and the file has usually just been written
/// by this run, so existence alone must not produce a never-approvable denial.
/// Positive Git evidence still must.
#[test]
fn existing_file_without_git_is_not_the_never_approvable_floor() {
    let s = |parts: &[&str]| parts.iter().map(|p| p.to_string()).collect::<Vec<_>>();

    // No Git anywhere above this directory.
    let plain = tempfile::TempDir::new().expect("temp dir");
    let plain_root = std::fs::canonicalize(plain.path()).expect("canonical temp dir");
    let scratch = plain_root.join("proof.txt");
    let plain_roots = vec![plain_root.to_string_lossy().to_string()];

    // Before the file exists, and after this run creates it. The verdict must
    // not change: nothing about the workspace differs between the two.
    assert!(
        universal_catastrophic_reason(
            "truncate",
            &s(&["-s", "0", "proof.txt"]),
            &plain_roots,
            &plain_root,
        )
        .is_none(),
        "a target that does not exist yet was already allowed"
    );
    std::fs::write(&scratch, b"x").expect("write scratch file");
    let after = universal_catastrophic_reason(
        "truncate",
        &s(&["-s", "0", "proof.txt"]),
        &plain_roots,
        &plain_root,
    );
    assert!(
        after.is_none(),
        "existence alone must not be never-approvable without Git: {after:?}"
    );

    // NEGATIVE CONTROL. Without this the change could pass by allowing
    // everything, which is the failure mode worth guarding.
    let repo = tempfile::TempDir::new().expect("temp dir");
    let repo_root = std::fs::canonicalize(repo.path()).expect("canonical temp dir");
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .current_dir(&repo_root)
            .args(args)
            .output()
            .expect("git")
    };
    git(&["init", "--quiet"]);
    git(&["config", "user.email", "floor@example.invalid"]);
    git(&["config", "user.name", "Floor"]);
    let tracked = repo_root.join("reviewed.txt");
    std::fs::write(&tracked, b"reviewed").expect("write tracked file");
    git(&["add", "reviewed.txt"]);
    git(&["commit", "--quiet", "--no-gpg-sign", "-m", "seed"]);

    let repo_roots = vec![repo_root.to_string_lossy().to_string()];
    assert!(
        universal_catastrophic_reason(
            "truncate",
            &s(&["-s", "0", "reviewed.txt"]),
            &repo_roots,
            &repo_root,
        )
        .is_some(),
        "a Git-tracked file must still hold the never-approvable floor"
    );
    // An untracked file beside it is not reviewed state either.
    std::fs::write(repo_root.join("scratch.txt"), b"x").expect("write untracked file");
    assert!(
        universal_catastrophic_reason(
            "truncate",
            &s(&["-s", "0", "scratch.txt"]),
            &repo_roots,
            &repo_root,
        )
        .is_none(),
        "an untracked file in a Git workspace is not reviewed project state"
    );
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
