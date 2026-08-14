use std::collections::{HashMap, HashSet};

/// Anthropic restricts both `tool_use.id` and the matching
/// `tool_result.tool_use_id` to ASCII letters, digits, `_`, and `-`. Durable
/// transcripts can originate in another provider or Harn subsystem whose id
/// vocabulary also permits separators such as `:`. Rewrite those ids as a pair
/// at the Anthropic egress boundary so persistence remains provider-neutral.
pub(super) fn normalize_tool_call_ids(messages: &mut [serde_json::Value]) {
    let mut replacements = HashMap::<String, String>::new();
    let mut claimed_ids = messages
        .iter()
        .filter_map(|message| message.get("content")?.as_array())
        .flatten()
        .filter_map(
            |block| match block.get("type").and_then(serde_json::Value::as_str) {
                Some("tool_use") => block.get("id"),
                Some("tool_result") => block.get("tool_use_id"),
                _ => None,
            },
        )
        .filter_map(serde_json::Value::as_str)
        .filter(|raw| {
            !raw.is_empty()
                && raw
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
        })
        .map(ToString::to_string)
        .collect::<HashSet<_>>();
    let mut next_empty = 0usize;
    let mut next_collision = 0usize;
    for message in messages {
        let Some(blocks) = message
            .get_mut("content")
            .and_then(serde_json::Value::as_array_mut)
        else {
            continue;
        };
        for block in blocks {
            let key = match block.get("type").and_then(serde_json::Value::as_str) {
                Some("tool_use") => "id",
                Some("tool_result") => "tool_use_id",
                _ => continue,
            };
            let Some(raw) = block.get(key).and_then(serde_json::Value::as_str) else {
                continue;
            };
            if !raw.is_empty()
                && raw
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
            {
                continue;
            }
            let normalized = if let Some(existing) = replacements.get(raw) {
                existing.clone()
            } else {
                let mut candidate = raw
                    .chars()
                    .map(|ch| {
                        if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
                            ch
                        } else {
                            '_'
                        }
                    })
                    .collect::<String>();
                if candidate.is_empty() {
                    candidate = format!("harn_tool_{next_empty}");
                    next_empty += 1;
                }
                let base = candidate.clone();
                while claimed_ids.contains(&candidate) {
                    candidate = format!("{base}_harn_{next_collision}");
                    next_collision += 1;
                }
                claimed_ids.insert(candidate.clone());
                replacements.insert(raw.to_string(), candidate.clone());
                candidate
            };
            block[key] = serde_json::Value::String(normalized);
        }
    }
}

/// Preserve a durable tool-result observation whose call envelope was removed
/// by compaction, interruption, or a legacy recorder. Anthropic requires every
/// `tool_result` to pair with the immediately preceding assistant message, so
/// an orphan becomes text rather than a fabricated call or discarded evidence.
pub(super) fn preserve_orphan_results_as_text(messages: &mut [serde_json::Value]) {
    let mut preceding_tool_use_ids = HashSet::<String>::new();
    for message in messages {
        let role = message
            .get("role")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        if role == "assistant" {
            preceding_tool_use_ids = assistant_tool_use_ids(message).unwrap_or_default();
            continue;
        }
        if role != "user" {
            preceding_tool_use_ids.clear();
            continue;
        }
        let Some(blocks) = message
            .get_mut("content")
            .and_then(serde_json::Value::as_array_mut)
        else {
            preceding_tool_use_ids.clear();
            continue;
        };
        let mut saw_tool_result = false;
        for block in blocks {
            if block.get("type").and_then(serde_json::Value::as_str) != Some("tool_result") {
                continue;
            }
            saw_tool_result = true;
            let id = block
                .get("tool_use_id")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            if preceding_tool_use_ids.remove(id) {
                continue;
            }
            let content = block
                .get("content")
                .cloned()
                .unwrap_or(serde_json::Value::String(String::new()));
            let text = content
                .as_str()
                .map(ToString::to_string)
                .unwrap_or_else(|| {
                    serde_json::to_string(&content)
                        .unwrap_or_else(|_| "result unavailable".to_string())
                });
            *block = serde_json::json!({
                "type": "text",
                "text": format!("[unpaired durable tool result]\n{text}"),
            });
        }
        // A recorder may persist results for one assistant turn as multiple
        // consecutive user messages. Keep only the still-unmatched ids across
        // those messages; an ordinary user turn closes the result envelope.
        if !saw_tool_result {
            preceding_tool_use_ids.clear();
        }
    }
}

