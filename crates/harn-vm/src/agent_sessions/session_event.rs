//! The one typed shape of an agent-session event in the session store.
//!
//! The journal writes a [`SessionEventPayload`] and every reader — run-record
//! projection, timeline, recap, restore, spend — decodes the same type, so an
//! emitter and a reader cannot quietly disagree about where a fact lives
//! (issue #6118 was exactly that drift, between hand-copied JSON pointers).
//!
//! Transcript events stay an open vocabulary: Harn scripts append their own
//! kinds and metadata. [`TranscriptKind`] names the kinds the runtime reads,
//! [`TranscriptMetadata`] types the fields it reads, and both carry everything
//! else through unchanged, so decoding is lossless and never refuses a row.
//!
//! Schema 2 tags every payload with its `record` and stamps
//! [`SESSION_EVENT_SCHEMA_HEADER`]. The transcript envelope keeps its schema-1
//! field names, so a schema-1 row is read by inferring its record from its
//! shape ([`SessionEventPayload::decode`]); nothing else differs.

use std::collections::BTreeMap;

use harn_session_store::{SessionEventKind, StoredEvent};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Number, Value};

/// Header naming the payload schema an agent-session event was written with.
pub(crate) const SESSION_EVENT_SCHEMA_HEADER: &str = "harn.agent_session_event.schema";
/// Schema 2: payloads carry a `record` tag. Schema 1 rows carry no header.
pub(crate) const SESSION_EVENT_SCHEMA_VERSION: &str = "2";

