//! `harn replay` JSON output. See `docs/src/cli-json-contract.md`.

use std::path::Path;

use serde::Serialize;

use crate::json_envelope::{self, JsonEnvelope};

pub(crate) const REPLAY_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ReplayReport {
    pub run_id: String,
    pub status: String,
    pub stage_count: usize,
    pub stages: Vec<ReplayStage>,
    pub transitions: Vec<ReplayTransition>,
    pub transcript_event_count: usize,
    pub fixture: ReplayFixtureResult,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ReplayStage {
    pub node_id: String,
    pub status: String,
    pub outcome: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub visible_text: Option<String>,
    /// Structured verification payload as recorded on the stage. Free-form
    /// JSON because different stage kinds attach different shapes here.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verification: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ReplayTransition {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from_node_id: Option<String>,
    pub to_node_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ReplayFixtureResult {
    pub pass: bool,
    pub failures: Vec<String>,
    pub stage_count: usize,
}

pub(crate) fn run_json(path: &str) -> i32 {
    let run = match load_run_record(Path::new(path)) {
        Ok(run) => run,
        Err(error) => {
            let envelope: JsonEnvelope<ReplayReport> =
                JsonEnvelope::err(REPLAY_SCHEMA_VERSION, "run_record_load_failed", error);
            println!("{}", json_envelope::to_string_pretty(&envelope));
            return 1;
        }
    };

    let fixture = run
        .replay_fixture
        .clone()
        .unwrap_or_else(|| harn_vm::orchestration::replay_fixture_from_run(&run));
    let report = harn_vm::orchestration::evaluate_run_against_fixture(&run, &fixture);

    let stages: Vec<ReplayStage> = run
        .stages
        .iter()
        .map(|stage| ReplayStage {
            node_id: stage.node_id.clone(),
            status: stage.status.clone(),
            outcome: stage.outcome.clone(),
            branch: stage.branch.clone(),
            visible_text: stage.visible_text.clone(),
            verification: stage.verification.clone(),
        })
        .collect();
    let transitions: Vec<ReplayTransition> = run
        .transitions
        .iter()
        .map(|t| ReplayTransition {
            from_node_id: t.from_node_id.clone(),
            to_node_id: t.to_node_id.clone(),
            branch: t.branch.clone(),
        })
        .collect();
    let transcript_event_count = run
        .transcript
        .as_ref()
        .and_then(|v| v.get("events"))
        .and_then(|v| v.as_array())
        .map(|v| v.len())
        .unwrap_or(0);

    let payload = ReplayReport {
        run_id: run.id.clone(),
        status: run.status.clone(),
        stage_count: run.stages.len(),
        stages,
        transitions,
        transcript_event_count,
        fixture: ReplayFixtureResult {
            pass: report.pass,
            failures: report.failures.clone(),
            stage_count: report.stage_count,
        },
    };

    let envelope = if payload.fixture.pass {
        JsonEnvelope::ok(REPLAY_SCHEMA_VERSION, payload)
    } else {
        JsonEnvelope {
            schema_version: REPLAY_SCHEMA_VERSION,
            ok: false,
            data: Some(payload),
            error: Some(json_envelope::JsonError {
                code: "replay_fixture_failed".to_string(),
                message: "embedded replay fixture did not pass".to_string(),
                details: serde_json::Value::Null,
            }),
            warnings: Vec::new(),
        }
    };
    let exit = if envelope.ok { 0 } else { 1 };
    println!("{}", json_envelope::to_string_pretty(&envelope));
    exit
}

fn load_run_record(path: &Path) -> Result<harn_vm::orchestration::RunRecord, String> {
    let raw = std::fs::read_to_string(path)
        .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    serde_json::from_str(&raw)
        .map_err(|error| format!("failed to parse run record {}: {error}", path.display()))
}
