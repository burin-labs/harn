//! Small generic helpers shared across crystallization submodules.

use std::collections::BTreeSet;

use serde_json::Value as JsonValue;
use sha2::{Digest, Sha256};

use super::types::{CrystallizationSideEffect, WorkflowCandidateExample};

pub(super) fn stable_candidate_id(
    sequence: &[String],
    examples: &[WorkflowCandidateExample],
) -> String {
    let mut hasher = Sha256::new();
    for item in sequence {
        hasher.update(item.as_bytes());
        hasher.update([0]);
    }
    for example in examples {
        hasher.update(example.source_hash.as_bytes());
        hasher.update([0]);
    }
    format!("candidate_{}", hex_prefix(hasher.finalize().as_slice(), 16))
}

pub(super) fn hash_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("sha256:{}", hex::encode(hasher.finalize()))
}

pub(super) fn hex_prefix(bytes: &[u8], chars: usize) -> String {
    hex::encode(bytes).chars().take(chars).collect::<String>()
}

pub(super) fn sorted_strings(items: impl Iterator<Item = String>) -> Vec<String> {
    let mut set = items.collect::<BTreeSet<_>>();
    set.retain(|item| !item.trim().is_empty());
    set.into_iter().collect()
}

pub(super) fn sorted_side_effects(
    items: Vec<CrystallizationSideEffect>,
) -> Vec<CrystallizationSideEffect> {
    let mut items = items
        .into_iter()
        .filter(|item| !item.kind.trim().is_empty() || !item.target.trim().is_empty())
        .collect::<Vec<_>>();
    items.sort_by_key(side_effect_sort_key);
    items.dedup_by(|left, right| side_effect_sort_key(left) == side_effect_sort_key(right));
    items
}

pub(super) fn side_effect_sort_key(item: &CrystallizationSideEffect) -> String {
    format!(
        "{}\x1f{}\x1f{}\x1f{}",
        item.kind,
        item.target,
        item.capability.clone().unwrap_or_default(),
        item.mutation.clone().unwrap_or_default()
    )
}

pub(super) fn is_scalar(value: &JsonValue) -> bool {
    matches!(
        value,
        JsonValue::String(_) | JsonValue::Number(_) | JsonValue::Bool(_) | JsonValue::Null
    )
}

pub(super) fn json_scalar_string(value: &JsonValue) -> String {
    match value {
        JsonValue::String(value) => value.clone(),
        other => other.to_string(),
    }
}

pub(super) fn sanitize_identifier(raw: &str) -> String {
    let mut out = String::new();
    for (idx, ch) in raw.chars().enumerate() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            if idx == 0 && ch.is_ascii_digit() {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
        } else if !out.ends_with('_') {
            out.push('_');
        }
    }
    let trimmed = out.trim_matches('_').to_string();
    if trimmed.is_empty() {
        "param".to_string()
    } else {
        trimmed
    }
}

pub(super) fn escape_harn_string(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}
