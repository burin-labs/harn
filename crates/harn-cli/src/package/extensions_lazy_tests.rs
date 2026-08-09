use super::*;
use crate::package::test_support::{test_vm, write_trigger_project};

fn write_local_trigger_project(
    root: &std::path::Path,
    handler: &str,
    module_source: &str,
) -> std::path::PathBuf {
    let manifest = format!(
        r#"
[package]
name = "workspace"

[exports]
handlers = "lib.harn"

[[triggers]]
id = "github-new-issue"
kind = "webhook"
provider = "webhook"
tier = "suggest"
match = {{ events = ["issues.opened"] }}
handler = "{handler}"
secrets = {{ signing_secret = "webhook/signing-secret" }}
"#,
    );
    write_trigger_project(root, &manifest, Some(module_source))
}

fn write_local_hook_project(
    root: &std::path::Path,
    handler: &str,
    module_source: &str,
) -> std::path::PathBuf {
    let manifest = format!(
        r#"
[package]
name = "workspace"

[exports]
handlers = "lib.harn"

[[hooks]]
event = "PostToolUse"
pattern = "*"
handler = "{handler}"
"#,
    );
    write_trigger_project(root, &manifest, Some(module_source))
}

fn write_local_predicate_project(
    root: &std::path::Path,
    module_source: &str,
) -> std::path::PathBuf {
    let manifest = r#"
[package]
name = "workspace"

[exports]
handlers = "lib.harn"

[[triggers]]
id = "github-new-issue"
kind = "webhook"
provider = "webhook"
tier = "suggest"
match = { events = ["issues.opened"] }
when = "handlers::should_handle"
handler = "handlers::on_new_issue"
secrets = { signing_secret = "webhook/signing-secret" }
"#;
    write_trigger_project(root, manifest, Some(module_source))
}

fn write_cross_trigger_predicate_project(
    root: &std::path::Path,
    module_source: &str,
) -> std::path::PathBuf {
    let manifest = r#"
[package]
name = "workspace"

[exports]
handlers = "lib.harn"

[[triggers]]
id = "first-local-handler"
kind = "webhook"
provider = "webhook"
tier = "suggest"
match = { events = ["issues.opened"] }
handler = "handlers::on_new_issue"
secrets = { signing_secret = "webhook/signing-secret" }

[[triggers]]
id = "second-invalid-predicate"
kind = "webhook"
provider = "webhook"
tier = "suggest"
match = { events = ["issues.edited"] }
when = "handlers::should_handle"
handler = "worker://issues"
secrets = { signing_secret = "webhook/signing-secret" }
"#;
    write_trigger_project(root, manifest, Some(module_source))
}

