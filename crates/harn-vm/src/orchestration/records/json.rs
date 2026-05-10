//! JSON helpers used while normalizing run records, planner-round summaries, and clarifying-question
//! eval specs.

use super::types::{
    ClarifyingQuestionEvalSpec, RunDeliverableSummaryRecord, RunStageRecord,
    RunTaskLedgerSummaryRecord,
};

pub(super) fn compact_json_value(value: &serde_json::Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| value.to_string())
}

pub(super) fn normalize_question_text(text: &str) -> String {
    text.chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch.is_whitespace() {
                ch.to_ascii_lowercase()
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

pub(super) fn clarifying_min_questions(spec: &ClarifyingQuestionEvalSpec) -> usize {
    spec.min_questions.max(1)
}

pub(super) fn clarifying_max_questions(spec: &ClarifyingQuestionEvalSpec) -> usize {
    spec.max_questions.unwrap_or(1).max(1)
}

pub(super) fn json_string_array(value: Option<&serde_json::Value>) -> Vec<String> {
    value
        .and_then(|value| value.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_string))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

pub(super) fn json_usize(value: Option<&serde_json::Value>) -> usize {
    value.and_then(|value| value.as_u64()).unwrap_or_default() as usize
}

pub(super) fn json_bool(value: Option<&serde_json::Value>) -> Option<bool> {
    value.and_then(|value| value.as_bool())
}

pub(super) fn stage_result_payload(stage: &RunStageRecord) -> Option<&serde_json::Value> {
    stage
        .artifacts
        .iter()
        .find_map(|artifact| artifact.data.as_ref())
}

pub(super) fn task_ledger_summary_from_value(
    value: &serde_json::Value,
) -> Option<RunTaskLedgerSummaryRecord> {
    let deliverables = value
        .get("deliverables")
        .and_then(|raw| raw.as_array())
        .map(|items| {
            items
                .iter()
                .map(|item| RunDeliverableSummaryRecord {
                    id: item
                        .get("id")
                        .and_then(|value| value.as_str())
                        .unwrap_or_default()
                        .to_string(),
                    text: item
                        .get("text")
                        .and_then(|value| value.as_str())
                        .unwrap_or_default()
                        .to_string(),
                    status: item
                        .get("status")
                        .and_then(|value| value.as_str())
                        .unwrap_or_default()
                        .to_string(),
                    note: item
                        .get("note")
                        .and_then(|value| value.as_str())
                        .map(str::to_string),
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let observations = json_string_array(value.get("observations"));
    let root_task = value
        .get("root_task")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_string();
    let rationale = value
        .get("rationale")
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_string();
    if root_task.is_empty()
        && rationale.is_empty()
        && deliverables.is_empty()
        && observations.is_empty()
    {
        return None;
    }
    let blocking_count = deliverables
        .iter()
        .filter(|deliverable| matches!(deliverable.status.as_str(), "open" | "blocked"))
        .count();
    Some(RunTaskLedgerSummaryRecord {
        root_task,
        rationale,
        deliverables,
        observations,
        blocking_count,
    })
}
