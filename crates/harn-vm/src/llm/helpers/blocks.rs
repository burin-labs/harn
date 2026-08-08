use crate::value::VmDictExt;
use std::collections::BTreeMap;

use crate::value::VmValue;

pub(super) fn default_visibility_for_role(role: &str) -> &'static str {
    match role {
        "tool_result" => "internal",
        _ => "public",
    }
}

pub(super) fn normalize_message_blocks(content: Option<&VmValue>, role: &str) -> Vec<VmValue> {
    let default_visibility = default_visibility_for_role(role);
    match content {
        Some(VmValue::List(items)) => items
            .iter()
            .map(|block| normalize_transcript_block(block, default_visibility))
            .collect(),
        Some(VmValue::Dict(block)) => {
            vec![normalize_transcript_block(
                &VmValue::Dict(block.clone()),
                default_visibility,
            )]
        }
        Some(VmValue::Nil) | None => Vec::new(),
        Some(other) => vec![VmValue::dict(BTreeMap::from([
            (
                "type".to_string(),
                VmValue::String(arcstr::ArcStr::from("text")),
            ),
            (
                "text".to_string(),
                VmValue::String(arcstr::ArcStr::from(other.display())),
            ),
            (
                "visibility".to_string(),
                VmValue::String(arcstr::ArcStr::from(default_visibility)),
            ),
        ]))],
    }
}

fn normalize_transcript_block(block: &VmValue, default_visibility: &str) -> VmValue {
    let mut normalized = block.as_dict().cloned().unwrap_or_else(|| {
        crate::value::DictMap::from_iter([(
            "text".to_string(),
            VmValue::String(arcstr::ArcStr::from(block.display())),
        )])
    });
    if !normalized.contains_key("type") {
        normalized.put_str("type", "text");
    }
    if !normalized.contains_key("visibility") {
        normalized.put_str("visibility", default_visibility);
    }
    VmValue::dict(normalized)
}

pub(super) fn overall_visibility(blocks: &[VmValue], default_visibility: &str) -> String {
    let mut resolved = default_visibility.to_string();
    for block in blocks {
        let Some(dict) = block.as_dict() else {
            continue;
        };
        let visibility = dict
            .get("visibility")
            .map(|value| value.display())
            .unwrap_or_else(|| default_visibility.to_string());
        if visibility == "public" {
            return visibility;
        }
        if visibility == "internal" {
            resolved = visibility;
        }
    }
    resolved
}

/// Flatten a message's content blocks into one display string.
///
/// Adjacent `text` / `output_text` blocks are concatenated with **no**
/// separator: a streamed assistant turn arrives as chunks split at arbitrary
/// byte offsets, so `["El Al", "to above it"]` is one word broken across two
/// blocks. Space-joining those corrupts the text ("El Al to above it").
/// Label-ish blocks (`<tool_call:…>`, `<image:…>`) are standalone tokens and
/// do get a space between them and their neighbours.
pub(super) fn render_blocks_text(blocks: &[VmValue]) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut previous_was_text = false;
    for block in blocks {
        let Some(dict) = block.as_dict() else {
            continue;
        };
        let kind = dict
            .get("type")
            .map(|value| value.display())
            .unwrap_or_else(|| "text".to_string());
        let is_text = matches!(kind.as_str(), "text" | "output_text");
        let text = match kind.as_str() {
            "text" | "output_text" => dict
                .get("text")
                .or_else(|| dict.get("content"))
                .map(|value| value.display())
                .unwrap_or_default(),
            "reasoning" => String::new(),
            "tool_call" => {
                let name = dict
                    .get("name")
                    .map(|value| value.display())
                    .unwrap_or_else(|| "tool".to_string());
                format!("<tool_call:{name}>")
            }
            "tool_result" => {
                let name = dict
                    .get("name")
                    .map(|value| value.display())
                    .unwrap_or_else(|| "tool".to_string());
                format!("<tool_result:{name}>")
            }
            "image" | "input_image" => render_assetish_label("image", dict),
            "file" | "document" | "attachment" => render_assetish_label(&kind, dict),
            other => format!("<{other}>"),
        };
        if text.is_empty() {
            continue;
        }
        match parts.last_mut() {
            Some(last) if is_text && previous_was_text => last.push_str(&text),
            _ => parts.push(text),
        }
        previous_was_text = is_text;
    }
    parts.join(" ")
}

fn render_assetish_label(kind: &str, dict: &crate::value::DictMap) -> String {
    let label = dict
        .get("name")
        .or_else(|| dict.get("title"))
        .or_else(|| dict.get("path"))
        .or_else(|| dict.get("asset_id"))
        .map(|value| value.display())
        .unwrap_or_else(|| kind.to_string());
    format!("<{kind}:{label}>")
}

#[cfg(test)]
mod tests {
    use super::render_blocks_text;
    use crate::value::{DictMap, VmDictExt, VmValue};

    fn block(kind: &str, text: &str) -> VmValue {
        let mut dict = DictMap::new();
        dict.put_str("type", kind);
        dict.put_str("text", text);
        VmValue::dict(dict)
    }

    #[test]
    fn streamed_text_chunks_concatenate_without_a_separator() {
        // A streamed assistant turn is chunked at arbitrary byte offsets, so
        // consecutive text blocks routinely split a word. Reproduced live
        // against claude-opus-5 on 2026-07-24, where the transcript rendered
        // "El Al" + "to above it" as "El Al to above it".
        let rendered = render_blocks_text(&[
            block("output_text", "La"),
            block("output_text", " Paz sits above sea level, with El Al"),
            block("output_text", "to above it."),
        ]);
        assert_eq!(
            rendered,
            "La Paz sits above sea level, with El Alto above it."
        );
    }

    #[test]
    fn label_blocks_stay_space_separated_from_text() {
        let mut tool = DictMap::new();
        tool.put_str("type", "tool_call");
        tool.put_str("name", "read_file");
        let rendered = render_blocks_text(&[
            block("text", "before"),
            VmValue::dict(tool),
            block("text", "after"),
        ]);
        assert_eq!(rendered, "before <tool_call:read_file> after");
    }

    #[test]
    fn empty_blocks_do_not_split_a_run_of_text() {
        let rendered = render_blocks_text(&[
            block("output_text", "one"),
            block("output_text", ""),
            block("output_text", "two"),
        ]);
        assert_eq!(rendered, "onetwo");
    }
}
