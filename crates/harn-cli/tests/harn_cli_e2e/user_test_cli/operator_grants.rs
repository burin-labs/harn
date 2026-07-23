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

pipeline main(_task) {{
  const receipt = git_push(
    "origin",
    "HEAD:refs/heads/main",
    {},
    {{ref: "refs/heads/main", expected_oid: "{}"}},
  )
  assert(receipt.success)
  assert_eq(receipt.approval?.schema, "harn.operator-approval-grant.v1")
  assert_eq(receipt.approval?.approver, "cli")
  assert_eq(receipt.approval?.grant_source, "explicit_cli_flag")
  __io_println("grant-ok")
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

pipeline test_stale_lease(_task) {{
  const receipt = git_push(
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

pipeline test_protected_push(_task) {{
  const receipt = git_push(
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
