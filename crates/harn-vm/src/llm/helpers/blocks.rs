use std::collections::BTreeMap;
use std::rc::Rc;

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
        Some(other) => vec![VmValue::Dict(Rc::new(BTreeMap::from([
            ("type".to_string(), VmValue::String(Rc::from("text"))),
            (
                "text".to_string(),
                VmValue::String(Rc::from(other.display())),
            ),
            (
                "visibility".to_string(),
                VmValue::String(Rc::from(default_visibility)),
            ),
        ])))],
    }
}

fn normalize_transcript_block(block: &VmValue, default_visibility: &str) -> VmValue {
    let mut normalized = block.as_dict().cloned().unwrap_or_else(|| {
        BTreeMap::from([(
            "text".to_string(),
            VmValue::String(Rc::from(block.display())),
        )])
    });
    if !normalized.contains_key("type") {
        normalized.insert("type".to_string(), VmValue::String(Rc::from("text")));
    }
    if !normalized.contains_key("visibility") {
        normalized.insert(
            "visibility".to_string(),
            VmValue::String(Rc::from(default_visibility)),
        );
    }
    VmValue::Dict(Rc::new(normalized))
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

pub(super) fn render_blocks_text(blocks: &[VmValue]) -> String {
    let mut parts = Vec::new();
    for block in blocks {
        let Some(dict) = block.as_dict() else {
            continue;
        };
        let kind = dict
            .get("type")
            .map(|value| value.display())
            .unwrap_or_else(|| "text".to_string());
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
        if !text.is_empty() {
            parts.push(text);
        }
    }
    parts.join(" ")
}

fn render_assetish_label(kind: &str, dict: &BTreeMap<String, VmValue>) -> String {
    let label = dict
        .get("name")
        .or_else(|| dict.get("title"))
        .or_else(|| dict.get("path"))
        .or_else(|| dict.get("asset_id"))
        .map(|value| value.display())
        .unwrap_or_else(|| kind.to_string());
    format!("<{kind}:{label}>")
}
