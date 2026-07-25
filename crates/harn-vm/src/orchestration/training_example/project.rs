//! Replay the verified event stream into one canonical training example.
//!
//! The walk binds three independent typed records together:
//!
//! - `message` events are the exact provider-visible conversation, in order;
//! - `provider_call_response` carries the typed tool calls the model made,
//!   including the calls parsed out of a text-channel response, which never
//!   appear as structured fields on the assistant message itself;
//! - `tool_result` receipts name the call each result answers, which a
//!   text-channel result cannot carry (it rides back as a plain `user` echo).
//!
//! Nothing is inferred from position alone. Each binding is *verified* against
//! an identity the runtime recorded, and a binding that does not check out is
//! a structured failure rather than a best guess.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde_json::Value as JsonValue;

use super::source::{SourceEvent, SourceEventKind};
use super::validate::validate_training_example_pairing;
use super::{
    AgentTrainingExample, TrainingExampleError, TrainingExampleRequest, TrainingMessage,
    TrainingProvenance, TrainingSource, TrainingToolCall, TrainingToolFunction, TrainingUsage,
    TRAINING_EXAMPLE_SCHEMA_VERSION,
};
use crate::orchestration::{RunRecord, RunTranscriptArtifactDescriptor};

/// Schema of the `tool_result` pairing receipt this projector requires.
/// The runtime stamps this exact string on every receipt it writes.
pub const TOOL_RESULT_RECEIPT_VERSION: &str = "harn.llm_tool_result_receipt.v1";

/// Schema of the `tool_call` dispatch receipt this projector requires.
pub const TOOL_CALL_RECEIPT_VERSION: &str = "harn.llm_tool_call_receipt.v1";

/// A `provider_call_response` awaiting the assistant message it produced.
struct PendingResponse {
    event_index: usize,
    call_id: String,
    /// Provider-native calls, empty on a text-channel response.
    native_calls: Vec<TrainingToolCall>,
    /// Native when present, otherwise the calls parsed out of the inline
    /// tagged blocks in the response text.
    merged_calls: Vec<TrainingToolCall>,
}

/// A `tool_result` receipt: which call the message at `key` answers.
#[derive(Clone)]
struct ToolResultReceipt {
    event_index: usize,
    call_id: String,
    tool_name: String,
    /// The session's claimed tool format — the channel results were served
    /// on. Distinct from the per-call effective format, which an escalation
    /// can change without re-claiming the session's.
    tool_format: Option<String>,
}

/// Identity a receipt and its message agree on: one message inside one
/// session. `message_index` alone is not unique across sessions.
type ReceiptKey = (String, u64);

fn receipt_key(event: &SourceEvent) -> ReceiptKey {
    (
        event
            .str_field("session_id")
            .unwrap_or_default()
            .to_string(),
        event.u64_field("message_index").unwrap_or(u64::MAX),
    )
}

/// Index the calls dispatched for each assistant turn, keyed by the message
/// they were parsed from.
fn index_dispatched_calls(
    events: &[SourceEvent],
) -> Result<BTreeMap<ReceiptKey, Vec<TrainingToolCall>>, TrainingExampleError> {
    let mut dispatched: BTreeMap<ReceiptKey, Vec<TrainingToolCall>> = BTreeMap::new();
    for event in events {
        if event.kind != SourceEventKind::ToolCall {
            continue;
        }
        let Some(version) = event.str_field("schema_version") else {
            return Err(TrainingExampleError::at(
                "unknown_event_version",
                event.index,
                "`tool_call` receipt declares no schema_version",
            ));
        };
        if version != TOOL_CALL_RECEIPT_VERSION {
            return Err(TrainingExampleError::at(
                "unknown_event_version",
                event.index,
                format!("`tool_call` receipt declares unsupported schema {version}"),
            ));
        }
        let key = (
            event
                .str_field("session_id")
                .unwrap_or_default()
                .to_string(),
            event.u64_field("assistant_message_index").ok_or_else(|| {
                TrainingExampleError::at(
                    "malformed_tool_call",
                    event.index,
                    "`tool_call` receipt carries no assistant_message_index",
                )
            })?,
        );
        let tool_name = event.str_field("tool_name").unwrap_or_default();
        let call_id = event.str_field("call_id").unwrap_or_default();
        if tool_name.is_empty() || call_id.is_empty() {
            return Err(TrainingExampleError::at(
                "malformed_tool_call",
                event.index,
                "`tool_call` receipt needs both call_id and tool_name",
            ));
        }
        dispatched.entry(key).or_default().push(TrainingToolCall {
            id: call_id.to_string(),
            call_type: "function".to_string(),
            function: TrainingToolFunction {
                name: tool_name.to_string(),
                arguments: decode_arguments(
                    event.value.get("arguments").cloned(),
                    tool_name,
                    event.index,
                )?,
            },
        });
    }
    Ok(dispatched)
}

