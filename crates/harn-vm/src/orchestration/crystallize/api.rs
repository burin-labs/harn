//! Top-level public API: crystallize traces, synthesize single-trace candidates, load/write artifacts.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde_json::{json, Value as JsonValue};

use super::super::{now_rfc3339, ReplayAllowlistRule, ReplayFixture, ReplayTraceRun, RunRecord};
use super::codegen::{generate_eval_pack, generate_harn_code, rejected_workflow_stub};
use super::normalize::{
    action_signature, cluster_key_for_candidate, constants_for_action,
    expected_receipts_for_examples, mine_candidates, normalize_trace, partition_mineable_traces,
    preconditions_for_action,
};
use super::shadow::{
    confidence_for, estimate_savings, infer_workflow_name, refresh_promotion_metadata,
    shadow_candidate,
};
use super::skill::refresh_skill_candidates;
use super::types::{
    CrystallizationAction, CrystallizationApproval, CrystallizationArtifacts, CrystallizationCost,
    CrystallizationInputFormat, CrystallizationReport, CrystallizationTrace, CrystallizationUsage,
    CrystallizeOptions, PromotionMetadata, RecoveryFeedbackSummary, SegmentKind, SegmentSummary,
    ShadowRunReport, WorkflowCandidate, WorkflowCandidateExample, WorkflowCandidateParameter,
    WorkflowCandidateStep, DEFAULT_MIN_EXAMPLES, TRACE_SCHEMA_VERSION,
};
use super::util::{
    hash_bytes, is_scalar, json_scalar_string, sanitize_identifier, sorted_side_effects,
    sorted_strings, stable_candidate_id,
};
use crate::value::VmError;

pub fn crystallize_traces(
    traces: Vec<CrystallizationTrace>,
    options: CrystallizeOptions,
) -> Result<CrystallizationArtifacts, VmError> {
    let min_examples = options.min_examples.max(DEFAULT_MIN_EXAMPLES);
    if traces.len() < min_examples {
        return Err(VmError::Runtime(format!(
            "crystallize requires at least {min_examples} traces, got {}",
            traces.len()
        )));
    }

    let normalized_all = traces.into_iter().map(normalize_trace).collect::<Vec<_>>();
    let (normalized, excluded_mining) = partition_mineable_traces(normalized_all);
    if normalized.len() < min_examples {
        return Err(VmError::Runtime(format!(
            "crystallize requires at least {min_examples} eligible traces, got {} (excluded {})",
            normalized.len(),
            excluded_mining.len()
        )));
    }
    let (shadow_traces, excluded_shadow) = partition_mineable_traces(
        options
            .shadow_traces
            .iter()
            .cloned()
            .map(normalize_trace)
            .collect(),
    );
    let mut shadow_pool = normalized.clone();
    shadow_pool.extend(shadow_traces);

    let mut candidates = mine_candidates(&normalized, min_examples, &options);
    let mut rejected_candidates = Vec::new();
    for candidate in &mut candidates {
        candidate.shadow = shadow_candidate(candidate, &shadow_pool);
        if !candidate.shadow.pass {
            candidate
                .rejection_reasons
                .extend(candidate.shadow.failures.clone());
        }
        refresh_promotion_metadata(candidate, min_examples, &options);
    }

    let mut accepted = Vec::new();
    for candidate in candidates {
        if candidate.is_safe_to_propose() {
            accepted.push(candidate);
        } else {
            rejected_candidates.push(candidate);
        }
    }
    accepted.sort_by(|left, right| {
        right
            .confidence
            .partial_cmp(&left.confidence)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| right.steps.len().cmp(&left.steps.len()))
    });

    let selected = accepted.first();
    let harn_code = selected
        .map(generate_harn_code)
        .unwrap_or_else(|| rejected_workflow_stub(&rejected_candidates));
    let eval_pack_toml = selected.map(generate_eval_pack).unwrap_or_default();

    let mut report = CrystallizationReport {
        version: 1,
        generated_at: now_rfc3339(),
        source_trace_count: normalized.len(),
        excluded_trace_count: excluded_mining.len() + excluded_shadow.len(),
        selected_candidate_id: selected.map(|candidate| candidate.id.clone()),
        candidates: accepted,
        rejected_candidates,
        skill_candidates: Vec::new(),
        rejected_skill_candidates: Vec::new(),
        warnings: Vec::new(),
        input_format: CrystallizationInputFormat {
            name: "harn.crystallization.trace".to_string(),
            version: TRACE_SCHEMA_VERSION,
            required_fields: vec!["id".to_string(), "actions".to_string()],
            preserved_fields: vec![
                "ordered actions".to_string(),
                "tool calls".to_string(),
                "model calls".to_string(),
                "human approvals".to_string(),
                "file mutations".to_string(),
                "external API calls".to_string(),
                "observed outputs".to_string(),
                "costs".to_string(),
                "timestamps".to_string(),
                "source hashes".to_string(),
                "Flow provenance references".to_string(),
            ],
        },
        harn_code_path: None,
        eval_pack_path: None,
        segment_summary: None,
        recovery_summary: None,
    };
    refresh_skill_candidates(&mut report, &shadow_pool);

    Ok(CrystallizationArtifacts {
        report,
        harn_code,
        eval_pack_toml,
    })
}