/// The payload of one durable agent-session event.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "record", rename_all = "snake_case")]
pub(crate) enum SessionEventPayload {
    /// A transcript event, and the provider message it added when it is one.
    Transcript(TranscriptRecord),
    /// The model-visible history was replaced, by compaction or truncation.
    Compaction(CompactionRecord),
    /// One previously added message was withdrawn.
    MessageRemoved(MessageRemovedRecord),
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct TranscriptRecord {
    #[serde(default)]
    pub transcript_event: Box<TranscriptEvent>,
    /// The provider message, verbatim: hydration hands it back to a provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_message: Option<Value>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct CompactionRecord {
    #[serde(default)]
    pub messages: Vec<Value>,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub source_event_ids: Vec<Option<String>>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct MessageRemovedRecord {
    pub source_event_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_message: Option<Value>,
}

impl SessionEventPayload {
    /// Decode a stored row. A schema-1 row has no `record` tag, so its record
    /// is inferred from its shape. `None` means the row was not written by the
    /// agent journal (another surface's custom event, a control word).
    pub(crate) fn decode(event: &StoredEvent) -> Option<Self> {
        if event.payload.get("record").is_some() {
            return serde_json::from_value(event.payload.clone()).ok();
        }
        let object = event.payload.as_object()?;
        match &event.kind {
            SessionEventKind::Compaction if object.contains_key("messages") => {
                serde_json::from_value(event.payload.clone())
                    .ok()
                    .map(Self::Compaction)
            }
            SessionEventKind::Custom { custom_type } if custom_type == "message_removed" => {
                serde_json::from_value(event.payload.clone())
                    .ok()
                    .map(Self::MessageRemoved)
            }
            _ if object.contains_key("transcript_event") || object.contains_key("raw_message") => {
                serde_json::from_value(event.payload.clone())
                    .ok()
                    .map(Self::Transcript)
            }
            _ => None,
        }
    }

    pub(crate) fn to_json(&self) -> Value {
        serde_json::to_value(self).expect("session event payload serializes")
    }

    /// The store discriminator this payload is filed under.
    pub(crate) fn store_kind(&self) -> SessionEventKind {
        match self {
            Self::Transcript(record) => record.store_kind(),
            Self::Compaction(_) => SessionEventKind::Compaction,
            Self::MessageRemoved(_) => SessionEventKind::Custom {
                custom_type: "message_removed".to_string(),
            },
        }
    }
}

/// Decode a stored row as a transcript record, or `None` for anything else.
pub(crate) fn transcript_record(event: &StoredEvent) -> Option<TranscriptRecord> {
    match SessionEventPayload::decode(event)? {
        SessionEventPayload::Transcript(record) => Some(record),
        _ => None,
    }
}

/// The kinds of transcript event the runtime reads. Everything else is
/// [`TranscriptKind::Other`], carried through by name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TranscriptKind {
    Message,
    LlmCall,
    ToolCall,
    ToolCallUpdate,
    ToolResult,
    AgentRunStarted,
    AgentRunTerminal,
    IterationStart,
    IterationEnd,
    LoopCheckpoint,
    SubAgentStart,
    ReasoningReceipt,
    Plan,
    PlanDocument,
    AssistantPublication,
    ProgressReported,
    TurnPhaseChanged,
    Other(String),
}

impl TranscriptKind {
    const NAMED: [(Self, &'static str); 17] = [
        (Self::Message, "message"),
        (Self::LlmCall, "llm_call"),
        (Self::ToolCall, "tool_call"),
        (Self::ToolCallUpdate, "tool_call_update"),
        (Self::ToolResult, "tool_result"),
        (Self::AgentRunStarted, "agent_run_started"),
        (Self::AgentRunTerminal, "agent_run_terminal"),
        (Self::IterationStart, "iteration_start"),
        (Self::IterationEnd, "iteration_end"),
        (Self::LoopCheckpoint, "loop_checkpoint"),
        (Self::SubAgentStart, "sub_agent_start"),
        (Self::ReasoningReceipt, "reasoning_receipt"),
        (Self::Plan, "plan"),
        (Self::PlanDocument, "plan_document"),
        (Self::AssistantPublication, "assistant_publication"),
        (Self::ProgressReported, "progress_reported"),
        (Self::TurnPhaseChanged, "turn_phase_changed"),
    ];

    pub(crate) fn from_wire(kind: &str) -> Self {
        Self::NAMED
            .iter()
            .find(|(_, name)| *name == kind)
            .map(|(variant, _)| variant.clone())
            .unwrap_or_else(|| Self::Other(kind.to_string()))
    }

    pub(crate) fn as_str(&self) -> &str {
        match self {
            Self::Other(kind) => kind,
            named => {
                Self::NAMED
                    .iter()
                    .find(|(variant, _)| variant == named)
                    .expect("every named transcript kind has a wire name")
                    .1
            }
        }
    }
}

/// One transcript event. Field names are the transcript envelope's own.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(into = "TranscriptEventWire")]
pub(crate) struct TranscriptEvent {
    pub id: Option<String>,
    /// `None` when the producer named no kind: a plain message, or an
    /// unlabeled audit row.
    pub kind: Option<TranscriptKind>,
    pub role: Option<String>,
    pub text: Option<String>,
    pub visibility: Option<String>,
    pub tool_call_id: Option<String>,
    pub input: Option<Value>,
    pub blocks: Option<Value>,
    pub metadata: Option<Box<TranscriptMetadata>>,
    /// Envelope fields the runtime does not read, kept verbatim.
    pub rest: Map<String, Value>,
}

#[derive(Default, Serialize, Deserialize)]
struct TranscriptEventWire {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    visibility: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    input: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    blocks: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    metadata: Option<Box<TranscriptMetadata>>,
    #[serde(flatten)]
    rest: Map<String, Value>,
}

impl From<TranscriptEventWire> for TranscriptEvent {
    fn from(wire: TranscriptEventWire) -> Self {
        Self {
            id: wire.id,
            kind: wire.kind.as_deref().map(TranscriptKind::from_wire),
            role: wire.role,
            text: wire.text,
            visibility: wire.visibility,
            tool_call_id: wire.tool_call_id,
            input: wire.input,
            blocks: wire.blocks,
            metadata: wire.metadata,
            rest: wire.rest,
        }
    }
}

impl<'de> Deserialize<'de> for TranscriptEvent {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Value::deserialize(deserializer).map(Self::from_json)
    }
}

impl From<TranscriptEvent> for TranscriptEventWire {
    fn from(event: TranscriptEvent) -> Self {
        Self {
            id: event.id,
            kind: event.kind.map(|kind| kind.as_str().to_string()),
            role: event.role,
            text: event.text,
            visibility: event.visibility,
            tool_call_id: event.tool_call_id,
            input: event.input,
            blocks: event.blocks,
            metadata: event.metadata,
            rest: event.rest,
        }
    }
}

impl TranscriptEvent {
    /// Decode a producer's transcript event. A malformed envelope keeps every
    /// field in [`Self::rest`] rather than being refused.
    pub(crate) fn from_json(value: Value) -> Self {
        match serde_json::from_value::<TranscriptEventWire>(value.clone()) {
            Ok(mut wire) => {
                if let Value::Object(original) = &value {
                    keep_explicit_nulls(original, &mut wire.rest);
                }
                wire.into()
            }
            Err(_) => match value {
                Value::Object(map) => {
                    let (mut wire, rest) = decode_valid_fields::<TranscriptEventWire>(map);
                    wire.rest.extend(rest);
                    wire.into()
                }
                other => Self {
                    rest: Map::from_iter([("value".to_string(), other)]),
                    ..Self::default()
                },
            },
        }
    }