#[tokio::test(flavor = "current_thread")]
async fn lazy_manifest_trigger_collection_defers_handler_module_initialization() {
    let tmp = tempfile::tempdir().unwrap();
    let harn_file = write_local_trigger_project(
        tmp.path(),
        "handlers::on_new_issue",
        r#"
import "std/triggers"

let must_not_initialize = 1 / 0

pub fn on_new_issue(harness: Harness, event: TriggerEvent) -> string {
  return event.kind
}
"#,
    );
    let extensions = load_runtime_extensions(&harn_file);
    assert!(extensions.triggers[0].execution_guard.is_none());
    let mut vm = test_vm();

    let collected = collect_manifest_triggers(&mut vm, &extensions)
        .await
        .expect("default collection must not initialize the handler module");
    let CollectedTriggerHandler::Local { callable, .. } = &collected[0].handler else {
        panic!("expected local handler");
    };
    assert!(matches!(callable, harn_vm::VmCallable::Lazy(_)));

    assert!(
        collect_manifest_triggers_with_initialization(
            &mut vm,
            &extensions,
            ManifestHandlerInitialization::Eager,
        )
        .await
        .is_err(),
        "explicit eager collection must initialize and validate the handler module"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn eager_and_lazy_manifest_trigger_collection_reject_missing_and_private_handlers() {
    for (case, module_source) in [
        (
            "missing",
            "let must_not_initialize = 1 / 0\npub fn other(event) { return event.kind }",
        ),
        (
            "private",
            "let must_not_initialize = 1 / 0\nfn on_new_issue(event) { return event.kind }",
        ),
    ] {
        let tmp = tempfile::tempdir().unwrap();
        let harn_file =
            write_local_trigger_project(tmp.path(), "handlers::on_new_issue", module_source);
        let extensions = load_runtime_extensions(&harn_file);

        for initialization in [
            ManifestHandlerInitialization::Eager,
            ManifestHandlerInitialization::OnDispatch,
        ] {
            let result = collect_manifest_triggers_with_initialization(
                &mut test_vm(),
                &extensions,
                initialization,
            )
            .await;
            assert!(
                result.is_err(),
                "{case} trigger handler unexpectedly collected in {initialization:?} mode"
            );
            assert!(
                result
                    .unwrap_err()
                    .to_string()
                    .contains("handler 'handlers::on_new_issue' is not exported"),
                "{case} trigger handler must fail the export boundary in {initialization:?} mode"
            );
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn eager_and_lazy_manifest_trigger_collection_validate_predicates_before_initialization() {
    let tmp = tempfile::tempdir().unwrap();
    let harn_file = write_local_predicate_project(
        tmp.path(),
        r#"
import "std/triggers"

let must_not_initialize = 1 / 0

pub fn on_new_issue(harness: Harness, event: TriggerEvent) -> string {
  return event.kind
}

pub fn should_handle(event: TriggerEvent) -> string {
  return event.kind
}
"#,
    );
    let extensions = load_runtime_extensions(&harn_file);

    for initialization in [
        ManifestHandlerInitialization::Eager,
        ManifestHandlerInitialization::OnDispatch,
    ] {
        let error = collect_manifest_triggers_with_initialization(
            &mut test_vm(),
            &extensions,
            initialization,
        )
        .await
        .expect_err("invalid predicate must fail before module initialization");
        assert!(
            error
                .to_string()
                .contains("must have signature fn(TriggerEvent) -> bool or Result<bool, _>"),
            "predicate signature must win over {initialization:?} initialization: {error}"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn all_trigger_declarations_validate_before_any_eager_initialization() {
    let tmp = tempfile::tempdir().unwrap();
    let harn_file = write_cross_trigger_predicate_project(
        tmp.path(),
        r#"
import "std/triggers"

let must_not_initialize = 1 / 0

pub fn on_new_issue(harness: Harness, event: TriggerEvent) -> string {
  return event.kind
}

pub fn should_handle(event: TriggerEvent) -> string {
  return event.kind
}
"#,
    );
    let extensions = load_runtime_extensions(&harn_file);

    for initialization in [
        ManifestHandlerInitialization::Eager,
        ManifestHandlerInitialization::OnDispatch,
    ] {
        let error = collect_manifest_triggers_with_initialization(
            &mut test_vm(),
            &extensions,
            initialization,
        )
        .await
        .expect_err("the second trigger predicate must fail before the first module initializes");
        assert!(
            error
                .to_string()
                .contains("must have signature fn(TriggerEvent) -> bool or Result<bool, _>"),
            "whole-set predicate validation must win over {initialization:?} initialization: {error}"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn eager_and_lazy_hook_installation_reject_missing_and_private_handlers() {
    for (case, module_source) in [
        ("missing", "pub fn other(ctx) { return ctx }"),
        ("private", "fn handle(ctx) { return ctx }"),
    ] {
        let tmp = tempfile::tempdir().unwrap();
        let harn_file = write_local_hook_project(tmp.path(), "handlers::handle", module_source);
        let extensions = load_runtime_extensions(&harn_file);
        assert_eq!(extensions.hooks.len(), 1, "{case} hook fixture must load");

        for initialization in [
            ManifestHandlerInitialization::Eager,
            ManifestHandlerInitialization::OnDispatch,
        ] {
            let result = install_manifest_hooks_with_initialization(
                &mut test_vm(),
                &extensions,
                initialization,
            )
            .await;
            assert!(
                result.is_err(),
                "{case} handler unexpectedly installed in {initialization:?} mode"
            );
            let error = result.unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("hook handler 'handle' is not exported by module 'handlers'"),
                "{case} handler must be rejected in {initialization:?} mode: {error}"
            );
        }
    }
}
