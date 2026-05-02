// `lock_harn_state()` returns a sync `MutexGuard` that is intentionally held
// across the test's `.await` points (the trigger registry is process-global
// and must stay locked for the duration of seed dispatch + replay). The
// in-crate `commands::trigger::replay::tests` module uses the same allow.
#![allow(clippy::await_holding_lock)]

//! In-process coverage of `harn trigger replay` and `harn trigger cancel`.
//!
//! Tier 1H follow-up (#1129, parent #1106) of the de-flake epic (#1057):
//! these tests previously ran the `harn` binary as a subprocess to seed a
//! trigger event via `trigger_fire(...)` from a Harn pipeline and then
//! invoked `harn trigger replay` / `harn trigger cancel` for the seeded
//! event. They now seed the workspace event log directly via
//! `harn_vm::Dispatcher::dispatch(...)` and call the corresponding
//! library APIs (`harn_cli::commands::trigger::replay::*`,
//! `harn_cli::commands::trigger::cancel::*`) to produce the JSON report
//! the CLI dispatcher would emit.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;

use harn_cli::commands::trigger::cancel::{cancel_event_in_process, CancelReport};
use harn_cli::commands::trigger::replay::{
    build_replay_vm, replay_bulk_in_process, replay_report_for_event_log, TriggerReplayReport,
};
use harn_cli::package;
use harn_cli::tests::common::{cwd_lock, harn_state_lock};
use harn_vm::event_log::{install_default_for_base_dir, AnyEventLog, EventLog, LogEvent, Topic};
use harn_vm::triggers::event::{GitHubEventCommon, GitHubEventPayload, GitHubIssuesEventPayload};
use harn_vm::{
    triggers::event::KnownProviderPayload, ProviderId, ProviderPayload, SignatureStatus,
    TriggerEvent,
};
use serde_json::Value;
use std::fs;
use tempfile::TempDir;
use time::OffsetDateTime;

const TRIGGER_ID: &str = "github-new-issue";

fn write_file(dir: &Path, relative: &str, contents: &str) {
    let path = dir.join(relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, contents).unwrap();
}

fn write_manifest(dir: &Path) {
    write_file(
        dir,
        "harn.toml",
        r#"
[package]
name = "fixture"

[exports]
handlers = "lib.harn"

[[triggers]]
id = "github-new-issue"
kind = "webhook"
provider = "github"
match = { events = ["issues.opened"] }
handler = "handlers::on_issue"
secrets = { signing_secret = "github/webhook-secret" }
"#,
    );
}

fn write_lib(dir: &Path, body: &str) {
    write_file(dir, "lib.harn", body);
}

fn run_in_harn_runtime<F, Fut, R>(future_factory: F) -> R
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = R>,
    R: Send + 'static,
{
    let handle = thread::Builder::new()
        .name("harn-cli-test".to_string())
        .stack_size(harn_cli::CLI_RUNTIME_STACK_SIZE)
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("build runtime");
            runtime.block_on(future_factory())
        })
        .expect("spawn runtime thread");
    handle.join().expect("runtime thread completed")
}

fn build_issue_event(delivery_id: &str, tenant_field: Option<&str>) -> TriggerEvent {
    let mut raw = serde_json::Map::new();
    raw.insert("action".to_string(), Value::String("opened".to_string()));
    if let Some(tenant) = tenant_field {
        raw.insert("tenant".to_string(), Value::String(tenant.to_string()));
    }
    TriggerEvent::new(
        ProviderId::from("github"),
        "issues.opened",
        None,
        delivery_id.to_string(),
        None,
        BTreeMap::new(),
        ProviderPayload::Known(KnownProviderPayload::GitHub(GitHubEventPayload::Issues(
            GitHubIssuesEventPayload {
                common: GitHubEventCommon {
                    event: "issues".to_string(),
                    action: Some("opened".to_string()),
                    delivery_id: Some(delivery_id.to_string()),
                    installation_id: Some(42),
                    topic: None,
                    repository: None,
                    repo: None,
                    raw: Value::Object(raw),
                },
                issue: serde_json::json!({}),
            },
        ))),
        SignatureStatus::Verified,
    )
}

/// Install the workspace's manifest triggers so `harn_vm` registers the
/// `github-new-issue` binding. Mirrors what `package::install_manifest_triggers`
/// does in the `harn run` / `harn trigger replay` paths.
async fn install_workspace_triggers(workspace_root: &Path) {
    let mut vm = build_replay_vm(workspace_root);
    let extensions = package::load_runtime_extensions(workspace_root);
    package::install_runtime_extensions(&extensions);
    package::install_manifest_triggers(&mut vm, &extensions)
        .await
        .expect("install manifest triggers");
}

