//! Command-line projection of the shared package owner.

#[doc(inline)]
pub use harn_package::package::*;

pub fn artifacts_manifest(output: Option<&std::path::Path>) {
    artifacts_manifest_with(
        output,
        crate::commands::dump_protocol_artifacts::manifest_json,
    );
}

pub fn artifacts_check(manifest: &std::path::Path, json: bool) {
    artifacts_check_with(
        manifest,
        json,
        crate::commands::dump_protocol_artifacts::manifest_json_from,
    );
}

/// Command-line failure presentation stays outside the linked package owner.
pub fn load_runtime_extensions(anchor: &std::path::Path) -> RuntimeExtensions {
    match try_load_runtime_extensions(anchor) {
        Ok(extensions) => extensions,
        Err(error) => {
            eprintln!("error: {error}");
            std::process::exit(crate::exit::RUN_SETUP_FAILURE);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, path::Path};

    #[tokio::test(flavor = "current_thread")]
    async fn lifecycle_resolves_qualified_activated_persona() {
        let tmp = tempfile::tempdir().unwrap();
        test_support::installed_persona_project_fixture(
            tmp.path(),
            "[package]\nname = \"consumer\"\n",
            &["reviewer"],
            false,
            "",
        );
        activate_persona(
            Some(&tmp.path().join("harn.toml")),
            "agents/reviewer",
            &PersonaAttenuation::default(),
            100,
        )
        .unwrap();
        let status = crate::commands::persona::status_payload(
            Some(&tmp.path().join("harn.toml")),
            &tmp.path().join("state"),
            "agents/reviewer",
            Some("2026-01-01T00:00:00Z"),
        )
        .await
        .unwrap();
        assert_eq!(status.name, "agents/reviewer");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn installed_persona_cli_projects_provenance_and_requires_activation() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("root.harn"),
            "pub pipeline run(task) -> dict { return {root: true} }\n",
        )
        .unwrap();
        test_support::installed_persona_project_fixture(
            tmp.path(),
            &format!(
                "[package]\nname = \"consumer\"\n\n{}",
                test_support::persona_manifest("reviewer", "root.harn#run")
            ),
            &["reviewer", "archivist"],
            false,
            "",
        );
        let manifest = tmp.path().join("harn.toml");
        let payload = crate::commands::persona::list_payload(Some(&manifest)).unwrap();
        assert_eq!(payload[0]["id"], "agents/archivist");
        assert_eq!(payload[0]["source"]["package_alias"], "agents");
        assert_eq!(payload[0]["source"]["kind"], "installed_package");
        assert_eq!(payload[0]["source"]["package_version"], "1.2.3");
        assert!(
            harn_modules::package_execution::is_canonical_package_content_hash(
                payload[0]["source"]["content_hash"].as_str().unwrap()
            )
        );
        assert_eq!(payload[0]["source"]["integrity"], "ok");
        let inspect =
            crate::commands::persona::inspect_payload(Some(&manifest), "agents/reviewer").unwrap();
        assert_eq!(inspect["id"], "agents/reviewer");
        assert_eq!(inspect["name"], "reviewer");
        let status_error = crate::commands::persona::status_payload(
            Some(&manifest),
            &tmp.path().join("state"),
            "agents/reviewer",
            None,
        )
        .await
        .unwrap_err();
        assert_eq!(
            status_error,
            "active runtime persona 'agents/reviewer' not found"
        );
    }

    #[test]
    fn artifacts_check_detects_drift_against_stale_vendored_manifest() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("manifest.json");
        let stale = serde_json::json!({
            "schemaVersion": 1,
            "generatedBy": "harn dump-protocol-artifacts",
        });
        fs::write(&path, serde_json::to_string_pretty(&stale).unwrap() + "\n").unwrap();
        let report = check_artifact_manifest_from(
            &path,
            Path::new(env!("CARGO_MANIFEST_DIR")),
            crate::commands::dump_protocol_artifacts::manifest_json_from,
        )
        .unwrap();
        assert!(!report.ok);
        assert_eq!(report.vendored_schema_version, Some(1));
        assert!(!report.differences.is_empty());
    }

    #[test]
    fn artifacts_check_passes_for_current_manifest() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("manifest.json");
        let source_anchor = Path::new(env!("CARGO_MANIFEST_DIR"));
        let current =
            crate::commands::dump_protocol_artifacts::manifest_json_from(source_anchor).unwrap();
        fs::write(&path, current).unwrap();
        let report = check_artifact_manifest_from(
            &path,
            source_anchor,
            crate::commands::dump_protocol_artifacts::manifest_json_from,
        )
        .unwrap();
        assert!(report.ok, "expected no drift, got {:?}", report.differences);
    }
}
