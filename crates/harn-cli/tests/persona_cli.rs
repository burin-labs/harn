//! In-process coverage of `harn persona` CLI dispatch.
//!
//! Tier 1H follow-up (#1106) of the de-flake epic (#1057, #1067):
//! the persona dispatcher in `crates/harn-cli/src/commands/persona.rs`
//! is small and the JSON payload it emits is the contract under test,
//! so each test calls the corresponding `*_payload` library fn directly
//! and asserts on the returned struct/JSON value rather than parsing
//! subprocess stdout.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use harn_cli::commands::{persona, persona_doctor, persona_scaffold, persona_supervision};
use harn_vm::event_log::EventLog as _;
use tempfile::TempDir;

fn write_manifest(body: &str) -> TempDir {
    let temp = TempDir::new().unwrap();
    fs::create_dir_all(temp.path().join(".git")).unwrap();
    fs::write(temp.path().join("harn.toml"), body).unwrap();
    temp
}

fn manifest_path(temp: &TempDir) -> std::path::PathBuf {
    temp.path().join("harn.toml")
}

fn write_persona_source(body: &str) -> (TempDir, std::path::PathBuf) {
    let temp = TempDir::new().unwrap();
    fs::create_dir_all(temp.path().join(".git")).unwrap();
    let path = temp.path().join("persona.harn");
    fs::write(&path, body).unwrap();
    (temp, path)
}

fn valid_manifest() -> &'static str {
    r#"
[[personas]]
name = "merge_captain"
description = "Owns merge readiness."
entry_workflow = "workflows/merge.harn#run"
tools = ["github"]
capabilities = ["git.get_diff", "project.test_commands"]
autonomy_tier = "act_with_approval"
receipt_policy = "required"
triggers = ["github.pr_opened"]
schedules = ["*/30 * * * *"]
handoffs = ["review_captain"]
context_packs = ["repo_policy"]
evals = ["merge_safety"]
budget = { daily_usd = 20.0, frontier_escalations = 3 }
model_policy = { default_model = "gpt-5.4-mini", escalation_model = "gpt-5.4" }

[[personas]]
name = "review_captain"
description = "Owns review quality."
entry_workflow = "workflows/review.harn#run"
tools = ["github"]
capabilities = ["git.get_diff"]
autonomy_tier = "suggest"
receipt_policy = "required"

[[personas]]
name = "oncall_captain"
description = "Owns incident intake."
entry_workflow = "workflows/oncall.harn#run"
tools = ["slack"]
capabilities = ["interaction.ask"]
autonomy_tier = "act_with_approval"
receipt_policy = "required"
"#
}

fn versioned_manifest() -> String {
    valid_manifest()
        .replacen(
            "name = \"merge_captain\"",
            "name = \"merge_captain\"\nversion = \"1.4.0\"",
            1,
        )
        .replacen(
            "name = \"review_captain\"",
            "name = \"review_captain\"\nversion = \"1.1.0\"",
            1,
        )
        .replacen(
            "name = \"oncall_captain\"",
            "name = \"oncall_captain\"\nversion = \"0.9.1\"",
            1,
        )
}

#[test]
fn persona_list_and_inspect_emit_stable_json() {
    let temp = write_manifest(valid_manifest());
    let manifest = manifest_path(&temp);

    let personas = persona::list_payload(Some(&manifest)).expect("list payload");
    assert_eq!(personas.len(), 3);

    let persona =
        persona::inspect_payload(Some(&manifest), "merge_captain").expect("inspect payload");
    assert_eq!(persona["name"], "merge_captain");
    assert_eq!(persona["autonomy_tier"], "act_with_approval");
    assert_eq!(persona["receipt_policy"], "required");
    assert_eq!(persona["capabilities"][0], "git.get_diff");
    assert_eq!(persona["model_policy"]["default_model"], "gpt-5.4-mini");
    assert_eq!(persona["budget"]["daily_usd"], 20.0);
    assert_eq!(persona["triggers"][0], "github.pr_opened");
    assert_eq!(persona["handoffs"][0], "review_captain");
    assert_eq!(persona["context_packs"][0], "repo_policy");
    assert_eq!(persona["evals"][0], "merge_safety");
}