/// Mirror `harn_vm::stdlib::triggers_stdlib::dispatch_trigger_event` so the
/// seed step writes the `trigger_event` log entry that the replay/cancel
/// loaders look up by event id, in addition to the outbox dispatch
/// outcome that `Dispatcher::dispatch` produces on its own.
async fn dispatch_seed_event(
    workspace_root: &Path,
    event_log: Arc<AnyEventLog>,
    event: TriggerEvent,
) {
    install_workspace_triggers(workspace_root).await;
    let binding = harn_vm::resolve_live_trigger_binding(TRIGGER_ID, None)
        .expect("resolve live binding for seed dispatch");

    // 1. Append the trigger_event record so the replay loader can find it.
    let topic = Topic::new("triggers.events").expect("trigger events topic");
    let record = serde_json::json!({
        "binding_id": binding.id.as_str(),
        "binding_version": binding.version,
        "replay_of_event_id": serde_json::Value::Null,
        "event": event.clone(),
    });
    event_log
        .append(&topic, LogEvent::new("trigger_event", record))
        .await
        .expect("append trigger_event log");

    // 2. Run the actual dispatcher so the outbox/dispatch_outcome events
    //    that `--diff` consumes are also populated.
    let dispatcher =
        harn_vm::Dispatcher::with_event_log(build_replay_vm(workspace_root), event_log);
    dispatcher
        .dispatch(&binding, event)
        .await
        .expect("dispatch seed event");
}

async fn replay_event(
    workspace_root: PathBuf,
    event_log: Arc<AnyEventLog>,
    event_id: String,
    as_of: Option<String>,
    diff: bool,
) -> TriggerReplayReport {
    replay_report_for_event_log(
        event_log,
        &workspace_root,
        &event_id,
        as_of.as_deref(),
        diff,
    )
    .await
    .expect("replay report")
}

#[test]
fn trigger_replay_diff_reports_structured_drift() {
    let temp = TempDir::new().unwrap();
    let workspace_root = temp.path().to_path_buf();
    write_manifest(&workspace_root);
    write_lib(
        &workspace_root,
        r#"
import "std/triggers"

pub fn on_issue(event: TriggerEvent) -> dict {
  return {
    event_id: event.id,
    replay_env: env("HARN_REPLAY"),
    child_replay_env: shell("printf '%s' \"$HARN_REPLAY\"").stdout,
  }
}
"#,
    );

    let report = run_in_harn_runtime(move || async move {
        let _state_guard = harn_state_lock::lock_harn_state();
        let _cwd_guard = cwd_lock::lock_cwd_async().await;
        harn_vm::reset_thread_local_state();
        let event_log = install_default_for_base_dir(&workspace_root).expect("install event log");
        let event = build_issue_event("delivery-diff", None);
        let event_id = event.id.0.clone();
        dispatch_seed_event(&workspace_root, event_log.clone(), event).await;
        let report = replay_event(workspace_root, event_log, event_id.clone(), None, true).await;
        harn_vm::reset_thread_local_state();
        (event_id, report)
    });

    let (event_id, report) = report;
    let value = serde_json::to_value(&report).expect("serialize replay report");
    assert_eq!(value["event_id"].as_str(), Some(event_id.as_str()));
    assert_eq!(value["replay"]["status"].as_str(), Some("succeeded"));
    assert_eq!(value["original"]["status"].as_str(), Some("succeeded"));
    assert_eq!(value["drift"]["changed"].as_bool(), Some(true));
    assert_eq!(
        value["drift"]["fields"]["result"]["original"]["replay_env"],
        Value::Null
    );
    assert_eq!(
        value["drift"]["fields"]["result"]["replayed"]["replay_env"],
        serde_json::json!("1")
    );
    assert_eq!(
        value["drift"]["fields"]["result"]["original"]["child_replay_env"],
        Value::String(String::new())
    );
    assert_eq!(
        value["drift"]["fields"]["result"]["replayed"]["child_replay_env"],
        serde_json::json!("1")
    );
}

#[test]
fn trigger_replay_as_of_uses_historical_binding_version() {
    let temp = TempDir::new().unwrap();
    let workspace_root = temp.path().to_path_buf();
    write_manifest(&workspace_root);

    let lib_v1 = r#"
import "std/triggers"

pub fn on_issue(event: TriggerEvent) -> dict {
  return { version: "v1" }
}
"#;
    let lib_v2 = r#"
import "std/triggers"

pub fn on_issue(event: TriggerEvent) -> dict {
  return { version: "v2" }
}
"#;
    write_lib(&workspace_root, lib_v1);

    let workspace_root_clone = workspace_root.clone();
    let report = run_in_harn_runtime(move || async move {
        let _state_guard = harn_state_lock::lock_harn_state();
        let _cwd_guard = cwd_lock::lock_cwd_async().await;
        harn_vm::reset_thread_local_state();
        let event_log =
            install_default_for_base_dir(&workspace_root_clone).expect("install event log");

        // Seed the v1 binding + event.
        let event_v1 = build_issue_event("delivery-as-of-v1", None);
        let event_id = event_v1.id.0.clone();
        dispatch_seed_event(&workspace_root_clone, event_log.clone(), event_v1).await;

        // Capture the cutoff timestamp once v1 is durably recorded.
        let as_of = OffsetDateTime::now_utc();

        // Re-install with v2 — the harn_vm registry stores both versions
        // and the v2 row's received_at advances strictly past `as_of`.
        write_lib(&workspace_root_clone, lib_v2);
        let event_v2 = build_issue_event("delivery-as-of-v2", None);
        dispatch_seed_event(&workspace_root_clone, event_log.clone(), event_v2).await;

        // Roll lib.harn back to v1 so the replay handler matches v1 again.
        write_lib(&workspace_root_clone, lib_v1);

        let as_of_str = as_of
            .format(&time::format_description::well_known::Rfc3339)
            .expect("format as-of");
        let report = replay_event(
            workspace_root_clone,
            event_log,
            event_id.clone(),
            Some(as_of_str.clone()),
            false,
        )
        .await;
        harn_vm::reset_thread_local_state();
        (as_of_str, report)
    });

    let (as_of_str, report) = report;
    let value = serde_json::to_value(&report).expect("serialize replay report");
    assert_eq!(value["binding_version"].as_u64(), Some(1));
    assert_eq!(value["replay"]["result"]["version"].as_str(), Some("v1"));
    assert_eq!(value["as_of"].as_str(), Some(as_of_str.as_str()));
}

