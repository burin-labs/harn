use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;
use serde_json::{Map, Value};

pub(super) const REQUIRED_BEHAVIOR_STRATA: &[&str] = &[
    "valid_tool_call",
    "parallel_tool_call",
    "no_tool_answer",
    "unavailable_tool_repair",
    "multi_turn_continuation",
];

#[derive(Default, Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum BehaviorStrataPolicy {
    #[default]
    Strict,
    LegacyUnclassified,
}

#[derive(Default, Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum BehaviorStrataStatus {
    Covered,
    #[default]
    Incomplete,
    LegacyUnclassified,
}

#[derive(Default, Debug, Serialize)]
pub(super) struct BehaviorStrataReport {
    pub(super) policy: BehaviorStrataPolicy,
    pub(super) status: BehaviorStrataStatus,
    pub(super) required: Vec<String>,
    pub(super) source: BTreeMap<String, u64>,
    pub(super) emitted: BTreeMap<String, u64>,
    pub(super) missing_required: Vec<String>,
    pub(super) unclassified_record_ids: Vec<String>,
}

pub(super) fn record_behavior_classes(
    record: &Map<String, Value>,
) -> Result<BTreeSet<String>, String> {
    let metadata = record.get("metadata").and_then(Value::as_object);
    let mut classes = BTreeSet::new();
    for value in [
        record.get("behavior_class"),
        record.get("behavior_classes"),
        metadata.and_then(|metadata| metadata.get("behavior_class")),
        metadata.and_then(|metadata| metadata.get("behavior_classes")),
    ]
    .into_iter()
    .flatten()
    {
        collect_behavior_classes(value, &mut classes)?;
    }
    Ok(classes)
}

pub(super) fn should_emit_no_tool_completion(behavior_classes: &BTreeSet<String>) -> bool {
    behavior_classes.contains("no_tool_answer")
        || behavior_classes.contains("unavailable_tool_repair")
}

pub(super) fn no_tool_row_behavior_classes(
    behavior_classes: &BTreeSet<String>,
) -> BTreeSet<String> {
    behavior_classes
        .iter()
        .filter(|class| matches!(class.as_str(), "no_tool_answer" | "unavailable_tool_repair"))
        .cloned()
        .collect()
}

pub(super) fn text_tool_row_behavior_classes(
    block_count: usize,
    behavior_classes: &BTreeSet<String>,
    has_prior_tool_turn: bool,
) -> BTreeSet<String> {
    let mut classes = BTreeSet::new();
    if block_count > 0 {
        classes.insert("valid_tool_call".to_string());
    }
    if block_count > 1 && behavior_classes.contains("parallel_tool_call") {
        classes.insert("parallel_tool_call".to_string());
    }
    if has_prior_tool_turn && behavior_classes.contains("multi_turn_continuation") {
        classes.insert("multi_turn_continuation".to_string());
    }
    classes
}

pub(super) fn increment_counts(counts: &mut BTreeMap<String, u64>, classes: &BTreeSet<String>) {
    for class in classes {
        *counts.entry(class.clone()).or_insert(0) += 1;
    }
}

pub(super) fn behavior_strata_report(
    source: BTreeMap<String, u64>,
    emitted: BTreeMap<String, u64>,
    policy: BehaviorStrataPolicy,
    unclassified_record_ids: Vec<String>,
) -> BehaviorStrataReport {
    let missing_required = REQUIRED_BEHAVIOR_STRATA
        .iter()
        .filter(|class| emitted.get(**class).copied().unwrap_or(0) == 0)
        .map(|class| (*class).to_string())
        .collect::<Vec<_>>();
    let status = if missing_required.is_empty() {
        BehaviorStrataStatus::Covered
    } else if policy == BehaviorStrataPolicy::LegacyUnclassified
        && source.is_empty()
        && !unclassified_record_ids.is_empty()
    {
        BehaviorStrataStatus::LegacyUnclassified
    } else {
        BehaviorStrataStatus::Incomplete
    };
    BehaviorStrataReport {
        policy,
        status,
        required: REQUIRED_BEHAVIOR_STRATA
            .iter()
            .map(|class| (*class).to_string())
            .collect(),
        source,
        emitted,
        missing_required,
        unclassified_record_ids,
    }
}

fn collect_behavior_classes(value: &Value, classes: &mut BTreeSet<String>) -> Result<(), String> {
    if let Some(text) = value.as_str() {
        classes.insert(canonical_behavior_class(text)?);
        return Ok(());
    }
    if let Some(items) = value.as_array() {
        for item in items {
            let Some(text) = item.as_str() else {
                return Err("behavior_classes entries must be strings".to_string());
            };
            classes.insert(canonical_behavior_class(text)?);
        }
        return Ok(());
    }
    Err("behavior_class must be a string or behavior_classes must be a list".to_string())
}

fn canonical_behavior_class(raw: &str) -> Result<String, String> {
    let value = raw.trim().to_ascii_lowercase().replace('-', "_");
    let canonical = match value.as_str() {
        "valid_tool_call" | "sequential_tool_call" => "valid_tool_call",
        "parallel_tool_call" | "parallel_tool_calls" => "parallel_tool_call",
        "no_tool_answer" | "no_tool" | "direct_answer" => "no_tool_answer",
        "unavailable_tool_repair" | "unknown_tool_repair" => "unavailable_tool_repair",
        "multi_turn_continuation" | "multi_turn_tool_result_continuation" => {
            "multi_turn_continuation"
        }
        "" => return Err("behavior_class must not be empty".to_string()),
        _ => return Err(format!("unsupported behavior_class `{raw}`")),
    };
    Ok(canonical.to_string())
}
