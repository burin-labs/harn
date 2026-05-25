//! Shadow execution, divergence collection, savings/confidence estimates, and promotion metadata refresh.

use std::collections::BTreeSet;

use serde_json::Value as JsonValue;

use super::super::{
    now_rfc3339, run_replay_oracle_trace, ReplayExpectation, ReplayOracleReport, ReplayOracleTrace,
};
use super::normalize::action_signature;
use super::types::{
    CrystallizationTrace, CrystallizeOptions, PromotionApprovalRecord, PromotionCriteria,
    PromotionDivergenceRecord, PromotionStatus, SavingsEstimate, SegmentKind, ShadowRunReport,
    ShadowTraceResult, WorkflowCandidate, WorkflowCandidateStep, DEFAULT_PROMOTION_MIN_CONFIDENCE,
};
use super::util::{sorted_side_effects, sorted_strings};

pub(super) fn refresh_promotion_metadata(
    candidate: &mut WorkflowCandidate,
    min_examples: usize,
    options: &CrystallizeOptions,
) {
    let min_confidence = if options.promotion_min_confidence > 0.0 {
        options.promotion_min_confidence
    } else {
        DEFAULT_PROMOTION_MIN_CONFIDENCE
    };
    let shadow_success_count = candidate
        .shadow
        .traces
        .iter()
        .filter(|trace| trace.pass)
        .count();
    let shadow_failure_count = candidate
        .shadow
        .traces
        .len()
        .saturating_sub(shadow_success_count);
    let sample_count = candidate
        .shadow
        .compared_traces
        .max(candidate.examples.len());
    let divergence_history = candidate
        .shadow
        .traces
        .iter()
        .filter(|trace| !trace.pass)
        .flat_map(divergence_records_for_shadow_trace)
        .collect::<Vec<_>>();
    let source_trace_hashes = sorted_strings(
        candidate
            .promotion
            .source_trace_hashes
            .iter()
            .cloned()
            .chain(
                candidate
                    .shadow
                    .traces
                    .iter()
                    .map(|trace| trace.source_hash.clone()),
            ),
    );
    let approval_history = options
        .approver
        .as_ref()
        .filter(|approver| !approver.trim().is_empty())
        .map(|approver| {
            vec![PromotionApprovalRecord {
                actor: approver.clone(),
                decision: "approved_for_shadow_promotion".to_string(),
                recorded_at: now_rfc3339(),
                reason: Some("approver supplied in crystallization options".to_string()),
            }]
        })
        .unwrap_or_default();
    let mut criteria = PromotionCriteria {
        min_examples,
        min_confidence,
        requires_shadow_pass: true,
        requires_no_rejections: true,
        requires_human_approval: true,
        approval_reason: Some(
            "workflow candidates start in shadow mode and require explicit human approval before promotion".to_string(),
        ),
        ..PromotionCriteria::default()
    };
    if sample_count < min_examples {
        criteria.reasons.push(format!(
            "sample count {sample_count} is below required minimum {min_examples}"
        ));
    }
    if candidate.confidence < min_confidence {
        criteria.reasons.push(format!(
            "confidence {:.2} is below required minimum {:.2}",
            candidate.confidence, min_confidence
        ));
    }
    if !candidate.shadow.pass {
        criteria
            .reasons
            .push("shadow comparison did not pass".to_string());
    }
    if !candidate.rejection_reasons.is_empty() {
        criteria
            .reasons
            .push("candidate has rejection reasons".to_string());
    }
    if approval_history.is_empty() {
        criteria
            .reasons
            .push("human approval has not been recorded".to_string());
    }
    criteria.status = if !candidate.rejection_reasons.is_empty()
        || !candidate.shadow.pass
        || sample_count < min_examples
        || candidate.confidence < min_confidence
    {
        PromotionStatus::Blocked
    } else if approval_history.is_empty() {
        PromotionStatus::NeedsApproval
    } else {
        PromotionStatus::Ready
    };

    candidate.promotion.sample_count = sample_count;
    candidate.promotion.source_trace_hashes = source_trace_hashes;
    candidate.promotion.confidence = candidate.confidence;
    candidate.promotion.shadow_success_count = shadow_success_count;
    candidate.promotion.shadow_failure_count = shadow_failure_count;
    candidate.promotion.divergence_history = divergence_history;
    candidate.promotion.approval_history = approval_history;
    candidate.promotion.criteria = criteria;
    candidate.promotion.estimated_time_token_savings = candidate.savings.clone();
}

