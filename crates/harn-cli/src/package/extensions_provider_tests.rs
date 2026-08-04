//! Tests for the manifest-derived provider catalog.
//!
//! These share the process-wide provider catalog, so each one takes
//! `lock_harn_state_async()`, which resets contributed providers on
//! acquisition. Tests that register schemas without going through
//! `collect_manifest_triggers` also take `lock_manifest_provider_schemas()`
//! underneath it, because that is the lock production registration holds.
//! Always in that order: `collect_manifest_triggers` takes the schema lock
//! itself, so no test may hold it across a call that does.
//!
//! They live apart from the rest of the trigger-collection tests for that
//! reason as much as for file length.

use super::*;
use crate::package::test_support::*;
use crate::tests::common::harn_state_lock::lock_harn_state_async;

#[tokio::test(flavor = "current_thread")]
async fn collect_manifest_triggers_accepts_harn_provider_override() {
    let _state_guard = lock_harn_state_async().await;
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
    let _state_guard = lock_harn_state_async().await;
    let _provider_schema_guard = lock_manifest_provider_schemas().await;

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
        harn_vm::provider_metadata("webhook").is_some(),
        "core providers survive package loads"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn build_manifest_provider_catalog_keeps_dynamic_providers_scoped() {
    let _state_guard = lock_harn_state_async().await;
    let _provider_schema_guard = lock_manifest_provider_schemas().await;

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
}

/// Register `provider` with `schema_name` the way a package load does, and
/// report what the global catalog ends up holding for it.
async fn register_provider_under_schema(provider: &str, schema_name: &str) -> String {
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
        test_harn_connector_source_with_schema(provider, schema_name),
    )
    .unwrap();
    let schemas = build_manifest_provider_schemas(&load_runtime_extensions(&harn_file))
        .await
        .expect("manifest provider schemas build");
    register_manifest_provider_schemas(schemas).expect("providers register");
    harn_vm::provider_metadata(provider)
        .expect("provider metadata registered")
        .schema_name
}

/// Regression for #6068: two lock holders that claim the same provider id
/// for different payload schemas must not collide.
///
/// Re-registering an id under a *different* schema name is the one
/// disagreement `ProviderCatalog::merge` refuses to settle by load order —
/// it returns `DuplicateProvider`. In the suite that surfaced as whichever
/// dependency-connector test the scheduler happened to run second, so the
/// failure moved between runs.
///
/// This drives the two claims through one process on purpose. Under
/// `cargo nextest` every test gets its own process, so a leak *between*
/// two `#[test]` functions is invisible there and only the single-process
/// `cargo test` fallback would catch it; a regression written that way
/// would pass vacuously in the lane that gates merges. Sequential
/// acquisitions of the one lock reproduce the same contention in either
/// runner.
///
/// Deliberately does not take `lock_manifest_provider_schemas()`: the
/// reset under test belongs to the state lock alone.
#[tokio::test(flavor = "current_thread")]
async fn the_state_lock_drops_providers_contributed_by_an_earlier_holder() {
    {
        let _state_guard = lock_harn_state_async().await;
        assert_eq!(
            register_provider_under_schema("contested-provider", "FirstEventPayload").await,
            "FirstEventPayload"
        );
    }

    let _state_guard = lock_harn_state_async().await;
    assert_eq!(
        register_provider_under_schema("contested-provider", "SecondEventPayload").await,
        "SecondEventPayload",
        "the second holder saw the first holder's contribution"
    );
}
