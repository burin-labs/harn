//! Optional Claude thread wire state over complete, already-admitted requests.
//!
//! The host owns eligibility, retention consent, accounting and transport. This
//! module never sends requests. Each prepared physical attempt must pass host
//! admission, including a recovery attempt. Keep canonical history separately.

use serde_json::{json, Map, Value};

#[cfg(test)]
mod tests;

/// Required only on physical attempts that contain a `thread` field.
pub const BETA: &str = "message-threads-2026-08-12";

/// Host-generated identity epochs, not credentials or provider session IDs.
/// Change the route epoch for endpoint/model routing, credential epoch for
/// account/workspace/credential changes, and policy epoch for consent changes.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Scope {
    pub route_epoch: u64,
    pub credential_epoch: u64,
    pub policy_epoch: u64,
    pub beta_epoch: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Create,
    Continue,
    Unthreaded,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrepareError {
    InvalidRequest,
    AlreadyThreaded,
    UnsupportedField,
    NoNewMessages,
    TurnPaused,
}

/// One in-memory, non-cloneable continuation owner. Construct it only after
/// the host has admitted the optional retention mode for the selected route.
#[derive(Default)]
pub struct MessageThread {
    head: Option<Head>,
    unsupported_scope: Option<Scope>,
}

struct Head {
    scope: Scope,
    fixed: Map<String, Value>,
    messages: Vec<Value>,
    id: String,
    paused: bool,
}

/// Borrows the owner exclusively until completion. Dropping an unfinished
/// attempt invalidates its cursor, including cancellation and partial streams.
#[must_use = "a prepared turn must be admitted and completed, or deliberately discarded"]
pub struct PreparedTurn<'a> {
    owner: &'a mut MessageThread,
    scope: Scope,
    full: Value,
    body: Value,
    mode: Mode,
    recovered: bool,
}

impl MessageThread {
    /// Forget local handles; this does not delete anything at the provider.
    pub fn reset(&mut self) {
        self.head = None;
    }

    /// Takes the full final wire body, before any thread transformation.
    /// Unknown fields are refused until their thread semantics are reviewed.
    pub fn prepare(
        &mut self,
        scope: Scope,
        full: &Value,
    ) -> Result<PreparedTurn<'_>, PrepareError> {
        let head = self.head.take();
        validate(full)?;
        let messages = full["messages"].as_array().expect("validated messages");
        let projected_messages = comparable_messages(messages);
        let mut body = full.clone();
        let mode = if self.unsupported_scope == Some(scope) {
            Mode::Unthreaded
        } else if let Some(head) = head.filter(|head| {
            head.scope == scope
                && head.fixed == fixed_fields(full)
                && projected_messages.starts_with(&head.messages)
        }) {
            let new_messages = &messages[head.messages.len()..];
            if head.paused && !new_messages.is_empty() {
                return Err(PrepareError::TurnPaused);
            }
            if !head.paused && new_messages.is_empty() {
                return Err(PrepareError::NoNewMessages);
            }
            body["messages"] = json!(new_messages);
            let object = body.as_object_mut().expect("validated object");
            object.remove("system");
            object.remove("tools");
            body["thread"] = json!({"type":"continue", "previous_message_id":head.id});
            Mode::Continue
        } else {
            body["thread"] = json!({"type":"create"});
            Mode::Create
        };
        Ok(PreparedTurn {
            owner: self,
            scope,
            full: full.clone(),
            body,
            mode,
            recovered: false,
        })
    }
}