fn divergence_records_for_shadow_trace(
    trace: &ShadowTraceResult,
) -> Vec<PromotionDivergenceRecord> {
    if let Some(report) = &trace.replay_oracle {
        if let Some(divergence) = &report.divergence {
            return vec![PromotionDivergenceRecord {
                trace_id: trace.trace_id.clone(),
                path: Some(divergence.path.clone()),
                message: divergence.message.clone(),
                left: Some(divergence.left.clone()),
                right: Some(divergence.right.clone()),
            }];
        }
    }
    trace
        .details
        .iter()
        .map(|detail| PromotionDivergenceRecord {
            trace_id: trace.trace_id.clone(),
            path: None,
            message: detail.clone(),
            left: None,
            right: None,
        })
        .collect()
}

pub(super) fn shadow_candidate(
    candidate: &WorkflowCandidate,
    traces: &[CrystallizationTrace],
) -> ShadowRunReport {
    let mut failures = Vec::new();
    let mut results = Vec::new();
    let mut compared_trace_ids = BTreeSet::new();
    for example in &candidate.examples {
        let Some(trace) = traces.iter().find(|trace| trace.id == example.trace_id) else {
            failures.push(format!("missing source trace {}", example.trace_id));
            continue;
        };
        compared_trace_ids.insert(trace.id.clone());
        let result = shadow_trace_result(candidate, trace, example.start_index);
        if !result.pass {
            failures.push(format!("trace {} failed shadow comparison", trace.id));
        }
        results.push(result);
    }

    for trace in traces {
        if compared_trace_ids.contains(&trace.id) {
            continue;
        }
        if let Some(start_index) = find_sequence_start(trace, &candidate.sequence_signature) {
            compared_trace_ids.insert(trace.id.clone());
            let result = shadow_trace_result(candidate, trace, start_index);
            if !result.pass {
                failures.push(format!("trace {} failed shadow comparison", trace.id));
            }
            results.push(result);
        }
    }

    ShadowRunReport {
        pass: failures.is_empty(),
        compared_traces: results.len(),
        failures,
        traces: results,
    }
}

pub(super) fn find_sequence_start(
    trace: &CrystallizationTrace,
    sequence: &[String],
) -> Option<usize> {
    if sequence.is_empty() || trace.actions.len() < sequence.len() {
        return None;
    }
    trace
        .actions
        .windows(sequence.len())
        .position(|window| window.iter().map(action_signature).collect::<Vec<_>>() == sequence)
}

fn shadow_trace_result(
    candidate: &WorkflowCandidate,
    trace: &CrystallizationTrace,
    start_index: usize,
) -> ShadowTraceResult {
    let mut details = Vec::new();
    let end = start_index + candidate.steps.len();
    if end > trace.actions.len() {
        details.push("candidate sequence extends past trace action list".to_string());
    } else {
        let signatures = trace.actions[start_index..end]
            .iter()
            .map(action_signature)
            .collect::<Vec<_>>();
        if signatures != candidate.sequence_signature {
            details.push("action sequence signature changed".to_string());
        }
        for (offset, step) in candidate.steps.iter().enumerate() {
            let action = &trace.actions[start_index + offset];
            if sorted_side_effects(action.side_effects.clone()) != step.side_effects {
                details.push(format!(
                    "step {} side effects differ for action {}",
                    step.index, action.id
                ));
            }
            if action.approval.as_ref().map(|approval| approval.required)
                != step.approval.as_ref().map(|approval| approval.required)
            {
                details.push(format!("step {} approval boundary differs", step.index));
            }
            if step.segment == SegmentKind::Deterministic {
                if let Some(expected) = &step.expected_output {
                    let actual = action.observed_output.as_ref().or(action.output.as_ref());
                    if actual != Some(expected) {
                        details.push(format!("step {} deterministic output differs", step.index));
                    }
                }
            }
        }
    }

    let (replay_oracle, compared_receipts) = replay_oracle_for_shadow(candidate, trace);
    if let Some(report) = &replay_oracle {
        if !report.passed {
            if let Some(divergence) = &report.divergence {
                details.push(format!(
                    "receipt replay diverged at {}: {}",
                    divergence.path, divergence.message
                ));
            } else {
                details.push("receipt replay oracle did not pass".to_string());
            }
        }
    }

    ShadowTraceResult {
        trace_id: trace.id.clone(),
        source_hash: trace.source_hash.clone().unwrap_or_default(),
        pass: details.is_empty(),
        details,
        compared_receipts,
        replay_oracle,
    }
}