/// Synthesize a single-trace crystallization candidate without going
/// through repeated-sequence mining. Used by the release-fixture ingest
/// path where the trace is the workflow: there is exactly one example
/// and the deterministic/agentic split comes from the source events
/// directly. The resulting [`CrystallizationArtifacts`] is bundle-ready
/// (`build_crystallization_bundle`) and validates with the existing
/// validator.
///
/// The trace is treated as the source example and `options.shadow_traces`
/// supply held-out siblings for replay/shadow gating. `extra_parameters`
/// and `extra_metadata` let callers seed parameters (e.g., release identity
/// fields like `current_version` / `next_version`) and metadata (segment /
/// recovery summaries) that the trace alone does not surface.
pub fn synthesize_candidate_from_trace(
    trace: CrystallizationTrace,
    options: CrystallizeOptions,
    extra_parameters: Vec<WorkflowCandidateParameter>,
    segment_summary: Option<SegmentSummary>,
    recovery_summary: Option<RecoveryFeedbackSummary>,
) -> Result<CrystallizationArtifacts, VmError> {
    if trace.actions.is_empty() {
        return Err(VmError::Runtime(
            "synthesize_candidate_from_trace requires a trace with at least one action".to_string(),
        ));
    }
    let normalized = normalize_trace(trace);
    let (shadow_traces, excluded_shadow) = partition_mineable_traces(
        options
            .shadow_traces
            .iter()
            .cloned()
            .map(normalize_trace)
            .collect(),
    );
    let mut shadow_pool = Vec::with_capacity(1 + shadow_traces.len());
    shadow_pool.push(normalized.clone());
    shadow_pool.extend(shadow_traces);

    let mut candidate = build_single_trace_candidate(&normalized, &options, extra_parameters);
    candidate.shadow = shadow_candidate(&candidate, &shadow_pool);
    if !candidate.shadow.pass {
        candidate
            .rejection_reasons
            .extend(candidate.shadow.failures.clone());
    }
    refresh_promotion_metadata(&mut candidate, 1, &options);
    let (accepted, rejected) = if candidate.is_safe_to_propose() {
        (vec![candidate], Vec::new())
    } else {
        (Vec::new(), vec![candidate])
    };

    let selected = accepted.first();
    let harn_code = selected
        .map(generate_harn_code)
        .unwrap_or_else(|| rejected_workflow_stub(&rejected));
    let eval_pack_toml = selected.map(generate_eval_pack).unwrap_or_default();
    let selected_id = selected.map(|candidate| candidate.id.clone());

    let mut report = CrystallizationReport {
        version: 1,
        generated_at: now_rfc3339(),
        source_trace_count: 1,
        excluded_trace_count: excluded_shadow.len(),
        selected_candidate_id: selected_id,
        candidates: accepted,
        rejected_candidates: rejected,
        skill_candidates: Vec::new(),
        rejected_skill_candidates: Vec::new(),
        warnings: Vec::new(),
        input_format: CrystallizationInputFormat {
            name: "harn.crystallization.trace".to_string(),
            version: TRACE_SCHEMA_VERSION,
            required_fields: vec!["id".to_string(), "actions".to_string()],
            preserved_fields: vec![
                "ordered actions".to_string(),
                "deterministic events".to_string(),
                "agentic events".to_string(),
                "tool/shell observations".to_string(),
                "recovery advice".to_string(),
                "release metadata".to_string(),
                "source hashes".to_string(),
            ],
        },
        harn_code_path: None,
        eval_pack_path: None,
        segment_summary,
        recovery_summary,
    };
    refresh_skill_candidates(&mut report, &shadow_pool);

    Ok(CrystallizationArtifacts {
        report,
        harn_code,
        eval_pack_toml,
    })
}

