use std::fs;
use std::path::Path;
use std::process::Command;

use tempfile::TempDir;

use crate::test_util::process::harn_e2e_binary;

fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(repo)
        .args(args)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("UTF-8 git output")
        .trim()
        .to_string()
}

/// A package repository whose tagged commit declares `manifest_version`.
///
/// The tag is always `v1.0.0`; the manifest version is a parameter so a test
/// can build the exact shape a registry cannot otherwise catch — a real tag
/// resolving to a real commit whose manifest sells a different version.
fn package_repo_at(manifest_version: &str) -> (TempDir, String) {
    let temp = tempfile::tempdir().expect("tempdir");
    let repo = temp.path();
    git(repo, &["init", "-q"]);
    git(repo, &["config", "user.email", "tests@example.com"]);
    git(repo, &["config", "user.name", "Harn Tests"]);
    git(repo, &["config", "core.hooksPath", "/dev/null"]);
    git(repo, &["config", "commit.gpgsign", "false"]);
    fs::write(
        repo.join("harn.toml"),
        format!(
            "[package]\nname = \"acme-pkg\"\nversion = \"{manifest_version}\"\n\n[exports]\ndefault = \"src/lib.harn\"\n"
        ),
    )
    .expect("write manifest");
    fs::create_dir_all(repo.join("src")).expect("create src");
    fs::write(repo.join("src/lib.harn"), "pub fn noop() {}\n").expect("write module");
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", "package"]);
    git(repo, &["tag", "v1.0.0"]);
    let rev = git(repo, &["rev-parse", "HEAD"]);
    (temp, rev)
}

fn registry_for(git_url: &Path, rev: &str, advertised_version: &str) -> String {
    // Registry sources must be absolute URLs; a `file://` repository keeps the
    // whole proof hermetic while still exercising the real git code path.
    let source = format!("file://{}", git_url.display());
    format!(
        r#"version = 2

[[package]]
name = "@acme/pkg"
repository = "{source}"
provenance = "{source}"

[[package.version]]
version = "{advertised_version}"
git = "{source}"
tag = "v1.0.0"
rev = "{rev}"
package = "acme-pkg"
provenance = "{source}"
"#
    )
}

fn verify_remote(index_body: &str) -> (bool, serde_json::Value) {
    let temp = tempfile::tempdir().expect("tempdir");
    let index = temp.path().join("harn-package-index.toml");
    fs::write(&index, index_body).expect("write registry fixture");
    let output = Command::new(harn_e2e_binary())
        .args([
            "package",
            "registry",
            "verify",
            index.to_str().unwrap(),
            "--remote",
            "--json",
        ])
        .output()
        .expect("run harn package registry verify --remote");
    let receipt: serde_json::Value =
        serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
            panic!(
                "printed JSON receipt ({error}); stdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            )
        });
    (output.status.success(), receipt)
}

#[test]
fn registry_verify_remote_proves_the_pinned_manifest_declares_the_advertised_version() {
    let (repo, rev) = package_repo_at("1.0.0");
    let (succeeded, receipt) = verify_remote(&registry_for(repo.path(), &rev, "1.0.0"));
    assert!(succeeded, "receipt: {receipt}");
    assert_eq!(receipt["ok"], true);
    assert_eq!(receipt["resolved_git_versions"], 1);
    assert_eq!(receipt["verified_manifests"], 1);
}

#[test]
fn registry_verify_remote_rejects_a_version_the_pinned_manifest_does_not_declare() {
    // The tag resolves to exactly the pinned commit, so tag identity passes.
    // Only reading the manifest catches that the commit declares 0.0.0.
    let (repo, rev) = package_repo_at("0.0.0");
    let (succeeded, receipt) = verify_remote(&registry_for(repo.path(), &rev, "1.0.0"));
    assert!(!succeeded, "receipt: {receipt}");
    assert_eq!(receipt["ok"], false);
    assert_eq!(
        receipt["resolved_git_versions"], 1,
        "tag identity must still pass: {receipt}"
    );
    assert_eq!(receipt["verified_manifests"], 0);
    let errors = receipt["errors"].as_array().expect("errors array");
    assert_eq!(errors.len(), 1, "receipt: {receipt}");
    let error = errors[0].as_str().expect("error string");
    assert!(
        error.contains("@acme/pkg@1.0.0")
            && error.contains("declares version 0.0.0")
            && error.contains("expected 1.0.0"),
        "error should name the package, the declared version, and the advertised one: {error}"
    );
}

#[test]
fn registry_verify_remote_rejects_a_manifest_naming_a_different_package() {
    let (repo, rev) = package_repo_at("1.0.0");
    let index = registry_for(repo.path(), &rev, "1.0.0")
        .replace("package = \"acme-pkg\"", "package = \"acme-other\"");
    let (succeeded, receipt) = verify_remote(&index);
    assert!(!succeeded, "receipt: {receipt}");
    assert_eq!(receipt["verified_manifests"], 0);
    let error = receipt["errors"][0].as_str().expect("error string");
    assert!(
        error.contains("declares package acme-pkg") && error.contains("expected acme-other"),
        "error should name both package identities: {error}"
    );
}

#[test]
fn registry_verify_cli_persists_the_same_success_receipt_it_prints() {
    let temp = tempfile::tempdir().expect("tempdir");
    let index = temp.path().join("harn-package-index.toml");
    let receipt = temp.path().join("registry-receipt.json");
    fs::write(
        &index,
        r#"version = 2

[[package]]
name = "@acme/pkg"
repository = "https://github.com/acme/pkg"
provenance = "https://github.com/acme/pkg"

[[package.version]]
version = "1.0.0"
git = "https://github.com/acme/pkg"
tag = "v1.0.0"
rev = "0123456789abcdef0123456789abcdef01234567"
provenance = "https://github.com/acme/pkg/releases/tag/v1.0.0"
"#,
    )
    .expect("write registry fixture");

    let output = Command::new(harn_e2e_binary())
        .args([
            "package",
            "registry",
            "verify",
            index.to_str().unwrap(),
            "--json",
            "--receipt-out",
            receipt.to_str().unwrap(),
        ])
        .output()
        .expect("run harn package registry verify");
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let printed: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("printed JSON receipt");
    let persisted: serde_json::Value =
        serde_json::from_slice(&fs::read(receipt).expect("persisted receipt"))
            .expect("persisted JSON receipt");
    assert_eq!(persisted, printed);
    assert_eq!(
        printed["schema_version"],
        "harn.package_registry_verification.v1"
    );
    assert_eq!(printed["ok"], true);
    assert_eq!(printed["package_count"], 1);
    assert_eq!(printed["version_count"], 1);
    // An offline run proves nothing about the pinned revision's manifest.
    assert_eq!(printed["verified_manifests"], 0);
}

#[test]
fn registry_verify_remote_leaves_a_yanked_version_unproven() {
    // The rev is not even a real commit. A yanked version is unresolvable, so
    // remote verification has no live claim to check and must not invent one.
    let (repo, _rev) = package_repo_at("1.0.0");
    let index = registry_for(
        repo.path(),
        "0123456789abcdef0123456789abcdef01234567",
        "1.0.0",
    )
    .replace(
        "package = \"acme-pkg\"",
        "package = \"acme-pkg\"\nyanked = true",
    );
    let (succeeded, receipt) = verify_remote(&index);
    assert!(succeeded, "receipt: {receipt}");
    assert_eq!(receipt["ok"], true);
    assert_eq!(receipt["version_count"], 1);
    assert_eq!(receipt["resolved_git_versions"], 0);
    assert_eq!(receipt["verified_manifests"], 0);
}
