//! Completed semantic-block list for streamed LLM responses.
//!
//! Live delta callbacks stay per-fragment. This owner only merges adjacent
//! same-type / same-visibility text into the finished `blocks` list. Tool
//! calls and provider-signed reasoning keep their existing boundaries.

/// Append a streamed text fragment to the completed block list.
///
/// Adjacent fragments that share `type` and `visibility` and carry only a
/// `text` payload merge in place. Identity-bearing blocks (tool calls,
/// signed thinking) are never extended.
pub(super) fn append_coalesced_text_block(
    blocks: &mut Vec<serde_json::Value>,
    block_type: &str,
    text: &str,
    visibility: &str,
) {
    if text.is_empty() {
        return;
    }
    if let Some(last) = blocks.last_mut() {
        if can_coalesce_text_block(last, block_type, visibility) {
            let existing = last
                .get("text")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            let mut merged = String::with_capacity(existing.len() + text.len());
            merged.push_str(existing);
            merged.push_str(text);
            last["text"] = serde_json::Value::String(merged);
            return;
        }
    }
    blocks.push(serde_json::json!({
        "type": block_type,
        "text": text,
        "visibility": visibility,
    }));
}

fn can_coalesce_text_block(block: &serde_json::Value, block_type: &str, visibility: &str) -> bool {
    if block.get("type").and_then(serde_json::Value::as_str) != Some(block_type) {
        return false;
    }
    if block.get("visibility").and_then(serde_json::Value::as_str) != Some(visibility) {
        return false;
    }
    if block
        .get("text")
        .and_then(serde_json::Value::as_str)
        .is_none()
    {
        return false;
    }
    // Provider-signed / identity-bearing blocks keep their boundaries even
    // when type and visibility happen to match a later fragment.
    block.get("signature").is_none()
        && block.get("id").is_none()
        && block.get("name").is_none()
        && block.get("arguments").is_none()
        && block.get("thinking").is_none()
}

#[cfg(test)]
mod tests {
    use super::append_coalesced_text_block;

    #[test]
    fn merges_adjacent_same_type_and_visibility() {
        let mut blocks = Vec::new();
        append_coalesced_text_block(&mut blocks, "reasoning", "The ", "private");
        append_coalesced_text_block(&mut blocks, "reasoning", "task", "private");
        assert_eq!(
            blocks,
            vec![serde_json::json!({
                "type": "reasoning",
                "text": "The task",
                "visibility": "private",
            })]
        );
    }

    #[test]
    fn type_or_visibility_transition_starts_a_new_block() {
        let mut blocks = Vec::new();
        append_coalesced_text_block(&mut blocks, "reasoning", "hidden", "private");
        append_coalesced_text_block(&mut blocks, "output_text", "shown", "public");
        append_coalesced_text_block(&mut blocks, "output_text", " more", "public");
        append_coalesced_text_block(&mut blocks, "output_text", "secret", "private");
        assert_eq!(
            blocks,
            vec![
                serde_json::json!({
                    "type": "reasoning",
                    "text": "hidden",
                    "visibility": "private",
                }),
                serde_json::json!({
                    "type": "output_text",
                    "text": "shown more",
                    "visibility": "public",
                }),
                serde_json::json!({
                    "type": "output_text",
                    "text": "secret",
                    "visibility": "private",
                }),
            ]
        );
    }

    #[test]
    fn does_not_merge_into_provider_signed_or_tool_blocks() {
        let mut blocks = vec![
            serde_json::json!({
                "type": "thinking",
                "thinking": "signed",
                "signature": "sig",
            }),
            serde_json::json!({
                "type": "tool_call",
                "id": "call_1",
                "name": "search_web",
                "arguments": {"q": "x"},
                "visibility": "internal",
            }),
        ];
        append_coalesced_text_block(&mut blocks, "reasoning", "later", "private");
        append_coalesced_text_block(&mut blocks, "output_text", "after", "public");
        assert_eq!(blocks.len(), 4);
        assert_eq!(blocks[0]["signature"], "sig");
        assert_eq!(blocks[1]["type"], "tool_call");
        assert_eq!(blocks[2]["text"], "later");
        assert_eq!(blocks[3]["text"], "after");
    }
}