/// Build a single-example candidate directly from a normalized trace.
/// This is the single-trace analog of `mine_candidates`: every action
/// becomes a step, every action's `fuzzy`/`model_call` flag drives the
/// segment kind, and parameters are taken from each action's
/// `parameters` map (plus any caller-supplied `extra_parameters`).
fn build_single_trace_candidate(
    trace: &CrystallizationTrace,
    options: &CrystallizeOptions,
    extra_parameters: Vec<WorkflowCandidateParameter>,
) -> WorkflowCandidate {
    let mut steps = Vec::with_capacity(trace.actions.len());
    let mut parameter_values: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut parameter_paths: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut warnings = Vec::new();
    let sequence: Vec<String> = trace.actions.iter().map(action_signature).collect();

    for (idx, action) in trace.actions.iter().enumerate() {
        let mut parameter_refs = BTreeSet::new();
        for (name, value) in &action.parameters {
            if is_scalar(value) {
                let ident = sanitize_identifier(name);
                parameter_values
                    .entry(ident.clone())
                    .or_default()
                    .insert(json_scalar_string(value));
                parameter_paths
                    .entry(ident.clone())
                    .or_default()
                    .insert(format!("steps[{idx}].parameters.{name}"));
                parameter_refs.insert(ident);
            }
        }

        let fuzzy = action.fuzzy.unwrap_or(false) || action.kind == "model_call";
        if fuzzy {
            warnings.push(format!(
                "step {} '{}' remains fuzzy and requires review/LLM handling",
                idx + 1,
                action.name
            ));
        }

        steps.push(WorkflowCandidateStep {
            index: idx + 1,
            kind: action.kind.clone(),
            name: action.name.clone(),
            segment: if fuzzy {
                SegmentKind::Fuzzy
            } else {
                SegmentKind::Deterministic
            },
            parameter_refs: parameter_refs.into_iter().collect(),
            constants: constants_for_action(action),
            preconditions: preconditions_for_action(action),
            side_effects: action.side_effects.clone(),
            capabilities: sorted_strings(action.capabilities.iter().cloned()),
            required_secrets: sorted_strings(action.required_secrets.iter().cloned()),
            approval: action.approval.clone(),
            expected_output: action
                .observed_output
                .clone()
                .or_else(|| action.output.clone()),
            review_notes: review_notes_for_action(action),
        });
    }

    let mut parameters: Vec<WorkflowCandidateParameter> = parameter_values
        .iter()
        .map(|(name, values)| WorkflowCandidateParameter {
            name: name.clone(),
            source_paths: parameter_paths
                .get(name)
                .map(|paths| paths.iter().cloned().collect())
                .unwrap_or_default(),
            examples: values.iter().take(5).cloned().collect(),
            required: true,
        })
        .collect();
    // Caller-supplied extras (e.g. release metadata fields) take
    // precedence and are appended in stable order. De-dupe by name so a
    // caller-supplied parameter overrides any same-name parameter
    // discovered from action parameters.
    let extra_names: BTreeSet<String> = extra_parameters.iter().map(|p| p.name.clone()).collect();
    parameters.retain(|p| !extra_names.contains(&p.name));
    parameters.extend(extra_parameters);
    parameters.sort_by(|left, right| left.name.cmp(&right.name));

    let example_refs = vec![WorkflowCandidateExample {
        trace_id: trace.id.clone(),
        source_hash: trace.source_hash.clone().unwrap_or_default(),
        start_index: 0,
        action_ids: trace
            .actions
            .iter()
            .map(|action| action.id.clone())
            .collect(),
    }];

    let capabilities = sorted_strings(
        steps
            .iter()
            .flat_map(|step| step.capabilities.iter().cloned()),
    );
    let required_secrets = sorted_strings(
        steps
            .iter()
            .flat_map(|step| step.required_secrets.iter().cloned()),
    );
    let approval_points = steps
        .iter()
        .filter_map(|step| step.approval.clone())
        .collect::<Vec<_>>();
    let side_effects = sorted_side_effects(
        steps
            .iter()
            .flat_map(|step| step.side_effects.iter().cloned())
            .collect(),
    );
    let expected_outputs = steps
        .iter()
        .filter_map(|step| step.expected_output.clone())
        .collect::<Vec<_>>();
    let expected_receipts = expected_receipts_for_examples(std::slice::from_ref(trace), &[(0, 0)]);

    let savings = estimate_savings(std::slice::from_ref(trace), &[(0, 0)], &steps);
    let confidence = confidence_for(&[(0, 0)], 1, &steps, true);
    let name = options
        .workflow_name
        .clone()
        .unwrap_or_else(|| infer_workflow_name(&steps));
    let package_name = options
        .package_name
        .clone()
        .unwrap_or_else(|| name.replace('_', "-"));

    WorkflowCandidate {
        id: stable_candidate_id(&sequence, &example_refs),
        name,
        confidence,
        cluster_key: cluster_key_for_candidate(std::slice::from_ref(trace), &[(0, 0)], &steps),
        sequence_signature: sequence,
        parameters,
        steps,
        examples: example_refs.clone(),
        capabilities: capabilities.clone(),
        required_secrets: required_secrets.clone(),
        approval_points,
        side_effects,
        expected_outputs,
        expected_receipts,
        warnings,
        rejection_reasons: Vec::new(),
        promotion: PromotionMetadata {
            source_trace_hashes: example_refs
                .iter()
                .map(|example| example.source_hash.clone())
                .collect(),
            author: options.author.clone(),
            approver: options.approver.clone(),
            created_at: now_rfc3339(),
            version: "0.1.0".to_string(),
            package_name,
            capability_set: capabilities,
            secrets_required: required_secrets,
            rollback_target: Some("keep source trace and previous package version".to_string()),
            eval_pack_link: options.eval_pack_link.clone(),
            ..PromotionMetadata::default()
        },
        savings,
        shadow: ShadowRunReport::default(),
    }
}

