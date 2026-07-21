//! Trace normalization, mineability checks, and repeated-sequence mining.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde_json::Value as JsonValue;

use super::super::new_id;
use super::super::now_unix_seconds_text;
use super::shadow::{confidence_for, estimate_savings, infer_workflow_name};
use super::types::{
    CrystallizationAction, CrystallizationSideEffect, CrystallizationTrace, CrystallizationUsage,
    CrystallizeOptions, PromotionMetadata, RepeatedSequence, SegmentKind, ShadowRunReport,
    WorkflowCandidate, WorkflowCandidateExample, WorkflowCandidateParameter, WorkflowCandidateStep,
    WorkflowClusterKey, TRACE_SCHEMA_VERSION,
};
use super::util::{
    hash_bytes, is_scalar, json_scalar_string, sanitize_identifier, sorted_side_effects,
    sorted_strings, stable_candidate_id,
};

pub(super) fn normalize_trace(mut trace: CrystallizationTrace) -> CrystallizationTrace {
    if trace.version == 0 {
        trace.version = TRACE_SCHEMA_VERSION;
    }
    if trace.id.trim().is_empty() {
        trace.id = new_id("trace");
    }
    if trace.source_hash.is_none() {
        let payload = serde_json::to_vec(&trace.actions).unwrap_or_default();
        trace.source_hash = Some(hash_bytes(&payload));
    }
    for (idx, action) in trace.actions.iter_mut().enumerate() {
        if action.id.trim().is_empty() {
            action.id = format!("action_{}", idx + 1);
        }
        if action.kind.trim().is_empty() {
            action.kind = "action".to_string();
        }
        if action.name.trim().is_empty() {
            action.name = action.kind.clone();
        }
        action.capabilities.sort();
        action.capabilities.dedup();
        action.required_secrets.sort();
        action.required_secrets.dedup();
        action.side_effects = sorted_side_effects(std::mem::take(&mut action.side_effects));
        if action.cost.model_calls == 0 && action.kind == "model_call" {
            action.cost.model_calls = 1;
        }
    }
    if trace.usage == CrystallizationUsage::default() {
        for action in &trace.actions {
            trace.usage.model_calls += action.cost.model_calls;
            trace.usage.input_tokens += action.cost.input_tokens;
            trace.usage.output_tokens += action.cost.output_tokens;
            trace.usage.total_cost_usd += action.cost.total_cost_usd;
            trace.usage.wall_ms += action.cost.wall_ms + action.duration_ms.unwrap_or_default();
        }
    }
    trace
}

pub(super) fn partition_mineable_traces(
    traces: Vec<CrystallizationTrace>,
) -> (Vec<CrystallizationTrace>, Vec<CrystallizationTrace>) {
    traces
        .into_iter()
        .partition(|trace| trace_is_mineable(trace).is_ok())
}

pub(super) fn trace_is_mineable(trace: &CrystallizationTrace) -> Result<(), String> {
    if metadata_has_unresolved_policy_violation(&trace.metadata) {
        return Err("trace has unresolved policy violations".to_string());
    }
    for action in &trace.actions {
        if metadata_has_unresolved_policy_violation(&action.metadata) {
            return Err(format!(
                "action {} has unresolved policy violations",
                action.id
            ));
        }
        if action_has_nondeterministic_side_effect(action) {
            return Err(format!(
                "action {} has nondeterministic side effects",
                action.id
            ));
        }
    }
    Ok(())
}

fn metadata_has_unresolved_policy_violation(metadata: &BTreeMap<String, JsonValue>) -> bool {
    for (key, value) in metadata {
        let lower = key.to_ascii_lowercase();
        if lower.contains("unresolved_policy_violation")
            || lower == "policy_violation_unresolved"
            || lower == "policy_violation"
        {
            if value.as_bool() == Some(true) {
                return true;
            }
            if value
                .as_str()
                .is_some_and(|text| text.eq_ignore_ascii_case("unresolved"))
            {
                return true;
            }
            if value.as_array().is_some_and(|items| !items.is_empty()) {
                return true;
            }
        }
        if lower == "policy_status"
            && value
                .as_str()
                .is_some_and(|text| text.eq_ignore_ascii_case("unresolved"))
        {
            return true;
        }
    }
    false
}

fn action_has_nondeterministic_side_effect(action: &CrystallizationAction) -> bool {
    if action.side_effects.is_empty() {
        return false;
    }
    if action
        .metadata
        .get("nondeterministic_side_effect")
        .and_then(JsonValue::as_bool)
        == Some(true)
    {
        return true;
    }
    if action.deterministic == Some(false) || action.fuzzy.unwrap_or(false) {
        return true;
    }
    action.side_effects.iter().any(|effect| {
        effect
            .metadata
            .get("nondeterministic")
            .and_then(JsonValue::as_bool)
            == Some(true)
    })
}

