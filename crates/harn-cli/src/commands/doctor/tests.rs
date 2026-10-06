use super::{
    build_host_info, build_summary, check_event_log, check_hardware, check_manifest_from,
    check_ollama_at, check_platform_capabilities, find_nearest_manifest, format_trigger_metrics,
    read_manifest, stdlib_capability_matrix, target_doctor_checks, DoctorCheck, DoctorReport,
    DoctorStatus, HardwareSnapshot, SandboxProfile, TargetInfo, DOCTOR_SCHEMA_VERSION,
};
use crate::json_envelope::JsonOutput;
use harn_vm::llm_config::{HealthcheckDef, ProviderDef};

#[test]
fn build_healthcheck_url_uses_base_and_path() {
    let def = ProviderDef {
        base_url: "https://example.com/api".to_string(),
        ..Default::default()
    };
    let healthcheck = HealthcheckDef {
        method: "GET".to_string(),
        path: Some("/health".to_string()),
        url: None,
        body: None,
    };

    assert_eq!(
        harn_vm::llm::build_healthcheck_url(&def, &healthcheck),
        "https://example.com/api/health"
    );
}

#[test]
fn find_nearest_manifest_walks_up() {
    let root = tempfile::tempdir().expect("tempdir");
    let nested = root.path().join("a/b/c");
    std::fs::create_dir_all(&nested).expect("create nested dirs");
    std::fs::write(
        root.path().join("harn.toml"),
        "[package]\nname = \"demo\"\n",
    )
    .expect("write manifest");

    let found = find_nearest_manifest(&nested).expect("manifest");
    assert_eq!(found, root.path().join("harn.toml"));
}

#[test]
fn read_manifest_accepts_basic_package() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("harn.toml");
    std::fs::write(&path, "[package]\nname = \"demo\"\n").expect("write manifest");

    let manifest = read_manifest(&path).expect("manifest parses");
    assert_eq!(
        manifest.package.and_then(|pkg| pkg.name),
        Some("demo".to_string())
    );
}

#[test]
fn event_log_check_reports_backend_and_location() {
    let _state_guard = crate::tests::common::harn_state_lock::lock_harn_state();
    let dir = tempfile::tempdir().expect("tempdir");
    let sqlite_path = dir.path().join(".harn/events.sqlite");
    std::env::set_var(harn_vm::event_log::HARN_EVENT_LOG_BACKEND_ENV, "sqlite");
    std::env::set_var(
        harn_vm::event_log::HARN_EVENT_LOG_SQLITE_PATH_ENV,
        &sqlite_path,
    );
    let checks = check_event_log();
    std::env::remove_var(harn_vm::event_log::HARN_EVENT_LOG_BACKEND_ENV);
    std::env::remove_var(harn_vm::event_log::HARN_EVENT_LOG_SQLITE_PATH_ENV);
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0].status, super::DoctorStatus::Ok);
    assert!(checks[0].detail.contains("sqlite"));
    assert!(checks[0]
        .detail
        .contains(&sqlite_path.display().to_string()));
}

