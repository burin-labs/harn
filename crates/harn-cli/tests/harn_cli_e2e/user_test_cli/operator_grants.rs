use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct GitFixture {
    root: tempfile::TempDir,
    repo: PathBuf,
    expected_oid: String,
}

fn git(cwd: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(cwd)
        .args(args)
        .output()
        .expect("spawn git");
    assert!(
        output.status.success(),
        "git {args:?} failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("git output is UTF-8")
        .trim()
        .to_string()
}

fn protected_push_fixture() -> GitFixture {
    let root = tempfile::TempDir::new().expect("tempdir");
    let remote = root.path().join("remote.git");
    let repo = root.path().join("repo");
    fs::create_dir(&repo).expect("create repo");
    git(root.path(), &["init", "--bare", remote.to_str().unwrap()]);
    git(&repo, &["init", "-b", "main"]);
    git(&repo, &["config", "user.email", "harn@example.test"]);
    git(&repo, &["config", "user.name", "Harn Test"]);
    fs::write(repo.join("value.txt"), "initial\n").expect("write initial");
    git(&repo, &["add", "value.txt"]);
    git(&repo, &["commit", "-m", "initial"]);
    git(
        &repo,
        &["remote", "add", "origin", remote.to_str().unwrap()],
    );
    git(&repo, &["push", "origin", "HEAD:refs/heads/main"]);
    let expected_oid = git(&repo, &["rev-parse", "HEAD"]);
    fs::write(repo.join("value.txt"), "protected update\n").expect("write update");
    git(&repo, &["commit", "-am", "protected update"]);
    GitFixture {
        root,
        repo,
        expected_oid,
    }
}

fn harn_string(value: impl AsRef<Path>) -> String {
    serde_json::to_string(&value.as_ref().to_string_lossy()).expect("serialize path")
}

fn run_harn(fixture: &GitFixture, args: &[&str]) -> Output {
    Command::new(super::binary_path())
        .current_dir(fixture.root.path())
        .args(args)
        .output()
        .expect("spawn harn")
}

fn assert_success(output: &Output, operation: &str) {
    assert!(
        output.status.success(),
        "{operation} failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn operator_grant_supports_run_and_stale_lease_user_test_receipts() {
    let fixture = protected_push_fixture();
    let run_script = fixture.root.path().join("protected_push.harn");
    fs::write(
        &run_script,
        format!(
            r#"import {{ git_push }} from "std/git"

pipeline main(harness: Harness, _task: unknown) {{
  const receipt = git_push(
    harness.process,
    "origin",
    "HEAD:refs/heads/main",
    {},
    {{ref: "refs/heads/main", expected_oid: "{}"}},
  )
  assert(receipt.success)
  assert_eq(receipt.approval?.schema, "harn.operator-approval-grant.v1")
  assert_eq(receipt.approval?.approver, "cli")
  assert_eq(receipt.approval?.grant_source, "explicit_cli_flag")
  harness.stdio.println("grant-ok")
}}
"#,
            harn_string(&fixture.repo),
            fixture.expected_oid,
        ),
    )
    .expect("write run script");

    let output = run_harn(
        &fixture,
        &[
            "run",
            "--allow-process-network",
            "--approve-risky",
            "git.push",
            run_script.to_str().unwrap(),
        ],
    );
    assert_success(&output, "harn run with operator grant");
    assert_eq!(String::from_utf8_lossy(&output.stdout), "grant-ok\n");

    fs::write(fixture.repo.join("value.txt"), "stale attempt\n").expect("write stale update");
    git(&fixture.repo, &["commit", "-am", "stale attempt"]);
    let suite = fixture.root.path().join("suite");
    fs::create_dir(&suite).expect("create suite");
    let stale_test = suite.join("test_stale_lease.harn");
    fs::write(
        &stale_test,
        format!(
            r#"import {{ git_push }} from "std/git"

pipeline test_stale_lease(harness: Harness, _task: unknown) {{
  const receipt = git_push(
    harness.process,
    "origin",
    "HEAD:refs/heads/main",
    {},
    {{ref: "refs/heads/main", expected_oid: "{}"}},
  )
  assert(!receipt.success)
  assert_eq(receipt.status, "lease_mismatch")
  assert_eq(receipt.data.expected_oid, "{}")
  assert(receipt.data.actual_oid != receipt.data.expected_oid)
}}
"#,
            harn_string(&fixture.repo),
            fixture.expected_oid,
            fixture.expected_oid,
        ),
    )
    .expect("write stale lease test");
    let output = run_harn(
        &fixture,
        &[
            "test",
            "--approve-risky",
            "git.push",
            stale_test.to_str().unwrap(),
        ],
    );
    assert_success(&output, "harn test stale lease receipt");
}

#[test]
fn user_test_requires_a_matching_operator_grant_across_worker_threads() {
    let fixture = protected_push_fixture();
    let test_file = fixture.root.path().join("test_protected_push.harn");
    fs::write(
        &test_file,
        format!(
            r#"import {{ git_push }} from "std/git"

pipeline test_protected_push(harness: Harness, _task: unknown) {{
  const receipt = git_push(
    harness.process,
    "origin",
    "HEAD:refs/heads/main",
    {},
    {{ref: "refs/heads/main", expected_oid: "{}"}},
  )
  assert(receipt.success)
  assert_eq(receipt.approval?.operation, "git.push")
  assert_eq(receipt.approval?.grant_source, "explicit_cli_flag")
}}
"#,
            harn_string(&fixture.repo),
            fixture.expected_oid,
        ),
    )
    .expect("write protected push test");

    let output = run_harn(&fixture, &["test", test_file.to_str().unwrap()]);
    assert!(
        !output.status.success(),
        "no-grant test unexpectedly passed"
    );
    let diagnostics = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        diagnostics.contains("approval required but no host bridge is attached"),
        "unexpected no-grant diagnostics:\n{diagnostics}"
    );

    let output = run_harn(
        &fixture,
        &[
            "test",
            "--parallel",
            "--jobs",
            "2",
            "--approve-risky",
            "git.push",
            test_file.to_str().unwrap(),
        ],
    );
    assert_success(&output, "parallel harn test with operator grant");
}

#[test]
fn ref_plumbing_pushes_can_skip_the_checkouts_pre_push_hook() {
    // A pre-push hook validates the commits a developer is publishing. Ref
    // plumbing publishes an OID the remote already holds, or deletes a ref, so
    // the hook has no subject and only contributes the checkout's branch and
    // tracking state as a failure mode. Falsifier: without `no_verify` the
    // same push through the same checkout is rejected by the hook.
    let fixture = protected_push_fixture();
    let hook = fixture.repo.join(".git/hooks/pre-push");
    fs::create_dir_all(hook.parent().expect("hooks dir")).expect("create hooks dir");
    fs::write(&hook, "#!/bin/sh\necho 'pre-push refused' >&2\nexit 1\n").expect("write hook");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).expect("chmod hook");
    }
    // A branch with no upstream is the normal state of a working checkout, and
    // is exactly what a hook that consults `@{upstream}` trips over.
    git(&fixture.repo, &["checkout", "-q", "-b", "no-upstream"]);

    let script = fixture.root.path().join("archive_ref.harn");
    fs::write(
        &script,
        format!(
            r#"import {{ git_push }} from "std/git"

pipeline main(harness: Harness, _task: unknown) {{
  const hooked = git_push(
    harness.process,
    "origin",
    "{oid}:refs/heads/hooked",
    {repo},
  )
  assert(!hooked.success)
  const plumbed = git_push(
    harness.process,
    "origin",
    "{oid}:refs/heads/archived",
    {repo},
    nil,
    {{no_verify: true}},
  )
  assert(plumbed.success)
  harness.stdio.println("plumbing-ok")
}}
"#,
            oid = fixture.expected_oid,
            repo = harn_string(&fixture.repo),
        ),
    )
    .expect("write script");

    let output = run_harn(
        &fixture,
        &[
            "run",
            "--allow-process-network",
            "--approve-risky",
            "git.push",
            script.to_str().unwrap(),
        ],
    );
    assert_success(&output, "harn run with ref plumbing push");
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("plumbing-ok"),
        "stdout:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );

    let remote = fixture.root.path().join("remote.git");
    let refs = git(&remote, &["for-each-ref", "--format=%(refname)"]);
    assert!(
        refs.contains("refs/heads/archived"),
        "the plumbing push must reach the remote: {refs}"
    );
    assert!(
        !refs.contains("refs/heads/hooked"),
        "the hooked push must not reach the remote: {refs}"
    );
}