#[test]
fn persona_cli_rejects_required_invalid_manifest_cases() {
    for (body, expected) in [
        (
            r#"
[[personas]]
name = "merge_captain"
description = "Owns merge readiness."
tools = ["github"]
capabilities = ["git.get_diff"]
autonomy_tier = "act_with_approval"
receipt_policy = "required"
"#,
            "missing required entry_workflow",
        ),
        (
            r#"
[[personas]]
name = "merge_captain"
description = "Owns merge readiness."
entry_workflow = "workflows/merge.harn#run"
tools = ["github"]
capabilities = ["unknown.do_thing"]
autonomy_tier = "act_with_approval"
receipt_policy = "required"
"#,
            "unknown capability 'unknown.do_thing'",
        ),
        (
            r#"
[[personas]]
name = "merge_captain"
description = "Owns merge readiness."
entry_workflow = "workflows/merge.harn#run"
tools = ["github"]
capabilities = ["git.get_diff"]
autonomy_tier = "act_with_approval"
receipt_policy = "required"
budget = { daily_usd = 2.0, surprise = 1 }
"#,
            "unknown budget field",
        ),
        (
            r#"
[[personas]]
name = "merge_captain"
description = "Owns merge readiness."
entry_workflow = "workflows/merge.harn#run"
tools = ["github"]
capabilities = ["git.get_diff"]
autonomy_tier = "act_with_approval"
receipt_policy = "required"
handoffs = ["review_captain"]
"#,
            "unknown handoff target 'review_captain'",
        ),
    ] {
        let temp = write_manifest(body);
        let manifest = manifest_path(&temp);
        let result = persona::list_payload(Some(&manifest));
        let Err(message) = result else {
            panic!("expected validation failure for {expected}, got {result:?}");
        };
        assert!(
            message.contains(expected),
            "expected {expected:?} in error: {message}"
        );
    }
}

fn workspace_relative_manifest(relative: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(relative)
}

#[test]
fn persona_manifest_flag_loads_example_personas() {
    let manifest = workspace_relative_manifest("examples/personas/harn.toml");
    let persona = persona::inspect_payload(Some(&manifest), "merge_captain").expect("inspect");
    assert_eq!(persona["name"], "merge_captain");
    assert_eq!(persona["receipt_policy"], "required");
}

#[tokio::test(flavor = "current_thread")]
async fn committed_persona_template_pack_has_no_doctor_red_checks() {
    let manifest = workspace_relative_manifest("examples/personas/harn.toml");
    let personas = persona::list_payload(Some(&manifest)).expect("list template personas");

    for persona in personas {
        let name = persona["name"].as_str().expect("persona name");
        let report =
            match persona_doctor::doctor_report_for_persona(Some(&manifest), name, 10_000).await {
                Ok(report) | Err(report) => report,
            };
        let red = report
            .checks
            .iter()
            .filter(|check| check.status == persona_doctor::DoctorStatus::Red)
            .map(|check| format!("{}: {}", check.name, check.message))
            .collect::<Vec<_>>();
        assert!(red.is_empty(), "persona {name} failed doctor: {red:?}");
    }
}