    pub(crate) fn is(&self, kind: TranscriptKind) -> bool {
        self.kind.as_ref() == Some(&kind)
    }

    pub(crate) fn metadata(&self) -> &TranscriptMetadata {
        static EMPTY: std::sync::LazyLock<TranscriptMetadata> =
            std::sync::LazyLock::new(TranscriptMetadata::default);
        self.metadata.as_deref().unwrap_or(&EMPTY)
    }

    pub(crate) fn metadata_json(&self) -> Value {
        self.metadata
            .as_ref()
            .map(|metadata| serde_json::to_value(metadata).expect("metadata serializes"))
            .unwrap_or(Value::Null)
    }
}

/// Metadata the runtime reads from transcript events, typed once. Producers
/// set only the fields their kind carries; [`Self::rest`] keeps every other
/// field verbatim. A metadata object whose known fields have unexpected
/// types keeps those fields in `rest` while retaining its valid typed facts.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub(crate) struct TranscriptMetadata {
    #[serde(flatten)]
    pub known: KnownMetadata,
    #[serde(flatten)]
    pub rest: Map<String, Value>,
}

impl<'de> Deserialize<'de> for TranscriptMetadata {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Typed {
            #[serde(flatten)]
            known: KnownMetadata,
            #[serde(flatten)]
            rest: Map<String, Value>,
        }
        let value = Map::<String, Value>::deserialize(deserializer)?;
        Ok(
            match serde_json::from_value::<Typed>(Value::Object(value.clone())) {
                Ok(mut typed) => {
                    keep_explicit_nulls(&value, &mut typed.rest);
                    Self {
                        known: typed.known,
                        rest: typed.rest,
                    }
                }
                Err(_) => {
                    let (known, rest) = decode_valid_fields(value);
                    Self { known, rest }
                }
            },
        )
    }
}

/// Recover fields independently only after the normal typed decode fails.
/// Serde's owning type decides which fields it understands; no second field
/// registry is needed, and malformed values remain available for round trips.
fn decode_valid_fields<T>(original: Map<String, Value>) -> (T, Map<String, Value>)
where
    T: serde::de::DeserializeOwned + Serialize,
{
    let mut valid = Map::new();
    let mut rest = Map::new();
    for (key, value) in original {
        let field = Value::Object(Map::from_iter([(key.clone(), value.clone())]));
        let decoded = serde_json::from_value::<T>(field)
            .ok()
            .and_then(|typed| serde_json::to_value(typed).ok());
        match decoded {
            Some(Value::Object(fields)) if fields.contains_key(&key) && !value.is_null() => {
                valid.extend(fields);
            }
            _ => {
                rest.insert(key, value);
            }
        }
    }
    let typed = serde_json::from_value(Value::Object(valid))
        .expect("individually decoded fields form a valid record");
    (typed, rest)
}