#[test]
fn trigger_replay_bulk_dry_run_filters_on_event_payload() {
    let temp = TempDir::new().unwrap();
    let workspace_root = temp.path().to_path_buf();
    write_manifest(&workspace_root);
    write_lib(
        &workspace_root,
        r#"
import "std/triggers"

pub fn on_issue(event: TriggerEvent) -> dict {
  return { ok: true }
}
"#,
    );

    let workspace_root_clone = workspace_root.clone();
    let (acme_event_id, value) = run_in_harn_runtime(move || async move {
        let _state_guard = harn_state_lock::lock_harn_state();
        let _cwd_guard = cwd_lock::lock_cwd_async().await;
        harn_vm::reset_thread_local_state();
        let event_log =
            install_default_for_base_dir(&workspace_root_clone).expect("install event log");
        let acme_event = build_issue_event("delivery-acme", Some("acme"));
        let acme_event_id = acme_event.id.0.clone();
        dispatch_seed_event(&workspace_root_clone, event_log.clone(), acme_event).await;
        let beta_event = build_issue_event("delivery-beta", Some("beta"));
        dispatch_seed_event(&workspace_root_clone, event_log.clone(), beta_event).await;

        let value = replay_bulk_in_process(
            event_log,
            &workspace_root_clone,
            "event.payload.tenant == 'acme'",
            false,
            true,
            None,
            None,
        )
        .await
        .expect("bulk replay payload");
        harn_vm::reset_thread_local_state();
        (acme_event_id, value)
    });

    assert_eq!(value["operation"].as_str(), Some("replay"));
    assert_eq!(value["dry_run"].as_bool(), Some(true));
    assert_eq!(value["matched_count"].as_u64(), Some(1));
    assert_eq!(
        value["items"][0]["event_id"].as_str(),
        Some(acme_event_id.as_str())
    );
    assert_eq!(value["items"][0]["status"].as_str(), Some("dry_run"));
    assert!(value["items"][0]["report"].is_null());
}

#[test]
fn trigger_cancel_reports_terminal_events_as_not_cancellable() {
    let temp = TempDir::new().unwrap();
    let workspace_root = temp.path().to_path_buf();
    write_manifest(&workspace_root);
    write_lib(
        &workspace_root,
        r#"
import "std/triggers"

pub fn on_issue(event: TriggerEvent) -> dict {
  return { ok: true }
}
"#,
    );

    let workspace_root_clone = workspace_root.clone();
    let (event_id, report) = run_in_harn_runtime(move || async move {
        let _state_guard = harn_state_lock::lock_harn_state();
        let _cwd_guard = cwd_lock::lock_cwd_async().await;
        harn_vm::reset_thread_local_state();
        let event_log =
            install_default_for_base_dir(&workspace_root_clone).expect("install event log");
        let event = build_issue_event("delivery-terminal", None);
        let event_id = event.id.0.clone();
        dispatch_seed_event(&workspace_root_clone, event_log.clone(), event).await;
        let report: CancelReport =
            cancel_event_in_process(event_log, &workspace_root_clone, &event_id)
                .await
                .expect("cancel report");
        harn_vm::reset_thread_local_state();
        (event_id, report)
    });

    let value = serde_json::to_value(&report).expect("serialize cancel report");
    assert_eq!(value["operation"].as_str(), Some("cancel"));
    assert_eq!(value["matched_count"].as_u64(), Some(1));
    assert_eq!(value["requested_count"].as_u64(), Some(0));
    assert_eq!(value["skipped_count"].as_u64(), Some(1));
    assert_eq!(
        value["items"][0]["status"].as_str(),
        Some("not_cancellable")
    );
    assert_eq!(
        value["items"][0]["event_id"].as_str(),
        Some(event_id.as_str())
    );
}
