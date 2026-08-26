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
async fn lazy_hook_installation_defers_missing_and_private_handlers() {
    for (case, module_source) in [
        ("missing", "pub fn other(ctx) { return ctx }"),
        ("private", "fn handle(ctx) { return ctx }"),
    ] {
        harn_vm::reset_thread_local_state();
        let tmp = tempfile::tempdir().unwrap();
        let harn_file = write_local_hook_project(tmp.path(), "handlers::handle", module_source);
        let extensions = load_runtime_extensions(&harn_file);
        assert_eq!(extensions.hooks.len(), 1, "{case} hook fixture must load");

        install_manifest_hooks_with_initialization(
            &mut test_vm(),
            &extensions,
            ManifestHandlerInitialization::OnDispatch,
        )
        .await
        .unwrap_or_else(|error| panic!("{case} unused handler initialized eagerly: {error}"));

        let error = install_manifest_hooks_with_initialization(
            &mut test_vm(),
            &extensions,
            ManifestHandlerInitialization::Eager,
        )
        .await
        .expect_err("eager validation must reject the broken handler");
        assert!(
            error
                .to_string()
                .contains("hook handler 'handle' is not exported by module 'handlers'"),
            "{case} handler must be rejected in eager mode: {error}"
        );
        harn_vm::reset_thread_local_state();
    }
}

#[tokio::test(flavor = "current_thread")]
async fn lazy_broken_hook_fails_closed_when_its_event_dispatches() {
    harn_vm::reset_thread_local_state();
    let tmp = tempfile::tempdir().unwrap();
    let harn_file = write_local_hook_project(
        tmp.path(),
        "handlers::handle",
        "pub fn other(ctx) { return ctx }",
    );
    let extensions = load_runtime_extensions(&harn_file);
    let mut vm = test_vm();
    install_manifest_hooks_with_initialization(
        &mut vm,
        &extensions,
        ManifestHandlerInitialization::OnDispatch,
    )
    .await
    .expect("lazy hook registration");
    let ctx = harn_vm::AsyncBuiltinCtx::from_vm(vm);

    let error = harn_vm::orchestration::run_post_tool_hooks_with_ctx(
        Some(&ctx),
        "write_file",
        &serde_json::json!({"path": "guarded.txt"}),
        "ok",
    )
    .await
    .expect_err("a matching unresolved policy hook must stop dispatch");
    assert!(
        error
            .to_string()
            .contains("function 'handle' is not exported by module"),
        "unexpected dispatch error: {error}"
    );
    harn_vm::reset_thread_local_state();
}

#[tokio::test(flavor = "current_thread")]
async fn lazy_hook_materializes_a_reachable_dependency_only_when_it_dispatches() {
    harn_vm::reset_thread_local_state();
    let tmp = tempfile::tempdir().unwrap();
    let harn_file = write_local_hook_project(
        tmp.path(),
        "handlers::handle",
        r#"
import "missing"

pub fn handle(_harness: Harness, _payload: dict) { return nil }
"#,
    );
    let manifest = std::fs::read_to_string(tmp.path().join(MANIFEST)).expect("read manifest");
    std::fs::write(
        tmp.path().join(MANIFEST),
        format!(
            "{manifest}\n[dependencies]\nmissing = {{ git = \"https://example.invalid/missing.git\" }}\n"
        ),
    )
    .expect("add unreachable dependency");
    let extensions = try_load_root_runtime_extensions(&harn_file)
        .expect("root hook registration must not prepare dependencies");
    let mut vm = test_vm();
    install_manifest_hooks_with_initialization(
        &mut vm,
        &extensions,
        ManifestHandlerInitialization::OnDispatch,
    )
    .await
    .expect("lazy hook registration");
    assert!(!tmp.path().join(LOCK_FILE).exists());

    let ctx = harn_vm::AsyncBuiltinCtx::from_vm(vm);
    let error = harn_vm::orchestration::run_post_tool_hooks_with_ctx(
        Some(&ctx),
        "write_file",
        &serde_json::json!({"path": "guarded.txt"}),
        "ok",
    )
    .await
    .expect_err("matching hook must reach dependency preparation and fail closed");
    assert!(
        error.to_string().contains("harn.lock"),
        "dispatch must report the missing dependency lock: {error}"
    );
    harn_vm::reset_thread_local_state();
}

#[tokio::test(flavor = "current_thread")]
async fn lazy_hook_resolves_a_path_dependency_from_a_fresh_generation() {
    harn_vm::reset_thread_local_state();
    let tmp = tempfile::tempdir().unwrap();
    let harn_file = write_local_hook_project(
        tmp.path(),
        "handlers::handle",
        r#"
import { policy_value } from "fixture_dep/policy"

pub fn handle(_harness: Harness, _payload: dict) {
  if policy_value() != 42 { return Err("dependency returned the wrong value") }
  return nil
}
"#,
    );
    let manifest = std::fs::read_to_string(tmp.path().join(MANIFEST)).expect("read manifest");
    std::fs::write(
        tmp.path().join(MANIFEST),
        format!(
            "{manifest}\n[dependencies]\nfixture_dep = {{ path = \"./vendor/fixture_dep\" }}\n"
        ),
    )
    .expect("add path dependency");
    let dependency = tmp.path().join("vendor/fixture_dep");
    std::fs::create_dir_all(&dependency).expect("create path dependency");
    std::fs::write(
        dependency.join(MANIFEST),
        "[package]\nname = \"fixture_dep\"\n",
    )
    .expect("write dependency manifest");
    std::fs::write(
        dependency.join("policy.harn"),
        "pub fn policy_value() -> int { return 42 }\n",
    )
    .expect("write dependency module");
    let cache = tempfile::tempdir().expect("package cache");
    let workspace = PackageWorkspace::for_test(tmp.path(), cache.path());
    install_packages_in(&workspace, false, None, false)
        .expect("create lock and initial generation");
    std::fs::remove_dir_all(tmp.path().join(".harn")).expect("remove initial generation");

    let extensions = try_load_root_runtime_extensions(&harn_file)
        .expect("root hook registration must leave the generation absent");
    let mut vm = test_vm();
    install_manifest_hooks_with_initialization(
        &mut vm,
        &extensions,
        ManifestHandlerInitialization::OnDispatch,
    )
    .await
    .expect("lazy hook registration");
    assert!(!tmp.path().join(".harn/package-current.toml").exists());

    let ctx = harn_vm::AsyncBuiltinCtx::from_vm(vm);
    harn_vm::orchestration::run_post_tool_hooks_with_ctx(
        Some(&ctx),
        "write_file",
        &serde_json::json!({"path": "guarded.txt"}),
        "ok",
    )
    .await
    .expect("matching hook must materialize and execute its path dependency");
    assert!(tmp.path().join(".harn/package-current.toml").is_file());
    harn_vm::reset_thread_local_state();
}