/// Every metadata field a runtime reader consumes. Numbers stay JSON numbers
/// so a value is written back exactly as it was produced; the accessors read
/// them leniently (a token count serialized as a float is still a count).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct KnownMetadata {
    // llm_call
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<Number>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<Number>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_read_tokens: Option<Number>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_write_tokens: Option<Number>,
    /// Whether the cache counts are a provider measurement. Absent on calls
    /// recorded before the field existed, which read as not reported.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_counts_reported: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<Number>,
    /// Measured subtotal and physical-attempt certainty for this call.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub known_cost_usd: Option<Number>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unpriced_calls: Option<Number>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage_unknown_calls: Option<Number>,
    /// Whether provider usage was reported or remains `"unknown"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accounting_status: Option<String>,
    /// Provider requests behind one logical call. Absent on calls recorded
    /// before the field existed, which is distinct from a recorded 1.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_attempts: Option<ProviderAttempts>,

    // tool_call, tool_call_update, tool_result
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    /// Arguments as the model produced them, before host normalization.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw_input: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<Value>,
    /// Tool lifecycle: `pending`, `in_progress`, `completed`, `failed`,
    /// `rejected`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<Number>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,

    // agent_run_started
    /// VM-owned execution identity stamped on the invocation boundary.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_receipts_reported: Option<bool>,

    // agent_run_terminal
    #[serde(skip_serializing_if = "Option::is_none")]
    pub final_status: Option<String>,
    /// Why the loop stopped; orthogonal to [`Self::final_status`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminal_class: Option<String>,
    /// Structured diagnostics or legacy text.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminal: Option<TerminalRecord>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub adaptive_budget: Option<Value>,

    // loop_checkpoint
    /// Which loop phase the checkpoint marks, e.g. `"iteration_start"`.
    #[serde(rename = "kind", skip_serializing_if = "Option::is_none")]
    pub checkpoint_kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub iteration: Option<Number>,

    // sub_agent_start
    #[serde(skip_serializing_if = "Option::is_none")]
    pub child_session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub child_run_id: Option<String>,

    // reasoning_receipt
    #[serde(skip_serializing_if = "Option::is_none")]
    pub receipt: Option<Value>,

    // plan_document
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan_document: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan_document_event: Option<Value>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct ProviderAttempts {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total: Option<Number>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rate_limited: Option<Number>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub empty_completion: Option<Number>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub other: Option<Number>,
    #[serde(flatten)]
    pub rest: Map<String, Value>,
}

/// The producer-owned terminal outcome recorded by `agent_session_finalize`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct TerminalRecord {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(rename = "terminalClass", skip_serializing_if = "Option::is_none")]
    pub terminal_class: Option<String>,
    #[serde(flatten)]
    pub rest: Map<String, Value>,
}