pub(super) fn assistant_tool_use_ids(message: &serde_json::Value) -> Option<HashSet<String>> {
    if message.get("role").and_then(serde_json::Value::as_str) != Some("assistant") {
        return None;
    }
    let ids: HashSet<String> = message
        .get("content")?
        .as_array()?
        .iter()
        .filter(|block| block.get("type").and_then(serde_json::Value::as_str) == Some("tool_use"))
        .filter_map(|block| {
            block
                .get("id")
                .and_then(serde_json::Value::as_str)
                .map(ToString::to_string)
        })
        .collect();
    (!ids.is_empty()).then_some(ids)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durable_tool_ids_are_normalized_as_a_pair() {
        let mut messages = vec![
            serde_json::json!({
                "role": "assistant",
                "content": [{"type": "tool_use", "id": "run:42/tool:7"}],
            }),
            serde_json::json!({
                "role": "user",
                "content": [{"type": "tool_result", "tool_use_id": "run:42/tool:7"}],
            }),
        ];

        normalize_tool_call_ids(&mut messages);

        assert_eq!(messages[0]["content"][0]["id"], "run_42_tool_7");
        assert_eq!(messages[1]["content"][0]["tool_use_id"], "run_42_tool_7");
    }

    #[test]
    fn normalized_ids_do_not_collide_with_other_durable_calls() {
        let mut messages = vec![
            serde_json::json!({
                "role": "assistant",
                "content": [
                    {"type": "tool_use", "id": "run:42"},
                    {"type": "tool_use", "id": "run/42"},
                    {"type": "tool_use", "id": "run_42"},
                ],
            }),
            serde_json::json!({
                "role": "user",
                "content": [
                    {"type": "tool_result", "tool_use_id": "run:42"},
                    {"type": "tool_result", "tool_use_id": "run/42"},
                    {"type": "tool_result", "tool_use_id": "run_42"},
                ],
            }),
        ];

        normalize_tool_call_ids(&mut messages);

        let tool_uses = messages[0]["content"].as_array().expect("assistant blocks");
        let tool_use_ids = tool_uses
            .iter()
            .map(|block| block["id"].as_str().expect("tool id"))
            .collect::<HashSet<_>>();
        assert_eq!(tool_use_ids.len(), 3);
        let tool_results = messages[1]["content"].as_array().expect("result blocks");
        for (tool_use, tool_result) in tool_uses.iter().zip(tool_results) {
            assert_eq!(tool_use["id"], tool_result["tool_use_id"]);
        }
    }

    #[test]
    fn orphaned_durable_tool_result_is_preserved_as_text() {
        let mut messages = vec![
            serde_json::json!({
                "role": "assistant",
                "content": [{"type": "tool_use", "id": "toolu_valid"}],
            }),
            serde_json::json!({
                "role": "user",
                "content": [
                    {"type": "tool_result", "tool_use_id": "toolu_valid", "content": "valid"},
                    {"type": "tool_result", "tool_use_id": "", "content": "legacy observation"},
                ],
            }),
        ];

        preserve_orphan_results_as_text(&mut messages);

        assert_eq!(messages[1]["content"][0]["type"], "tool_result");
        assert_eq!(messages[1]["content"][1]["type"], "text");
        assert!(messages[1]["content"][1]["text"]
            .as_str()
            .expect("preserved text")
            .contains("legacy observation"));
    }

    #[test]
    fn matching_results_may_span_consecutive_user_messages() {
        let mut messages = vec![
            serde_json::json!({
                "role": "assistant",
                "content": [
                    {"type": "tool_use", "id": "toolu_first"},
                    {"type": "tool_use", "id": "toolu_second"},
                ],
            }),
            serde_json::json!({
                "role": "user",
                "content": [{"type": "tool_result", "tool_use_id": "toolu_first", "content": "one"}],
            }),
            serde_json::json!({
                "role": "user",
                "content": [{"type": "tool_result", "tool_use_id": "toolu_second", "content": "two"}],
            }),
        ];

        preserve_orphan_results_as_text(&mut messages);

        assert_eq!(messages[1]["content"][0]["type"], "tool_result");
        assert_eq!(messages[2]["content"][0]["type"], "tool_result");
    }
}