fn review_notes_for_action(action: &CrystallizationAction) -> Vec<String> {
    let mut notes = Vec::new();
    if action.kind == "shell_failure"
        || matches!(
            action
                .metadata
                .get("success")
                .and_then(|value| value.as_bool()),
            Some(false)
        )
    {
        notes.push(format!(
            "shell/tool step '{}' failed in the source trace; reviewer should confirm \
             whether deterministic recovery is possible before promotion.",
            action.name
        ));
    }
    if let Some(hint) = action
        .metadata
        .get("recovery_hint")
        .and_then(|value| value.as_str())
        .filter(|hint| !hint.trim().is_empty())
    {
        notes.push(format!("recovery hint from source trace: {hint}"));
    }
    if action.kind == "agent_recovery_advice" {
        notes.push(
            "agent-authored recovery advice; treat as advisory, never as deterministic truth."
                .to_string(),
        );
    }
    notes
}

pub fn load_crystallization_traces_from_dir(
    dir: &Path,
) -> Result<Vec<CrystallizationTrace>, VmError> {
    let mut paths = Vec::new();
    collect_json_paths(dir, &mut paths)?;
    if paths.is_empty() {
        return Err(VmError::Runtime(format!(
            "no .json trace files found under {}",
            dir.display()
        )));
    }
    paths.sort();
    paths
        .iter()
        .map(|path| load_crystallization_trace(path))
        .collect()
}