/// Index every pairing receipt before the walk, so binding a result never
/// depends on where the receipt landed relative to its message.
fn index_receipts(
    events: &[SourceEvent],
) -> Result<BTreeMap<ReceiptKey, ToolResultReceipt>, TrainingExampleError> {
    let mut message_keys: BTreeMap<ReceiptKey, usize> = BTreeMap::new();
    for event in events {
        if event.kind == SourceEventKind::Message {
            *message_keys.entry(receipt_key(event)).or_default() += 1;
        }
    }
    let mut receipts = BTreeMap::new();
    for event in events {
        if event.kind != SourceEventKind::ToolResult {
            continue;
        }
        let Some(version) = event.str_field("schema_version") else {
            return Err(TrainingExampleError::at(
                "unknown_event_version",
                event.index,
                "`tool_result` receipt declares no schema_version",
            ));
        };
        if version != TOOL_RESULT_RECEIPT_VERSION {
            return Err(TrainingExampleError::at(
                "unknown_event_version",
                event.index,
                format!("`tool_result` receipt declares unsupported schema {version}"),
            ));
        }
        let call_id = event.str_field("call_id").unwrap_or_default();
        if call_id.is_empty() {
            return Err(TrainingExampleError::at(
                "malformed_tool_result_receipt",
                event.index,
                "`tool_result` receipt carries no call_id",
            ));
        }
        if event.u64_field("message_index").is_none() {
            return Err(TrainingExampleError::at(
                "malformed_tool_result_receipt",
                event.index,
                "`tool_result` receipt carries no message_index",
            ));
        }
        let key = receipt_key(event);
        // Bind now, not during the walk: a receipt naming a message that is
        // not in this transcript is a source defect, and reporting it here
        // keeps it from surfacing later as a confusing "unanswered call".
        match message_keys.get(&key).copied().unwrap_or(0) {
            1 => {}
            0 => {
                return Err(TrainingExampleError::at(
                    "unbound_tool_result_receipt",
                    event.index,
                    format!(
                        "the receipt for call {call_id} names message {} of session `{}`, which \
                         is not in this transcript",
                        key.1, key.0
                    ),
                ))
            }
            count => {
                return Err(TrainingExampleError::at(
                    "unbound_tool_result_receipt",
                    event.index,
                    format!(
                        "the receipt for call {call_id} names message {} of session `{}`, which \
                         {count} messages claim",
                        key.1, key.0
                    ),
                ))
            }
        }
        if let Some(existing) = receipts.insert(
            key,
            ToolResultReceipt {
                event_index: event.index,
                call_id: call_id.to_string(),
                tool_name: event.str_field("tool_name").unwrap_or_default().to_string(),
                tool_format: event
                    .str_field("tool_format")
                    .filter(|value| !value.is_empty())
                    .map(str::to_string),
            },
        ) {
            return Err(TrainingExampleError::at(
                "duplicate_tool_result_receipt",
                event.index,
                format!(
                    "two receipts (events {} and {}) claim the same message",
                    existing.event_index, event.index
                ),
            ));
        }
    }
    Ok(receipts)
}

/// A tool call the assistant made that has not been answered yet.
struct OpenCall {
    event_index: usize,
    id: String,
    name: String,
}

/// A source fact that must be identical across the whole run, because one
/// training example can only carry one value for it. `set` records the first
/// value and rejects a later disagreement with a structured error.
struct PinnedFact {
    label: &'static str,
    error_kind: &'static str,
    value: Option<String>,
}

