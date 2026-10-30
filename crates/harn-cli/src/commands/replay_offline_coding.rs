//! Exact, provider-free replay of one recorded coding turn.
//!
//! The fixture is the interface. Its workspace seed, program, LLM tape, and
//! expectations are copied into an isolated directory and executed through the
//! ordinary `harn run` path. Comparison stays here so callers cannot mistake a
//! projected historical record for reproduced behavior.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs;
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::commands::run::{
    execute_run_json_with_options, CliLlmMockMode, RunExecutionOptions, RunJsonOptions,
    RunProfileOptions, RunSandboxOptions,
};
use crate::json_envelope::{self, JsonEnvelope, JsonError};
use harn_hostlib::workspace_checkpoint::{ReplayWorkspace, SeedLimits, SeedReceipt};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value as JsonValue};

mod canonical_replay;
use canonical_replay::canonical_replay_result;

pub(crate) const OFFLINE_CODING_REPLAY_SCHEMA: &str = "harn.offline-coding-replay.v1";
const TOOL_SEQUENCE: &str = "tool_sequence";
const WORKSPACE_EFFECTS: &str = "workspace_effects";
const TERMINAL_VERDICT: &str = "terminal_verdict";
const PROVIDER_ISOLATION: &str = "provider_isolation";
const NETWORK_ISOLATION: &str = "network_isolation";

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OfflineCodingReplayFixture {
    #[serde(rename = "_type")]
    type_name: String,
    schema_version: String,
    workspace_seed: String,
    program: String,
    llm_mock: String,
    effect_root: String,
    #[serde(default)]
    argv: Vec<String>,
    expected: OfflineCodingExpected,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct OfflineCodingExpected {
    tools: Vec<ExpectedToolCall>,
    effects: Vec<ExpectedEffect>,
    terminal: ExpectedTerminal,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ExpectedToolCall {
    id: String,
    name: String,
    status: harn_vm::agent_events::ToolCallStatus,
    args_blake3: String,
    result_blake3: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    exit_code: Option<i64>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ExpectedEffect {
    path: String,
    before_blake3: Option<String>,
    after_blake3: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExpectedTerminal {
    outcome: harn_vm::agent_events::AgentTerminalOutcome,
    exit_code: i32,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct OfflineCodingReplayData {
    kind: &'static str,
    source: String,
    producer: ReplayProducer,
    runs: Vec<OfflineCodingReplayReceipt>,
    repeatability: RepeatabilityReceipt,
    pending_count: usize,
    pending_comparisons: Vec<String>,
    missing_count: usize,
    missing_comparisons: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
struct ReplayProducer {
    harn_version: &'static str,
    source_revision: Option<&'static str>,
}

#[derive(Clone, Debug, Serialize)]
struct RepeatabilityReceipt {
    pass: bool,
    baseline_run: Option<usize>,
    divergent_runs: Vec<usize>,
}

#[derive(Clone, Debug, Serialize)]
struct OfflineCodingReplayReceipt {
    run: usize,
    workspace_seed: Option<SeedReceipt>,
    pass: bool,
    comparisons: Vec<ComparisonReceipt>,
    tools: ToolComparison,
    effects: EffectComparison,
    terminal: TerminalComparison,
    isolation: IsolationReceipt,
    pending_count: usize,
    pending_comparisons: Vec<String>,
    missing_count: usize,
    missing_comparisons: Vec<String>,
    failures: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
struct ComparisonReceipt {
    name: String,
    status: ComparisonStatus,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
enum ComparisonStatus {
    Passed,
    Failed,
    Missing,
}

#[derive(Clone, Debug, Serialize)]
struct ToolComparison {
    pass: bool,
    expected: Vec<ExpectedToolCall>,
    actual: Vec<ObservedToolCall>,
    successful: Vec<String>,
    rejected: Vec<String>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
struct ObservedToolCall {
    id: String,
    name: String,
    status: harn_vm::agent_events::ToolCallStatus,
    args_blake3: String,
    result_blake3: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    exit_code: Option<i64>,
}

#[derive(Clone, Debug, Serialize)]
struct EffectComparison {
    pass: bool,
    expected: Vec<ExpectedEffect>,
    actual: Vec<ExpectedEffect>,
}

#[derive(Clone, Debug, Serialize)]
struct TerminalComparison {
    pass: bool,
    expected: harn_vm::agent_events::AgentTerminalOutcome,
    actual: Option<harn_vm::agent_events::AgentTerminalOutcome>,
    expected_exit_code: i32,
    actual_exit_code: Option<i32>,
}

#[derive(Clone, Debug, Serialize)]
struct IsolationReceipt {
    provider: ProviderIsolationReceipt,
    network: NetworkIsolationReceipt,
}

#[derive(Clone, Debug, Default, Serialize)]
struct ProviderIsolationReceipt {
    pass: bool,
    checkpoint_count: usize,
    builtin_count: usize,
    miss_count: usize,
}

#[derive(Clone, Debug, Serialize)]
struct NetworkIsolationReceipt {
    pass: bool,
    sandbox_enabled: bool,
    process_network_allowed: bool,
}

struct ObservedReplay {
    tools: Option<Vec<ObservedToolCall>>,
    successful_tools: Vec<String>,
    rejected_tools: Vec<String>,
    effects: Option<Vec<ExpectedEffect>>,
    terminal: Option<harn_vm::agent_events::AgentTerminalOutcome>,
    exit_code: i32,
    execution_error: Option<String>,
    observation_errors: Vec<String>,
    provider_isolation: ProviderIsolationReceipt,
    network_isolation: NetworkIsolationReceipt,
}

#[derive(Clone, Default)]
struct AgentEventCapture(Arc<Mutex<Vec<harn_vm::agent_events::AgentEvent>>>);

impl AgentEventCapture {
    fn events(&self) -> Result<Vec<harn_vm::agent_events::AgentEvent>, String> {
        self.0
            .lock()
            .map_err(|_| "agent event capture lock was poisoned".to_string())
            .map(|events| events.clone())
    }
}

impl harn_vm::agent_events::AgentEventSink for AgentEventCapture {
    fn handle_event(&self, event: &harn_vm::agent_events::AgentEvent) {
        if let Ok(mut events) = self.0.lock() {
            events.push(event.clone());
        }
    }
}

#[derive(Clone, Default)]
struct RunEventCapture(Arc<Mutex<Vec<u8>>>);

impl RunEventCapture {
    fn events(&self) -> Result<Vec<JsonValue>, String> {
        let bytes = self
            .0
            .lock()
            .map_err(|_| "run event capture lock was poisoned".to_string())?
            .clone();
        let body = String::from_utf8(bytes)
            .map_err(|error| format!("run event stream was not UTF-8: {error}"))?;
        body.lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| {
                serde_json::from_str(line)
                    .map_err(|error| format!("invalid run event JSON: {error}"))
            })
            .collect()
    }
}

impl Write for RunEventCapture {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0
            .lock()
            .map_err(|_| io::Error::other("run event capture lock was poisoned"))?
            .extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub(crate) fn is_offline_coding_fixture(value: &JsonValue) -> bool {
    value.get("_type").and_then(JsonValue::as_str) == Some("offline_coding_replay")
}

pub(crate) fn parse_fixture(
    value: JsonValue,
    path: &Path,
) -> Result<OfflineCodingReplayFixture, String> {
    let fixture: OfflineCodingReplayFixture = serde_json::from_value(value).map_err(|error| {
        format!(
            "failed to parse offline coding replay {}: {error}",
            path.display()
        )
    })?;
    if fixture.type_name != "offline_coding_replay" {
        return Err(format!(
            "offline coding replay {} has unsupported _type {:?}",
            path.display(),
            fixture.type_name
        ));
    }
    if fixture.schema_version != OFFLINE_CODING_REPLAY_SCHEMA {
        return Err(format!(
            "offline coding replay {} uses unsupported schema_version {:?}; expected {:?}",
            path.display(),
            fixture.schema_version,
            OFFLINE_CODING_REPLAY_SCHEMA
        ));
    }
    Ok(fixture)
}

pub(crate) async fn run(
    fixture_path: &Path,
    fixture: &OfflineCodingReplayFixture,
    runs: usize,
    json_output: bool,
) -> i32 {
    let mut receipts = Vec::with_capacity(runs);
    let mut workspace = prepare_workspace(fixture_path, fixture);
    for index in 0..runs {
        let result = match &mut workspace {
            Ok(workspace) => {
                async {
                    if index > 0 {
                        workspace.restore().map_err(|error| error.to_string())?;
                    }
                    execute_in_workspace(workspace, fixture, index + 1).await
                }
                .await
            }
            Err(error) => Err(error.clone()),
        };
        match result {
            Ok(receipt) => receipts.push(receipt),
            Err(error) => receipts.push(setup_failure_receipt(index + 1, fixture, error)),
        }
    }
    let pending_comparisons = receipts
        .iter()
        .flat_map(|receipt| receipt.pending_comparisons.iter().cloned())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let missing_comparisons = receipts
        .iter()
        .flat_map(|receipt| receipt.missing_comparisons.iter().cloned())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let repeatability = repeatability_receipt(&receipts);
    let ok = receipts.iter().all(|receipt| receipt.pass)
        && repeatability.pass
        && pending_comparisons.is_empty()
        && missing_comparisons.is_empty();
    let data = OfflineCodingReplayData {
        kind: "offline_coding",
        source: fixture_path.to_string_lossy().into_owned(),
        producer: ReplayProducer {
            harn_version: env!("CARGO_PKG_VERSION"),
            source_revision: match env!("HARN_BUILD_REVISION") {
                "" => None,
                revision => Some(revision),
            },
        },
        runs: receipts,
        repeatability,
        pending_count: pending_comparisons.len(),
        pending_comparisons,
        missing_count: missing_comparisons.len(),
        missing_comparisons,
    };
    if json_output {
        let envelope = if ok {
            JsonEnvelope::ok(crate::commands::replay::REPLAY_SCHEMA_VERSION, data)
        } else {
            JsonEnvelope {
                schema_version: crate::commands::replay::REPLAY_SCHEMA_VERSION,
                ok: false,
                data: Some(data),
                error: Some(JsonError {
                    code: "offline_coding_replay_failed".to_string(),
                    message: "one or more offline coding replay comparisons failed".to_string(),
                    details: JsonValue::Null,
                }),
                warnings: Vec::new(),
            }
        };
        println!("{}", json_envelope::to_string_pretty(&envelope));
    } else {
        print_human(ok, &data);
    }
    i32::from(!ok)
}

fn prepare_workspace(
    fixture_path: &Path,
    fixture: &OfflineCodingReplayFixture,
) -> Result<ReplayWorkspace, String> {
    let base = fixture_path.parent().unwrap_or_else(|| Path::new("."));
    let seed = resolve_existing_directory(base, &fixture.workspace_seed, "workspace_seed")?;
    ReplayWorkspace::capture(&seed, SeedLimits::default()).map_err(|error| error.to_string())
}

#[cfg(test)]
async fn execute_once(
    fixture_path: &Path,
    fixture: &OfflineCodingReplayFixture,
    run: usize,
) -> Result<OfflineCodingReplayReceipt, String> {
    let workspace = prepare_workspace(fixture_path, fixture)?;
    execute_in_workspace(&workspace, fixture, run).await
}

async fn execute_in_workspace(
    replay: &ReplayWorkspace,
    fixture: &OfflineCodingReplayFixture,
    run: usize,
) -> Result<OfflineCodingReplayReceipt, String> {
    let workspace = replay.path().to_path_buf();
    let program = resolve_workspace_file(&workspace, &fixture.program, "program")?;
    let llm_mock = resolve_workspace_file(&workspace, &fixture.llm_mock, "llm_mock")?;
    let effect_root = resolve_workspace_directory(&workspace, &fixture.effect_root, "effect_root")?;
    let before = snapshot_tree(&effect_root)?;

    let mut argv = vec![workspace.to_string_lossy().into_owned()];
    argv.extend(fixture.argv.iter().cloned());
    let capture = RunEventCapture::default();
    let agent_capture = AgentEventCapture::default();
    let sandbox = RunSandboxOptions::sandboxed(false).with_workspace_root(&workspace);
    let network_isolation = NetworkIsolationReceipt {
        pass: sandbox.enabled && !sandbox.allow_process_network,
        sandbox_enabled: sandbox.enabled,
        process_network_allowed: sandbox.allow_process_network,
    };
    let sink: Arc<dyn harn_vm::agent_events::AgentEventSink> = Arc::new(agent_capture.clone());
    let outcome = harn_vm::llm::scope_agent_event_sink(
        Some(sink),
        execute_run_json_with_options(
            &program.to_string_lossy(),
            false,
            HashSet::new(),
            argv,
            Vec::new(),
            CliLlmMockMode::Replay {
                fixture_path: llm_mock,
            },
            None,
            RunProfileOptions::default(),
            Box::new(capture.clone()),
            RunJsonOptions { quiet: true },
            RunExecutionOptions {
                sandbox,
                ..RunExecutionOptions::default()
            },
        ),
    )
    .await;

    let mut observation_errors = Vec::new();
    let run_events = match capture.events() {
        Ok(events) => events,
        Err(error) => {
            observation_errors.push(error);
            Vec::new()
        }
    };
    let agent_events = match agent_capture.events() {
        Ok(events) => events,
        Err(error) => {
            observation_errors.push(error);
            Vec::new()
        }
    };
    let actual_tools = match tool_calls_from_agent_events(&agent_events, &workspace) {
        Ok(tools) => Some(tools),
        Err(error) => {
            observation_errors.push(error);
            None
        }
    };
    let (successful_tools, rejected_tools) = match tool_dispositions_from_result(&run_events) {
        Ok(value) => value,
        Err(error) => {
            observation_errors.push(error);
            (Vec::new(), Vec::new())
        }
    };
    let actual_effects = match snapshot_tree(&effect_root) {
        Ok(after) => Some(tree_diff(&before, &after)),
        Err(error) => {
            observation_errors.push(error);
            None
        }
    };
    let actual_terminal = match terminal_from_events(&run_events) {
        Ok(terminal) => Some(terminal),
        Err(error) => {
            observation_errors.push(error);
            None
        }
    };
    let execution_error = error_from_events(&run_events);
    let provider_isolation = provider_isolation_from_events(&agent_events);

    let mut receipt = compare(
        run,
        fixture,
        ObservedReplay {
            tools: actual_tools,
            successful_tools,
            rejected_tools,
            effects: actual_effects,
            terminal: actual_terminal,
            exit_code: outcome.exit_code,
            execution_error,
            observation_errors,
            provider_isolation,
            network_isolation,
        },
    );
    receipt.workspace_seed = Some(replay.receipt().clone());
    Ok(receipt)
}

fn compare(
    run: usize,
    fixture: &OfflineCodingReplayFixture,
    observed: ObservedReplay,
) -> OfflineCodingReplayReceipt {
    let mut missing = Vec::new();
    if fixture.expected.tools.is_empty() {
        missing.push(TOOL_SEQUENCE.to_string());
    }
    if fixture.expected.effects.is_empty() {
        missing.push(WORKSPACE_EFFECTS.to_string());
    }
    if observed.tools.is_none() {
        missing.push(TOOL_SEQUENCE.to_string());
    }
    if observed.effects.is_none() {
        missing.push(WORKSPACE_EFFECTS.to_string());
    }
    if observed.terminal.is_none() {
        missing.push(TERMINAL_VERDICT.to_string());
    }
    if observed.provider_isolation.checkpoint_count == 0 {
        missing.push(PROVIDER_ISOLATION.to_string());
    }
    if !observed.network_isolation.sandbox_enabled {
        missing.push(NETWORK_ISOLATION.to_string());
    }

    missing.sort();
    missing.dedup();
    let exact_tool_outcomes = observed.tools.as_ref().is_some_and(|tools| {
        fixture.expected.tools.len() == tools.len()
            && fixture
                .expected
                .tools
                .iter()
                .zip(tools)
                .all(|(expected, actual)| {
                    expected.id == actual.id
                        && expected.name == actual.name
                        && expected.status == actual.status
                        && expected.args_blake3 == actual.args_blake3
                        && actual.result_blake3.as_ref() == Some(&expected.result_blake3)
                        && expected
                            .exit_code
                            .is_none_or(|code| actual.exit_code == Some(code))
                })
    });
    let dispositions_match = observed.tools.as_ref().is_some_and(|tools| {
        let completed = tools
            .iter()
            .filter(|tool| tool.status == harn_vm::agent_events::ToolCallStatus::Completed)
            .map(|tool| tool.name.clone())
            .collect::<Vec<_>>();
        let failed = tools
            .iter()
            .filter(|tool| tool.status == harn_vm::agent_events::ToolCallStatus::Failed)
            .map(|tool| tool.name.clone())
            .collect::<Vec<_>>();
        completed == observed.successful_tools && failed == observed.rejected_tools
    });
    let tools_pass =
        !fixture.expected.tools.is_empty() && exact_tool_outcomes && dispositions_match;
    let effects_pass = !fixture.expected.effects.is_empty()
        && observed.effects.as_ref().is_some_and(|effects| {
            normalized_effects(&fixture.expected.effects) == normalized_effects(effects)
        });
    let terminal_pass = observed.terminal.as_ref() == Some(&fixture.expected.terminal.outcome)
        && fixture.expected.terminal.exit_code == observed.exit_code;

    let mut failures = Vec::new();
    if !tools_pass {
        failures.push("tool order, inputs, or results diverged".to_string());
    }
    if !effects_pass {
        failures.push("workspace final diff diverged".to_string());
    }
    if !terminal_pass {
        failures.push(format!(
            "terminal verdict diverged (expected {:?}:{}, actual {:?}:{})",
            fixture.expected.terminal.outcome,
            fixture.expected.terminal.exit_code,
            observed.terminal,
            observed.exit_code
        ));
    }
    if !observed.provider_isolation.pass {
        failures.push(format!(
            "provider fixture isolation failed (checkpoints={}, builtin={}, misses={})",
            observed.provider_isolation.checkpoint_count,
            observed.provider_isolation.builtin_count,
            observed.provider_isolation.miss_count,
        ));
    }
    if !observed.network_isolation.pass {
        failures.push("process network isolation was not enabled".to_string());
    }
    if let Some(error) = observed.execution_error {
        failures.push(format!("re-execution failed: {error}"));
    }
    for error in &observed.observation_errors {
        failures.push(format!("post-execution observation failed: {error}"));
    }
    for name in &missing {
        failures.push(format!("comparison {name} is missing evidence"));
    }

    let comparisons = vec![
        comparison(
            TOOL_SEQUENCE,
            tools_pass,
            missing.iter().any(|name| name == TOOL_SEQUENCE),
        ),
        comparison(
            WORKSPACE_EFFECTS,
            effects_pass,
            missing.iter().any(|name| name == WORKSPACE_EFFECTS),
        ),
        comparison(
            TERMINAL_VERDICT,
            terminal_pass,
            missing.iter().any(|name| name == TERMINAL_VERDICT),
        ),
        comparison(
            PROVIDER_ISOLATION,
            observed.provider_isolation.pass,
            observed.provider_isolation.checkpoint_count == 0,
        ),
        comparison(
            NETWORK_ISOLATION,
            observed.network_isolation.pass,
            !observed.network_isolation.sandbox_enabled,
        ),
    ];
    let pass = failures.is_empty() && missing.is_empty();
    OfflineCodingReplayReceipt {
        run,
        workspace_seed: None,
        pass,
        comparisons,
        tools: ToolComparison {
            pass: tools_pass,
            expected: fixture.expected.tools.clone(),
            actual: observed.tools.unwrap_or_default(),
            successful: observed.successful_tools,
            rejected: observed.rejected_tools,
        },
        effects: EffectComparison {
            pass: effects_pass,
            expected: normalized_effects(&fixture.expected.effects),
            actual: observed
                .effects
                .as_deref()
                .map(normalized_effects)
                .unwrap_or_default(),
        },
        terminal: TerminalComparison {
            pass: terminal_pass,
            expected: fixture.expected.terminal.outcome.clone(),
            actual: observed.terminal,
            expected_exit_code: fixture.expected.terminal.exit_code,
            actual_exit_code: Some(observed.exit_code),
        },
        isolation: IsolationReceipt {
            provider: observed.provider_isolation,
            network: observed.network_isolation,
        },
        pending_count: 0,
        pending_comparisons: Vec::new(),
        missing_count: missing.len(),
        missing_comparisons: missing,
        failures,
    }
}

fn setup_failure_receipt(
    run: usize,
    fixture: &OfflineCodingReplayFixture,
    error: String,
) -> OfflineCodingReplayReceipt {
    OfflineCodingReplayReceipt {
        run,
        workspace_seed: None,
        pass: false,
        comparisons: [
            TOOL_SEQUENCE,
            WORKSPACE_EFFECTS,
            TERMINAL_VERDICT,
            PROVIDER_ISOLATION,
            NETWORK_ISOLATION,
        ]
        .into_iter()
        .map(|name| ComparisonReceipt {
            name: name.to_string(),
            status: ComparisonStatus::Missing,
        })
        .collect(),
        tools: ToolComparison {
            pass: false,
            expected: fixture.expected.tools.clone(),
            actual: Vec::new(),
            successful: Vec::new(),
            rejected: Vec::new(),
        },
        effects: EffectComparison {
            pass: false,
            expected: normalized_effects(&fixture.expected.effects),
            actual: Vec::new(),
        },
        terminal: TerminalComparison {
            pass: false,
            expected: fixture.expected.terminal.outcome.clone(),
            actual: None,
            expected_exit_code: fixture.expected.terminal.exit_code,
            actual_exit_code: None,
        },
        isolation: IsolationReceipt {
            provider: ProviderIsolationReceipt::default(),
            network: NetworkIsolationReceipt {
                pass: false,
                sandbox_enabled: false,
                process_network_allowed: false,
            },
        },
        pending_count: 0,
        pending_comparisons: Vec::new(),
        missing_count: 5,
        missing_comparisons: vec![
            TOOL_SEQUENCE.to_string(),
            WORKSPACE_EFFECTS.to_string(),
            TERMINAL_VERDICT.to_string(),
            PROVIDER_ISOLATION.to_string(),
            NETWORK_ISOLATION.to_string(),
        ],
        failures: vec![error],
    }
}

fn comparison(name: &str, pass: bool, missing: bool) -> ComparisonReceipt {
    ComparisonReceipt {
        name: name.to_string(),
        status: if missing {
            ComparisonStatus::Missing
        } else if pass {
            ComparisonStatus::Passed
        } else {
            ComparisonStatus::Failed
        },
    }
}

fn terminal_from_events(
    events: &[JsonValue],
) -> Result<harn_vm::agent_events::AgentTerminalOutcome, String> {
    let terminal = result_value(events)?
        .get("terminal")
        .ok_or_else(|| "run result does not contain AgentResult.terminal".to_string())?;
    serde_json::from_value(terminal.clone())
        .map_err(|error| format!("run result terminal is not an AgentTerminalOutcome: {error}"))
}

fn tool_calls_from_agent_events(
    events: &[harn_vm::agent_events::AgentEvent],
    workspace: &Path,
) -> Result<Vec<ObservedToolCall>, String> {
    let mut calls = Vec::new();
    for event in events {
        if let harn_vm::agent_events::AgentEvent::ToolCall {
            tool_call_id,
            tool_name,
            raw_input,
            parsing,
            ..
        } = event
        {
            if *parsing == Some(true) {
                continue;
            }
            calls.push(ObservedToolCall {
                id: tool_call_id.clone(),
                name: tool_name.clone(),
                status: harn_vm::agent_events::ToolCallStatus::Pending,
                args_blake3: canonical_blake3(raw_input),
                result_blake3: None,
                exit_code: None,
            });
        }
        if let harn_vm::agent_events::AgentEvent::ToolCallUpdate {
            tool_call_id,
            status,
            raw_output,
            data,
            parsing,
            ..
        } = event
        {
            if parsing.is_some()
                || !matches!(
                    status,
                    harn_vm::agent_events::ToolCallStatus::Completed
                        | harn_vm::agent_events::ToolCallStatus::Failed
                )
            {
                continue;
            }
            let call = calls
                .iter_mut()
                .rev()
                .find(|call| call.id == *tool_call_id)
                .ok_or_else(|| {
                    format!("terminal tool update has no matching call: {tool_call_id}")
                })?;
            call.status = *status;
            let result = data
                .clone()
                .or_else(|| raw_output.as_ref().map(decode_rendered_tool_result));
            call.result_blake3 = result
                .as_ref()
                .map(|value| canonical_replay_result(value, workspace))
                .map(|value| canonical_blake3(&value));
            call.exit_code = data
                .as_ref()
                .and_then(|value| value["run_outcome"]["exit_code"].as_i64())
                .or_else(|| data.as_ref().and_then(|value| value["exit_code"].as_i64()))
                .or_else(|| {
                    result
                        .as_ref()
                        .and_then(|value| value["exit_code"].as_i64())
                });
        }
    }
    if calls.is_empty() {
        return Err("agent event stream contains no tool calls".to_string());
    }
    if let Some(call) = calls.iter().find(|call| {
        !matches!(
            call.status,
            harn_vm::agent_events::ToolCallStatus::Completed
                | harn_vm::agent_events::ToolCallStatus::Failed
        )
    }) {
        return Err(format!(
            "tool call {} has no terminal execution status",
            call.id
        ));
    }
    Ok(calls)
}

fn decode_rendered_tool_result(value: &JsonValue) -> JsonValue {
    value.as_str().map_or_else(
        || value.clone(),
        |text| serde_json::from_str(text).unwrap_or_else(|_| value.clone()),
    )
}

fn canonical_blake3(value: &JsonValue) -> String {
    blake3::hash(&harn_vm::canonical_json::to_vec(value))
        .to_hex()
        .to_string()
}

fn repeatability_receipt(receipts: &[OfflineCodingReplayReceipt]) -> RepeatabilityReceipt {
    let Some(baseline) = receipts.first() else {
        return RepeatabilityReceipt {
            pass: false,
            baseline_run: None,
            divergent_runs: Vec::new(),
        };
    };
    let divergent_runs = receipts
        .iter()
        .skip(1)
        .filter(|candidate| {
            candidate.tools.actual != baseline.tools.actual
                || candidate.effects.actual != baseline.effects.actual
                || candidate.terminal.actual != baseline.terminal.actual
                || candidate.terminal.actual_exit_code != baseline.terminal.actual_exit_code
        })
        .map(|receipt| receipt.run)
        .collect::<Vec<_>>();
    RepeatabilityReceipt {
        pass: divergent_runs.is_empty(),
        baseline_run: Some(baseline.run),
        divergent_runs,
    }
}

fn tool_dispositions_from_result(
    events: &[JsonValue],
) -> Result<(Vec<String>, Vec<String>), String> {
    let tools = &result_value(events)?["tools"];
    let read = |field: &str| -> Result<Vec<String>, String> {
        tools[field]
            .as_array()
            .ok_or_else(|| format!("AgentResult.tools.{field} is missing"))?
            .iter()
            .enumerate()
            .map(|(index, value)| {
                value
                    .as_str()
                    .map(str::to_string)
                    .ok_or_else(|| format!("AgentResult.tools.{field}[{index}] is not a string"))
            })
            .collect()
    };
    Ok((read("successful")?, read("rejected")?))
}

fn provider_isolation_from_events(
    events: &[harn_vm::agent_events::AgentEvent],
) -> ProviderIsolationReceipt {
    let checkpoints = events.iter().filter_map(|event| match event {
        harn_vm::agent_events::AgentEvent::TypedCheckpoint { checkpoint, .. }
            if checkpoint["schema"] == "harn.llm_mock_fixture_consumption.v1" =>
        {
            Some(checkpoint)
        }
        _ => None,
    });
    let mut receipt = ProviderIsolationReceipt::default();
    let mut all_cli_matches = true;
    for checkpoint in checkpoints {
        receipt.checkpoint_count += 1;
        let source = checkpoint["source"].as_str();
        let matched = checkpoint["matched"].as_bool() == Some(true);
        if source == Some("builtin") {
            receipt.builtin_count += 1;
        }
        if !matched {
            receipt.miss_count += 1;
        }
        all_cli_matches &= source == Some("cli_replay") && matched;
    }
    receipt.pass = receipt.checkpoint_count > 0 && all_cli_matches;
    receipt
}

fn result_value(events: &[JsonValue]) -> Result<&JsonValue, String> {
    events
        .iter()
        .find(|event| event["data"]["event_type"] == "result")
        .map(|event| &event["data"]["value"])
        .ok_or_else(|| "run event stream has no terminal result event".to_string())
}

fn error_from_events(events: &[JsonValue]) -> Option<String> {
    events
        .iter()
        .find(|event| event["data"]["event_type"] == "error")
        .map(|event| {
            event["data"]["error"]
                .get("message")
                .and_then(JsonValue::as_str)
                .unwrap_or("run emitted an error event without a message")
                .to_string()
        })
}

fn normalized_effects(effects: &[ExpectedEffect]) -> Vec<ExpectedEffect> {
    let mut normalized = effects.to_vec();
    normalized.sort_by(|left, right| left.path.cmp(&right.path));
    normalized
}

fn tree_diff(
    before: &BTreeMap<String, String>,
    after: &BTreeMap<String, String>,
) -> Vec<ExpectedEffect> {
    before
        .keys()
        .chain(after.keys())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .filter_map(|path| {
            let old = before.get(path).cloned();
            let new = after.get(path).cloned();
            (old != new).then(|| ExpectedEffect {
                path: path.clone(),
                before_blake3: old,
                after_blake3: new,
            })
        })
        .collect()
}

fn snapshot_tree(root: &Path) -> Result<BTreeMap<String, String>, String> {
    let mut snapshot = BTreeMap::new();
    snapshot_tree_into(root, root, &mut snapshot)?;
    Ok(snapshot)
}

fn snapshot_tree_into(
    root: &Path,
    current: &Path,
    snapshot: &mut BTreeMap<String, String>,
) -> Result<(), String> {
    let mut entries = fs::read_dir(current)
        .map_err(|error| format!("failed to read replay tree {}: {error}", current.display()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| {
            format!(
                "failed to enumerate replay tree {}: {error}",
                current.display()
            )
        })?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| format!("failed to inspect {}: {error}", path.display()))?;
        if metadata.file_type().is_symlink() {
            return Err(format!(
                "replay trees must not contain symlinks: {}",
                path.display()
            ));
        }
        if metadata.is_dir() {
            if !harn_vm::process_sandbox::is_workspace_runtime_directory(&entry.file_name()) {
                snapshot_tree_into(root, &path, snapshot)?;
            }
        } else if metadata.is_file() {
            let relative = path
                .strip_prefix(root)
                .expect("walked replay path stays under root")
                .to_string_lossy()
                .replace('\\', "/");
            let bytes = fs::read(&path)
                .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
            snapshot.insert(relative, blake3::hash(&bytes).to_hex().to_string());
        } else {
            return Err(format!("unsupported replay tree entry: {}", path.display()));
        }
    }
    Ok(())
}

fn resolve_existing_directory(base: &Path, value: &str, field: &str) -> Result<PathBuf, String> {
    let relative = validated_relative(value, field)?;
    let canonical_base = base.canonicalize().map_err(|error| {
        format!(
            "failed to resolve fixture directory {}: {error}",
            base.display()
        )
    })?;
    let path = base.join(relative);
    let canonical = path
        .canonicalize()
        .map_err(|error| format!("failed to resolve {field} {}: {error}", path.display()))?;
    if !canonical.starts_with(&canonical_base) {
        return Err(format!(
            "{field} resolves outside the fixture directory: {}",
            canonical.display()
        ));
    }
    if !canonical.is_dir() {
        return Err(format!(
            "{field} is not a directory: {}",
            canonical.display()
        ));
    }
    Ok(canonical)
}

fn resolve_workspace_file(workspace: &Path, value: &str, field: &str) -> Result<PathBuf, String> {
    let path = workspace.join(validated_relative(value, field)?);
    if !path.is_file() {
        return Err(format!(
            "{field} is not a file in the replay workspace: {}",
            path.display()
        ));
    }
    Ok(path)
}

fn resolve_workspace_directory(
    workspace: &Path,
    value: &str,
    field: &str,
) -> Result<PathBuf, String> {
    let path = workspace.join(validated_relative(value, field)?);
    if !path.is_dir() {
        return Err(format!(
            "{field} is not a directory in the replay workspace: {}",
            path.display()
        ));
    }
    Ok(path)
}

fn validated_relative(value: &str, field: &str) -> Result<PathBuf, String> {
    let path = Path::new(value);
    if value.trim().is_empty()
        || path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(format!(
            "{field} must be a non-empty relative path without '..': {value:?}"
        ));
    }
    Ok(path.to_path_buf())
}

fn print_human(ok: bool, receipt: &OfflineCodingReplayData) {
    println!(
        "{}",
        crate::localization::formatted!(
            "Offline coding replay: {}",
            if ok { "PASS" } else { "FAIL" }
        )
    );
    for run in &receipt.runs {
        println!(
            "{}",
            crate::localization::formatted!(
                "Run {}: {}",
                run.run,
                if run.pass { "PASS" } else { "FAIL" }
            )
        );
        for comparison in &run.comparisons {
            println!("  {}: {:?}", comparison.name, comparison.status);
        }
        for failure in &run.failures {
            println!(
                "{}",
                crate::localization::formatted!("  failure: {failure}", failure = failure)
            );
        }
    }
    println!(
        "{}",
        crate::localization::formatted!(
            "Pending comparisons ({}): {}",
            receipt.pending_count,
            json!(receipt.pending_comparisons)
        )
    );
    println!(
        "{}",
        crate::localization::formatted!(
            "Missing comparisons ({}): {}",
            receipt.missing_count,
            json!(receipt.missing_comparisons)
        )
    );
}

#[cfg(test)]
#[path = "replay_offline_coding_tests.rs"]
mod tests;