#[test]
fn format_trigger_metrics_renders_snapshot() {
    let rendered = format_trigger_metrics(&harn_vm::TriggerMetricsSnapshot {
        received: 1,
        dispatched: 2,
        failed: 3,
        dlq: 4,
        in_flight: 5,
        last_received_ms: None,
        cost_total_usd_micros: 0,
        cost_today_usd_micros: 0,
        cost_hour_usd_micros: 0,
        autonomous_decisions_total: 0,
        autonomous_decisions_today: 0,
        autonomous_decisions_hour: 0,
    });
    assert_eq!(
        rendered,
        "received=1 dispatched=2 failed=3 dlq=4 in_flight=5"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn check_manifest_reports_loaded_triggers() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(dir.path().join(".git")).expect("git dir");
    std::fs::write(
        dir.path().join("harn.toml"),
        r#"
[package]
name = "workspace"

[exports]
handlers = "lib.harn"

[[triggers]]
id = "github-new-issue"
kind = "webhook"
provider = "webhook"
match = { events = ["issues.opened"] }
handler = "handlers::on_new_issue"
budget = { daily_cost_usd = 5.0, max_concurrent = 10 }
secrets = { signing_secret = "webhook/signing-secret" }
"#,
    )
    .expect("write manifest");
    std::fs::write(
        dir.path().join("lib.harn"),
        r#"
import "std/triggers"

pub fn on_new_issue(harness: Harness, event: TriggerEvent) {
  harness.stdio.log(event.kind)
}
"#,
    )
    .expect("write lib");

    let checks = check_manifest_from(dir.path()).await;

    let trigger = checks
        .iter()
        .find(|check| check.label == "trigger:github-new-issue")
        .expect("trigger check");
    assert_eq!(trigger.status, DoctorStatus::Ok);
    assert!(trigger.detail.contains("webhook via webhook"));
    assert!(trigger.detail.contains("handler=local"));
    assert!(trigger.detail.contains("state=active"));
    assert!(trigger.detail.contains("version=1"));
    assert!(trigger.detail.contains("metrics=received=0"));

    let dispatcher = checks
        .iter()
        .find(|check| check.label == "dispatcher")
        .expect("dispatcher check");
    assert_eq!(dispatcher.status, DoctorStatus::Ok);
    assert_eq!(
        dispatcher.detail,
        "in_flight=0 retry_queue_depth=0 dlq_depth=0"
    );
}

fn check(id: &str, status: DoctorStatus) -> DoctorCheck {
    DoctorCheck {
        id: id.to_string(),
        status,
        label: id.to_string(),
        detail: String::new(),
        ..Default::default()
    }
}

#[test]
fn report_envelope_carries_capability_matrix_and_stable_ids() {
    let checks = vec![
        check("harn_version", DoctorStatus::Ok),
        check("creds:openai", DoctorStatus::Warn),
    ];
    let report = DoctorReport {
        host: build_host_info(&[]),
        providers_config_path: String::new(),
        model_defaults: serde_json::Value::Null,
        targets: vec![TargetInfo {
            triple: "x86_64-unknown-linux-gnu".to_string(),
            installed: true,
            buildable: Some(true),
            reasons: Vec::new(),
            checked: true,
        }],
        providers: vec![super::ProviderInfo {
            name: "anthropic".to_string(),
            configured: true,
            reachable: Some(true),
            latency_ms: Some(120),
            errors: Vec::new(),
            probed: true,
        }],
        capabilities: stdlib_capability_matrix(),
        checks: checks.iter().map(super::DoctorCheckJson::from).collect(),
        hardware: HardwareSnapshot {
            ram_gb: Some(16),
            gpu: "mps".to_string(),
            free_disk_gb: Some(100),
        },
        summary: build_summary(&checks),
        next_step: "test next step".to_string(),
    };
    let envelope = report.into_envelope();
    let value = serde_json::to_value(&envelope).unwrap();
    assert_eq!(value["schemaVersion"], DOCTOR_SCHEMA_VERSION);
    assert_eq!(value["ok"], true);
    let data = &value["data"];
    assert!(data["host"]["os"].is_string());
    assert!(data["host"]["arch"].is_string());
    assert_eq!(data["targets"][0]["triple"], "x86_64-unknown-linux-gnu");
    assert_eq!(data["targets"][0]["buildable"], true);
    assert_eq!(data["providers"][0]["name"], "anthropic");
    assert_eq!(data["providers"][0]["reachable"], true);
    assert_eq!(data["providers"][0]["latency_ms"], 120);
    assert!(!data["capabilities"].as_array().unwrap().is_empty());
    let checks_arr = data["checks"].as_array().expect("checks array");
    assert_eq!(checks_arr[0]["id"], "harn_version");
    assert_eq!(checks_arr[0]["status"], "ok");
    assert_eq!(checks_arr[1]["id"], "creds:openai");
    assert_eq!(checks_arr[1]["status"], "warn");
    assert_eq!(data["hardware"]["ram_gb"], 16);
    assert_eq!(data["hardware"]["gpu"], "mps");
    assert_eq!(data["next_step"], "test next step");
}

#[test]
fn hardware_check_does_not_fail_on_unknown_platform() {
    let (check, _snapshot) = check_hardware();
    assert_ne!(
        check.status,
        DoctorStatus::Fail,
        "hardware check returned Fail unexpectedly: {}",
        check.detail
    );
}

#[tokio::test(flavor = "current_thread")]
async fn ollama_check_skips_when_binary_missing() {
    let result = check_ollama_at(None).await;
    assert_eq!(result.status, DoctorStatus::Skip);
    assert!(
        result.detail.contains("not installed") || result.detail.contains("not callable"),
        "unexpected ollama detail: {}",
        result.detail
    );
}

#[test]
fn summary_aggregates_status_counts_and_blocked_flows() {
    let checks = vec![
        DoctorCheck {
            id: "rustc".to_string(),
            status: DoctorStatus::Fail,
            blocks: vec!["build", "test"],
            ..Default::default()
        },
        DoctorCheck {
            id: "node".to_string(),
            status: DoctorStatus::Fail,
            blocks: vec!["portal"],
            ..Default::default()
        },
        DoctorCheck {
            id: "creds:openai".to_string(),
            status: DoctorStatus::Warn,
            blocks: vec!["scripting"], // not blocking — only Fail counts
            ..Default::default()
        },
        DoctorCheck {
            id: "harn_version".to_string(),
            status: DoctorStatus::Ok,
            ..Default::default()
        },
        DoctorCheck {
            id: "metadata".to_string(),
            status: DoctorStatus::Skip,
            ..Default::default()
        },
    ];
    let summary = build_summary(&checks);
    assert_eq!(summary.ok, 1);
    assert_eq!(summary.warning, 1);
    assert_eq!(summary.blocking, 2);
    assert_eq!(summary.skip, 1);
    // Sorted, deduplicated, alphabetical.
    assert_eq!(summary.blocked_flows, vec!["build", "portal", "test"]);
}

#[test]
fn capability_matrix_lists_known_capabilities() {
    let entries = stdlib_capability_matrix();
    let names: std::collections::BTreeSet<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    assert!(names.contains("workspace.read_text"), "names: {names:?}");
    assert!(names.contains("network"), "names: {names:?}");
    assert!(names.contains("process.exec"), "names: {names:?}");
    for entry in &entries {
        assert!(
            !entry.available_in_sandbox_profile.is_empty(),
            "{} should list at least one sandbox profile",
            entry.name
        );
        for profile in &entry.available_in_sandbox_profile {
            assert!(
                SandboxProfile::parse(profile).is_some(),
                "unknown sandbox profile '{profile}'"
            );
        }
    }
}

#[test]
fn host_info_reports_os_arch_and_harn_version() {
    let info = build_host_info(&[]);
    assert_eq!(info.os, std::env::consts::OS);
    assert_eq!(info.arch, std::env::consts::ARCH);
    assert_eq!(info.harn_version, env!("CARGO_PKG_VERSION"));
    assert!(!info.process_sandbox.backend.is_empty());
    assert!(!info.process_sandbox.filesystem_mechanism.is_empty());
}

#[test]
fn target_checks_skipped_when_not_probed() {
    let targets = vec![
        TargetInfo {
            triple: "x86_64-apple-darwin".to_string(),
            installed: true,
            buildable: None,
            reasons: Vec::new(),
            checked: false,
        },
        TargetInfo {
            triple: "wasm32-unknown-unknown".to_string(),
            installed: false,
            buildable: Some(false),
            reasons: vec!["target not installed".to_string()],
            checked: true,
        },
    ];
    let checks = target_doctor_checks(&targets);
    // Only the probed target emits a check entry.
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0].id, "target:wasm32-unknown-unknown");
    assert_eq!(checks[0].status, DoctorStatus::Warn);
    assert!(checks[0]
        .fix_command
        .as_deref()
        .map(|s| s.contains("rustup target add"))
        .unwrap_or(false));
}

#[test]
fn platform_capability_check_emits_known_ids() {
    let checks = check_platform_capabilities();
    let ids: std::collections::BTreeSet<&str> = checks.iter().map(|c| c.id.as_str()).collect();
    assert!(ids.contains("platform:file-watcher"), "ids: {ids:?}");
    assert!(ids.contains("platform:browser-opener"), "ids: {ids:?}");
    assert!(ids.contains("platform:process-sandbox"), "ids: {ids:?}");
    // None of the platform checks should ever be FAIL — they're
    // best-effort capability probes with documented fallbacks.
    for check in &checks {
        assert_ne!(check.status, DoctorStatus::Fail, "{}", check.detail);
    }
}