impl PinnedFact {
    const fn new(label: &'static str, error_kind: &'static str) -> Self {
        Self {
            label,
            error_kind,
            value: None,
        }
    }

    fn set(&mut self, event_index: usize, incoming: &str) -> Result<(), TrainingExampleError> {
        match self.value.as_deref() {
            None => {
                self.value = Some(incoming.to_string());
                Ok(())
            }
            Some(existing) if existing == incoming => Ok(()),
            Some(existing) => Err(TrainingExampleError::at(
                self.error_kind,
                event_index,
                format!(
                    "{} changed mid-run ({existing} -> {incoming}); this artifact holds more \
                     than one training context, so name a single eligible run explicitly \
                     instead of projecting the whole artifact",
                    self.label
                ),
            )),
        }
    }

    fn require(&self, kind: &str) -> Result<String, TrainingExampleError> {
        self.value.clone().ok_or_else(|| {
            TrainingExampleError::new(kind, format!("run never recorded {}", self.label))
        })
    }
}

#[derive(Default)]
struct ToolCatalog {
    schemas: Vec<JsonValue>,
    hash: Option<String>,
    content_hash: Option<String>,
    names: BTreeSet<String>,
}

struct Walk<'a> {
    request: &'a TrainingExampleRequest,
    messages: Vec<TrainingMessage>,
    catalog: ToolCatalog,
    catalog_event_index: Option<usize>,
    system: PinnedFact,
    provider: PinnedFact,
    model: PinnedFact,
    effective_tool_format: PinnedFact,
    declared_tool_format: Option<String>,
    route_policy: Option<String>,
    session: PinnedFact,
    pending_response: Option<PendingResponse>,
    receipts: BTreeMap<ReceiptKey, ToolResultReceipt>,
    consumed_receipts: BTreeSet<ReceiptKey>,
    dispatched: BTreeMap<ReceiptKey, Vec<TrainingToolCall>>,
    open_calls: Vec<OpenCall>,
    provider_calls: usize,
    input_tokens: u64,
    output_tokens: u64,
}

pub(crate) fn project(
    run: &RunRecord,
    descriptor: &RunTranscriptArtifactDescriptor,
    transcript_path: &Path,
    events: &[SourceEvent],
    request: &TrainingExampleRequest,
) -> Result<AgentTrainingExample, TrainingExampleError> {
    let mut walk = Walk {
        request,
        messages: Vec::new(),
        catalog: ToolCatalog::default(),
        catalog_event_index: None,
        system: PinnedFact::new("the system prompt", "system_prompt_changed"),
        provider: PinnedFact::new("the provider", "route_changed"),
        model: PinnedFact::new("the model", "route_changed"),
        effective_tool_format: PinnedFact::new("the effective tool format", "tool_format_changed"),
        declared_tool_format: None,
        route_policy: None,
        session: PinnedFact::new("the session id", "ambiguous_authority"),
        pending_response: None,
        receipts: index_receipts(events)?,
        consumed_receipts: BTreeSet::new(),
        dispatched: index_dispatched_calls(events)?,
        open_calls: Vec::new(),
        provider_calls: 0,
        input_tokens: 0,
        output_tokens: 0,
    };

    for event in events {
        walk.ingest(event)?;
    }
    walk.finish(run, descriptor, transcript_path, events)
}

