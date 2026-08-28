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
        if let Some(existing) = coalescible_text_mut(last, block_type, visibility) {
            existing.push_str(text);
            return;
        }
    }
    blocks.push(serde_json::json!({
        "type": block_type,
        "text": text,
        "visibility": visibility,
    }));
}

fn coalescible_text_mut<'a>(
    block: &'a mut serde_json::Value,
    block_type: &str,
    visibility: &str,
) -> Option<&'a mut String> {
    let object = block.as_object_mut()?;
    // Only the canonical plain-text shape is safe to extend. Any additional
    // provider metadata carries a semantic boundary, whether or not this
    // module knows that field's name yet.
    if object.len() != 3
        || object.get("type").and_then(serde_json::Value::as_str) != Some(block_type)
        || object.get("visibility").and_then(serde_json::Value::as_str) != Some(visibility)
    {
        return None;
    }
    match object.get_mut("text") {
        Some(serde_json::Value::String(text)) => Some(text),
        _ => None,
    }
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
        let mut blocks = vec![serde_json::json!({
            "type": "reasoning",
            "text": "signed",
            "visibility": "private",
            "signature": "sig",
        })];
        append_coalesced_text_block(&mut blocks, "reasoning", "later", "private");
        blocks.push(serde_json::json!({
            "type": "tool_call",
            "id": "call_1",
            "name": "search_web",
            "arguments": {"q": "x"},
            "visibility": "internal",
        }));
        append_coalesced_text_block(&mut blocks, "output_text", "after", "public");
        assert_eq!(blocks.len(), 4);
        assert_eq!(blocks[0]["signature"], "sig");
        assert_eq!(blocks[1]["text"], "later");
        assert_eq!(blocks[2]["type"], "tool_call");
        assert_eq!(blocks[3]["text"], "after");
    }
}
