use std::collections::{HashMap, HashSet};

/// Results, including recovery placeholders, echo the paired member namespace.
/// Pair by call id so a same-named ordinary function cannot inherit a toolset.
pub(super) fn echo_toolset_names(messages: &mut [serde_json::Value]) {
    let namespaces: HashMap<String, String> = messages
        .iter()
        .filter_map(|message| message.get("content")?.as_array())
        .flatten()
        .filter(|block| block["type"] == "tool_use")
        .filter_map(|block| {
            Some((
                block["id"].as_str()?.to_owned(),
                block["toolset_name"].as_str()?.to_owned(),
            ))
        })
        .collect();
    for message in messages {
        let Some(blocks) = message
            .get_mut("content")
            .and_then(serde_json::Value::as_array_mut)
        else {
            continue;
        };
        for block in blocks {
            if block["type"] == "tool_result" {
                if let Some(namespace) = block["tool_use_id"]
                    .as_str()
                    .and_then(|id| namespaces.get(id))
                {
                    block["toolset_name"] = serde_json::json!(namespace);
                }
            }
        }
    }
}

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
}