pub fn load_crystallization_trace(path: &Path) -> Result<CrystallizationTrace, VmError> {
    let content = std::fs::read_to_string(path).map_err(|error| {
        VmError::Runtime(format!(
            "failed to read crystallization trace {}: {error}",
            path.display()
        ))
    })?;
    let value: JsonValue = serde_json::from_str(&content).map_err(|error| {
        VmError::Runtime(format!(
            "failed to parse crystallization trace {}: {error}",
            path.display()
        ))
    })?;

    let mut trace = if value.get("actions").is_some() {
        serde_json::from_value::<CrystallizationTrace>(value).map_err(|error| {
            VmError::Runtime(format!(
                "failed to decode crystallization trace {}: {error}",
                path.display()
            ))
        })?
    } else if value.get("stages").is_some() || value.get("_type") == Some(&json!("workflow_run")) {
        let run: RunRecord = serde_json::from_value(value).map_err(|error| {
            VmError::Runtime(format!(
                "failed to decode run record {} as crystallization input: {error}",
                path.display()
            ))
        })?;
        trace_from_run_record(run)
    } else {
        return Err(VmError::Runtime(format!(
            "{} is neither a crystallization trace nor a workflow run record",
            path.display()
        )));
    };
    if trace.source.is_none() {
        trace.source = Some(path.display().to_string());
    }
    if trace.source_hash.is_none() {
        trace.source_hash = Some(hash_bytes(content.as_bytes()));
    }
    Ok(normalize_trace(trace))
}

pub fn write_crystallization_artifacts(
    mut artifacts: CrystallizationArtifacts,
    workflow_path: &Path,
    report_path: &Path,
    eval_pack_path: Option<&Path>,
) -> Result<CrystallizationReport, VmError> {
    crate::atomic_io::atomic_write(workflow_path, artifacts.harn_code.as_bytes()).map_err(
        |error| {
            VmError::Runtime(format!(
                "failed to write generated workflow {}: {error}",
                workflow_path.display()
            ))
        },
    )?;

    artifacts.report.harn_code_path = Some(workflow_path.display().to_string());
    if let Some(path) = eval_pack_path {
        if !artifacts.eval_pack_toml.trim().is_empty() {
            crate::atomic_io::atomic_write(path, artifacts.eval_pack_toml.as_bytes()).map_err(
                |error| {
                    VmError::Runtime(format!(
                        "failed to write eval pack {}: {error}",
                        path.display()
                    ))
                },
            )?;
            artifacts.report.eval_pack_path = Some(path.display().to_string());
            if let Some(candidate) = artifacts.report.candidates.first_mut() {
                candidate.promotion.eval_pack_link = Some(path.display().to_string());
            }
        }
    }

    let report_json = serde_json::to_string_pretty(&artifacts.report)
        .map_err(|error| VmError::Runtime(format!("failed to encode report JSON: {error}")))?;
    crate::atomic_io::atomic_write(report_path, report_json.as_bytes()).map_err(|error| {
        VmError::Runtime(format!(
            "failed to write crystallization report {}: {error}",
            report_path.display()
        ))
    })?;
    Ok(artifacts.report)
}

