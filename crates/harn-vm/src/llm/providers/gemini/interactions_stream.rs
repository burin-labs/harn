//! SSE event → Interaction envelope assembly for the Gemini Interactions API.
//!
//! Interactions streams an *interaction* as typed events rather than as partial
//! copies of the final response: `step.start` announces a step at an index,
//! `step.delta` appends to it, `step.stop` closes it, and
//! `interaction.completed` carries the id, status, and usage. Tool arguments
//! arrive as a byte-split JSON string across many `arguments_delta` frames.
//!
//! [`InteractionStream`] rebuilds exactly the envelope the non-streaming route
//! returns, so [`super::interactions::parse_response`] stays the single owner
//! of step → transcript mapping and a streamed turn cannot drift from an
//! unstreamed one.

use std::collections::BTreeMap;

use serde_json::{json, Map, Value};

use super::interactions::{STEP_FUNCTION_CALL, STEP_MODEL_OUTPUT, STEP_THOUGHT};

/// Terminal SSE payload; the frame is not JSON.
pub(crate) const DONE_SENTINEL: &str = "[DONE]";

/// Accumulates streamed Interactions events into one interaction envelope.
#[derive(Debug, Default)]
pub(crate) struct InteractionStream {
    interaction: Map<String, Value>,
    /// Steps keyed by the wire `index`, which is what orders them — events for
    /// different steps interleave only in index order, but a `BTreeMap` makes
    /// that an invariant of the structure rather than an assumption.
    steps: BTreeMap<u64, StepBuilder>,
    error: Option<Value>,
}

#[derive(Debug, Default)]
struct StepBuilder {
    step: Map<String, Value>,
    /// Concatenated `text` deltas — the model output, or a thought summary.
    text: String,
    /// Concatenated `signature` deltas of a `thought` step.
    signature: String,
    /// Concatenated `arguments` deltas of a `function_call` step: a JSON object
    /// split at arbitrary byte boundaries, only parseable once closed.
    arguments: String,
}

/// What the caller should do with one consumed event.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum StreamAction {
    /// Nothing to surface.
    None,
    /// Newly streamed assistant text, ready to forward to a delta channel.
    Text(String),
    /// A terminal Interaction event arrived.
    Done,
}

impl InteractionStream {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Consume one decoded `data:` frame.
    pub(crate) fn push(&mut self, event: &Value) -> StreamAction {
        match event
            .get("event_type")
            .and_then(Value::as_str)
            .unwrap_or_default()
        {
            "interaction.created" | "interaction.completed" => {
                if let Some(interaction) = event.get("interaction").and_then(Value::as_object) {
                    for (key, value) in interaction {
                        // `steps` on the envelope is the server's own view; the
                        // streamed step events are the authority here, and an
                        // unstored interaction reports an empty id, so neither
                        // may overwrite what the stream already established.
                        if key == "steps" || is_blank(value) {
                            continue;
                        }
                        self.interaction.insert(key.clone(), value.clone());
                    }
                }
                if event.get("event_type").and_then(Value::as_str) == Some("interaction.completed")
                {
                    return StreamAction::Done;
                }
                StreamAction::None
            }
            "interaction.status_update" => {
                if let Some(status) = event.get("status").filter(|value| !is_blank(value)) {
                    self.interaction
                        .insert("status".to_string(), status.clone());
                }
                StreamAction::None
            }
            "step.start" => {
                let Some(index) = step_index(event) else {
                    return StreamAction::None;
                };
                let builder = self.steps.entry(index).or_default();
                if let Some(step) = event.get("step").and_then(Value::as_object) {
                    for (key, value) in step {
                        builder.step.insert(key.clone(), value.clone());
                    }
                }
                StreamAction::None
            }
            "step.delta" => self.push_delta(event),
            "step.stop" => {
                if let Some(index) = step_index(event) {
                    if let Some(builder) = self.steps.get_mut(&index) {
                        builder.close();
                    }
                }
                StreamAction::None
            }
            "error" => {
                self.error = event.get("error").cloned();
                StreamAction::Done
            }
            _ => StreamAction::None,
        }
    }

    fn push_delta(&mut self, event: &Value) -> StreamAction {
        let Some(index) = step_index(event) else {
            return StreamAction::None;
        };
        let Some(delta) = event.get("delta") else {
            return StreamAction::None;
        };
        let builder = self.steps.entry(index).or_default();

        // Route by which payload field is present rather than by the delta's
        // own `type` discriminant: the field is what the accumulator needs, and
        // Google has already shipped more than one spelling of the tag
        // (`thought_signature`, `arguments_delta`) for the same three payloads.
        if let Some(signature) = delta.get("signature").and_then(Value::as_str) {
            builder.signature.push_str(signature);
            return StreamAction::None;
        }
        if let Some(arguments) = delta.get("arguments").and_then(Value::as_str) {
            builder.arguments.push_str(arguments);
            return StreamAction::None;
        }
        if let Some(text) = delta.get("text").and_then(Value::as_str) {
            builder.text.push_str(text);
            // Only assistant output is user-visible; a thought summary stays on
            // the private reasoning channel and is never forwarded as a delta.
            if builder.step_type() == STEP_MODEL_OUTPUT && !text.is_empty() {
                return StreamAction::Text(text.to_string());
            }
        }
        StreamAction::None
    }

    /// The assembled interaction envelope, in the shape the non-streaming route
    /// returns. A stream cut short still yields every closed step.
    pub(crate) fn finish(mut self) -> Value {
        if let Some(error) = self.error {
            return json!({"error": error});
        }
        let steps: Vec<Value> = self
            .steps
            .into_values()
            .map(|mut builder| {
                builder.close();
                Value::Object(builder.step)
            })
            .collect();
        self.interaction.insert("steps".to_string(), json!(steps));
        Value::Object(self.interaction)
    }
}

impl StepBuilder {
    fn step_type(&self) -> &str {
        self.step
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default()
    }

    /// Fold the accumulated buffers into the step. Idempotent, so closing on
    /// `step.stop` and again on `finish` (for a truncated stream) is safe.
    fn close(&mut self) {
        match self.step_type() {
            STEP_THOUGHT => {
                if !self.signature.is_empty() {
                    self.step
                        .insert("signature".to_string(), json!(self.signature));
                }
                if !self.text.is_empty() {
                    self.step.insert(
                        "summary".to_string(),
                        json!([{"type": "text", "text": self.text}]),
                    );
                }
            }
            STEP_MODEL_OUTPUT if !self.text.is_empty() => {
                self.step.insert(
                    "content".to_string(),
                    json!([{"type": "text", "text": self.text}]),
                );
            }
            STEP_FUNCTION_CALL => {
                // The concatenation is only valid JSON once every delta has
                // arrived. A stream truncated mid-arguments leaves whatever
                // `step.start` announced (an empty object) rather than
                // inventing a half-parsed call.
                if let Ok(arguments) = serde_json::from_str::<Value>(&self.arguments) {
                    self.step.insert("arguments".to_string(), arguments);
                }
            }
            _ => {}
        }
    }
}

fn step_index(event: &Value) -> Option<u64> {
    event.get("index").and_then(Value::as_u64)
}

fn is_blank(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::String(text) => text.is_empty(),
        _ => false,
    }
}