pub(super) fn mine_candidates(
    traces: &[CrystallizationTrace],
    min_examples: usize,
    options: &CrystallizeOptions,
) -> Vec<WorkflowCandidate> {
    let signatures = traces
        .iter()
        .map(|trace| {
            trace
                .actions
                .iter()
                .map(action_signature)
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let Some((sequence, examples)) = best_repeated_sequence(&signatures, min_examples) else {
        return Vec::new();
    };

    let mut example_refs = Vec::new();
    for (trace_index, start_index) in &examples {
        let trace = &traces[*trace_index];
        example_refs.push(WorkflowCandidateExample {
            trace_id: trace.id.clone(),
            source_hash: trace.source_hash.clone().unwrap_or_default(),
            start_index: *start_index,
            action_ids: trace.actions[*start_index..*start_index + sequence.len()]
                .iter()
                .map(|action| action.id.clone())
                .collect(),
        });
    }

    let mut steps = Vec::new();
    let mut parameter_values: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut parameter_paths: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut rejection_reasons = Vec::new();
    let mut warnings = Vec::new();

    for step_index in 0..sequence.len() {
        let actions = examples
            .iter()
            .map(|(trace_index, start_index)| {
                &traces[*trace_index].actions[*start_index + step_index]
            })
            .collect::<Vec<_>>();
        let first = actions[0];
        if !compatible_side_effects(&actions) {
            rejection_reasons.push(format!(
                "step {} '{}' has divergent side effects across examples",
                step_index + 1,
                first.name
            ));
        }

        let mut parameter_refs = BTreeSet::new();
        for action in &actions {
            for (name, value) in &action.parameters {
                if is_scalar(value) {
                    parameter_values
                        .entry(sanitize_identifier(name))
                        .or_default()
                        .insert(json_scalar_string(value));
                    parameter_paths
                        .entry(sanitize_identifier(name))
                        .or_default()
                        .insert(format!("steps[{step_index}].parameters.{name}"));
                    parameter_refs.insert(sanitize_identifier(name));
                }
            }
        }
        collect_varying_parameters(
            &actions,
            "inputs",
            |action| &action.inputs,
            &mut parameter_values,
            &mut parameter_paths,
            &mut parameter_refs,
        );

        let fuzzy = first.fuzzy.unwrap_or(false)
            || first.kind == "model_call"
            || actions.iter().any(|action| action.fuzzy.unwrap_or(false));
        if fuzzy {
            warnings.push(format!(
                "step {} '{}' remains fuzzy and requires review/LLM handling",
                step_index + 1,
                first.name
            ));
        }

        steps.push(WorkflowCandidateStep {
            index: step_index + 1,
            kind: first.kind.clone(),
            name: first.name.clone(),
            segment: if fuzzy {
                SegmentKind::Fuzzy
            } else {
                SegmentKind::Deterministic
            },
            parameter_refs: parameter_refs.into_iter().collect(),
            constants: constants_for_action(first),
            preconditions: preconditions_for_action(first),
            side_effects: first.side_effects.clone(),
            capabilities: sorted_strings(first.capabilities.iter().cloned()),
            required_secrets: sorted_strings(first.required_secrets.iter().cloned()),
            approval: first.approval.clone(),
            expected_output: stable_expected_output(&actions),
            review_notes: Vec::new(),
        });
    }

    let parameters = parameter_values
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
        .collect::<Vec<_>>();

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
    let expected_receipts = expected_receipts_for_examples(traces, &examples);
    let savings = estimate_savings(traces, &examples, &steps);
    let confidence = confidence_for(
        &examples,
        traces.len(),
        &steps,
        rejection_reasons.is_empty(),
    );
    let name = options
        .workflow_name
        .clone()
        .unwrap_or_else(|| infer_workflow_name(&steps));
    let package_name = options
        .package_name
        .clone()
        .unwrap_or_else(|| name.replace('_', "-"));

    vec![WorkflowCandidate {
        id: stable_candidate_id(&sequence, &example_refs),
        name,
        confidence,
        cluster_key: cluster_key_for_candidate(traces, &examples, &steps),
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
        rejection_reasons,
        promotion: PromotionMetadata {
            source_trace_hashes: example_refs
                .iter()
                .map(|example| example.source_hash.clone())
                .collect(),
            author: options.author.clone(),
            approver: options.approver.clone(),
            created_at: now_unix_seconds_text(),
            version: "0.1.0".to_string(),
            package_name,
            capability_set: capabilities,
            secrets_required: required_secrets,
            rollback_target: Some("keep source traces and previous package version".to_string()),
            eval_pack_link: options.eval_pack_link.clone(),
            ..PromotionMetadata::default()
        },
        savings,
        shadow: ShadowRunReport::default(),
    }]
}

fn best_repeated_sequence(
    signatures: &[Vec<String>],
    min_examples: usize,
) -> Option<RepeatedSequence> {
    let mut occurrences: BTreeMap<Vec<String>, Vec<(usize, usize)>> = BTreeMap::new();
    for (trace_index, trace_signatures) in signatures.iter().enumerate() {
        for start in 0..trace_signatures.len() {
            let max_len = (trace_signatures.len() - start).min(12);
            for len in 2..=max_len {
                let sequence = trace_signatures[start..start + len].to_vec();
                occurrences
                    .entry(sequence)
                    .or_default()
                    .push((trace_index, start));
            }
        }
    }

    occurrences
        .into_iter()
        .filter_map(|(sequence, positions)| {
            let mut seen = BTreeSet::new();
            let mut examples = Vec::new();
            for (trace_index, start) in positions {
                if seen.insert(trace_index) {
                    examples.push((trace_index, start));
                }
            }
            if examples.len() >= min_examples {
                Some((sequence, examples))
            } else {
                None
            }
        })
        .max_by(
            |(left_sequence, left_examples), (right_sequence, right_examples)| {
                left_examples
                    .len()
                    .cmp(&right_examples.len())
                    .then_with(|| left_sequence.len().cmp(&right_sequence.len()))
            },
        )
}

pub(super) fn action_signature(action: &CrystallizationAction) -> String {
    let mut parameter_keys = action.parameters.keys().cloned().collect::<Vec<_>>();
    parameter_keys.sort();
    format!(
        "{}:{}:{}",
        action.kind,
        action.name,
        parameter_keys.join(",")
    )
}

pub(super) fn compatible_side_effects(actions: &[&CrystallizationAction]) -> bool {
    let first = sorted_side_effects(actions[0].side_effects.clone());
    actions
        .iter()
        .skip(1)
        .all(|action| sorted_side_effects(action.side_effects.clone()) == first)
}

fn collect_varying_parameters(
    actions: &[&CrystallizationAction],
    root: &str,
    value_for: impl Fn(&CrystallizationAction) -> &JsonValue,
    parameter_values: &mut BTreeMap<String, BTreeSet<String>>,
    parameter_paths: &mut BTreeMap<String, BTreeSet<String>>,
    parameter_refs: &mut BTreeSet<String>,
) {
    let mut paths = BTreeMap::<String, Vec<JsonValue>>::new();
    for action in actions {
        collect_scalar_paths(value_for(action), root, &mut paths);
    }
    for (path, values) in paths {
        if values.len() != actions.len() {
            continue;
        }
        let unique = values
            .iter()
            .map(json_scalar_string)
            .collect::<BTreeSet<_>>();
        if unique.len() < 2 {
            continue;
        }
        let name = parameter_name_for_path(&path);
        parameter_values
            .entry(name.clone())
            .or_default()
            .extend(unique);
        parameter_paths
            .entry(name.clone())
            .or_default()
            .insert(path);
        parameter_refs.insert(name);
    }
}

fn collect_scalar_paths(
    value: &JsonValue,
    prefix: &str,
    out: &mut BTreeMap<String, Vec<JsonValue>>,
) {
    match value {
        JsonValue::Object(map) => {
            for (key, child) in map {
                collect_scalar_paths(child, &format!("{prefix}.{key}"), out);
            }
        }
        JsonValue::Array(items) => {
            for (idx, child) in items.iter().enumerate() {
                collect_scalar_paths(child, &format!("{prefix}[{idx}]"), out);
            }
        }
        _ if is_scalar(value) => {
            out.entry(prefix.to_string())
                .or_default()
                .push(value.clone());
        }
        _ => {}
    }
}

fn parameter_name_for_path(path: &str) -> String {
    let lower = path.to_ascii_lowercase();
    for (needle, name) in [
        ("version", "version"),
        ("repo_path", "repo_path"),
        ("repo", "repo_path"),
        ("branch_name", "branch_name"),
        ("branch", "branch_name"),
        ("release_target", "release_target"),
        ("target", "release_target"),
    ] {
        if lower.contains(needle) {
            return name.to_string();
        }
    }
    let tail = path
        .rsplit(['.', '['])
        .next()
        .unwrap_or("param")
        .trim_end_matches(']');
    sanitize_identifier(tail)
}

pub(super) fn constants_for_action(action: &CrystallizationAction) -> BTreeMap<String, JsonValue> {
    let mut constants = BTreeMap::new();
    constants.insert("kind".to_string(), serde_json::json!(action.kind));
    constants.insert("name".to_string(), serde_json::json!(action.name));
    if action.deterministic.unwrap_or(false) {
        constants.insert("deterministic".to_string(), serde_json::json!(true));
    }
    constants
}

pub(super) fn preconditions_for_action(action: &CrystallizationAction) -> Vec<String> {
    let mut out = Vec::new();
    for capability in &action.capabilities {
        out.push(format!("capability '{capability}' is available"));
    }
    for secret in &action.required_secrets {
        out.push(format!("secret '{secret}' is configured"));
    }
    if let Some(approval) = &action.approval {
        if approval.required {
            out.push("human approval boundary is preserved".to_string());
        }
    }
    out
}

pub(super) fn stable_expected_output(actions: &[&CrystallizationAction]) -> Option<JsonValue> {
    let first = actions[0]
        .observed_output
        .as_ref()
        .or(actions[0].output.as_ref())?;
    if actions
        .iter()
        .all(|action| action.observed_output.as_ref().or(action.output.as_ref()) == Some(first))
    {
        Some(first.clone())
    } else {
        None
    }
}

pub(super) fn expected_receipts_for_examples(
    traces: &[CrystallizationTrace],
    examples: &[(usize, usize)],
) -> Vec<JsonValue> {
    examples
        .iter()
        .find_map(|(trace_index, _)| {
            traces
                .get(*trace_index)
                .and_then(|trace| trace.replay_run.as_ref())
                .map(|run| run.effect_receipts.clone())
                .filter(|receipts| !receipts.is_empty())
        })
        .unwrap_or_default()
}

pub(super) fn cluster_key_for_candidate(
    traces: &[CrystallizationTrace],
    examples: &[(usize, usize)],
    steps: &[WorkflowCandidateStep],
) -> WorkflowClusterKey {
    let goal = examples.iter().find_map(|(trace_index, _)| {
        traces.get(*trace_index).and_then(|trace| {
            trace
                .metadata
                .get("goal")
                .and_then(JsonValue::as_str)
                .map(str::to_string)
                .or_else(|| trace.workflow_id.clone())
        })
    });
    let tool_sequence = steps
        .iter()
        .filter(|step| step.kind == "tool_call" || step.kind == "file_mutation")
        .map(|step| step.name.clone())
        .collect::<Vec<_>>();
    let touched_artifact_types = sorted_strings(
        steps
            .iter()
            .flat_map(|step| step.side_effects.iter().map(artifact_type_for_side_effect)),
    );
    let success_criteria = sorted_strings(examples.iter().filter_map(|(trace_index, _)| {
        traces
            .get(*trace_index)
            .and_then(|trace| trace.metadata.get("success_criteria"))
            .and_then(JsonValue::as_str)
            .map(str::to_string)
    }));

    WorkflowClusterKey {
        goal,
        tool_sequence,
        touched_artifact_types,
        success_criteria,
    }
}

fn artifact_type_for_side_effect(effect: &CrystallizationSideEffect) -> String {
    if let Some(kind) = effect
        .metadata
        .get("artifact_type")
        .and_then(JsonValue::as_str)
        .filter(|kind| !kind.trim().is_empty())
    {
        return kind.to_string();
    }
    let lower_kind = effect.kind.to_ascii_lowercase();
    if lower_kind.contains("git") {
        "git".to_string()
    } else if lower_kind.contains("file") {
        Path::new(&effect.target)
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| format!("file:{ext}"))
            .unwrap_or_else(|| "file".to_string())
    } else if lower_kind.contains("receipt") {
        "receipt".to_string()
    } else if lower_kind.contains("publish") {
        "package_publish".to_string()
    } else if !lower_kind.is_empty() {
        lower_kind
    } else {
        "unknown".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn action_with_side_effect(
        deterministic: Option<bool>,
        fuzzy: Option<bool>,
    ) -> CrystallizationAction {
        CrystallizationAction {
            id: "a1".to_string(),
            kind: "tool_call".to_string(),
            side_effects: vec![CrystallizationSideEffect {
                kind: "fs_write".to_string(),
                target: "/tmp/out".to_string(),
                ..CrystallizationSideEffect::default()
            }],
            deterministic,
            fuzzy,
            ..CrystallizationAction::default()
        }
    }

    #[test]
    fn explicitly_nondeterministic_action_is_flagged_even_when_not_fuzzy() {
        // Rejected tool calls and human-approval steps are emitted with
        // `deterministic: Some(false), fuzzy: Some(false)`; carrying a side
        // effect they must still be treated as nondeterministic.
        let action = action_with_side_effect(Some(false), Some(false));
        assert!(action_has_nondeterministic_side_effect(&action));
    }

    #[test]
    fn fuzzy_action_is_flagged_even_when_determinism_unset() {
        let action = action_with_side_effect(None, Some(true));
        assert!(action_has_nondeterministic_side_effect(&action));
    }

    #[test]
    fn deterministic_non_fuzzy_action_is_not_flagged() {
        let action = action_with_side_effect(Some(true), Some(false));
        assert!(!action_has_nondeterministic_side_effect(&action));
    }
}