#[test]
fn persona_inspect_reports_source_declared_steps() {
    let (_temp, path) = write_persona_source(
        r#"
@persona(name: "merge_captain")
fn merge_captain(ctx) {
  plan_step(ctx)
  verify_step(ctx)
}

@step(name: "plan", model: "gpt-5.4-mini", approval: optional, receipt: audit, error_boundary: fail, retry: {max_attempts: 2})
fn plan_step(ctx) {
  return ctx
}

@step(name: "verify", approval: required, receipt: none, error_boundary: escalate)
fn verify_step(ctx) {
  return ctx
}
"#,
    );
    let persona =
        persona::inspect_payload(Some(&path), "merge_captain").expect("inspect source persona");
    assert_eq!(persona["steps"].as_array().unwrap().len(), 2);
    assert_eq!(persona["steps"][0]["name"], "plan");
    assert_eq!(persona["steps"][0]["model"], "gpt-5.4-mini");
    assert_eq!(persona["steps"][0]["retry"]["max_attempts"], 2);
    assert_eq!(persona["steps"][1]["error_boundary"], "escalate");
}

#[test]
fn lint_fails_strict_when_persona_body_calls_non_step_helper() {
    use std::collections::HashSet;

    let (_temp, path) = write_persona_source(
        r#"
@persona(name: "merge_captain")
fn merge_captain(ctx) {
  helper(ctx)
}

fn helper(ctx) {
  return ctx
}
"#,
    );
    let source = fs::read_to_string(&path).unwrap();
    let program = harn_parser::parse_source(&source).expect("parse");
    let module_graph = harn_modules::build(std::slice::from_ref(&path));
    let allowlist: Vec<String> = Vec::new();
    let options = harn_lint::LintOptions {
        file_path: Some(&path),
        persona_step_allowlist: &allowlist,
        ..Default::default()
    };
    let diagnostics = harn_lint::lint_with_module_graph(
        &program,
        &[],
        Some(&source),
        &HashSet::new(),
        &module_graph,
        &path,
        &options,
    );
    assert!(
        diagnostics
            .iter()
            .any(|diag| diag.rule == "persona-body-must-call-steps"),
        "expected persona-body-must-call-steps diagnostic, got: {:?}",
        diagnostics
            .iter()
            .map(|d| d.rule.as_ref())
            .collect::<Vec<_>>()
    );
}

#[test]
fn persona_manifest_flag_loads_fixer_persona() {
    let manifest = workspace_relative_manifest("personas/fixer/harn.toml");
    let persona = persona::inspect_payload(Some(&manifest), "fixer").expect("inspect");
    assert_eq!(persona["name"], "fixer");
    assert_eq!(persona["triggers"][0], "invariant.blocked_with_remediation");
    assert_eq!(persona["entry_workflow"], "manifest.harn#run");
    assert_eq!(persona["receipt_policy"], "required");
}