impl Walk<'_> {
    fn ingest(&mut self, event: &SourceEvent) -> Result<(), TrainingExampleError> {
        match event.kind {
            SourceEventKind::SystemPrompt => self.ingest_system_prompt(event),
            SourceEventKind::ToolSchemas => self.ingest_tool_schemas(event),
            SourceEventKind::ProviderCallRequest => self.ingest_request(event),
            SourceEventKind::ProviderCallResponse => self.ingest_response(event),
            SourceEventKind::ToolResult | SourceEventKind::ToolCall => Ok(()),
            SourceEventKind::Message => self.ingest_message(event),
            SourceEventKind::Other => Ok(()),
        }
    }

    fn ingest_system_prompt(&mut self, event: &SourceEvent) -> Result<(), TrainingExampleError> {
        let content = event.str_field("content").unwrap_or_default();
        self.system.set(event.index, content)
    }

    fn ingest_tool_schemas(&mut self, event: &SourceEvent) -> Result<(), TrainingExampleError> {
        let schemas = event
            .value
            .get("schemas")
            .and_then(JsonValue::as_array)
            .ok_or_else(|| {
                TrainingExampleError::at(
                    "malformed_tool_catalog",
                    event.index,
                    "`tool_schemas` has no `schemas` array",
                )
            })?;
        if let Some(previous) = self.catalog_event_index {
            if self.catalog.schemas != *schemas {
                return Err(TrainingExampleError::at(
                    "tool_catalog_changed",
                    event.index,
                    format!(
                        "the served tool catalog changed after event {previous}; one training \
                         example cannot teach two catalogs"
                    ),
                ));
            }
            return Ok(());
        }
        self.catalog.names = schemas.iter().filter_map(schema_name).collect();
        self.catalog.schemas = schemas.clone();
        self.catalog.hash = event
            .value
            .get("hash")
            .map(json_scalar_text)
            .filter(|value| !value.is_empty());
        self.catalog.content_hash = event
            .str_field("content_hash")
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        self.catalog_event_index = Some(event.index);
        Ok(())
    }

    fn ingest_request(&mut self, event: &SourceEvent) -> Result<(), TrainingExampleError> {
        if let Some(format) = event.str_field("tool_format").filter(|f| !f.is_empty()) {
            self.effective_tool_format.set(event.index, format)?;
        }
        if self.route_policy.is_none() {
            self.route_policy = event
                .str_field("route_policy")
                .filter(|value| !value.is_empty())
                .map(str::to_string);
        }
        Ok(())
    }

    fn ingest_response(&mut self, event: &SourceEvent) -> Result<(), TrainingExampleError> {
        if let Some(unbound) = self.pending_response.as_ref() {
            return Err(TrainingExampleError::at(
                "unbound_provider_response",
                event.index,
                format!(
                    "provider call {} (event {}) produced no assistant message before the next call",
                    unbound.call_id, unbound.event_index
                ),
            ));
        }
        let provider = event.str_field("provider").unwrap_or_default();
        let model = event.str_field("model").unwrap_or_default();
        self.provider.set(event.index, provider)?;
        self.model.set(event.index, model)?;
        self.provider_calls += 1;
        self.input_tokens += event.u64_field("input_tokens").unwrap_or_default();
        self.output_tokens += event.u64_field("output_tokens").unwrap_or_default();
        let native_calls = tool_calls_from_array(event, "tool_calls")?;
        let merged_calls = {
            let merged = tool_calls_from_array(event, "parsed_tool_calls")?;
            if merged.is_empty() {
                native_calls.clone()
            } else {
                merged
            }
        };
        self.pending_response = Some(PendingResponse {
            event_index: event.index,
            call_id: event.str_field("call_id").unwrap_or_default().to_string(),
            native_calls,
            merged_calls,
        });
        Ok(())
    }

    fn ingest_message(&mut self, event: &SourceEvent) -> Result<(), TrainingExampleError> {
        if let Some(session_id) = event.str_field("session_id").filter(|id| !id.is_empty()) {
            self.session.set(event.index, session_id)?;
        }
        let body = event.value.get("message").cloned().unwrap_or_else(|| {
            serde_json::json!({
                "role": event.value.get("role").cloned().unwrap_or(JsonValue::Null),
                "content": event.value.get("content").cloned().unwrap_or(JsonValue::Null),
            })
        });
        let role = body
            .get("role")
            .and_then(JsonValue::as_str)
            .unwrap_or_default()
            .to_string();
        let content = message_text(&body);

        // A receipt claims exactly the message the runtime injected for it,
        // by the index `inject_message` returned. Looking the binding up by
        // that recorded identity — rather than by adjacency in the stream —
        // means a reordered or interleaved log cannot mispair a result.
        let key = receipt_key(event);
        if let Some(receipt) = self.receipts.get(&key) {
            if !self.consumed_receipts.insert(key) {
                return Err(TrainingExampleError::at(
                    "unbound_tool_result_receipt",
                    event.index,
                    format!(
                        "more than one message claims the receipt for call {}",
                        receipt.call_id
                    ),
                ));
            }
            let receipt = receipt.clone();
            return self.bind_tool_result(event, receipt, &role, content);
        }
        if is_tool_result_role(&role) {
            return Err(TrainingExampleError::at(
                "orphaned_tool_result",
                event.index,
                format!("`{role}` message arrived with no tool_result receipt naming its call"),
            ));
        }
        match role.as_str() {
            "assistant" => self.bind_assistant(event, content),
            "system" | "user" | "developer" => {
                if let Some(open) = self.open_calls.first() {
                    return Err(TrainingExampleError::at(
                        "unpaired_tool_call",
                        event.index,
                        format!(
                            "call {} ({}) from event {} has no tool result before this `{role}` turn",
                            open.id, open.name, open.event_index
                        ),
                    ));
                }
                self.messages.push(TrainingMessage {
                    role,
                    content,
                    ..TrainingMessage::default()
                });
                Ok(())
            }
            other => Err(TrainingExampleError::at(
                "unsupported_message_role",
                event.index,
                format!("message role `{other}` has no canonical training projection"),
            )),
        }
    }

    fn bind_tool_result(
        &mut self,
        event: &SourceEvent,
        receipt: ToolResultReceipt,
        role: &str,
        content: String,
    ) -> Result<(), TrainingExampleError> {
        // A native-channel result carries the id itself; when it does, the two
        // independent records must agree.
        if let Some(native_id) = native_tool_result_id(&event.value) {
            if native_id != receipt.call_id {
                return Err(TrainingExampleError::at(
                    "tool_result_identity_mismatch",
                    event.index,
                    format!(
                        "message names call {native_id} but its receipt names {}",
                        receipt.call_id
                    ),
                ));
            }
        }
        if self.declared_tool_format.is_none() {
            self.declared_tool_format.clone_from(&receipt.tool_format);
        }
        let position = self
            .open_calls
            .iter()
            .position(|open| open.id == receipt.call_id);
        let Some(position) = position else {
            return Err(TrainingExampleError::at(
                "orphaned_tool_result",
                event.index,
                format!(
                    "`{role}` message answers call {}, which no preceding assistant turn made \
                     (or which was already answered)",
                    receipt.call_id
                ),
            ));
        };
        if position != 0 {
            let expected = &self.open_calls[0];
            return Err(TrainingExampleError::at(
                "out_of_order_tool_result",
                event.index,
                format!(
                    "call {} was answered before {}, which the assistant requested first",
                    receipt.call_id, expected.id
                ),
            ));
        }
        let open = self.open_calls.remove(0);
        let name = if receipt.tool_name.is_empty() {
            open.name
        } else {
            receipt.tool_name
        };
        self.messages.push(TrainingMessage {
            role: "tool".to_string(),
            content,
            tool_calls: Vec::new(),
            tool_call_id: Some(receipt.call_id),
            name: Some(name),
        });
        Ok(())
    }

    /// The typed calls this assistant turn made.
    ///
    /// A turn that dispatched tools has a dispatch receipt per call; a turn
    /// that made none has no receipts. The two records must agree on which
    /// tools were called and in what order — they are captured independently,
    /// at the provider boundary and at the dispatch boundary, so agreement is
    /// real corroboration rather than a restatement. Ids come from dispatch
    /// because those are the ids results answer under; on a text-channel run
    /// the provider capture's synthetic ids belong to a different id space.
    fn calls_for_assistant_turn(
        &self,
        event: &SourceEvent,
        response: &PendingResponse,
    ) -> Result<Vec<TrainingToolCall>, TrainingExampleError> {
        let key = receipt_key(event);
        let dispatched = self.dispatched.get(&key).cloned().unwrap_or_default();
        let observed = &response.merged_calls;
        if dispatched.is_empty() {
            if observed.is_empty() {
                return Ok(Vec::new());
            }
            return Err(TrainingExampleError::at(
                "unpaired_tool_call",
                event.index,
                format!(
                    "the provider returned {} tool call(s) for response {} but none were \
                     dispatched, so no result can pair with them",
                    observed.len(),
                    response.call_id
                ),
            ));
        }
        let dispatched_names: Vec<&str> = dispatched
            .iter()
            .map(|call| call.function.name.as_str())
            .collect();
        let observed_names: Vec<&str> = observed
            .iter()
            .map(|call| call.function.name.as_str())
            .collect();
        if dispatched_names != observed_names {
            return Err(TrainingExampleError::at(
                "tool_call_mismatch",
                event.index,
                format!(
                    "dispatch recorded calls to [{}] but provider response {} recorded [{}]",
                    dispatched_names.join(", "),
                    response.call_id,
                    observed_names.join(", ")
                ),
            ));
        }
        Ok(dispatched)
    }

    fn bind_assistant(
        &mut self,
        event: &SourceEvent,
        content: String,
    ) -> Result<(), TrainingExampleError> {
        if let Some(open) = self.open_calls.first() {
            return Err(TrainingExampleError::at(
                "unpaired_tool_call",
                event.index,
                format!(
                    "call {} ({}) from event {} has no tool result before the next assistant turn",
                    open.id, open.name, open.event_index
                ),
            ));
        }
        let Some(response) = self.pending_response.take() else {
            return Err(TrainingExampleError::at(
                "assistant_message_without_response",
                event.index,
                "assistant message has no preceding provider_call_response to source its \
                 typed tool calls from",
            ));
        };
        // The assistant message's own structured calls (native channel) and the
        // response record's calls are two independent captures of one decision.
        // Disagreement means one of them is not describing this turn.
        let message_calls = assistant_message_tool_calls(&event.value, event.index)?;
        if !message_calls.is_empty() && message_calls != response.native_calls {
            return Err(TrainingExampleError::at(
                "tool_call_mismatch",
                event.index,
                format!(
                    "assistant message tool calls disagree with provider response {} (event {})",
                    response.call_id, response.event_index
                ),
            ));
        }
        // Identity comes from dispatch, which is the only record of the id a
        // result will answer under; the response capture is the independent
        // cross-check that the dispatched calls really are this turn's.
        let calls = self.calls_for_assistant_turn(event, &response)?;
        for call in &calls {
            if !self.catalog.names.is_empty() && !self.catalog.names.contains(&call.function.name) {
                return Err(TrainingExampleError::at(
                    "tool_outside_catalog",
                    event.index,
                    format!(
                        "assistant called `{}`, which the served tool catalog does not declare",
                        call.function.name
                    ),
                ));
            }
            self.open_calls.push(OpenCall {
                event_index: event.index,
                id: call.id.clone(),
                name: call.function.name.clone(),
            });
        }
        let mut seen = BTreeSet::new();
        for call in &calls {
            if !seen.insert(call.id.clone()) {
                return Err(TrainingExampleError::at(
                    "duplicate_tool_call_id",
                    event.index,
                    format!("assistant turn reuses tool call id {}", call.id),
                ));
            }
        }
        self.messages.push(TrainingMessage {
            role: "assistant".to_string(),
            content,
            tool_calls: calls,
            tool_call_id: None,
            name: None,
        });
        Ok(())
    }

    fn finish(
        mut self,
        run: &RunRecord,
        descriptor: &RunTranscriptArtifactDescriptor,
        transcript_path: &Path,
        events: &[SourceEvent],
    ) -> Result<AgentTrainingExample, TrainingExampleError> {
        if let Some(open) = self.open_calls.first() {
            return Err(TrainingExampleError::at(
                "unpaired_tool_call",
                open.event_index,
                format!("run ended with call {} ({}) unanswered", open.id, open.name),
            ));
        }
        // A receipt that no message claimed means the runtime recorded an
        // answer whose turn is missing from the stream.
        if let Some((_, receipt)) = self
            .receipts
            .iter()
            .find(|(key, _)| !self.consumed_receipts.contains(*key))
        {
            return Err(TrainingExampleError::at(
                "unbound_tool_result_receipt",
                receipt.event_index,
                format!(
                    "the tool_result receipt for call {} names a message that is not in the \
                     transcript",
                    receipt.call_id
                ),
            ));
        }
        if let Some(response) = self.pending_response.as_ref() {
            return Err(TrainingExampleError::at(
                "unbound_provider_response",
                response.event_index,
                format!(
                    "run ended with provider call {} that produced no assistant message",
                    response.call_id
                ),
            ));
        }
        if self.catalog_event_index.is_none() {
            return Err(TrainingExampleError::new(
                "missing_tool_catalog",
                "run never recorded a tool_schemas event, so the served catalog is unknown; \
                 inferring one from prompt prose or observed arguments would teach a schema \
                 the model never saw",
            ));
        }
        let session_id = self.session.require("missing_session_id")?;
        if let Some(expected) = self.request.session_id.as_deref() {
            if session_id != expected {
                return Err(TrainingExampleError::new(
                    "session_id_mismatch",
                    format!("transcript belongs to session {session_id}, not {expected}"),
                ));
            }
        }
        if let Some(recorded) = descriptor.session_id.as_deref() {
            if !recorded.is_empty() && recorded != session_id {
                return Err(TrainingExampleError::new(
                    "session_id_mismatch",
                    format!(
                        "descriptor names session {recorded} but the events belong to \
                         {session_id}"
                    ),
                ));
            }
        }
        let system = self.system.require("missing_system_prompt")?;
        self.messages.insert(
            0,
            TrainingMessage {
                role: "system".to_string(),
                content: system,
                ..TrainingMessage::default()
            },
        );
        // A run with no assistant turn is not a training example; emitting one
        // would put an empty row into a corpus that later reads as valid.
        if !self
            .messages
            .iter()
            .any(|message| message.role == "assistant")
        {
            return Err(TrainingExampleError::new(
                "empty_projection",
                "run recorded no assistant turn",
            ));
        }
        validate_training_example_pairing(&self.messages)
            .map_err(|error| TrainingExampleError::new(&error.kind, error.message))?;

        let first = events.first().expect("non-empty events");
        let last = events.last().expect("non-empty events");
        let example = AgentTrainingExample {
            schema_version: TRAINING_EXAMPLE_SCHEMA_VERSION.to_string(),
            messages: self.messages,
            tools: self.catalog.schemas,
            provenance: TrainingProvenance {
                run_id: run.id.clone(),
                session_id,
                // Only an unambiguous single-stage run can name its stage; a
                // multi-stage run's sidecar does not attribute provider calls
                // to a stage, and guessing one would be provenance fiction.
                stage_id: match run.stages.as_slice() {
                    [stage] => Some(stage.id.clone()),
                    _ => None,
                },
                provider: self.provider.require("missing_route")?,
                model: self.model.require("missing_route")?,
                route_policy: self.route_policy,
                declared_tool_format: self.declared_tool_format,
                effective_tool_format: self.effective_tool_format.require("missing_tool_format")?,
                tool_catalog_hash: self.catalog.hash.unwrap_or_default(),
                tool_catalog_content_hash: self.catalog.content_hash,
                terminal_status: descriptor.terminal_status.clone().unwrap_or_default(),
                usage: TrainingUsage {
                    provider_calls: self.provider_calls,
                    input_tokens: self.input_tokens,
                    output_tokens: self.output_tokens,
                },
                source: TrainingSource {
                    descriptor_schema_version: descriptor.schema_version.clone(),
                    transcript_path: transcript_path.to_string_lossy().into_owned(),
                    transcript_sha256: descriptor.sha256.clone(),
                    transcript_byte_len: descriptor.byte_len,
                    event_count: events.len(),
                    first_event_index: first.index,
                    last_event_index: last.index,
                    first_event_id: first.identity(),
                    last_event_id: last.identity(),
                },
            },
        };
        Ok(example)
    }
}

