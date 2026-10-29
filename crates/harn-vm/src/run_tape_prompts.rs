//! Prompt provenance on the run tape.
//!
//! Every prompt asset render is recorded once per distinct output. At each
//! model call the request's text fields are searched for those outputs, and
//! every verbatim occurrence becomes an attributed segment, so a reader can
//! go from any request byte to the render, and through its spans to the
//! `.harn.prompt` line, that produced it.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use crate::stdlib::template::{PromptSourceSpan, PromptSpanKind};
use crate::testbench::tape::{
    content_hash, AttributedSegment, CallAttribution, TapeRecordKind, TapeRecorder,
};

/// Shorter renders match too much unrelated text to be evidence.
const MIN_ATTRIBUTABLE_BYTES: usize = 24;

/// One recorded render, as a request carries it: the render without its
/// surrounding whitespace, which assembly routinely trims, and the byte ranges
/// in that text a filled-in value produced. Every other byte is template text.
struct Render {
    text: Arc<str>,
    /// Where `text` starts in the full render.
    offset: usize,
    values: Arc<[(usize, usize)]>,
}

#[derive(Default)]
pub(super) struct PromptLedger {
    renders: BTreeMap<String, Render>,
    /// Renders recorded since the previous model call.
    fresh: BTreeSet<String>,
    /// Renders the previous model call contained. A prompt rendered once and
    /// resent every turn stays a candidate through this set.
    carried: BTreeSet<String>,
    consumers: BTreeMap<String, Vec<u64>>,
}

pub(super) fn render_id(template_uri: &str, rendered: &str) -> String {
    let mut bytes = Vec::with_capacity(template_uri.len() + rendered.len() + 1);
    bytes.extend_from_slice(template_uri.as_bytes());
    bytes.push(0);
    bytes.extend_from_slice(rendered.as_bytes());
    content_hash(&bytes)
}

/// Where a request byte came from, innermost render first.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Origin {
    Unmapped,
    TemplateText,
    FilledValue,
}

impl PromptLedger {
    pub(super) fn record(
        &mut self,
        recorder: &TapeRecorder,
        render_id: &str,
        template_uri: &str,
        rendered: &str,
        spans: &[PromptSourceSpan],
    ) {
        let core = rendered.trim();
        if core.len() < MIN_ATTRIBUTABLE_BYTES {
            return;
        }
        let offset = rendered.len() - rendered.trim_start().len();
        if !self.renders.contains_key(render_id) {
            let encoded = serde_json::to_vec(spans).unwrap_or_default();
            recorder.record(TapeRecordKind::PromptRender {
                render_id: render_id.to_string(),
                template_uri: template_uri.to_string(),
                rendered: recorder.payload_cas(rendered.as_bytes().to_vec()),
                spans: recorder.payload_cas(encoded),
            });
            let end = offset + core.len();
            let values = spans
                .iter()
                .filter(|span| {
                    matches!(
                        span.kind,
                        PromptSpanKind::Expr | PromptSpanKind::LegacyBareInterp
                    )
                })
                .map(|span| (span.output_start.max(offset), span.output_end.min(end)))
                .filter(|(start, end)| start < end)
                .map(|(start, end)| (start - offset, end - offset))
                .collect();
            self.renders.insert(
                render_id.to_string(),
                Render {
                    text: Arc::from(core),
                    offset,
                    values,
                },
            );
        }
        self.fresh.insert(render_id.to_string());
    }

    pub(super) fn consumers(&self, render_id: &str) -> Vec<u64> {
        self.consumers.get(render_id).cloned().unwrap_or_default()
    }

    /// Attribute one model call's request (the digest surface) to renders.
    pub(super) fn attribute(
        &mut self,
        surface: &serde_json::Map<String, serde_json::Value>,
        call_index: u64,
    ) -> CallAttribution {
        let fields = text_fields(surface);
        // Longest first, so a render interpolated into another is painted
        // after it and its own text wins for the bytes they share.
        let mut candidates: Vec<(&String, &Render)> = self
            .fresh
            .iter()
            .chain(self.carried.iter())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .filter_map(|id| Some((id, self.renders.get(id)?)))
            .collect();
        candidates.sort_by(|a, b| b.1.text.len().cmp(&a.1.text.len()).then(a.0.cmp(b.0)));

        let mut attribution = CallAttribution::default();
        let mut matched = BTreeSet::new();
        for field in &fields {
            let mut origin = vec![Origin::Unmapped; field.text.len()];
            for (id, render) in &candidates {
                for (start, _) in field.text.match_indices(&*render.text) {
                    let end = start + render.text.len();
                    attribution.segments.push(AttributedSegment {
                        field: field.name.clone(),
                        start: start as u64,
                        end: end as u64,
                        render_id: (*id).clone(),
                        render_offset: render.offset as u64,
                    });
                    origin[start..end].fill(Origin::TemplateText);
                    for &(value_start, value_end) in render.values.iter() {
                        origin[start + value_start..start + value_end].fill(Origin::FilledValue);
                    }
                    matched.insert((*id).clone());
                }
            }
            let bytes = field.text.len() as u64;
            let count = |kind| origin.iter().filter(|&&o| o == kind).count() as u64;
            let (template, value) = (count(Origin::TemplateText), count(Origin::FilledValue));
            if field.name == "system" {
                attribution.system_bytes += bytes;
                attribution.system_template_bytes += template;
                attribution.system_value_bytes += value;
            }
            if !field.tool_result {
                attribution.prompt_bytes += bytes;
                attribution.prompt_template_bytes += template;
                attribution.prompt_value_bytes += value;
            }
        }
        for id in &matched {
            self.consumers
                .entry(id.clone())
                .or_default()
                .push(call_index);
        }
        self.fresh.clear();
        self.carried = matched;
        attribution
    }
}

struct TextField<'a> {
    name: String,
    text: &'a str,
    tool_result: bool,
}

fn text_fields(surface: &serde_json::Map<String, serde_json::Value>) -> Vec<TextField<'_>> {
    let mut fields = Vec::new();
    if let Some(system) = surface.get("system").and_then(|v| v.as_str()) {
        fields.push(TextField {
            name: "system".to_string(),
            text: system,
            tool_result: false,
        });
    }
    let messages = surface.get("messages").and_then(|v| v.as_array());
    for (i, message) in messages.into_iter().flatten().enumerate() {
        let tool_message = message.get("role").and_then(|r| r.as_str()) == Some("tool");
        match message.get("content") {
            Some(serde_json::Value::String(text)) => fields.push(TextField {
                name: format!("messages[{i}]"),
                text,
                tool_result: tool_message,
            }),
            Some(serde_json::Value::Array(blocks)) => {
                for (j, block) in blocks.iter().enumerate() {
                    let kind = block.get("type").and_then(|t| t.as_str());
                    let text = block
                        .get("text")
                        .or_else(|| block.get("content"))
                        .and_then(|t| t.as_str());
                    if let Some(text) = text {
                        fields.push(TextField {
                            name: format!("messages[{i}].content[{j}]"),
                            text,
                            tool_result: tool_message || kind == Some("tool_result"),
                        });
                    }
                }
            }
            _ => {}
        }
    }
    fields
}