#[test]
fn a_leased_ref_deletion_can_also_skip_the_checkouts_pre_push_hook() {
    // Deleting a ref under a lease is the canonical ref-plumbing operation, and
    // it is the one that needs both flags at once. The lease makes the push a
    // `--force-with-lease=<ref>:<oid>`, which only reaches the remote through
    // the reviewed dispatch; adding `--no-verify` used to move the lease off
    // the exact position that dispatch recognized, so the push fell through to
    // the generic command floor and was denied as a bare force push — naming
    // neither the lease nor the hook. Falsifier: the same leased delete without
    // `no_verify` is rejected by the hook and the ref survives.
    let fixture = protected_push_fixture();
    let hook = fixture.repo.join(".git/hooks/pre-push");
    fs::create_dir_all(hook.parent().expect("hooks dir")).expect("create hooks dir");
    fs::write(&hook, "#!/bin/sh\necho 'pre-push refused' >&2\nexit 1\n").expect("write hook");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).expect("chmod hook");
    }
    let remote = fixture.root.path().join("remote.git");
    git(
        &remote,
        &[
            "update-ref",
            "refs/heads/attempt",
            fixture.expected_oid.trim(),
        ],
    );
    git(&fixture.repo, &["checkout", "-q", "-b", "no-upstream"]);

    let script = fixture.root.path().join("delete_ref.harn");
    fs::write(
        &script,
        format!(
            r#"import {{ git_push }} from "std/git"

pipeline main(harness: Harness, _task: unknown) {{
  const lease = {{ref: "refs/heads/attempt", expected_oid: "{oid}"}}
  const hooked = git_push(harness.process, "origin", ":refs/heads/attempt", {repo}, lease)
  assert(!hooked.success)
  const plumbed = git_push(
    harness.process,
    "origin",
    ":refs/heads/attempt",
    {repo},
    lease,
    {{no_verify: true}},
  )
  assert(plumbed.success)
  harness.stdio.println("leased-delete-ok")
}}
"#,
            oid = fixture.expected_oid.trim(),
            repo = harn_string(&fixture.repo),
        ),
    )
    .expect("write script");

    let output = run_harn(
        &fixture,
        &[
            "run",
            "--allow-process-network",
            "--approve-risky",
            "git.push",
            script.to_str().unwrap(),
        ],
    );
    assert_success(&output, "harn run with leased ref deletion");
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("leased-delete-ok"),
        "stdout:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );

    let refs = git(&remote, &["for-each-ref", "--format=%(refname)"]);
    assert!(
        !refs.contains("refs/heads/attempt"),
        "the leased deletion must reach the remote: {refs}"
    );
}