impl PreparedTurn<'_> {
    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// Only the transport body; full history remains available for admission.
    pub fn body(&self) -> &Value {
        &self.body
    }

    pub fn full_request(&self) -> &Value {
        &self.full
    }

    pub fn required_beta(&self) -> Option<&'static str> {
        (self.mode != Mode::Unthreaded).then_some(BETA)
    }

    /// At most one recovery, only for documented pre-generation rejections.
    /// A true result prepares another physical attempt; it does not admit it.
    pub fn recover_before_generation(&mut self, status: u16, error: &Value) -> bool {
        if self.recovered || self.mode == Mode::Unthreaded {
            return false;
        }
        let expected_type = match status {
            400 => "invalid_request_error",
            404 => "not_found_error",
            _ => return false,
        };
        if error.pointer("/error/type").and_then(Value::as_str) != Some(expected_type) {
            return false;
        }
        let code = error
            .pointer("/error/details/error_code")
            .and_then(Value::as_str);
        let mode = match (status, code, self.mode) {
            (404, Some("thread_not_found"), Mode::Continue)
            | (
                400,
                Some("thread_already_continued" | "thread_continue_in_flight"),
                Mode::Continue,
            ) => Mode::Create,
            (400, Some("thread_unsupported_request"), _) => {
                self.owner.unsupported_scope = Some(self.scope);
                Mode::Unthreaded
            }
            _ => return false,
        };
        self.body = self.full.clone();
        if mode == Mode::Create {
            self.body["thread"] = json!({"type":"create"});
        }
        self.mode = mode;
        self.recovered = true;
        true
    }

    /// Call only after a complete JSON reply or the stream's `message_stop`.
    /// Returns whether this response supplies a candidate continuation cursor;
    /// the provider can still expire or decline to store that candidate.
    pub fn complete(self, response: &Value) -> bool {
        if self.mode == Mode::Unthreaded
            || response["type"] != "message"
            || response["model"] != self.full["model"]
            || response["role"] != "assistant"
            || response
                .pointer("/usage/iterations")
                .is_some_and(|iterations| !iterations.as_array().is_some_and(Vec::is_empty))
        {
            return false;
        }
        let Some(stop) = response["stop_reason"].as_str() else {
            return false;
        };
        if !matches!(stop, "end_turn" | "tool_use" | "pause_turn") {
            return false;
        }
        let Some(id) = response["id"].as_str().filter(|id| !id.trim().is_empty()) else {
            return false;
        };
        let Some(content) = response["content"].as_array() else {
            return false;
        };
        if !content.iter().all(|block| {
            block.is_object() && block["type"].as_str().is_some_and(|kind| !kind.is_empty())
        }) {
            return false;
        }
        let mut messages = self.full["messages"]
            .as_array()
            .expect("validated messages")
            .clone();
        messages.push(json!({"role":"assistant", "content":content}));
        self.owner.head = Some(Head {
            scope: self.scope,
            fixed: fixed_fields(&self.full),
            messages: comparable_messages(&messages),
            id: id.to_string(),
            paused: stop == "pause_turn",
        });
        true
    }
}

// Messages accepts a string as shorthand for one plain text block. Harn's
// canonical text-only assistant history uses that shorthand, while native
// responses always return blocks. Compare their identical text projections,
// without rewriting the physical body or discarding any block metadata.
fn comparable_messages(messages: &[Value]) -> Vec<Value> {
    messages
        .iter()
        .cloned()
        .map(|mut message| {
            if let Some(text) = message["content"].as_str() {
                message["content"] = json!([{"type":"text", "text":text}]);
            }
            message
        })
        .collect()
}

fn validate(full: &Value) -> Result<(), PrepareError> {
    let Some(object) = full.as_object() else {
        return Err(PrepareError::InvalidRequest);
    };
    if object.contains_key("thread") {
        return Err(PrepareError::AlreadyThreaded);
    }
    if !full["model"]
        .as_str()
        .is_some_and(|model| !model.is_empty())
        || !full["messages"].as_array().is_some_and(|messages| {
            !messages.is_empty()
                && messages.iter().all(|message| {
                    matches!(message["role"].as_str(), Some("user" | "assistant"))
                        && matches!(
                            message.get("content"),
                            Some(Value::String(_) | Value::Array(_))
                        )
                })
        })
    {
        return Err(PrepareError::InvalidRequest);
    }
    if object.keys().any(|key| {
        !matches!(
            key.as_str(),
            "model"
                | "messages"
                | "system"
                | "tools"
                | "max_tokens"
                | "temperature"
                | "top_p"
                | "top_k"
                | "tool_choice"
                | "thinking"
                | "output_config"
                | "stop_sequences"
                | "stream"
                | "metadata"
                | "service_tier"
                | "cache_control"
                | "speed"
        )
    }) {
        return Err(PrepareError::UnsupportedField);
    }
    Ok(())
}

fn fixed_fields(full: &Value) -> Map<String, Value> {
    let mut fields = full.as_object().expect("validated object").clone();
    for free in [
        "messages",
        "max_tokens",
        "stop_sequences",
        "stream",
        "metadata",
        "service_tier",
    ] {
        fields.remove(free);
    }
    fields
}
