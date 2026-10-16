use crate::package::registry::verify::verify_package_registry_impl;
use crate::package::test_support::run_git;
use crate::package::*;

fn valid_index() -> &'static str {
    r#"
version = 2

[[package]]
name = "@acme/pkg"
repository = "https://github.com/acme/pkg"
provenance = "https://github.com/acme/pkg"

[[package.version]]
version = "1.0.0"
git = "https://github.com/acme/pkg"
tag = "v1.0.0"
rev = "0123456789abcdef0123456789abcdef01234567"
package = "pkg"
provenance = "https://github.com/acme/pkg/releases/tag/v1.0.0"
"#
}

#[test]
fn verification_receipt_reports_validated_inventory() {
    let temp = tempfile::tempdir().unwrap();
    let index = temp.path().join("index.toml");
    fs::write(&index, valid_index()).unwrap();
    let receipt = verify_package_registry_impl(index.to_str().unwrap(), false);
    assert!(receipt.ok);
    assert_eq!(
        receipt.schema_version,
        "harn.package_registry_verification.v1"
    );
    assert_eq!(receipt.index_version, 2);
    assert_eq!(receipt.package_count, 1);
    assert_eq!(receipt.version_count, 1);
    assert_eq!(receipt.resolved_git_versions, 0);
}

#[test]
fn verification_receipt_retains_schema_failure() {
    let temp = tempfile::tempdir().unwrap();
    let index = temp.path().join("index.toml");
    fs::write(
        &index,
        valid_index().replacen("version = 2", "version = 1", 1),
    )
    .unwrap();
    let receipt = verify_package_registry_impl(index.to_str().unwrap(), false);
    assert!(!receipt.ok);
    assert!(receipt.errors[0].contains("expected 2"));
}

#[test]
fn remote_verification_rejects_a_tag_commit_mismatch() {
    let temp = tempfile::tempdir().unwrap();
    let repository = temp.path().join("package");
    fs::create_dir(&repository).unwrap();
    run_git(&repository, &["init"]);
    fs::write(repository.join("harn.toml"), "[package]\nname = \"pkg\"\n").unwrap();
    run_git(&repository, &["add", "harn.toml"]);
    run_git(&repository, &["commit", "-m", "fixture"]);
    run_git(&repository, &["tag", "v1.0.0"]);
    let repository_url = Url::from_directory_path(&repository).unwrap().to_string();
    let index = temp.path().join("index.toml");
    fs::write(
        &index,
        format!(
            r#"version = 2

[[package]]
name = "@acme/pkg"
repository = "{repository_url}"
provenance = "{repository_url}"

[[package.version]]
version = "1.0.0"
git = "{repository_url}"
tag = "v1.0.0"
rev = "ffffffffffffffffffffffffffffffffffffffff"
package = "pkg"
provenance = "{repository_url}"
"#
        ),
    )
    .unwrap();

    let receipt = verify_package_registry_impl(index.to_str().unwrap(), true);
    assert!(!receipt.ok);
    assert_eq!(receipt.resolved_git_versions, 0);
    assert!(
        receipt.errors[0].contains("tag v1.0.0 resolves to"),
        "{}",
        receipt.errors[0]
    );
}