fn collect_json_paths(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), VmError> {
    let entries = std::fs::read_dir(dir).map_err(|error| {
        VmError::Runtime(format!(
            "failed to read crystallization trace dir {}: {error}",
            dir.display()
        ))
    })?;
    for entry in entries {
        let entry = entry.map_err(|error| {
            VmError::Runtime(format!(
                "failed to read entry in trace dir {}: {error}",
                dir.display()
            ))
        })?;
        let path = entry.path();
        if path.is_dir() {
            collect_json_paths(&path, out)?;
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("json") {
            out.push(path);
        }
    }
    Ok(())
}

fn trace_from_run_record(run: RunRecord) -> CrystallizationTrace {
    let mut actions = Vec::new();
    for stage in &run.stages {
        actions.push(CrystallizationAction {
            id: stage.id.clone(),
            kind: if stage.kind.is_empty() {
                "stage".to_string()
            } else {
                stage.kind.clone()
            },
            name: stage.node_id.clone(),
            timestamp: Some(stage.started_at.clone()),
            output: stage.visible_text.as_ref().map(|text| json!(text)),
            observed_output: stage.visible_text.as_ref().map(|text| json!(text)),
            duration_ms: stage.usage.as_ref().map(|usage| usage.total_duration_ms),
            cost: stage
                .usage
                .as_ref()
                .map(|usage| CrystallizationCost {
                    model_calls: usage.call_count,
                    input_tokens: usage.input_tokens,
                    output_tokens: usage.output_tokens,
                    total_cost_usd: usage.total_cost,
                    wall_ms: usage.total_duration_ms,
                    model: usage.models.first().cloned(),
                })
                .unwrap_or_default(),
            deterministic: Some(
                stage
                    .usage
                    .as_ref()
                    .map(|usage| usage.call_count == 0)
                    .unwrap_or(true),
            ),
            fuzzy: Some(
                stage
                    .usage
                    .as_ref()
                    .is_some_and(|usage| usage.call_count > 0),
            ),
            metadata: stage.metadata.clone(),
            ..CrystallizationAction::default()
        });
    }
    for tool in &run.tool_recordings {
        actions.push(CrystallizationAction {
            id: tool.tool_use_id.clone(),
            kind: "tool_call".to_string(),
            name: tool.tool_name.clone(),
            timestamp: Some(tool.timestamp.clone()),
            output: Some(json!(tool.result)),
            observed_output: Some(json!(tool.result)),
            duration_ms: Some(tool.duration_ms as i64),
            deterministic: Some(!tool.is_rejected),
            fuzzy: Some(false),
            metadata: BTreeMap::from([
                ("args_hash".to_string(), json!(tool.args_hash)),
                ("iteration".to_string(), json!(tool.iteration)),
                ("is_rejected".to_string(), json!(tool.is_rejected)),
            ]),
            ..CrystallizationAction::default()
        });
    }
    for question in &run.hitl_questions {
        actions.push(CrystallizationAction {
            id: question.request_id.clone(),
            kind: "human_approval".to_string(),
            name: question.agent.clone(),
            timestamp: Some(question.asked_at.clone()),
            approval: Some(CrystallizationApproval {
                prompt: Some(question.prompt.clone()),
                required: true,
                boundary: Some("hitl".to_string()),
                ..CrystallizationApproval::default()
            }),
            deterministic: Some(false),
            fuzzy: Some(false),
            metadata: question
                .trace_id
                .as_ref()
                .map(|trace_id| BTreeMap::from([("trace_id".to_string(), json!(trace_id))]))
                .unwrap_or_default(),
            ..CrystallizationAction::default()
        });
    }
    actions.sort_by(|left, right| left.timestamp.cmp(&right.timestamp));

    CrystallizationTrace {
        version: TRACE_SCHEMA_VERSION,
        id: run.id.clone(),
        workflow_id: Some(run.workflow_id.clone()),
        started_at: Some(run.started_at.clone()),
        finished_at: run.finished_at.clone(),
        actions,
        replay_run: run.replay_fixture.as_ref().map(replay_run_from_fixture),
        replay_allowlist: default_trace_replay_allowlist(),
        usage: run
            .usage
            .map(|usage| CrystallizationUsage {
                model_calls: usage.call_count,
                input_tokens: usage.input_tokens,
                output_tokens: usage.output_tokens,
                total_cost_usd: usage.total_cost,
                wall_ms: usage.total_duration_ms,
            })
            .unwrap_or_default(),
        metadata: run.metadata,
        ..CrystallizationTrace::default()
    }
}

fn replay_run_from_fixture(fixture: &ReplayFixture) -> ReplayTraceRun {
    ReplayTraceRun {
        run_id: fixture.source_run_id.clone(),
        final_artifacts: fixture
            .stage_assertions
            .iter()
            .map(|assertion| {
                json!({
                    "node_id": assertion.node_id.clone(),
                    "expected_status": assertion.expected_status.clone(),
                    "expected_outcome": assertion.expected_outcome.clone(),
                    "expected_branch": assertion.expected_branch.clone(),
                    "required_artifact_kinds": assertion.required_artifact_kinds.clone(),
                    "visible_text_contains": assertion.visible_text_contains.clone(),
                })
            })
            .collect(),
        policy_decisions: vec![json!({
            "workflow_id": fixture.workflow_id.clone(),
            "expected_status": fixture.expected_status.clone(),
            "eval_kind": fixture.eval_kind.clone(),
        })],
        ..ReplayTraceRun::default()
    }
}

fn default_trace_replay_allowlist() -> Vec<ReplayAllowlistRule> {
    vec![ReplayAllowlistRule {
        path: "/run_id".to_string(),
        reason: "run ids are allocated per execution".to_string(),
        replacement: None,
    }]
}