/// A trimmed, non-empty string. Blank is the producer declining to say.
pub(crate) fn nonblank(value: &Option<String>) -> Option<String> {
    value
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

/// A JSON number read as an integer; a float is truncated, because a count
/// is integral in meaning however it was serialized.
pub(crate) fn integer(value: &Option<Number>) -> Option<i64> {
    let number = value.as_ref()?;
    number
        .as_i64()
        .or_else(|| number.as_f64().map(|number| number as i64))
}

pub(crate) fn float(value: &Option<Number>) -> Option<f64> {
    value.as_ref()?.as_f64()
}

impl TranscriptRecord {
    pub(crate) fn new(transcript_event: Value, raw_message: Option<Value>) -> Self {
        Self {
            transcript_event: Box::new(TranscriptEvent::from_json(transcript_event)),
            raw_message,
        }
    }

    fn store_kind(&self) -> SessionEventKind {
        if self.harn_result_fact("kind").and_then(Value::as_str)
            == Some(TranscriptKind::ToolResult.as_str())
        {
            return SessionEventKind::ToolResult;
        }
        let event = &self.transcript_event;
        let message_path = self.raw_message.is_some();
        let kind = match &event.kind {
            None if message_path => SessionEventKind::Message,
            None => SessionEventKind::Custom {
                custom_type: "transcript_event".to_string(),
            },
            Some(TranscriptKind::Message) if message_path => SessionEventKind::Message,
            Some(TranscriptKind::ToolCall) => SessionEventKind::ToolCall,
            Some(TranscriptKind::ToolResult) => SessionEventKind::ToolResult,
            Some(TranscriptKind::Plan | TranscriptKind::PlanDocument) => SessionEventKind::Plan,
            // The store owns the remaining named variants (compaction,
            // reminders, permission decisions, control words); everything
            // else is filed under its own name.
            Some(kind) => match SessionEventKind::from_discriminator(kind.as_str()) {
                Some(SessionEventKind::Message) | None => SessionEventKind::Custom {
                    custom_type: kind.as_str().to_string(),
                },
                Some(named) => named,
            },
        };
        // A narrated native call has a public message as well as tool
        // lifecycle events. Converting that message into a tool row lets the
        // result merge erase its text and chronological position.
        let has_public_text =
            nonblank(&event.visibility).as_deref() == Some("public") && self.text().is_some();
        if matches!(kind, SessionEventKind::Message)
            && self.journal_tool_call_id().is_some()
            && !has_public_text
        {
            return SessionEventKind::ToolCall;
        }
        kind
    }

    fn raw(&self, key: &str) -> Option<&Value> {
        self.raw_message.as_ref()?.get(key)
    }

    fn raw_str(&self, key: &str) -> Option<String> {
        self.raw(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    }

    /// The provider's first tool call, when the provider message carries any.
    fn first_provider_call(&self) -> Option<&Value> {
        self.raw("tool_calls")?.as_array()?.first()
    }

    /// Harn's storage-only facts on a provider tool result, stripped before
    /// provider egress.
    fn harn_result_fact(&self, key: &str) -> Option<&Value> {
        self.raw("_harn")?.get(key)
    }

    pub(crate) fn kind(&self) -> Option<&TranscriptKind> {
        self.transcript_event.kind.as_ref()
    }

    pub(crate) fn metadata(&self) -> &KnownMetadata {
        &self.transcript_event.metadata().known
    }

    /// Human-visible text: the transcript envelope's, falling back to the
    /// provider message it was built from.
    pub(crate) fn text(&self) -> Option<String> {
        nonblank(&self.transcript_event.text).or_else(|| self.raw_str("content"))
    }

    /// Speaker, as the envelope or the provider message records it.
    pub(crate) fn role(&self) -> Option<String> {
        nonblank(&self.transcript_event.role).or_else(|| self.raw_str("role"))
    }

    /// Envelope visibility. Older provider messages carry none; readers
    /// decide whether that absence is safe. A non-public value never is.
    pub(crate) fn visibility(&self) -> Option<String> {
        nonblank(&self.transcript_event.visibility)
    }

    pub(crate) fn transcript_event_id(&self) -> Option<String> {
        nonblank(&self.transcript_event.id)
    }

    /// Tool name across the envelope and both provider-message placements.
    pub(crate) fn tool_name(&self) -> Option<String> {
        nonblank(&self.metadata().tool_name)
            .or_else(|| self.raw_str("name"))
            .or_else(|| self.harn_result_fact("tool_name").and_then(from_str))
            .or_else(|| {
                self.first_provider_call()
                    .and_then(|call| call.get("name"))
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|name| !name.is_empty())
                    .map(str::to_string)
            })
    }

    /// The name this row itself declares, on the envelope, its metadata, or
    /// the provider message, without reading the provider's call list.
    pub(crate) fn declared_tool_name(&self) -> Option<String> {
        let named = |map: &Map<String, Value>| {
            ["tool_name", "name"]
                .iter()
                .find_map(|key| map.get(*key).and_then(from_str))
        };
        named(&self.transcript_event.rest)
            .or_else(|| {
                self.transcript_event
                    .metadata()
                    .known
                    .tool_name
                    .clone()
                    .filter(|name| !name.trim().is_empty())
            })
            .or_else(|| named(&self.transcript_event.metadata().rest))
            .or_else(|| self.raw_message.as_ref()?.as_object().and_then(named))
            .or_else(|| self.harn_result_fact("tool_name").and_then(from_str))
    }

    /// The provider's own entry for `tool_call_id` in an assistant message's
    /// `tool_calls`.
    pub(crate) fn provider_tool_call(&self, tool_call_id: &str) -> Option<&Value> {
        self.raw("tool_calls")?
            .as_array()?
            .iter()
            .find(|call| call.get("id").and_then(Value::as_str) == Some(tool_call_id))
    }

    /// Tool arguments, preferring the most specific input recorded and
    /// falling back to the model's pre-normalization copy.
    pub(crate) fn tool_input(&self) -> Option<Value> {
        let metadata = self.metadata();
        metadata
            .input
            .clone()
            .or_else(|| self.raw("input").cloned())
            .or_else(|| {
                self.first_provider_call()
                    .and_then(|call| call.get("arguments"))
                    .cloned()
            })
            .or_else(|| metadata.raw_input.clone())
    }

    /// Tool output, preferring the structured copy over rendered text.
    pub(crate) fn tool_output(&self) -> Option<Value> {
        self.metadata()
            .output
            .clone()
            .or_else(|| self.raw("content").cloned())
            .or_else(|| self.transcript_event.text.clone().map(Value::String))
    }

    /// Whether the envelope marks this a tool error.
    pub(crate) fn audit_is_error(&self) -> bool {
        self.metadata().is_error == Some(true)
    }

    /// Whether the envelope or the provider-neutral result marks an error.
    pub(crate) fn is_error(&self) -> bool {
        self.audit_is_error()
            || self.raw("is_error").and_then(Value::as_bool) == Some(true)
            || self.harn_result_fact("outcome").and_then(Value::as_str) == Some("error")
    }

    /// The call identity: the journal header, the envelope's metadata, or the
    /// provider-neutral result fact, in that order.
    pub(crate) fn tool_call_id(&self, headers: &BTreeMap<String, String>) -> Option<String> {
        headers
            .get("tool_call_id")
            .cloned()
            .or_else(|| nonblank(&self.metadata().tool_call_id))
            .or_else(|| {
                self.harn_result_fact("tool_call_id")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
    }

    /// The identity the journal files a row under when it writes it: the
    /// provider message's id fields, then the envelope's, then its metadata's.
    /// Whether a row is something a person can fork or rewind to: a message, a
    /// tool call or result, or a published answer. Run, goal and other
    /// bookkeeping rows are not, so they carry no source identity.
    pub(crate) fn is_history_position(&self) -> bool {
        matches!(
            self.store_kind(),
            SessionEventKind::Message | SessionEventKind::ToolCall | SessionEventKind::ToolResult
        ) || self
            .transcript_event
            .is(TranscriptKind::AssistantPublication)
    }

    pub(crate) fn journal_tool_call_id(&self) -> Option<String> {
        fn from_value(value: &Value) -> Option<String> {
            ["tool_call_id", "tool_use_id", "toolUseId"]
                .iter()
                .find_map(|key| value.get(key).and_then(from_str))
                .or_else(|| {
                    value
                        .get("tool_calls")
                        .and_then(Value::as_array)
                        .filter(|calls| calls.len() == 1)
                        .and_then(|calls| calls.first())
                        .and_then(|call| call.get("id"))
                        .and_then(from_str)
                })
        }
        if let Some(id) = self.raw_message.as_ref().and_then(from_value) {
            return Some(id);
        }
        let envelope = serde_json::to_value(&self.transcript_event).unwrap_or(Value::Null);
        let metadata = envelope.get("metadata");
        if let Some(id) = from_value(&envelope) {
            return Some(id);
        }
        metadata
            .and_then(from_value)
            .or_else(|| self.harn_result_fact("tool_call_id").and_then(from_str))
    }

    /// The provider message's own id, which the journal files as `message_id`.
    pub(crate) fn message_id(&self) -> Option<String> {
        self.raw_str("message_id")
            .or_else(|| self.raw_str("messageId"))
    }

    /// Producer-owned facts the dispatcher projected onto a tool result,
    /// including its declared `mutation_status` and `changed_paths`.
    pub(crate) fn tool_result_data(&self) -> Option<&Value> {
        self.harn_result_fact("data")
    }

    /// The typed deterministic postcondition a tool producer emitted, the only
    /// verification fact a reader accepts.
    pub(crate) fn tool_verification(&self) -> Option<&Value> {
        self.tool_result_data()?.get("verification")
    }
}

/// A typed `Option` field reads an explicit `null` as absent and would drop
/// it on write. Keep the producer's `null` in `rest` so a write is lossless.
fn keep_explicit_nulls(original: &Map<String, Value>, rest: &mut Map<String, Value>) {
    for (key, value) in original {
        if value.is_null() {
            rest.entry(key.clone()).or_insert(Value::Null);
        }
    }
}

fn from_str(value: &Value) -> Option<String> {
    value
        .as_str()
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
}

#[cfg(test)]
#[path = "session_event_tests.rs"]
mod tests;
