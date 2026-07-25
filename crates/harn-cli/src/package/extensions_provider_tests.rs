//! Tests for the manifest-derived provider catalog.
//!
//! These share the process-wide provider catalog, so each one takes
//! `lock_manifest_provider_schemas()` and resets the catalog around its
//! assertions. They live apart from the rest of the trigger-collection tests
//! for that reason as much as for file length.

use super::*;
use crate::package::test_support::*;

#[tokio::test(flavor = "current_thread")]
async fn collect_manifest_triggers_accepts_harn_provider_override() {
    let tmp = tempfile::tempdir().unwrap();
    let harn_file = write_trigger_project(
        tmp.path(),
        r#"
[[providers]]
id = "echo"
connector = { harn = "./echo_connector.harn" }

[[triggers]]
id = "echo-webhook"
kind = "webhook"
provider = "echo"
path = "/hooks/echo"
match = { path = "/hooks/echo", events = ["echo.received"] }
handler = "worker://echo-queue"
"#,
        None,
    );
    fs::write(
        tmp.path().join("echo_connector.harn"),
        test_harn_connector_source("echo"),
    )
    .unwrap();

    let mut vm = test_vm();
    let collected = collect_manifest_triggers(&mut vm, &load_runtime_extensions(&harn_file))
        .await
        .expect("trigger collection succeeds");
    assert_eq!(collected.len(), 1);
    assert_eq!(collected[0].config.provider.as_str(), "echo");
    assert_eq!(
        harn_vm::provider_metadata("echo")
            .expect("provider metadata registered")
            .schema_name,
        "EchoEventPayload"
    );
}

/// The reported failure was an orchestrator harness losing its `echo`
/// connector because a persona command loaded a package in the same process a
/// moment later. Loading a package contributes providers; it does not declare
/// the whole catalog.
#[tokio::test(flavor = "current_thread")]
async fn loading_a_second_package_keeps_the_first_packages_providers() {
    let _provider_schema_guard = lock_manifest_provider_schemas().await;
    harn_vm::reset_provider_catalog();

    let mut tmpdirs = Vec::new();
    for provider in ["echo-first", "echo-second"] {
        let tmp = tempfile::tempdir().unwrap();
        let harn_file = write_trigger_project(
            tmp.path(),
            &format!(
                r#"
[[providers]]
id = "{provider}"
connector = {{ harn = "./echo_connector.harn" }}
"#
            ),
            None,
        );
        fs::write(
            tmp.path().join("echo_connector.harn"),
            test_harn_connector_source(provider),
        )
        .unwrap();
        let schemas = build_manifest_provider_schemas(&load_runtime_extensions(&harn_file))
            .await
            .expect("manifest provider schemas build");
        register_manifest_provider_schemas(schemas).expect("providers register");
        tmpdirs.push(tmp);
    }

    for provider in ["echo-first", "echo-second"] {
        assert!(
            harn_vm::provider_metadata(provider).is_some(),
            "provider '{provider}' was erased by another package's load"
        );
    }
    assert!(
        harn_vm::provider_metadata("github").is_some(),
        "builtin providers survive package loads"
    );

    harn_vm::reset_provider_catalog();
}

#[tokio::test(flavor = "current_thread")]
async fn build_manifest_provider_catalog_keeps_dynamic_providers_scoped() {
    let _provider_schema_guard = lock_manifest_provider_schemas().await;
    harn_vm::reset_provider_catalog();

    for provider in ["echo-a", "echo-b", "echo-c"] {
        let tmp = tempfile::tempdir().unwrap();
        let harn_file = write_trigger_project(
            tmp.path(),
            &format!(
                r#"
[[providers]]
id = "{provider}"
connector = {{ harn = "./echo_connector.harn" }}
"#
            ),
            None,
        );
        fs::write(
            tmp.path().join("echo_connector.harn"),
            test_harn_connector_source(provider),
        )
        .unwrap();

        {
            let catalog = build_manifest_provider_catalog(&load_runtime_extensions(&harn_file))
                .await
                .expect("manifest provider catalog builds");
            assert_eq!(
                catalog
                    .metadata_for(provider)
                    .expect("dynamic provider metadata")
                    .schema_name,
                "EchoEventPayload"
            );
        }

        assert!(
            harn_vm::provider_metadata(provider).is_none(),
            "building a package provider catalog must not mutate the global catalog"
        );
    }

    harn_vm::reset_provider_catalog();
}