fn schema_name(schema: &JsonValue) -> Option<String> {
    schema
        .get("name")
        .or_else(|| {
            schema
                .get("function")
                .and_then(|function| function.get("name"))
        })
        .and_then(JsonValue::as_str)
        .map(str::to_string)
}

fn json_scalar_text(value: &JsonValue) -> String {
    match value {
        JsonValue::String(text) => text.clone(),
        JsonValue::Null => String::new(),
        other => other.to_string(),
    }
}

fn is_tool_result_role(role: &str) -> bool {
    matches!(role, "tool" | "tool_result")
}

fn native_tool_result_id(event: &JsonValue) -> Option<&str> {
    let body = event.get("message")?;
    body.get("tool_call_id")
        .or_else(|| body.get("tool_use_id"))
        .and_then(JsonValue::as_str)
        .filter(|value| !value.is_empty())
}

/// Flatten a message body's content into the text the model saw.
///
/// Content is a plain string on most turns and a content-block list on
/// multimodal or Anthropic-shaped turns. Non-text blocks (images) contribute
/// no text; they stay out of the projected string rather than being described
/// in prose, which would teach the model a caption it never received.
fn message_text(body: &JsonValue) -> String {
    match body.get("content") {
        Some(JsonValue::String(text)) => text.clone(),
        Some(JsonValue::Array(blocks)) => blocks
            .iter()
            .filter_map(|block| match block {
                JsonValue::String(text) => Some(text.clone()),
                JsonValue::Object(_) => block
                    .get("text")
                    .and_then(JsonValue::as_str)
                    .map(str::to_string),
                _ => None,
            })
            .collect(),
        _ => String::new(),
    }
}

