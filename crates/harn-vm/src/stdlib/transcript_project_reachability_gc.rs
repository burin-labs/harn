use super::*;
use std::collections::BTreeSet;

fn push_unique_usize(values: &mut Vec<usize>, value: usize) {
    if !values.contains(&value) {
        values.push(value);
    }
}

/// `reachability_gc`: keep the raw transcript intact, but replace stale
/// model-visible tool-result bodies with compact audit pointers when no
/// current roots reference their identifiers. Recent messages, explicit
/// caller roots, and write-barrier refs form the root set.
pub(super) fn project(raw: &[JsonValue], policy: &ProjectionPolicy) -> ProjectionDecision {
    if policy.gc_require_write_barrier && !policy.gc_has_write_barrier {
        let mut decision = project_raw(raw);
        decision.reason = "reachability_gc_write_barrier_missing".to_string();
        decision.root_labels = reachability_root_labels(raw, policy);
        return decision;
    }

    let root_start = raw.len().saturating_sub(policy.gc_root_window);
    let roots = ReachabilityRoots::collect(raw, root_start, policy);
    let root_labels = reachability_root_labels(raw, policy);
    let mut messages = raw.to_vec();
    let mut redacted_indices = Vec::new();
    let mut redaction_pointers = Vec::new();
    let mut reclaimed_chars = 0usize;

    for idx in 0..root_start {
        for candidate in tool_result_candidates(idx, &raw[idx]) {
            let content_chars = candidate.content.chars().count();
            if content_chars < policy.gc_min_chars || candidate.is_error {
                continue;
            }
            let identifiers = tool_result_reachability_identifiers(raw, &candidate);
            if roots.references_any(&identifiers) {
                continue;
            }
            let pointer = redaction_pointer(&candidate);
            let replacement = redacted_tool_result_body(&pointer);
            messages[idx] =
                redact_tool_result_message(&messages[idx], &candidate, &replacement, &pointer);
            reclaimed_chars += content_chars.saturating_sub(replacement.chars().count());
            push_unique_usize(&mut redacted_indices, idx);
            redaction_pointers.push(pointer);
        }
    }

    let reclaimed_tokens = reclaimed_chars / 4;
    ProjectionDecision {
        messages,
        kept_indices: (0..raw.len()).collect(),
        dropped_indices: Vec::new(),
        redacted_indices,
        reclaimed_tokens,
        reclaimed_chars,
        redaction_pointers,
        root_labels,
        reason: "reachability_gc".to_string(),
    }
}

#[derive(Debug, Default)]
struct ReachabilityRoots {
    references: BTreeSet<String>,
}

impl ReachabilityRoots {
    fn collect(raw: &[JsonValue], root_start: usize, policy: &ProjectionPolicy) -> Self {
        let mut roots = Self::default();
        for text in &policy.gc_root_texts {
            collect_root_tokens(text, &mut roots.references);
        }
        for message in raw.iter().skip(root_start) {
            collect_root_references(message, &mut roots.references);
        }
        roots
    }

    fn references_any(&self, identifiers: &[String]) -> bool {
        identifiers
            .iter()
            .any(|identifier| self.references.contains(identifier))
    }
}

fn reachability_root_labels(raw: &[JsonValue], policy: &ProjectionPolicy) -> Vec<String> {
    let mut labels = Vec::new();
    if policy.gc_root_window > 0 && !raw.is_empty() {
        push_unique(
            &mut labels,
            format!("last_{}_messages", policy.gc_root_window.min(raw.len())),
        );
    }
    for label in &policy.gc_root_labels {
        push_unique(&mut labels, label.clone());
    }
    if policy.gc_require_write_barrier {
        push_unique(&mut labels, "write_barrier_required".to_string());
    }
    labels
}

fn tool_result_reachability_identifiers(
    raw: &[JsonValue],
    candidate: &ToolResultCandidate,
) -> Vec<String> {
    let mut identifiers = Vec::new();
    collect_identifiers_from_str(&candidate.content, &mut identifiers);
    collect_identifiers_from_json(&candidate.node, &mut identifiers);
    let result_call = ToolCallInfo {
        tool_call_id: candidate.tool_call_id.clone(),
        tool_name: candidate.tool_name.clone(),
        node: None,
    };
    let mut matched_call = false;
    for previous in raw[..candidate.message_idx].iter().rev() {
        if previous.get("role").and_then(JsonValue::as_str) != Some("assistant") {
            continue;
        }
        for call in extract_tool_calls(previous) {
            if !tool_calls_correlate(&call, &result_call) {
                continue;
            }
            if let Some(node) = call.node {
                collect_identifiers_from_json(&node, &mut identifiers);
            }
            matched_call = true;
            break;
        }
        if matched_call {
            break;
        }
    }
    identifiers.sort();
    identifiers.dedup();
    identifiers
}

