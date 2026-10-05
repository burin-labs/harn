//! Registry index parsing, validation, and version selection.

use crate::package::*;

fn registry_package_with_versions(versions: &[(&str, bool)]) -> RegistryPackage {
    let entries: Vec<serde_json::Value> = versions
        .iter()
        .map(|(version, yanked)| {
            serde_json::json!({
                "version": version,
                "git": "https://example.com/pkg.git",
                "yanked": yanked,
            })
        })
        .collect();
    serde_json::from_value(serde_json::json!({
        "name": "@burin/pkg",
        "repository": "https://example.com/pkg.git",
        "version": entries,
    }))
    .expect("registry package fixture deserializes")
}

#[test]
fn latest_registry_version_prefers_stable_over_newer_prerelease() {
    let package = registry_package_with_versions(&[
        ("1.2.0", false),
        ("2.0.0-rc.1", false),
        ("1.3.0", false),
    ]);
    let latest = latest_registry_version(&package).expect("a version is selected");
    assert_eq!(
        latest.version, "1.3.0",
        "a prerelease must not shadow the highest stable release"
    );
}

#[test]
fn latest_registry_version_falls_back_to_prerelease_when_no_stable_exists() {
    let package =
        registry_package_with_versions(&[("0.1.0-alpha.1", false), ("0.1.0-alpha.2", false)]);
    let latest = latest_registry_version(&package).expect("a version is selected");
    assert_eq!(
        latest.version, "0.1.0-alpha.2",
        "packages with only prereleases still resolve to the highest prerelease"
    );
}

#[test]
fn registry_index_accepts_archive_versions_and_requires_checksums() {
    let content = r#"
version = 2

[[package]]
name = "@acme/rules"
repository = "https://github.com/acme/rules"
provenance = "https://github.com/acme/rules"

[[package.version]]
version = "1.0.0"
archive = "https://cdn.example.test/acme-rules-1.0.0.tar.gz"
package = "acme-rules"
checksum = "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
provenance = "https://github.com/acme/rules/releases/tag/v1.0.0"
"#;
    let index = parse_package_registry_index("fixture", content).unwrap();
    assert_eq!(index.packages[0].versions[0].git, None);
    assert_eq!(
        index.packages[0].versions[0].archive.as_deref(),
        Some("https://cdn.example.test/acme-rules-1.0.0.tar.gz")
    );

    let missing_checksum = r#"
version = 2

[[package]]
name = "@acme/rules"
repository = "https://github.com/acme/rules"
provenance = "https://github.com/acme/rules"

[[package.version]]
version = "1.0.0"
archive = "https://cdn.example.test/acme-rules-1.0.0.tar.gz"
provenance = "https://github.com/acme/rules/releases/tag/v1.0.0"
"#;
    let error = parse_package_registry_index("fixture", missing_checksum).unwrap_err();
    assert!(error.to_string().contains("must specify checksum"));
}

#[test]
fn registry_index_rejects_invalid_names_and_duplicate_versions() {
    let content = r#"
version = 2

[[package]]
name = "@bad/"
repository = "https://github.com/acme/acme-lib"
provenance = "https://github.com/acme/acme-lib"

[[package.version]]
version = "1.0.0"
git = "https://github.com/acme/acme-lib"
tag = "v1.0.0"
rev = "0123456789abcdef0123456789abcdef01234567"
provenance = "https://github.com/acme/acme-lib/releases/tag/v1.0.0"
"#;
    let error = parse_package_registry_index("fixture", content).unwrap_err();
    assert!(error.to_string().contains("invalid package name"));

    let content = r#"
version = 2

[[package]]
name = "@burin/acme-lib"
repository = "https://github.com/acme/acme-lib"
provenance = "https://github.com/acme/acme-lib"

[[package.version]]
version = "1.0.0"
git = "https://github.com/acme/acme-lib"
tag = "v1.0.0"
rev = "0123456789abcdef0123456789abcdef01234567"
provenance = "https://github.com/acme/acme-lib/releases/tag/v1.0.0"

[[package.version]]
version = "1.0.0"
git = "https://github.com/acme/acme-lib"
tag = "v1.0.0"
rev = "0123456789abcdef0123456789abcdef01234567"
provenance = "https://github.com/acme/acme-lib/releases/tag/v1.0.0"
"#;
    let error = parse_package_registry_index("fixture", content).unwrap_err();
    assert!(error.to_string().contains("more than once"));
}

fn valid_git_index(version_entry: &str) -> String {
    format!(
        r#"
version = 2

[[package]]
name = "@burin/acme-lib"
repository = "https://github.com/acme/acme-lib"
provenance = "https://github.com/acme/acme-lib"

[[package.version]]
version = "1.0.0"
git = "https://github.com/acme/acme-lib"
package = "acme-lib"
provenance = "https://github.com/acme/acme-lib/releases/tag/v1.0.0"
{version_entry}
"#
    )
}

#[test]
fn registry_v2_rejects_legacy_schema_and_mutable_refs() {
    let legacy = valid_git_index(
        r#"tag = "v1.0.0"
rev = "0123456789abcdef0123456789abcdef01234567""#,
    )
    .replacen("version = 2", "version = 1", 1);
    let error = parse_package_registry_index("fixture", &legacy).unwrap_err();
    assert!(error.to_string().contains("expected 2"));

    let mutable = valid_git_index(
        r#"branch = "main"
rev = "0123456789abcdef0123456789abcdef01234567""#,
    );
    let error = parse_package_registry_index("fixture", &mutable).unwrap_err();
    assert!(error.to_string().contains("unknown field `branch`"));
}

#[test]
fn registry_v2_requires_tag_commit_and_coherent_provenance() {
    let symbolic_rev = valid_git_index(
        r#"tag = "v1.0.0"
rev = "v1.0.0""#,
    );
    let error = parse_package_registry_index("fixture", &symbolic_rev).unwrap_err();
    assert!(error.to_string().contains("full 40- or 64-character"));

    let wrong_provenance = valid_git_index(
        r#"tag = "v1.0.0"
rev = "0123456789abcdef0123456789abcdef01234567""#,
    )
    .replace(
        "https://github.com/acme/acme-lib/releases/tag/v1.0.0",
        "https://github.com/other/acme-lib/releases/tag/v1.0.0",
    );
    let error = parse_package_registry_index("fixture", &wrong_provenance).unwrap_err();
    assert!(error
        .to_string()
        .contains("must identify the declared package repository"));
}

#[test]
fn registry_v2_rejects_duplicate_immutable_content_identity() {
    let mut content = valid_git_index(
        r#"tag = "v1.0.0"
rev = "0123456789abcdef0123456789abcdef01234567""#,
    );
    content.push_str(
        r#"

[[package.version]]
version = "1.0.1"
git = "https://github.com/acme/acme-lib"
tag = "v1.0.1"
rev = "0123456789abcdef0123456789abcdef01234567"
package = "acme-lib"
provenance = "https://github.com/acme/acme-lib/releases/tag/v1.0.1"
"#,
    );
    let error = parse_package_registry_index("fixture", &content).unwrap_err();
    assert!(error
        .to_string()
        .contains("reuses immutable content identity"));
}