fn assistant_message_tool_calls(
    event: &JsonValue,
    event_index: usize,
) -> Result<Vec<TrainingToolCall>, TrainingExampleError> {
    let Some(body) = event.get("message") else {
        return Ok(Vec::new());
    };
    let Some(calls) = body.get("tool_calls").and_then(JsonValue::as_array) else {
        return Ok(Vec::new());
    };
    calls
        .iter()
        .map(|call| normalize_tool_call(call, event_index))
        .collect()
}

fn tool_calls_from_array(
    event: &SourceEvent,
    key: &str,
) -> Result<Vec<TrainingToolCall>, TrainingExampleError> {
    let Some(calls) = event.value.get(key).and_then(JsonValue::as_array) else {
        return Ok(Vec::new());
    };
    calls
        .iter()
        .map(|call| normalize_tool_call(call, event.index))
        .collect()
}

/// Normalize the provider-shaped and text-parsed call captures into one
/// canonical structure. Both shapes are Harn-recorded, so the accepted key
/// spellings are a closed set — an unrecognised shape is a hard error, not a
/// silently empty call.
/// OpenAI-compatible providers serialise arguments as a JSON string; the
/// canonical example carries the decoded object so a consumer never needs a
/// second wire parser.
fn decode_arguments(
    raw: Option<JsonValue>,
    name: &str,
    event_index: usize,
) -> Result<JsonValue, TrainingExampleError> {
    match raw {
        None | Some(JsonValue::Null) => Ok(JsonValue::Object(serde_json::Map::new())),
        Some(JsonValue::String(text)) if text.trim().is_empty() => {
            Ok(JsonValue::Object(serde_json::Map::new()))
        }
        Some(JsonValue::String(text)) => serde_json::from_str(&text).map_err(|error| {
            TrainingExampleError::at(
                "malformed_tool_call",
                event_index,
                format!("call to `{name}` has unparseable JSON arguments: {error}"),
            )
        }),
        Some(other) => Ok(other),
    }
}

fn normalize_tool_call(
    call: &JsonValue,
    event_index: usize,
) -> Result<TrainingToolCall, TrainingExampleError> {
    let function = call.get("function");
    let name = function
        .and_then(|function| function.get("name"))
        .or_else(|| call.get("name"))
        .and_then(JsonValue::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            TrainingExampleError::at(
                "malformed_tool_call",
                event_index,
                "recorded tool call has no name",
            )
        })?;
    let id = call
        .get("id")
        .or_else(|| call.get("call_id"))
        .and_then(JsonValue::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            TrainingExampleError::at(
                "malformed_tool_call",
                event_index,
                format!("recorded call to `{name}` has no id, so no result can pair with it"),
            )
        })?;
    let arguments = decode_arguments(
        function
            .and_then(|function| function.get("arguments"))
            .or_else(|| call.get("arguments"))
            .or_else(|| call.get("args"))
            .cloned(),
        name,
        event_index,
    )?;
    Ok(TrainingToolCall {
        id: id.to_string(),
        call_type: "function".to_string(),
        function: TrainingToolFunction {
            name: name.to_string(),
            arguments,
        },
    })
}