fn tool_calls_correlate(call: &ToolCallInfo, result: &ToolCallInfo) -> bool {
    if let (Some(left), Some(right)) = (&call.tool_call_id, &result.tool_call_id) {
        return left == right;
    }
    if let (Some(left), Some(right)) = (&call.tool_name, &result.tool_name) {
        return left == right;
    }
    false
}

fn collect_identifiers_from_json(value: &JsonValue, out: &mut Vec<String>) {
    collect_candidate_references(value, None, out);
}

fn collect_candidate_references(value: &JsonValue, field: Option<&str>, out: &mut Vec<String>) {
    match value {
        JsonValue::String(text) if field.is_some_and(is_reference_field) => {
            push_reference_value(text, out);
        }
        JsonValue::String(text) => collect_identifiers_from_str(text, out),
        JsonValue::Number(number) if field.is_some_and(is_reference_field) => {
            out.push(number.to_string());
        }
        JsonValue::Array(items) => {
            for item in items {
                collect_candidate_references(item, field, out);
            }
        }
        JsonValue::Object(map) => {
            for (key, value) in map {
                if key == "_harn" {
                    collect_harn_metadata_identifiers(value, out);
                } else {
                    collect_candidate_references(value, Some(key), out);
                }
            }
        }
        _ => {}
    }
}

fn collect_harn_metadata_identifiers(value: &JsonValue, out: &mut Vec<String>) {
    let Some(map) = value.as_object() else {
        return;
    };
    for (key, value) in map {
        // Lifecycle vocabulary describes Harn's envelope, not the work. Treating
        // `_harn.kind = "tool_result"` as a semantic identifier makes every
        // result with the same envelope keep every older result reachable.
        if !matches!(
            key.as_str(),
            "role" | "type" | "kind" | "status" | "outcome" | "schema"
        ) {
            collect_candidate_references(value, Some(key), out);
        }
    }
}

fn collect_root_references(value: &JsonValue, out: &mut BTreeSet<String>) {
    match value {
        JsonValue::String(text) => collect_root_tokens(text, out),
        JsonValue::Number(number) => {
            out.insert(number.to_string());
        }
        JsonValue::Array(items) => {
            for item in items {
                collect_root_references(item, out);
            }
        }
        JsonValue::Object(map) => {
            for value in map.values() {
                collect_root_references(value, out);
            }
        }
        _ => {}
    }
}

fn collect_root_tokens(text: &str, out: &mut BTreeSet<String>) {
    for token in reference_tokens(text) {
        out.insert(token.to_string());
    }
}

fn push_reference_value(text: &str, out: &mut Vec<String>) {
    let value = text.trim();
    if !value.is_empty() {
        out.push(value.to_string());
    }
    for token in reference_tokens(text) {
        out.push(token.to_string());
    }
}

fn is_reference_field(field: &str) -> bool {
    matches!(
        field,
        "id" | "ids"
            | "number"
            | "path"
            | "paths"
            | "symbol"
            | "symbols"
            | "sha"
            | "hash"
            | "commit"
            | "revision"
            | "ref"
            | "reference"
            | "name"
            | "kind"
    ) || field.ends_with("_id")
        || field.ends_with("_ids")
        || field.ends_with("_number")
        || field.ends_with("_path")
        || field.ends_with("_paths")
        || field.ends_with("_symbol")
        || field.ends_with("_symbols")
        || field.ends_with("_sha")
        || field.ends_with("_hash")
        || field.ends_with("_ref")
}

