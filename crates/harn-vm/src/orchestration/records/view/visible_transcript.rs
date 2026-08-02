use serde::Deserialize;
use serde_json::Value;

use crate::redact::RedactionPolicy;

use super::{bounded_join, non_empty_string, redact_bounded, TEXT_LIMIT};

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
struct EmbeddedTranscript {
    events: Vec<EmbeddedTranscriptEvent>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
struct EmbeddedTranscriptEvent {
    kind: String,
    role: String,
    visibility: String,
    blocks: Vec<EmbeddedTranscriptBlock>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
struct EmbeddedTranscriptBlock {
    #[serde(rename = "type")]
    kind: String,
    visibility: String,
    text: Option<String>,
}

/// Recover user-visible output for workflows that persist the canonical
/// transcript but do not copy it onto a stage. This projection is deliberately
/// fail-closed: only public assistant message events and their public text
/// blocks are eligible. Event-level text, private blocks, and reasoning blocks
/// are never trusted as visible output.
pub(super) fn public_assistant_transcript_text(
    value: Option<&Value>,
    policy: &RedactionPolicy,
) -> Option<String> {
    let transcript = EmbeddedTranscript::deserialize(value?).ok()?;
    bounded_join(
        transcript.events.into_iter().filter_map(|event| {
            if event.kind != "message" || event.role != "assistant" || event.visibility != "public"
            {
                return None;
            }
            let text = event
                .blocks
                .into_iter()
                .filter(|block| {
                    block.visibility == "public"
                        && matches!(block.kind.as_str(), "text" | "output_text")
                })
                .filter_map(|block| block.text)
                .collect::<String>();
            non_empty_string(&text).map(|text| redact_bounded(&text, policy, TEXT_LIMIT))
        }),
        TEXT_LIMIT,
    )
}