#[tokio::test(flavor = "current_thread")]
async fn persona_scaffolder_creates_doctor_clean_package() {
    for template in [
        "deterministic-sweeper",
        "hybrid-classify-then-act",
        "frontier-judgment-loop",
    ] {
        let temp = TempDir::new().unwrap();
        let result = persona_scaffold::scaffold_persona_package(
            "my_release_captain",
            template,
            &temp.path().join("personas"),
            false,
        )
        .await
        .expect("scaffold persona");
        assert!(result.root.join("harn.toml").exists());
        assert!(result.root.join("src/my_release_captain.harn").exists());
        assert!(result
            .root
            .join("tests/my_release_captain_smoke.harn")
            .exists());
        assert!(result
            .root
            .join("tests/my_release_captain_smoke.expected")
            .exists());
        assert!(result.root.join("fixtures/happy_path.json").exists());
        assert!(result.root.join("prompts/system.harn.prompt").exists());
        assert!(result.root.join("evals/smoke.eval.json").exists());
        assert!(result.root.join("README.md").exists());

        let manifest = result.root.join("harn.toml");
        let persona =
            persona::inspect_payload(Some(&manifest), "my_release_captain").expect("inspect");
        assert_eq!(persona["name"], "my_release_captain");
        assert!(!persona["steps"].as_array().unwrap().is_empty());
        let canonical_manifest = workspace_relative_manifest(&format!(
            "crates/harn-cli/assets/persona-templates/{template}/harn.toml"
        ));
        let canonical = persona::inspect_payload(Some(&canonical_manifest), "template_persona")
            .expect("inspect canonical template");
        for field in [
            "tools",
            "capabilities",
            "autonomy_tier",
            "receipt_policy",
            "triggers",
            "model_policy",
            "budget",
        ] {
            assert_eq!(
                persona[field], canonical[field],
                "{field} drifted from canonical {template} template"
            );
        }

        let report = persona_doctor::doctor_report_for_persona(
            Some(&manifest),
            "my_release_captain",
            10_000,
        )
        .await
        .expect("doctor report");
        assert!(
            report
                .checks
                .iter()
                .all(|check| check.status == persona_doctor::DoctorStatus::Green),
            "strict scaffold profile was not green for {template:?}: {report:#?}"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn persona_doctor_rejects_a_missing_exact_entry_symbol() {
    let temp = write_manifest(valid_manifest());
    let workflow_dir = temp.path().join("workflows");
    fs::create_dir_all(&workflow_dir).unwrap();
    fs::write(
        workflow_dir.join("merge.harn"),
        r#"
@persona(name: "merge_captain", tools: [github])
pub fn merge_captain(task) {
  return task
}

pipeline typo(task) {
  return merge_captain(task)
}
"#,
    )
    .unwrap();

    let report = persona_doctor::doctor_report_for_persona(
        Some(&manifest_path(&temp)),
        "merge_captain",
        10_000,
    )
    .await
    .expect_err("missing #run must fail doctor");

    assert!(report.checks.iter().any(|check| {
        check.name == "entry-symbol"
            && check.status == persona_doctor::DoctorStatus::Red
            && check.message.contains("run")
    }));
}

#[test]
fn persona_manifest_flag_loads_merge_captain_persona() {
    let manifest = workspace_relative_manifest("personas/merge_captain/harn.toml");
    let persona = persona::inspect_payload(Some(&manifest), "merge_captain").expect("inspect");
    assert_eq!(persona["name"], "merge_captain");
    assert_eq!(persona["entry_workflow"], "manifest.harn#run");
    assert_eq!(persona["receipt_policy"], "required");
    assert_eq!(persona["autonomy_tier"], "act_with_approval");
    let triggers = persona["triggers"].as_array().expect("triggers array");
    assert!(triggers.iter().any(|t| t == "github.pr_opened"));
    let capabilities = persona["capabilities"]
        .as_array()
        .expect("capabilities array");
    assert!(capabilities.iter().any(|c| c == "process.exec"));
    assert!(capabilities.iter().any(|c| c == "runtime.dry_run"));
    assert_eq!(persona["evals"][0], "merge_captain_smoke");
}

#[test]
fn persona_manifest_flag_loads_ship_captain_persona() {
    let manifest = workspace_relative_manifest("personas/ship_captain/harn.toml");
    let persona = persona::inspect_payload(Some(&manifest), "ship_captain").expect("inspect");
    assert_eq!(persona["name"], "ship_captain");
    assert_eq!(persona["triggers"][0], "flow.atom_stream_updated");
    assert_eq!(persona["entry_workflow"], "manifest.harn#run");
    assert_eq!(persona["receipt_policy"], "required");
    assert_eq!(persona["evals"][0], "slice_quality");
}

#[tokio::test(flavor = "current_thread")]
async fn persona_runtime_status_tick_and_budget_are_persisted() {
    let temp = write_manifest(valid_manifest());
    let manifest = manifest_path(&temp);
    let state_dir = temp.path().join(".harn-personas-test");

    let status = persona::status_payload(Some(&manifest), &state_dir, "merge_captain", None)
        .await
        .expect("initial status");
    assert_eq!(status.state.as_str(), "idle");
    assert_eq!(status.role, "merge_captain");
    assert!(status.template_ref.is_none());
    assert_eq!(status.queued_events, 0);
    assert_eq!(status.budget.daily_usd, Some(20.0));

    let receipt = persona::tick_payload(
        Some(&manifest),
        &state_dir,
        "merge_captain",
        Some("2026-04-24T12:30:00Z"),
        0.25,
        12,
    )
    .await
    .expect("tick");
    assert_eq!(receipt.status, "completed");
    assert!(receipt
        .lease
        .as_ref()
        .map(|lease| lease.id.starts_with("persona_lease_"))
        .unwrap_or(false));

    // Pin the status query to the same UTC day as the tick above. Without
    // --at, the budget window is computed from real wall-clock time, so
    // the assertion silently breaks the moment the test runs after the
    // tick's UTC midnight.
    let status = persona::status_payload(
        Some(&manifest),
        &state_dir,
        "merge_captain",
        Some("2026-04-24T13:00:00Z"),
    )
    .await
    .expect("status after tick");
    assert_eq!(status.state.as_str(), "idle");
    assert_eq!(status.last_run.as_deref(), Some("2026-04-24T12:30:00Z"));
    assert_eq!(status.budget.spent_today_usd, 0.25);
    assert_eq!(status.budget.tokens_today, 12);
    assert_eq!(status.value_receipts.len(), 2);
    assert_eq!(status.value_receipts[0].kind.as_str(), "run_started");
    assert_eq!(status.value_receipts[1].kind.as_str(), "run_completed");
}

#[tokio::test(flavor = "current_thread")]
async fn persona_pause_resume_disable_trigger_controls_are_durable() {
    let temp = write_manifest(valid_manifest());
    let manifest = manifest_path(&temp);
    let state_dir = temp.path().join(".harn-personas-test");

    let _ = persona::pause_payload(
        Some(&manifest),
        &state_dir,
        "merge_captain",
        Some("2026-04-24T12:00:00Z"),
    )
    .await
    .expect("pause");

    let receipt = persona::trigger_payload(
        Some(&manifest),
        &state_dir,
        "merge_captain",
        "github",
        "pull_request",
        &[
            "repository=burin-labs/harn".to_string(),
            "number=462".to_string(),
        ],
        Some("2026-04-24T12:00:01Z"),
        0.0,
        0,
    )
    .await
    .expect("trigger while paused");
    assert_eq!(receipt.status, "queued");
    assert_eq!(receipt.work_key, "github:burin-labs/harn:pr:462");
    let queued = persona::status_payload(
        Some(&manifest),
        &state_dir,
        "merge_captain",
        Some("2026-04-24T12:00:02Z"),
    )
    .await
    .expect("queued status");
    assert_eq!(queued.queued_events, 1);
    assert_eq!(queued.queued_work[0].provider, "github");
    assert!(queued.current_assignment.is_none());

    let handoff = persona::trigger_payload(
        Some(&manifest),
        &state_dir,
        "merge_captain",
        "handoff",
        "review",
        &[
            "dedupe_key=handoff-1379".to_string(),
            "handoff_id=handoff-1379".to_string(),
            "handoff_kind=merge_receipt".to_string(),
            "source_persona=review_captain".to_string(),
            "task=Review durable persona receipt".to_string(),
        ],
        Some("2026-04-24T12:00:03Z"),
        0.0,
        0,
    )
    .await
    .expect("handoff while paused");
    assert_eq!(handoff.status, "queued");
    let queued = persona::status_payload(
        Some(&manifest),
        &state_dir,
        "merge_captain",
        Some("2026-04-24T12:00:04Z"),
    )
    .await
    .expect("queued handoff status");
    assert_eq!(queued.queued_events, 2);
    assert_eq!(queued.handoff_inbox.len(), 1);
    assert_eq!(
        queued.handoff_inbox[0].handoff_kind.as_deref(),
        Some("merge_receipt")
    );

    let resumed = persona::resume_payload(
        Some(&manifest),
        &state_dir,
        "merge_captain",
        Some("2026-04-24T12:01:00Z"),
    )
    .await
    .expect("resume");
    assert_eq!(resumed.state.as_str(), "idle");
    assert_eq!(resumed.queued_events, 0);
    assert!(resumed.handoff_inbox.is_empty());

    let _ = persona::disable_payload(
        Some(&manifest),
        &state_dir,
        "merge_captain",
        Some("2026-04-24T12:02:00Z"),
    )
    .await
    .expect("disable");

    let receipt = persona::trigger_payload(
        Some(&manifest),
        &state_dir,
        "merge_captain",
        "slack",
        "message",
        &[
            "channel=C123".to_string(),
            "ts=1713988800.000100".to_string(),
        ],
        Some("2026-04-24T12:02:01Z"),
        0.0,
        0,
    )
    .await
    .expect("trigger while disabled");
    assert_eq!(receipt.status, "dead_lettered");
}

#[tokio::test(flavor = "current_thread")]
async fn persona_supervision_tail_projects_multiplexed_ndjson_contract() {
    let temp = write_manifest(&versioned_manifest());
    let manifest = manifest_path(&temp);
    let state_dir = temp.path().join(".harn-personas-test");

    let _ = persona::pause_payload(
        Some(&manifest),
        &state_dir,
        "merge_captain",
        Some("2026-05-10T14:00:00Z"),
    )
    .await
    .expect("pause merge captain");
    let _ = persona::trigger_payload(
        Some(&manifest),
        &state_dir,
        "merge_captain",
        "github",
        "pull_request",
        &[
            "repository=burin-labs/harn".to_string(),
            "number=1490".to_string(),
        ],
        Some("2026-05-10T14:00:01Z"),
        0.0,
        0,
    )
    .await
    .expect("queue github work");
    let _ = persona::trigger_payload(
        Some(&manifest),
        &state_dir,
        "merge_captain",
        "handoff",
        "review",
        &[
            "dedupe_key=handoff-1490".to_string(),
            "handoff_id=handoff-1490".to_string(),
            "handoff_kind=review_request".to_string(),
            "source_persona=oncall_captain".to_string(),
            "task=Review issue 1490 tail output".to_string(),
        ],
        Some("2026-05-10T14:00:02Z"),
        0.0,
        0,
    )
    .await
    .expect("queue handoff");
    let _ = persona::resume_payload(
        Some(&manifest),
        &state_dir,
        "merge_captain",
        Some("2026-05-10T14:00:03Z"),
    )
    .await
    .expect("resume merge captain");
    let _ = persona::tick_payload(
        Some(&manifest),
        &state_dir,
        "oncall_captain",
        Some("2026-05-10T14:00:04Z"),
        0.0,
        0,
    )
    .await
    .expect("tick oncall captain");

    let log = harn_vm::event_log::install_default_for_base_dir(&state_dir)
        .expect("open persona event log");
    let merge_binding = harn_vm::PersonaRuntimeBinding {
        name: "merge_captain".to_string(),
        template_ref: Some("merge_captain@1.4.0".to_string()),
        entry_workflow: "workflows/merge.harn#run".to_string(),
        schedules: Vec::new(),
        triggers: Vec::new(),
        budget: harn_vm::PersonaBudgetPolicy::default(),
        stages: Vec::new(),
    };
    let _ = harn_vm::report_repair_worker_status(
        &log,
        &merge_binding,
        harn_vm::PersonaRepairWorkerStatusUpdate {
            persona_id: String::new(),
            template_ref: None,
            repair_worker_id: "rw_1490".to_string(),
            lifecycle: harn_vm::PersonaRepairWorkerLifecycle::Running,
            work_key: Some("github:burin-labs/harn:pr:1490".to_string()),
            lease_id: Some("persona_lease_1490".to_string()),
            scratchpad_url: Some("file:///tmp/rw_1490".to_string()),
            last_heartbeat_ms: 0,
            occurred_at_ms: 0,
        },
        harn_vm::parse_persona_ms("2026-05-10T14:00:05Z").expect("timestamp"),
    )
    .await
    .expect("repair worker status");
    let _ = harn_vm::restore_persona_checkpoint(
        &log,
        &merge_binding,
        harn_vm::PersonaCheckpointRestoreRequest {
            checkpoint_id: "cp_1490".to_string(),
            work_key: Some("github:burin-labs/harn:pr:1490".to_string()),
            resumed_from: None,
        },
        harn_vm::parse_persona_ms("2026-05-10T14:00:06Z").expect("timestamp"),
    )
    .await
    .expect("checkpoint restore ack");
    log.flush().await.expect("flush supervision events");

    let frames = persona_supervision::tail_payload(
        Some(&manifest),
        &state_dir,
        &persona_supervision::PersonaSupervisionTailOptions::default(),
    )
    .await
    .expect("tail supervision frames");
    assert!(
        frames
            .windows(2)
            .all(|pair| pair[0].event_id < pair[1].event_id),
        "tail cursors must be strictly increasing: {frames:?}"
    );

    let kinds: BTreeSet<_> = frames
        .iter()
        .map(|frame| frame.update_kind.as_str())
        .collect();
    for expected in [
        "control",
        "queue_position",
        "repair_worker_status",
        "handoff",
        "checkpoint",
        "receipt",
    ] {
        assert!(
            kinds.contains(expected),
            "expected update_kind={expected}; saw {kinds:?}"
        );
    }
    assert!(frames.iter().any(|frame| {
        frame.persona_id == "merge_captain"
            && frame.persona_kind == "merge_captain"
            && frame.persona_version.as_deref() == Some("1.4.0")
    }));
    assert!(frames.iter().any(|frame| {
        frame.persona_id == "oncall_captain"
            && frame.persona_kind == "oncall_captain"
            && frame.persona_version.as_deref() == Some("0.9.1")
            && frame.update_kind == "receipt"
    }));
    assert!(frames.iter().any(|frame| {
        frame.persona_id == "merge_captain"
            && frame.update_kind == "repair_worker_status"
            && frame.payload["lifecycle"] == "running"
            && frame.payload["repair_worker_id"] == "rw_1490"
    }));

    let cursor = frames.last().expect("at least one frame").event_id;
    let resumed = persona_supervision::tail_payload(
        Some(&manifest),
        &state_dir,
        &persona_supervision::PersonaSupervisionTailOptions {
            since_event_id: Some(cursor),
            ..Default::default()
        },
    )
    .await
    .expect("tail after cursor");
    assert!(
        resumed.is_empty(),
        "strict cursor replay yielded duplicates"
    );

    let limited = persona_supervision::tail_payload(
        Some(&manifest),
        &state_dir,
        &persona_supervision::PersonaSupervisionTailOptions {
            limit: Some(2),
            ..Default::default()
        },
    )
    .await
    .expect("limited tail");
    assert_eq!(limited.len(), 2);

    let oncall = persona_supervision::tail_payload(
        Some(&manifest),
        &state_dir,
        &persona_supervision::PersonaSupervisionTailOptions {
            persona: Some("oncall_captain".to_string()),
            ..Default::default()
        },
    )
    .await
    .expect("filtered tail");
    assert!(!oncall.is_empty());
    assert!(oncall
        .iter()
        .all(|frame| frame.persona_id == "oncall_captain"));
}

#[tokio::test(flavor = "current_thread")]
async fn persona_supervision_tail_follow_streams_new_events() {
    let temp = write_manifest(valid_manifest());
    let manifest = manifest_path(&temp);
    let state_dir = temp.path().join(".harn-personas-test");

    // Drive `drive_tail` in-process so the test can deterministically
    // wait for the tail to arm its filesystem watcher (via `ready_tx`)
    // before writing the next event. The previous subprocess+stdout race
    // was inherently flaky because there is no observable signal for
    // "subprocess has reached the wait loop" without a sentinel line.
    let writer = Arc::new(std::sync::Mutex::new(Vec::<u8>::new()));
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let options = persona_supervision::PersonaSupervisionTailOptions {
        persona: Some("merge_captain".to_string()),
        follow: true,
        limit: Some(1),
        ..Default::default()
    };

    let manifest_clone = manifest.clone();
    let state_dir_clone = state_dir.clone();
    let writer_clone = Arc::clone(&writer);
    let task = tokio::spawn(async move {
        let mut guard = SharedWriter(writer_clone);
        persona_supervision::drive_tail(
            Some(&manifest_clone),
            &state_dir_clone,
            &options,
            &mut guard,
            Some(ready_tx),
        )
        .await
    });

    // `ready_rx` fires only after the tail has flushed its initial read
    // (empty, here) and is about to wait for changes — so any event we
    // append now must traverse the follow path.
    tokio::time::timeout(Duration::from_secs(5), ready_rx)
        .await
        .expect("tail signalled ready")
        .expect("ready oneshot delivered");

    let _ = persona::tick_payload(
        Some(&manifest),
        &state_dir,
        "merge_captain",
        Some("2026-05-10T15:00:00Z"),
        0.0,
        0,
    )
    .await
    .expect("append followed event");

    let result = tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("tail finished")
        .expect("tail task did not panic");
    result.expect("tail completed without error");

    let bytes = writer.lock().unwrap().clone();
    let line = std::str::from_utf8(&bytes)
        .expect("utf8 tail output")
        .trim()
        .to_string();
    let frame: serde_json::Value = serde_json::from_str(&line).expect("follow JSON");
    assert_eq!(frame["persona_id"], "merge_captain");
    assert_eq!(frame["update_kind"], "receipt");
    assert_eq!(frame["payload"]["status"], "completed");
    assert_eq!(frame["occurred_at"], "2026-05-10T15:00:00Z");
}

struct SharedWriter(Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for SharedWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[tokio::test(flavor = "current_thread")]
async fn persona_runtime_blocks_budget_exhaustion() {
    let temp = write_manifest(
        r#"
[[personas]]
name = "merge_captain"
description = "Owns merge readiness."
entry_workflow = "workflows/merge.harn#run"
tools = ["github"]
capabilities = ["git.get_diff"]
autonomy_tier = "act_with_approval"
receipt_policy = "required"
triggers = ["github.pr_opened"]
budget = { daily_usd = 0.01, run_usd = 0.01, max_tokens = 10 }
"#,
    );
    let manifest = manifest_path(&temp);
    let state_dir = temp.path().join(".harn-personas-test");

    let receipt = persona::trigger_payload(
        Some(&manifest),
        &state_dir,
        "merge_captain",
        "github",
        "check_run",
        &[
            "repository=burin-labs/harn".to_string(),
            "check_name=ci".to_string(),
        ],
        Some("2026-04-24T12:00:00Z"),
        0.02,
        1,
    )
    .await
    .expect("trigger over budget");
    assert_eq!(receipt.status, "budget_exhausted");
    assert!(receipt
        .error
        .as_deref()
        .is_some_and(|message| message.contains("run_usd")));

    let status = persona::status_payload(
        Some(&manifest),
        &state_dir,
        "merge_captain",
        Some("2026-04-24T12:00:01Z"),
    )
    .await
    .expect("status after exhaustion");
    assert!(status
        .last_error
        .as_deref()
        .is_some_and(|message| message.contains("run_usd")));
    assert!(status.budget.exhausted);
    assert_eq!(status.budget.reason.as_deref(), Some("run_usd"));
    assert!(status.budget.last_receipt_id.is_some());
}
