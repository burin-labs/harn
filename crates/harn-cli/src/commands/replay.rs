//! `harn replay` JSON output. See `docs/src/cli-json-contract.md`.

use std::path::{Path, PathBuf};

use harn_vm::event_log::{AnyEventLog, SqliteEventLog};
use harn_vm::orchestration::{
    load_agent_session_replay_events_from_log, ReplayAllowlistRule, ReplayDivergence,
    ReplayExpectation, ReplayOracleTrace, ReplayTraceRun,
};
use serde::Serialize;
use serde_json::{json, Value as JsonValue};

use crate::cli::ReplayArgs;
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

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ReplayRunsEnvelope {
    #[serde(rename = "schemaVersion")]
    pub schema_version: u32,
    pub ok: bool,
    pub source: ReplaySourceSummary,
    pub reports: Vec<ReplayReport>,
    pub runs: Vec<Vec<JsonValue>>,
    pub determinism: ReplayDeterminismSummary,
    pub error: Option<json_envelope::JsonError>,
    pub warnings: Vec<json_envelope::JsonWarning>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ReplaySourceSummary {
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub events_db: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ReplayDeterminismSummary {
    pub pass: bool,
    pub compared_runs: usize,
    pub allowlist: Vec<ReplayAllowlistRule>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub divergence: Option<ReplayDivergence>,
    pub failures: Vec<String>,
}

struct ReplayExecution {
    report: ReplayReport,
    trace_run: ReplayTraceRun,
    event_sequence: Vec<JsonValue>,
}

enum ReplaySource {
    RunRecord {
        path: PathBuf,
    },
    ReplayTrace {
        path: PathBuf,
        trace: Box<ReplayOracleTrace>,
    },
    Session {
        session_id: String,
        events_db: PathBuf,
    },
}

impl ReplaySource {
    fn summary(&self) -> ReplaySourceSummary {
        match self {
            Self::RunRecord { path } => ReplaySourceSummary {
                kind: "run_record".to_string(),
                path: Some(path.to_string_lossy().into_owned()),
                session_id: None,
                events_db: None,
            },
            Self::ReplayTrace { path, .. } => ReplaySourceSummary {
                kind: "replay_trace".to_string(),
                path: Some(path.to_string_lossy().into_owned()),
                session_id: None,
                events_db: None,
            },
            Self::Session {
                session_id,
                events_db,
            } => ReplaySourceSummary {
                kind: "event_log_session".to_string(),
                path: None,
                session_id: Some(session_id.clone()),
                events_db: Some(events_db.to_string_lossy().into_owned()),
            },
        }
    }

    fn allowlist(&self) -> Vec<ReplayAllowlistRule> {
        match self {
            Self::ReplayTrace { trace, .. } => trace.allowlist.clone(),
            Self::RunRecord { .. } | Self::Session { .. } => default_replay_allowlist(),
        }
    }

    fn load_error_code(&self) -> &'static str {
        match self {
            Self::RunRecord { .. } => "run_record_load_failed",
            Self::ReplayTrace { .. } => "replay_fixture_load_failed",
            Self::Session { .. } => "replay_load_failed",
        }
    }
}

pub(crate) fn run(args: ReplayArgs) -> i32 {
    if args.runs == 0 {
        if args.json {
            print_json_error("invalid_runs", "--runs must be at least 1");
        } else {
            eprintln!("error: --runs must be at least 1");
        }
        return 1;
    }

    let source = match resolve_source(&args) {
        Ok(source) => source,
        Err(error) => {
            if args.json {
                print_json_error("replay_source_invalid", error);
            } else {
                eprintln!("error: {error}");
            }
            return 1;
        }
    };

    if args.json {
        run_json(source, args.runs)
    } else {
        run_human(source, args.runs)
    }
}

fn run_json(source: ReplaySource, runs: usize) -> i32 {
    let executions = match execute_runs(&source, runs) {
        Ok(executions) => executions,
        Err(error) => {
            print_json_error(source.load_error_code(), error);
            return 1;
        }
    };

    if runs == 1 {
        let execution = executions
            .into_iter()
            .next()
            .expect("execute_runs returns one execution for runs=1");
        let envelope = envelope_for_report(execution.report);
        let exit = i32::from(!envelope.ok);
        println!("{}", json_envelope::to_string_pretty(&envelope));
        return exit;
    }

    let determinism = determinism_summary(&source, &executions);
    let reports = executions
        .iter()
        .map(|execution| execution.report.clone())
        .collect::<Vec<_>>();
    let runs = executions
        .iter()
        .map(|execution| execution.event_sequence.clone())
        .collect::<Vec<_>>();
    let fixture_failed = reports.iter().any(|report| !report.fixture.pass);
    let ok = !fixture_failed && determinism.pass;
    let error = if ok {
        None
    } else {
        Some(json_envelope::JsonError {
            code: if fixture_failed {
                "replay_fixture_failed".to_string()
            } else {
                "replay_determinism_failed".to_string()
            },
            message: if fixture_failed {
                "one or more replay fixtures did not pass".to_string()
            } else {
                "allowlist-stripped replay runs diverged".to_string()
            },
            details: serde_json::Value::Null,
        })
    };
    let payload = ReplayRunsEnvelope {
        schema_version: REPLAY_SCHEMA_VERSION,
        ok,
        source: source.summary(),
        reports,
        runs,
        determinism,
        error,
        warnings: Vec::new(),
    };
    let exit = i32::from(!payload.ok);
    println!("{}", to_string_pretty(&payload));
    exit
}

fn run_human(source: ReplaySource, runs: usize) -> i32 {
    let executions = match execute_runs(&source, runs) {
        Ok(executions) => executions,
        Err(error) => {
            eprintln!("error: {error}");
            return 1;
        }
    };
    for (index, execution) in executions.iter().enumerate() {
        if runs > 1 {
            println!("Replay run {}:", index + 1);
        }
        print_report_human(&execution.report);
    }
    if runs > 1 {
        let determinism = determinism_summary(&source, &executions);
        let fixture_failed = executions
            .iter()
            .any(|execution| !execution.report.fixture.pass);
        println!(
            "Determinism: {} (compared {} run(s))",
            if determinism.pass { "PASS" } else { "FAIL" },
            determinism.compared_runs
        );
        for failure in determinism.failures {
            println!("  {failure}");
        }
        return i32::from(fixture_failed || !determinism.pass);
    }
    i32::from(!executions[0].report.fixture.pass)
}

fn resolve_source(args: &ReplayArgs) -> Result<ReplaySource, String> {
    if let Some(session_id) = &args.session_id {
        let events_db = args
            .events_db
            .as_ref()
            .ok_or_else(|| "--events-db is required with --session-id".to_string())?;
        return Ok(ReplaySource::Session {
            session_id: session_id.clone(),
            events_db: PathBuf::from(events_db),
        });
    }
    if let Some(fixture) = &args.fixture {
        return resolve_fixture_source(Path::new(fixture));
    }
    if let Some(path) = &args.path {
        return Ok(ReplaySource::RunRecord {
            path: PathBuf::from(path),
        });
    }
    Err("one of <PATH>, --fixture, or --session-id must be provided".to_string())
}

fn resolve_fixture_source(path: &Path) -> Result<ReplaySource, String> {
    let raw = std::fs::read_to_string(path)
        .map_err(|error| format!("failed to read fixture {}: {error}", path.display()))?;
    let value: JsonValue = serde_json::from_str(&raw)
        .map_err(|error| format!("failed to parse fixture {}: {error}", path.display()))?;
    if value
        .get("schema_version")
        .and_then(JsonValue::as_str)
        .is_some_and(|schema| schema == harn_vm::REPLAY_TRACE_SCHEMA_VERSION)
    {
        let trace = serde_json::from_value::<ReplayOracleTrace>(value)
            .map_err(|error| format!("failed to parse replay trace {}: {error}", path.display()))?;
        return Ok(ReplaySource::ReplayTrace {
            path: path.to_path_buf(),
            trace: Box::new(trace),
        });
    }
    Ok(ReplaySource::RunRecord {
        path: path.to_path_buf(),
    })
}

fn execute_runs(source: &ReplaySource, runs: usize) -> Result<Vec<ReplayExecution>, String> {
    (0..runs).map(|index| execute_once(source, index)).collect()
}

fn execute_once(source: &ReplaySource, index: usize) -> Result<ReplayExecution, String> {
    match source {
        ReplaySource::RunRecord { path } => execute_run_record(path),
        ReplaySource::ReplayTrace { trace, .. } => execute_replay_trace(trace, index),
        ReplaySource::Session {
            session_id,
            events_db,
        } => execute_session_replay(session_id, events_db),
    }
}

fn execute_run_record(path: &Path) -> Result<ReplayExecution, String> {
    let run = harn_vm::orchestration::load_run_record(path)
        .map_err(|error| format!("failed to load run record {}: {error}", path.display()))?;
    let report = replay_report_from_run(&run);
    let raw_events = event_sequence_from_report(&report);
    let trace_run = ReplayTraceRun {
        run_id: report.run_id.clone(),
        event_log_entries: raw_events,
        ..ReplayTraceRun::default()
    };
    let event_sequence = canonical_event_sequence(&trace_run, &default_replay_allowlist())?;
    Ok(ReplayExecution {
        report,
        trace_run,
        event_sequence,
    })
}

fn execute_session_replay(session_id: &str, events_db: &Path) -> Result<ReplayExecution, String> {
    let log = AnyEventLog::Sqlite(
        SqliteEventLog::open_read_only(events_db.to_path_buf(), 1).map_err(|error| {
            format!(
                "failed to open events db {} read-only: {error}",
                events_db.display()
            )
        })?,
    );
    let events =
        futures::executor::block_on(load_agent_session_replay_events_from_log(&log, session_id))
            .map_err(|error| format!("failed to read session events: {error}"))?;
    if events.is_empty() {
        return Err(format!(
            "event log {} does not contain events for session_id {session_id:?}",
            events_db.display()
        ));
    }
    let run =
        harn_vm::session_bundle::import_run_record_from_agent_session_events(session_id, &events)
            .map_err(|error| {
            format!("failed to reconstruct run record from session events: {error}")
        })?;
    let report = replay_report_from_run(&run);
    let raw_events = events
        .iter()
        .map(|entry| {
            let event = serde_json::to_value(&entry.event).unwrap_or(JsonValue::Null);
            json!({
                "event_id": entry.event_id,
                "topic": format!(
                    "observability.agent_events.{}",
                    harn_vm::event_log::sanitize_topic_component(session_id)
                ),
                "kind": entry.kind,
                "occurred_at_ms": entry.occurred_at_ms,
                "payload": {
                    "session_id": session_id,
                    "event": event,
                },
            })
        })
        .collect::<Vec<_>>();
    let trace_run = ReplayTraceRun {
        run_id: report.run_id.clone(),
        event_log_entries: raw_events,
        ..ReplayTraceRun::default()
    };
    let event_sequence = canonical_event_sequence(&trace_run, &default_replay_allowlist())?;
    Ok(ReplayExecution {
        report,
        trace_run,
        event_sequence,
    })
}

fn execute_replay_trace(
    trace: &ReplayOracleTrace,
    index: usize,
) -> Result<ReplayExecution, String> {
    let trace_run = if index.is_multiple_of(2) {
        trace.first_run.clone()
    } else {
        trace.second_run.clone()
    };
    let fixture = trace_fixture_result(trace, &trace_run)?;
    let report = ReplayReport {
        run_id: trace_run.run_id.clone(),
        status: if fixture.pass { "completed" } else { "failed" }.to_string(),
        stage_count: 0,
        stages: Vec::new(),
        transitions: Vec::new(),
        transcript_event_count: trace_run.event_log_entries.len(),
        fixture,
    };
    let event_sequence = canonical_event_sequence(&trace_run, &trace.allowlist)?;
    Ok(ReplayExecution {
        report,
        trace_run,
        event_sequence,
    })
}

fn replay_report_from_run(run: &harn_vm::orchestration::RunRecord) -> ReplayReport {
    let fixture = run
        .replay_fixture
        .clone()
        .unwrap_or_else(|| harn_vm::orchestration::replay_fixture_from_run(run));
    let report = harn_vm::orchestration::evaluate_run_against_fixture(run, &fixture);

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

    ReplayReport {
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
    }
}

fn envelope_for_report(payload: ReplayReport) -> JsonEnvelope<ReplayReport> {
    if payload.fixture.pass {
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
    }
}

fn print_report_human(report: &ReplayReport) {
    println!("Replay: {}", report.run_id);
    for stage in &report.stages {
        println!(
            "[{}] status={} outcome={} branch={}",
            stage.node_id,
            stage.status,
            stage.outcome,
            stage.branch.clone().unwrap_or_else(|| "-".to_string())
        );
        if let Some(text) = &stage.visible_text {
            println!("  visible: {text}");
        }
        if let Some(verification) = &stage.verification {
            println!("  verification: {verification}");
        }
    }
    println!(
        "Transcript events persisted: {}",
        report.transcript_event_count
    );
    println!(
        "Embedded replay fixture: {}",
        if report.fixture.pass { "PASS" } else { "FAIL" }
    );
    for failure in &report.fixture.failures {
        println!("  failure: {failure}");
    }
    for transition in &report.transitions {
        println!(
            "transition {} -> {} ({})",
            transition
                .from_node_id
                .clone()
                .unwrap_or_else(|| "start".to_string()),
            transition.to_node_id,
            transition
                .branch
                .clone()
                .unwrap_or_else(|| "default".to_string())
        );
    }
}

fn trace_fixture_result(
    trace: &ReplayOracleTrace,
    trace_run: &ReplayTraceRun,
) -> Result<ReplayFixtureResult, String> {
    let oracle = ReplayOracleTrace {
        name: format!("{} replay fixture", trace.name),
        expect: ReplayExpectation::Match,
        protocol_fixture_refs: trace.protocol_fixture_refs.clone(),
        allowlist: trace.allowlist.clone(),
        first_run: trace.first_run.clone(),
        second_run: trace_run.clone(),
        ..ReplayOracleTrace::default()
    };
    let report = harn_vm::run_replay_oracle_trace(&oracle)
        .map_err(|error| format!("failed to evaluate replay trace: {error}"))?;
    let failures = report
        .divergence
        .as_ref()
        .map(|divergence| vec![divergence.message.clone()])
        .unwrap_or_default();
    Ok(ReplayFixtureResult {
        pass: report.passed,
        failures,
        stage_count: 0,
    })
}

fn determinism_summary(
    source: &ReplaySource,
    executions: &[ReplayExecution],
) -> ReplayDeterminismSummary {
    let allowlist = source.allowlist();
    if executions.len() <= 1 {
        return ReplayDeterminismSummary {
            pass: true,
            compared_runs: executions.len(),
            allowlist,
            divergence: None,
            failures: Vec::new(),
        };
    }

    let baseline = executions[0].trace_run.clone();
    for (index, execution) in executions.iter().enumerate().skip(1) {
        let trace = ReplayOracleTrace {
            name: format!("harn replay run {} determinism", index + 1),
            expect: ReplayExpectation::Match,
            allowlist: allowlist.clone(),
            first_run: baseline.clone(),
            second_run: execution.trace_run.clone(),
            ..ReplayOracleTrace::default()
        };
        match harn_vm::run_replay_oracle_trace(&trace) {
            Ok(report) if report.passed => {}
            Ok(report) => {
                let failure = report
                    .divergence
                    .as_ref()
                    .map(|divergence| divergence.message.clone())
                    .unwrap_or_else(|| format!("run {} diverged", index + 1));
                return ReplayDeterminismSummary {
                    pass: false,
                    compared_runs: executions.len(),
                    allowlist,
                    divergence: report.divergence,
                    failures: vec![failure],
                };
            }
            Err(error) => {
                return ReplayDeterminismSummary {
                    pass: false,
                    compared_runs: executions.len(),
                    allowlist,
                    divergence: None,
                    failures: vec![error.to_string()],
                };
            }
        }
    }

    ReplayDeterminismSummary {
        pass: true,
        compared_runs: executions.len(),
        allowlist,
        divergence: None,
        failures: Vec::new(),
    }
}

fn default_replay_allowlist() -> Vec<ReplayAllowlistRule> {
    vec![
        ReplayAllowlistRule {
            path: "/run_id".to_string(),
            reason: "run ids are allocated per replay execution".to_string(),
            replacement: None,
        },
        ReplayAllowlistRule {
            path: "/event_log_entries/*/event_id".to_string(),
            reason: "EventLog offsets are backend-local".to_string(),
            replacement: None,
        },
        ReplayAllowlistRule {
            path: "/event_log_entries/*/occurred_at_ms".to_string(),
            reason: "append timestamps are wall-clock observations".to_string(),
            replacement: None,
        },
    ]
}

fn event_sequence_from_report(report: &ReplayReport) -> Vec<JsonValue> {
    let mut events = Vec::new();
    for (index, stage) in report.stages.iter().enumerate() {
        events.push(json!({
            "event_id": index + 1,
            "occurred_at_ms": 0,
            "kind": "replay_stage",
            "payload": stage,
        }));
    }
    for (index, transition) in report.transitions.iter().enumerate() {
        events.push(json!({
            "event_id": report.stages.len() + index + 1,
            "occurred_at_ms": 0,
            "kind": "replay_transition",
            "payload": transition,
        }));
    }
    if events.is_empty() {
        events.push(json!({
            "event_id": 1,
            "occurred_at_ms": 0,
            "kind": "replay_report",
            "payload": report,
        }));
    }
    events
}

fn canonical_event_sequence(
    run: &ReplayTraceRun,
    allowlist: &[ReplayAllowlistRule],
) -> Result<Vec<JsonValue>, String> {
    let canonical = harn_vm::canonicalize_run(run, allowlist)
        .map_err(|error| format!("failed to apply replay allowlist: {error}"))?;
    Ok(flatten_trace_run_value(&canonical))
}

fn flatten_trace_run_value(run: &JsonValue) -> Vec<JsonValue> {
    let mut events = Vec::new();
    for section in [
        "event_log_entries",
        "trigger_firings",
        "llm_interactions",
        "protocol_interactions",
        "approval_interactions",
        "effect_receipts",
        "persona_runtime_states",
        "agent_transcript_deltas",
        "final_artifacts",
        "policy_decisions",
        "channel_receipts",
        "lifecycle_receipts",
    ] {
        if let Some(values) = run.get(section).and_then(JsonValue::as_array) {
            append_trace_section(&mut events, section, values);
        }
    }
    events
}

fn append_trace_section(events: &mut Vec<JsonValue>, section: &str, values: &[JsonValue]) {
    for value in values {
        let mut value = value.clone();
        if let JsonValue::Object(map) = &mut value {
            map.entry("replay_section".to_string())
                .or_insert_with(|| JsonValue::String(section.to_string()));
            events.push(value);
        } else {
            events.push(json!({
                "replay_section": section,
                "value": value,
            }));
        }
    }
}

fn print_json_error(code: &str, message: impl Into<String>) {
    let envelope: JsonEnvelope<JsonValue> =
        JsonEnvelope::err(REPLAY_SCHEMA_VERSION, code, message.into());
    println!("{}", json_envelope::to_string_pretty(&envelope));
}

fn to_string_pretty<T: Serialize>(value: &T) -> String {
    serde_json::to_string_pretty(value).expect("replay JSON payload serializes")
}