fn reference_tokens(text: &str) -> impl Iterator<Item = &str> {
    text.split(|ch: char| {
        !(ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.' | '/' | '\\' | ':' | '#'))
    })
    .map(|token| {
        token.trim_matches(|ch: char| matches!(ch, ':' | '.' | ',' | ';' | '"' | '\'' | '`'))
    })
    .filter(|token| !token.is_empty())
}

fn collect_identifiers_from_str(text: &str, out: &mut Vec<String>) {
    for token in reference_tokens(text) {
        if is_reachability_identifier(token) {
            out.push(token.to_string());
        }
    }
}

fn is_reachability_identifier(token: &str) -> bool {
    if token.len() < 3 {
        return false;
    }
    if token
        .strip_prefix('#')
        .is_some_and(|number| !number.is_empty() && number.chars().all(|c| c.is_ascii_digit()))
    {
        return true;
    }
    if token.contains('/') || token.contains('\\') || token.contains("::") {
        return true;
    }
    if is_git_object_id(token) || is_uuid(token) {
        return true;
    }
    if token.rsplit_once('.').is_some_and(|(_, ext)| {
        (1..=8).contains(&ext.len()) && ext.chars().all(|c| c.is_ascii_alphanumeric())
    }) {
        return true;
    }
    if token.len() >= 6 && token.contains('_') {
        return true;
    }
    let has_lower = token.chars().any(|c| c.is_ascii_lowercase());
    let has_upper = token.chars().any(|c| c.is_ascii_uppercase());
    has_lower && has_upper && token.len() >= 4
}

fn is_git_object_id(token: &str) -> bool {
    (7..=64).contains(&token.len()) && token.chars().all(|c| c.is_ascii_hexdigit())
}

fn is_uuid(token: &str) -> bool {
    let segment_lengths = [8, 4, 4, 4, 12];
    let mut segments = token.split('-');
    segment_lengths.into_iter().all(|expected_len| {
        segments.next().is_some_and(|segment| {
            segment.len() == expected_len && segment.chars().all(|c| c.is_ascii_hexdigit())
        })
    }) && segments.next().is_none()
}

fn redact_tool_result_message(
    message: &JsonValue,
    candidate: &ToolResultCandidate,
    replacement: &str,
    pointer: &JsonValue,
) -> JsonValue {
    let mut projected = message.clone();
    let marker = serde_json::json!({
        "redacted": true,
        "policy": "reachability_gc",
        "redaction_pointer": pointer,
    });
    if let Some(map) = projected.as_object_mut() {
        if let Some(block_idx) = candidate.block_idx {
            if let Some(blocks) = map.get_mut("content").and_then(JsonValue::as_array_mut) {
                if let Some(block) = blocks.get_mut(block_idx) {
                    block["content"] = JsonValue::String(replacement.to_string());
                    block["_harn_projection"] = marker;
                }
            }
            append_redaction_pointer(map, pointer);
        } else {
            map.insert(
                "content".to_string(),
                JsonValue::String(replacement.to_string()),
            );
            map.insert("_harn_projection".to_string(), marker);
        }
    }
    projected
}

fn append_redaction_pointer(map: &mut serde_json::Map<String, JsonValue>, pointer: &JsonValue) {
    let entry = map
        .entry("_harn_projection".to_string())
        .or_insert_with(|| {
            serde_json::json!({
                "redacted": true,
                "policy": "reachability_gc",
                "redaction_pointers": [],
            })
        });
    if !entry.is_object() {
        *entry = serde_json::json!({
            "redacted": true,
            "policy": "reachability_gc",
            "redaction_pointers": [],
        });
    }
    if let Some(obj) = entry.as_object_mut() {
        obj.insert("redacted".to_string(), JsonValue::Bool(true));
        obj.insert(
            "policy".to_string(),
            JsonValue::String("reachability_gc".to_string()),
        );
        obj.entry("redaction_pointers".to_string())
            .or_insert_with(|| JsonValue::Array(Vec::new()));
        if let Some(pointers) = obj
            .get_mut("redaction_pointers")
            .and_then(JsonValue::as_array_mut)
        {
            pointers.push(pointer.clone());
        }
    }
}

fn redaction_pointer(candidate: &ToolResultCandidate) -> JsonValue {
    let content_chars = candidate.content.chars().count();
    let source = match candidate.block_idx {
        Some(block_idx) => format!(
            "transcript.messages[{}].content[{block_idx}].content",
            candidate.message_idx
        ),
        None => format!("transcript.messages[{}].content", candidate.message_idx),
    };
    serde_json::json!({
        "policy": "reachability_gc",
        "source": source,
        "source_index": candidate.message_idx,
        "source_block_index": candidate.block_idx,
        "content_hash": hash_string(&candidate.content),
        "content_chars": content_chars,
        "estimated_tokens_reclaimed": content_chars / 4,
        "tool_call_id": candidate.tool_call_id.clone(),
        "tool_name": candidate.tool_name.clone(),
        "reason": "stale_tool_result_unreachable",
    })
}

fn redacted_tool_result_body(pointer: &JsonValue) -> String {
    let source_index = pointer
        .get("source_index")
        .and_then(JsonValue::as_u64)
        .unwrap_or_default();
    let content_hash = pointer
        .get("content_hash")
        .and_then(JsonValue::as_str)
        .unwrap_or("sha256:");
    let estimated = pointer
        .get("estimated_tokens_reclaimed")
        .and_then(JsonValue::as_u64)
        .unwrap_or_default();
    format!(
        "[tool result reclaimed by reachability_gc; source_index={source_index}; content_hash={content_hash}; estimated_tokens_reclaimed={estimated}. Raw content remains in the transcript audit trail.]"
    )
}