fn replay_oracle_for_shadow(
    candidate: &WorkflowCandidate,
    trace: &CrystallizationTrace,
) -> (Option<ReplayOracleReport>, usize) {
    let Some(first_run) = trace.replay_run.as_ref() else {
        return (None, 0);
    };
    if first_run.effect_receipts.is_empty() && candidate.expected_receipts.is_empty() {
        return (None, 0);
    }
    let mut second_run = first_run.clone();
    second_run.run_id = format!("shadow_{}_{}", candidate.id, trace.id);
    second_run.effect_receipts = candidate.expected_receipts.clone();
    let compared_receipts = first_run
        .effect_receipts
        .len()
        .max(second_run.effect_receipts.len());
    let oracle_trace = ReplayOracleTrace {
        name: format!("{}_shadow_receipts_{}", candidate.name, trace.id),
        description: Some(
            "crystallization shadow receipt comparison using the replay oracle".to_string(),
        ),
        expect: ReplayExpectation::Match,
        allowlist: trace.replay_allowlist.clone(),
        first_run: first_run.clone(),
        second_run,
        ..ReplayOracleTrace::default()
    };
    let oracle_name = oracle_trace.name.clone();
    let second_run_counts = oracle_trace.second_run.counts();
    let report = match run_replay_oracle_trace(&oracle_trace) {
        Ok(report) => report,
        Err(error) => ReplayOracleReport {
            name: oracle_name,
            expectation: ReplayExpectation::Match,
            passed: false,
            first_run_counts: first_run.counts(),
            second_run_counts,
            protocol_fixture_refs: Vec::new(),
            divergence: Some(super::super::ReplayDivergence {
                path: "/".to_string(),
                left: JsonValue::Null,
                right: JsonValue::Null,
                message: error.to_string(),
            }),
        },
    };
    (Some(report), compared_receipts)
}

pub(super) fn estimate_savings(
    traces: &[CrystallizationTrace],
    examples: &[(usize, usize)],
    steps: &[WorkflowCandidateStep],
) -> SavingsEstimate {
    let mut estimate = SavingsEstimate::default();
    for (trace_index, start_index) in examples {
        let trace = &traces[*trace_index];
        for action in &trace.actions[*start_index..*start_index + steps.len()] {
            if action.kind == "model_call" || action.fuzzy.unwrap_or(false) {
                estimate.remaining_model_calls += action.cost.model_calls.max(1);
            } else {
                estimate.model_calls_avoided += action.cost.model_calls;
                estimate.input_tokens_avoided += action.cost.input_tokens;
                estimate.output_tokens_avoided += action.cost.output_tokens;
                estimate.estimated_cost_usd_avoided += action.cost.total_cost_usd;
                estimate.wall_ms_avoided +=
                    action.cost.wall_ms + action.duration_ms.unwrap_or_default();
            }
        }
    }
    estimate.cpu_runtime_cost_usd = 0.0;
    estimate
}

pub(super) fn confidence_for(
    examples: &[(usize, usize)],
    trace_count: usize,
    steps: &[WorkflowCandidateStep],
    safe: bool,
) -> f64 {
    if !safe || trace_count == 0 {
        return 0.0;
    }
    let coverage = examples.len() as f64 / trace_count as f64;
    let deterministic = steps
        .iter()
        .filter(|step| step.segment == SegmentKind::Deterministic)
        .count() as f64
        / steps.len().max(1) as f64;
    coverage.mul_add(0.65, deterministic * 0.35).min(0.99)
}

pub(super) fn infer_workflow_name(steps: &[WorkflowCandidateStep]) -> String {
    let names = steps
        .iter()
        .map(|step| step.name.to_ascii_lowercase())
        .collect::<Vec<_>>()
        .join("_");
    if names.contains("version") || names.contains("release") {
        "crystallized_version_bump".to_string()
    } else {
        "crystallized_workflow".to_string()
    }
}
