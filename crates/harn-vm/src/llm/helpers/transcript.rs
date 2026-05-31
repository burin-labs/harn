use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

use crate::event_log::{active_event_log, EventLog, LogEvent, Topic};
use crate::value::{VmError, VmValue};

use super::blocks::{
    default_visibility_for_role, normalize_message_blocks, overall_visibility, render_blocks_text,
};
use super::messages::json_messages_to_vm;
use super::{vm_value_to_json, TRANSCRIPT_ASSET_TYPE, TRANSCRIPT_TYPE, TRANSCRIPT_VERSION};

/// Canonical `kind` for a [`SystemReminder`] transcript event.
pub(crate) const SYSTEM_REMINDER_EVENT_KIND: &str = "system_reminder";
pub(crate) const REMINDER_LIFECYCLE_TOPIC: &str = "transcript.reminder.lifecycle";
pub(crate) const REMINDER_INJECTED_EVENT_KIND: &str = "transcript.reminder.injected";
pub(crate) const REMINDER_FIRED_EVENT_KIND: &str = "transcript.reminder.fired";
pub(crate) const REMINDER_DEDUPED_EVENT_KIND: &str = "transcript.reminder.deduped";
pub(crate) const REMINDER_EXPIRED_EVENT_KIND: &str = "transcript.reminder.expired";
pub(crate) const REMINDER_DROPPED_EVENT_KIND: &str = "transcript.reminder.dropped";
pub(crate) const REMINDER_INHERITED_EVENT_KIND: &str = "transcript.reminder.inherited";
pub(crate) const REMINDER_PROVIDER_EVALUATED_EVENT_KIND: &str =
    "transcript.reminder.provider_evaluated";
pub(crate) const SUSPENSION_EVENT_KIND: &str = "suspension";
pub(crate) const RESUMPTION_EVENT_KIND: &str = "resumption";
pub(crate) const DRAIN_DECISION_EVENT_KIND: &str = "drain_decision";

#[allow(dead_code)] // schema surface reserved for lifecycle producers
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SuspensionInitiator {
    #[serde(rename = "self")]
    Self_,
    Parent,
    Operator,
    Triggered,
}

#[allow(dead_code)] // schema surface reserved for lifecycle producers
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResumptionInitiator {
    Parent,
    Operator,
    Triggered,
    DrainAgent,
}

#[allow(dead_code)] // schema surface reserved for lifecycle producers
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DrainDecisionItemCategory {
    SuspendedSubagent,
    QueuedTrigger,
    PartialHandoff,
    InFlightLlmCall,
}

#[allow(dead_code)] // schema surface reserved for lifecycle producers
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DrainDecisionAction {
    Resume,
    Cancel,
    Handoff,
    Acknowledge,
    Defer,
    Wait,
    Finalize,
}

#[allow(dead_code)] // schema surface reserved for lifecycle producers
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Suspension {
    pub handle: String,
    pub initiator: SuspensionInitiator,
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conditions: Option<JsonValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume_by_mechanism: Option<String>,
    pub suspended_at_turn: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub span_id: Option<String>,
}

#[allow(dead_code)] // schema surface reserved for lifecycle producers
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TriggerMatch {
    pub source: String,
    pub event_id: String,
    pub filter_summary: String,
}

#[allow(dead_code)] // schema surface reserved for lifecycle producers
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Resumption {
    pub handle: String,
    pub initiator: ResumptionInitiator,
    pub initiator_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<JsonValue>,
    pub input_hash: String,
    pub continue_transcript: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_match: Option<TriggerMatch>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub linked_suspension_span_id: Option<String>,
    pub resumed_at_turn: i64,
}

#[allow(dead_code)] // schema surface reserved for lifecycle producers
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DrainDecisionItem {
    pub category: DrainDecisionItemCategory,
    pub id: String,
    pub summary: String,
}

#[allow(dead_code)] // schema surface reserved for lifecycle producers
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DrainDecision {
    pub pipeline_id: String,
    pub item: DrainDecisionItem,
    pub action: DrainDecisionAction,
    pub reason: String,
    pub decided_at_turn: i64,
    pub settlement_agent_session_id: String,
}

/// How a [`SystemReminder`] propagates across sub-agent handoffs.
///
/// Matches the per-reminder policy in the R-01 epic (#1815): `all` rides
/// every spawned sub-agent transcript; `session` stays inside the
/// originating session tree; `none` is opaque to children.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReminderPropagate {
    All,
    Session,
    None,
}

impl ReminderPropagate {
    #[allow(dead_code)] // reserved for the R-02+ wire renderers
    pub fn as_str(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Session => "session",
            Self::None => "none",
        }
    }
}

/// Preferred provider rendering for a [`SystemReminder`]. Final wire
/// role is decided at render time by the R-06 capability dispatch — this
/// is a hint, not a binding.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReminderRoleHint {
    System,
    Developer,
    UserBlock,
    EphemeralCache,
}

impl ReminderRoleHint {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Developer => "developer",
            Self::UserBlock => "user_block",
            Self::EphemeralCache => "ephemeral_cache",
        }
    }
}

/// Producer of a [`SystemReminder`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReminderSource {
    StdlibProvider,
    Hook,
    Bridge,
    InPipeline,
    Inherited,
}

impl ReminderSource {
    #[allow(dead_code)] // reserved for the R-02+ wire renderers
    pub fn as_str(self) -> &'static str {
        match self {
            Self::StdlibProvider => "stdlib_provider",
            Self::Hook => "hook",
            Self::Bridge => "bridge",
            Self::InPipeline => "in_pipeline",
            Self::Inherited => "inherited",
        }
    }
}

/// Lifecycle envelope of a `system_reminder` transcript event.
///
/// Serializes to the JSON shape under the `reminder` key in the
/// transcript event:
///
/// ```json
/// {
///   "kind": "system_reminder",
///   "reminder": {
///     "id": "0190…",
///     "tags": ["token_pressure"],
///     "dedupe_key": "token_pressure",
///     "ttl_turns": 3,
///     "preserve_on_compact": true,
///     "propagate": "session",
///     "role_hint": "system",
///     "source": "stdlib_provider",
///     "body": "Approaching context window cap.",
///     "fired_at_turn": 4
///   }
/// }
/// ```
///
/// `tags`, `body`, `propagate`, `role_hint`, `source`, `fired_at_turn`,
/// and `preserve_on_compact` are required wire fields. `dedupe_key` and
/// `ttl_turns` are optional — `dedupe_key: None` means each reminder is
/// retained independently, and `ttl_turns: None` means "persist until
/// removed or compacted away."
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SystemReminder {
    pub id: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dedupe_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttl_turns: Option<i64>,
    pub preserve_on_compact: bool,
    pub propagate: ReminderPropagate,
    pub role_hint: ReminderRoleHint,
    pub source: ReminderSource,
    pub body: String,
    pub fired_at_turn: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub originating_agent_id: Option<String>,
}

impl SystemReminder {
    /// Convenience: build a reminder with reasonable defaults for the
    /// body/source/turn, leaving the rest at protocol defaults.
    #[allow(dead_code)] // reserved for the R-02+ stdlib reminder providers
    pub fn new(body: impl Into<String>, source: ReminderSource, fired_at_turn: i64) -> Self {
        Self {
            id: uuid::Uuid::now_v7().to_string(),
            tags: Vec::new(),
            dedupe_key: None,
            ttl_turns: None,
            preserve_on_compact: false,
            propagate: ReminderPropagate::Session,
            role_hint: ReminderRoleHint::System,
            source,
            body: body.into(),
            fired_at_turn,
            originating_agent_id: None,
        }
    }
}

pub(crate) fn transcript_message_list(
    transcript: &BTreeMap<String, VmValue>,
) -> Result<Vec<VmValue>, VmError> {
    match transcript.get("messages") {
        Some(VmValue::List(list)) => Ok((**list).clone()),
        Some(_) => Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
            "transcript.messages must be a list",
        )))),
        None => Ok(Vec::new()),
    }
}

pub(crate) fn transcript_asset_list(
    transcript: &BTreeMap<String, VmValue>,
) -> Result<Vec<VmValue>, VmError> {
    match transcript.get("assets") {
        Some(VmValue::List(list)) => Ok((**list).clone()),
        Some(_) => Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
            "transcript.assets must be a list",
        )))),
        None => Ok(Vec::new()),
    }
}

fn transcript_string_field(transcript: &BTreeMap<String, VmValue>, key: &str) -> Option<String> {
    transcript.get(key).and_then(|v| match v {
        VmValue::String(s) if !s.is_empty() => Some(s.to_string()),
        _ => None,
    })
}

pub(crate) fn transcript_summary_text(transcript: &BTreeMap<String, VmValue>) -> Option<String> {
    transcript_string_field(transcript, "summary")
}

pub(crate) fn transcript_id(transcript: &BTreeMap<String, VmValue>) -> Option<String> {
    transcript_string_field(transcript, "id")
}

pub(crate) fn new_transcript_with(
    id: Option<String>,
    messages: Vec<VmValue>,
    summary: Option<String>,
    metadata: Option<VmValue>,
) -> VmValue {
    new_transcript_with_events(
        id,
        messages,
        summary,
        metadata,
        Vec::new(),
        Vec::new(),
        None,
    )
}

pub(crate) fn new_transcript_with_events(
    id: Option<String>,
    messages: Vec<VmValue>,
    summary: Option<String>,
    metadata: Option<VmValue>,
    extra_events: Vec<VmValue>,
    assets: Vec<VmValue>,
    state: Option<&str>,
) -> VmValue {
    new_transcript_with_event_prefix(
        id,
        messages,
        summary,
        metadata,
        Vec::new(),
        extra_events,
        assets,
        state,
    )
}

pub(crate) fn new_transcript_with_event_prefix(
    id: Option<String>,
    messages: Vec<VmValue>,
    summary: Option<String>,
    metadata: Option<VmValue>,
    prefix_events: Vec<VmValue>,
    extra_events: Vec<VmValue>,
    assets: Vec<VmValue>,
    state: Option<&str>,
) -> VmValue {
    let mut transcript = BTreeMap::new();
    let mut events = prefix_events;
    events.extend(transcript_events_from_messages(&messages));
    events.extend(extra_events);
    transcript.insert(
        "_type".to_string(),
        VmValue::String(std::sync::Arc::from(TRANSCRIPT_TYPE)),
    );
    transcript.insert("version".to_string(), VmValue::Int(TRANSCRIPT_VERSION));
    transcript.insert(
        "id".to_string(),
        VmValue::String(std::sync::Arc::from(
            id.unwrap_or_else(|| uuid::Uuid::now_v7().to_string()),
        )),
    );
    transcript.insert(
        "messages".to_string(),
        VmValue::List(std::sync::Arc::new(messages)),
    );
    transcript.insert(
        "events".to_string(),
        VmValue::List(std::sync::Arc::new(events)),
    );
    transcript.insert(
        "assets".to_string(),
        VmValue::List(std::sync::Arc::new(assets)),
    );
    if let Some(summary) = summary {
        transcript.insert(
            "summary".to_string(),
            VmValue::String(std::sync::Arc::from(summary)),
        );
    }
    if let Some(metadata) = metadata {
        transcript.insert("metadata".to_string(), metadata);
    }
    if let Some(state) = state {
        transcript.insert(
            "state".to_string(),
            VmValue::String(std::sync::Arc::from(state)),
        );
    }
    VmValue::Dict(std::sync::Arc::new(transcript))
}

pub(crate) fn transcript_event_from_message(message: &VmValue) -> VmValue {
    let dict = message.as_dict().cloned().unwrap_or_default();
    // Reminder-shaped messages (kind: "system_reminder", reminder: {...})
    // surface as typed reminder events so downstream consumers see the
    // canonical envelope instead of an opaque message-shaped event.
    if dict.get("kind").map(|v| v.display()).as_deref() == Some(SYSTEM_REMINDER_EVENT_KIND) {
        if let Some(reminder) = dict.get("reminder") {
            return transcript_reminder_event_from_value(reminder);
        }
    }
    if dict.get("kind").map(|v| v.display()).as_deref() == Some(SUSPENSION_EVENT_KIND) {
        if let Some(suspension) = dict.get("suspension") {
            return transcript_suspension_event_from_value(suspension);
        }
    }
    if dict.get("kind").map(|v| v.display()).as_deref() == Some(RESUMPTION_EVENT_KIND) {
        if let Some(resumption) = dict.get("resumption") {
            return transcript_resumption_event_from_value(resumption);
        }
    }
    if dict.get("kind").map(|v| v.display()).as_deref() == Some(DRAIN_DECISION_EVENT_KIND) {
        if let Some(drain) = dict.get("drain") {
            return transcript_drain_decision_event_from_value(drain);
        }
    }
    let role = dict
        .get("role")
        .map(|v| v.display())
        .unwrap_or_else(|| "user".to_string());
    let blocks = normalize_message_blocks(dict.get("content"), &role);
    let text = render_blocks_text(&blocks);
    let visibility = overall_visibility(&blocks, default_visibility_for_role(&role));
    let kind = if role == "tool_result" {
        "tool_result"
    } else {
        "message"
    };
    let mut event = BTreeMap::new();
    event.insert(
        "id".to_string(),
        VmValue::String(std::sync::Arc::from(uuid::Uuid::now_v7().to_string())),
    );
    event.insert(
        "kind".to_string(),
        VmValue::String(std::sync::Arc::from(kind)),
    );
    event.insert(
        "role".to_string(),
        VmValue::String(std::sync::Arc::from(role.as_str())),
    );
    event.insert(
        "visibility".to_string(),
        VmValue::String(std::sync::Arc::from(visibility)),
    );
    event.insert(
        "text".to_string(),
        VmValue::String(std::sync::Arc::from(text)),
    );
    event.insert(
        "blocks".to_string(),
        VmValue::List(std::sync::Arc::new(blocks)),
    );
    VmValue::Dict(std::sync::Arc::new(event))
}

pub(crate) fn transcript_events_from_messages(messages: &[VmValue]) -> Vec<VmValue> {
    messages.iter().map(transcript_event_from_message).collect()
}

#[cfg(test)]
pub(crate) fn transcript_to_vm_with_events(
    id: Option<String>,
    summary: Option<String>,
    metadata: Option<serde_json::Value>,
    messages: &[serde_json::Value],
    extra_events: Vec<VmValue>,
    assets: Vec<VmValue>,
    state: Option<&str>,
) -> VmValue {
    let metadata_vm = metadata.as_ref().map(crate::stdlib::json_to_vm_value);
    new_transcript_with_events(
        id,
        json_messages_to_vm(messages),
        summary,
        metadata_vm,
        extra_events,
        assets,
        state,
    )
}

pub(crate) fn transcript_to_vm_with_event_prefix(
    id: Option<String>,
    summary: Option<String>,
    metadata: Option<serde_json::Value>,
    messages: &[serde_json::Value],
    prefix_events: Vec<VmValue>,
    extra_events: Vec<VmValue>,
    assets: Vec<VmValue>,
    state: Option<&str>,
) -> VmValue {
    let metadata_vm = metadata.as_ref().map(crate::stdlib::json_to_vm_value);
    new_transcript_with_event_prefix(
        id,
        json_messages_to_vm(messages),
        summary,
        metadata_vm,
        prefix_events,
        extra_events,
        assets,
        state,
    )
}

pub(crate) fn transcript_event(
    kind: &str,
    role: &str,
    visibility: &str,
    text: &str,
    metadata: Option<serde_json::Value>,
) -> VmValue {
    let mut event = BTreeMap::new();
    event.insert(
        "id".to_string(),
        VmValue::String(std::sync::Arc::from(uuid::Uuid::now_v7().to_string())),
    );
    event.insert(
        "kind".to_string(),
        VmValue::String(std::sync::Arc::from(kind)),
    );
    event.insert(
        "role".to_string(),
        VmValue::String(std::sync::Arc::from(role)),
    );
    event.insert(
        "visibility".to_string(),
        VmValue::String(std::sync::Arc::from(visibility)),
    );
    event.insert(
        "text".to_string(),
        VmValue::String(std::sync::Arc::from(text)),
    );
    event.insert(
        "blocks".to_string(),
        VmValue::List(std::sync::Arc::new(vec![VmValue::Dict(
            std::sync::Arc::new(BTreeMap::from([
                (
                    "type".to_string(),
                    VmValue::String(std::sync::Arc::from("text")),
                ),
                (
                    "text".to_string(),
                    VmValue::String(std::sync::Arc::from(text)),
                ),
                (
                    "visibility".to_string(),
                    VmValue::String(std::sync::Arc::from(visibility)),
                ),
            ])),
        )])),
    );
    if let Some(metadata) = metadata {
        event.insert(
            "metadata".to_string(),
            crate::stdlib::json_to_vm_value(&metadata),
        );
    }
    VmValue::Dict(std::sync::Arc::new(event))
}

pub(crate) fn normalize_transcript_asset(value: &VmValue) -> VmValue {
    let mut asset = value.as_dict().cloned().unwrap_or_default();
    asset.insert(
        "_type".to_string(),
        VmValue::String(std::sync::Arc::from(TRANSCRIPT_ASSET_TYPE)),
    );
    if !asset.contains_key("id") {
        asset.insert(
            "id".to_string(),
            VmValue::String(std::sync::Arc::from(uuid::Uuid::now_v7().to_string())),
        );
    }
    if !asset.contains_key("kind") {
        asset.insert(
            "kind".to_string(),
            VmValue::String(std::sync::Arc::from("blob")),
        );
    }
    if !asset.contains_key("visibility") {
        asset.insert(
            "visibility".to_string(),
            VmValue::String(std::sync::Arc::from("internal")),
        );
    }
    if value.as_dict().is_none() {
        asset.insert(
            "storage".to_string(),
            VmValue::Dict(std::sync::Arc::new(BTreeMap::from([(
                "path".to_string(),
                VmValue::String(std::sync::Arc::from(value.display())),
            )]))),
        );
    }
    VmValue::Dict(std::sync::Arc::new(asset))
}

pub(crate) fn is_transcript_value(value: &VmValue) -> bool {
    value
        .as_dict()
        .and_then(|d| d.get("_type"))
        .map(|v| v.display())
        .as_deref()
        == Some(TRANSCRIPT_TYPE)
}

/// Build a `system_reminder` transcript event from a typed
/// [`SystemReminder`] envelope. The returned event shares the standard
/// `{ id, kind, role, visibility, text, blocks }` shape with every
/// other transcript event so existing consumers ignore it cleanly, and
/// additionally carries the lifecycle envelope under `reminder` plus a
/// flattened `metadata` block for hosts that key off `event.metadata`.
pub(crate) fn transcript_reminder_event(reminder: &SystemReminder) -> VmValue {
    let reminder_json = serde_json::to_value(reminder).unwrap_or(serde_json::Value::Null);
    // `system_reminder` events are public by default — they're intended
    // to nudge the model on the next turn — but never duplicate into
    // the durable message list, so they ride on the event log only.
    let envelope = transcript_event(
        SYSTEM_REMINDER_EVENT_KIND,
        reminder.role_hint.as_str(),
        "public",
        reminder.body.as_str(),
        Some(reminder_json.clone()),
    );
    let mut event = envelope.as_dict().cloned().unwrap_or_default();
    // The shared `transcript_event` envelope already wrote the lifecycle
    // JSON under `metadata`. The typed `reminder` slot is the canonical
    // home for the lifecycle payload; `metadata` is the back-compat
    // mirror observers already key off.
    event.insert(
        "reminder".to_string(),
        crate::stdlib::json_to_vm_value(&reminder_json),
    );
    VmValue::Dict(std::sync::Arc::new(event))
}

#[derive(Clone, Debug, Default)]
pub(crate) struct ReminderPostTurnReport {
    pub transcript: Option<VmValue>,
    pub decremented_count: usize,
    pub expired: Vec<SystemReminder>,
    pub remaining_count: usize,
}

pub(crate) fn apply_reminder_post_turn(transcript: &VmValue, turn: i64) -> ReminderPostTurnReport {
    let Some(dict) = transcript.as_dict() else {
        return ReminderPostTurnReport::default();
    };
    let Some(VmValue::List(events)) = dict.get("events") else {
        return ReminderPostTurnReport {
            transcript: Some(transcript.clone()),
            ..ReminderPostTurnReport::default()
        };
    };

    let mut changed = false;
    let mut decremented_count = 0;
    let mut remaining_count = 0;
    let mut expired = Vec::new();
    let mut next_events = Vec::with_capacity(events.len());

    for event in events.iter() {
        let Some(reminder) = reminder_from_event(event) else {
            next_events.push(event.clone());
            continue;
        };
        if reminder.fired_at_turn > turn {
            next_events.push(event.clone());
            remaining_count += 1;
            continue;
        }
        match reminder.ttl_turns {
            Some(ttl) if ttl <= 1 => {
                changed = true;
                expired.push(reminder);
            }
            Some(ttl) => {
                let mut updated = reminder;
                updated.ttl_turns = Some(ttl - 1);
                next_events.push(replace_reminder_payload(event, &updated));
                changed = true;
                decremented_count += 1;
                remaining_count += 1;
            }
            None => {
                next_events.push(event.clone());
                remaining_count += 1;
            }
        }
    }

    if !changed {
        return ReminderPostTurnReport {
            transcript: Some(transcript.clone()),
            decremented_count,
            expired,
            remaining_count,
        };
    }

    let mut next = dict.clone();
    next.insert(
        "events".to_string(),
        VmValue::List(std::sync::Arc::new(next_events)),
    );
    if !expired.is_empty() {
        let mut lifecycle = BTreeMap::new();
        lifecycle.insert("last_post_turn".to_string(), VmValue::Int(turn));
        next.insert(
            "reminder_lifecycle".to_string(),
            VmValue::Dict(std::sync::Arc::new(lifecycle)),
        );
    }
    ReminderPostTurnReport {
        transcript: Some(VmValue::Dict(std::sync::Arc::new(next))),
        decremented_count,
        expired,
        remaining_count,
    }
}

fn json_string_field(payload: &JsonValue, key: &str) -> Option<String> {
    payload
        .get(key)
        .and_then(JsonValue::as_str)
        .map(str::to_string)
        .filter(|value| !value.is_empty())
}

fn ensure_reminder_correlation(payload: &mut JsonValue) {
    let session_id = json_string_field(payload, "session_id")
        .or_else(|| json_string_field(payload, "transcript_id"))
        .or_else(crate::agent_sessions::current_session_id);
    let task_id =
        json_string_field(payload, "task_id").or_else(crate::agent_sessions::current_tool_call_id);
    let agent_id = json_string_field(payload, "agent_id").or_else(|| session_id.clone());
    let Some(obj) = payload.as_object_mut() else {
        return;
    };

    if obj
        .get("session_id")
        .and_then(JsonValue::as_str)
        .is_none_or(str::is_empty)
    {
        obj.insert(
            "session_id".to_string(),
            session_id.map(JsonValue::String).unwrap_or(JsonValue::Null),
        );
    }
    if obj
        .get("task_id")
        .and_then(JsonValue::as_str)
        .is_none_or(str::is_empty)
    {
        obj.insert(
            "task_id".to_string(),
            task_id.map(JsonValue::String).unwrap_or(JsonValue::Null),
        );
    }
    if obj
        .get("agent_id")
        .and_then(JsonValue::as_str)
        .is_none_or(str::is_empty)
    {
        obj.insert(
            "agent_id".to_string(),
            agent_id.map(JsonValue::String).unwrap_or(JsonValue::Null),
        );
    }
}

pub(crate) fn reminder_lifecycle_payload(
    session_id: Option<&str>,
    reminder: &SystemReminder,
) -> JsonValue {
    let mut payload = serde_json::json!({
        "session_id": session_id,
        "reminder_id": &reminder.id,
        "tags": &reminder.tags,
        "dedupe_key": &reminder.dedupe_key,
        "source": reminder.source.as_str(),
        "role_hint": reminder.role_hint.as_str(),
        "ttl_turns": &reminder.ttl_turns,
        "propagate": reminder.propagate.as_str(),
        "originating_agent_id": &reminder.originating_agent_id,
    });
    ensure_reminder_correlation(&mut payload);
    payload
}

pub(crate) fn emit_reminder_lifecycle_event(kind: &str, mut payload: JsonValue) {
    let Some(log) = active_event_log() else {
        return;
    };
    let Ok(topic) = Topic::new(REMINDER_LIFECYCLE_TOPIC) else {
        return;
    };
    ensure_reminder_correlation(&mut payload);
    let event = LogEvent::new(kind, payload);
    if tokio::runtime::Handle::try_current().is_ok() {
        if let Ok(join) = std::thread::Builder::new()
            .name("harn-reminder-event-log".to_string())
            .spawn(move || {
                let _ = futures::executor::block_on(log.append(&topic, event));
            })
        {
            let _ = join.join();
        }
    } else {
        let _ = futures::executor::block_on(log.append(&topic, event));
    }
}

/// Convert a Harn dict (or any json-shaped value) into a normalized
/// reminder event. Missing required fields fall back to protocol
/// defaults (`propagate=session`, `role_hint=system`, `source=in_pipeline`,
/// `preserve_on_compact=false`, `tags=[]`, `fired_at_turn=0`) so pipeline
/// authors can call `transcript_reminder_event({body: "..."})` without
/// listing every knob. Unrecognized enum values silently fall back to
/// the same defaults; strict-typed callers should construct a
/// [`SystemReminder`] directly.
pub(crate) fn transcript_reminder_event_from_value(value: &VmValue) -> VmValue {
    let reminder = reminder_from_vm_value(value);
    transcript_reminder_event(&reminder)
}

#[allow(dead_code)] // typed constructor reserved for lifecycle producers
pub(crate) fn transcript_suspension_event(suspension: &Suspension) -> VmValue {
    let payload = serde_json::to_value(suspension).unwrap_or(JsonValue::Null);
    lifecycle_transcript_event(
        SUSPENSION_EVENT_KIND,
        "suspension",
        &payload,
        suspension.reason.as_str(),
    )
}

pub(crate) fn transcript_suspension_event_from_value(value: &VmValue) -> VmValue {
    let payload = vm_value_to_json(value);
    let text = payload
        .get("reason")
        .and_then(JsonValue::as_str)
        .unwrap_or_default();
    lifecycle_transcript_event(SUSPENSION_EVENT_KIND, "suspension", &payload, text)
}

#[allow(dead_code)] // typed constructor reserved for lifecycle producers
pub(crate) fn transcript_resumption_event(resumption: &Resumption) -> VmValue {
    let payload = serde_json::to_value(resumption).unwrap_or(JsonValue::Null);
    lifecycle_transcript_event(
        RESUMPTION_EVENT_KIND,
        "resumption",
        &payload,
        resumption.initiator_id.as_str(),
    )
}

pub(crate) fn transcript_resumption_event_from_value(value: &VmValue) -> VmValue {
    let payload = vm_value_to_json(value);
    let text = payload
        .get("initiator_id")
        .and_then(JsonValue::as_str)
        .unwrap_or_default();
    lifecycle_transcript_event(RESUMPTION_EVENT_KIND, "resumption", &payload, text)
}

#[allow(dead_code)] // typed constructor reserved for lifecycle producers
pub(crate) fn transcript_drain_decision_event(drain: &DrainDecision) -> VmValue {
    let payload = serde_json::to_value(drain).unwrap_or(JsonValue::Null);
    lifecycle_transcript_event(
        DRAIN_DECISION_EVENT_KIND,
        "drain",
        &payload,
        drain.reason.as_str(),
    )
}

pub(crate) fn transcript_drain_decision_event_from_value(value: &VmValue) -> VmValue {
    let payload = vm_value_to_json(value);
    let text = payload
        .get("reason")
        .and_then(JsonValue::as_str)
        .unwrap_or_default();
    lifecycle_transcript_event(DRAIN_DECISION_EVENT_KIND, "drain", &payload, text)
}

fn lifecycle_transcript_event(
    kind: &str,
    payload_key: &str,
    payload: &JsonValue,
    text: &str,
) -> VmValue {
    let envelope = transcript_event(kind, "system", "public", text, Some(payload.clone()));
    let mut event = envelope.as_dict().cloned().unwrap_or_default();
    event.insert(
        payload_key.to_string(),
        crate::stdlib::json_to_vm_value(payload),
    );
    VmValue::Dict(std::sync::Arc::new(event))
}

pub(crate) fn reminder_from_vm_value(value: &VmValue) -> SystemReminder {
    let dict = value.as_dict().cloned().unwrap_or_default();
    let id = dict
        .get("id")
        .map(|v| v.display())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| uuid::Uuid::now_v7().to_string());
    let tags = match dict.get("tags") {
        Some(VmValue::List(items)) => items.iter().map(|v| v.display()).collect(),
        _ => Vec::new(),
    };
    let dedupe_key = dict
        .get("dedupe_key")
        .and_then(string_value)
        .filter(|s| !s.is_empty());
    let ttl_turns = dict.get("ttl_turns").and_then(|v| match v {
        VmValue::Int(n) => Some(*n),
        VmValue::Nil => None,
        _ => None,
    });
    let preserve_on_compact = dict
        .get("preserve_on_compact")
        .map(|v| v.is_truthy())
        .unwrap_or(false);
    let propagate = dict
        .get("propagate")
        .and_then(string_value)
        .and_then(|s| match s.as_str() {
            "all" => Some(ReminderPropagate::All),
            "session" => Some(ReminderPropagate::Session),
            "none" => Some(ReminderPropagate::None),
            _ => None,
        })
        .unwrap_or(ReminderPropagate::Session);
    let role_hint = dict
        .get("role_hint")
        .and_then(string_value)
        .and_then(|s| match s.as_str() {
            "system" => Some(ReminderRoleHint::System),
            "developer" => Some(ReminderRoleHint::Developer),
            "user_block" => Some(ReminderRoleHint::UserBlock),
            "ephemeral_cache" => Some(ReminderRoleHint::EphemeralCache),
            _ => None,
        })
        .unwrap_or(ReminderRoleHint::System);
    let source = dict
        .get("source")
        .and_then(string_value)
        .and_then(|s| match s.as_str() {
            "stdlib_provider" => Some(ReminderSource::StdlibProvider),
            "hook" => Some(ReminderSource::Hook),
            "bridge" => Some(ReminderSource::Bridge),
            "in_pipeline" => Some(ReminderSource::InPipeline),
            "inherited" => Some(ReminderSource::Inherited),
            _ => None,
        })
        .unwrap_or(ReminderSource::InPipeline);
    let body = dict.get("body").and_then(string_value).unwrap_or_default();
    let fired_at_turn = dict
        .get("fired_at_turn")
        .and_then(|v| match v {
            VmValue::Int(n) => Some(*n),
            _ => None,
        })
        .unwrap_or(0);
    let originating_agent_id = dict
        .get("originating_agent_id")
        .and_then(string_value)
        .filter(|s| !s.is_empty());

    SystemReminder {
        id,
        tags,
        dedupe_key,
        ttl_turns,
        preserve_on_compact,
        propagate,
        role_hint,
        source,
        body,
        fired_at_turn,
        originating_agent_id,
    }
}

pub(crate) fn reminder_from_event(event: &VmValue) -> Option<SystemReminder> {
    let dict = event.as_dict()?;
    if dict.get("kind").map(|value| value.display()).as_deref() != Some(SYSTEM_REMINDER_EVENT_KIND)
    {
        return None;
    }
    let reminder = dict.get("reminder")?;
    serde_json::from_value(vm_value_to_json(reminder)).ok()
}

fn reminder_propagates_to_handoff(reminder: &SystemReminder) -> bool {
    match reminder.propagate {
        ReminderPropagate::All => true,
        ReminderPropagate::Session => reminder.source != ReminderSource::Inherited,
        ReminderPropagate::None => false,
    }
}

pub(crate) fn inherited_reminder_for_handoff(
    mut reminder: SystemReminder,
    source_agent_id: &str,
) -> SystemReminder {
    if reminder.originating_agent_id.is_none() {
        reminder.originating_agent_id = Some(source_agent_id.to_string());
    }
    reminder.source = ReminderSource::Inherited;
    reminder
}

pub(crate) fn reminder_propagation_from_transcript(
    transcript: &VmValue,
    source_agent_id: &str,
) -> Vec<SystemReminder> {
    let Some(events) = transcript
        .as_dict()
        .and_then(|dict| dict.get("events"))
        .and_then(|events| match events {
            VmValue::List(events) => Some(events),
            _ => None,
        })
    else {
        return Vec::new();
    };

    events
        .iter()
        .filter_map(reminder_from_event)
        .filter(reminder_propagates_to_handoff)
        .map(|reminder| inherited_reminder_for_handoff(reminder, source_agent_id))
        .collect()
}

pub(crate) fn replace_reminder_payload(event: &VmValue, reminder: &SystemReminder) -> VmValue {
    let reminder_json = serde_json::to_value(reminder).unwrap_or(JsonValue::Null);
    let reminder_value = crate::stdlib::json_to_vm_value(&reminder_json);
    let mut dict = event.as_dict().cloned().unwrap_or_default();
    dict.insert("reminder".to_string(), reminder_value.clone());
    dict.insert("metadata".to_string(), reminder_value);
    VmValue::Dict(std::sync::Arc::new(dict))
}

fn string_value(value: &VmValue) -> Option<String> {
    match value {
        VmValue::String(s) => Some(s.to_string()),
        VmValue::Nil => None,
        _ => Some(value.display()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reminder_round_trips_through_serde() {
        let reminder = SystemReminder {
            id: "0190abcd-1234-7000-8000-000000000001".to_string(),
            tags: vec!["token_pressure".to_string(), "file_changed".to_string()],
            dedupe_key: Some("token_pressure".to_string()),
            ttl_turns: Some(3),
            preserve_on_compact: true,
            propagate: ReminderPropagate::Session,
            role_hint: ReminderRoleHint::Developer,
            source: ReminderSource::StdlibProvider,
            body: "Approaching context window cap.".to_string(),
            fired_at_turn: 4,
            originating_agent_id: None,
        };
        let json = serde_json::to_value(&reminder).expect("serialize reminder");
        assert_eq!(json["propagate"], "session");
        assert_eq!(json["role_hint"], "developer");
        assert_eq!(json["source"], "stdlib_provider");
        assert_eq!(json["tags"][0], "token_pressure");
        assert_eq!(json["ttl_turns"], 3);
        let parsed: SystemReminder = serde_json::from_value(json).expect("deserialize reminder");
        assert_eq!(parsed, reminder);
    }

    #[test]
    fn reminder_event_carries_canonical_envelope() {
        let reminder = SystemReminder::new("Test reminder body", ReminderSource::InPipeline, 7);
        let event = transcript_reminder_event(&reminder);
        let dict = event.as_dict().expect("event is a dict");
        assert_eq!(
            dict.get("kind").map(|v| v.display()).as_deref(),
            Some(SYSTEM_REMINDER_EVENT_KIND)
        );
        assert_eq!(
            dict.get("visibility").map(|v| v.display()).as_deref(),
            Some("public")
        );
        assert_eq!(
            dict.get("text").map(|v| v.display()).as_deref(),
            Some("Test reminder body")
        );
        let reminder_dict = dict
            .get("reminder")
            .and_then(|v| v.as_dict())
            .expect("reminder slot is a dict");
        assert_eq!(
            reminder_dict.get("source").map(|v| v.display()).as_deref(),
            Some("in_pipeline")
        );
        assert_eq!(
            reminder_dict.get("fired_at_turn").and_then(|v| match v {
                VmValue::Int(n) => Some(*n),
                _ => None,
            }),
            Some(7)
        );
        // Metadata mirrors the reminder envelope for observers that
        // already key off `event.metadata`.
        let metadata_dict = dict
            .get("metadata")
            .and_then(|v| v.as_dict())
            .expect("metadata mirrors reminder");
        assert_eq!(
            metadata_dict.get("body").map(|v| v.display()).as_deref(),
            Some("Test reminder body")
        );
    }

    #[test]
    fn message_with_reminder_kind_promotes_to_typed_event() {
        let dict = BTreeMap::from([
            (
                "kind".to_string(),
                VmValue::String(std::sync::Arc::from(SYSTEM_REMINDER_EVENT_KIND)),
            ),
            (
                "reminder".to_string(),
                crate::stdlib::json_to_vm_value(&serde_json::json!({
                    "body": "Reload tools",
                    "source": "hook",
                    "tags": ["tool_changed"],
                    "fired_at_turn": 2,
                    "preserve_on_compact": true,
                    "propagate": "all",
                    "role_hint": "developer",
                })),
            ),
        ]);
        let event = transcript_event_from_message(&VmValue::Dict(std::sync::Arc::new(dict)));
        let event_dict = event.as_dict().expect("event is dict");
        assert_eq!(
            event_dict.get("kind").map(|v| v.display()).as_deref(),
            Some(SYSTEM_REMINDER_EVENT_KIND)
        );
        let reminder = event_dict
            .get("reminder")
            .and_then(|v| v.as_dict())
            .expect("typed reminder slot");
        assert_eq!(
            reminder.get("propagate").map(|v| v.display()).as_deref(),
            Some("all")
        );
        assert_eq!(
            reminder.get("role_hint").map(|v| v.display()).as_deref(),
            Some("developer")
        );
        assert_eq!(
            reminder.get("source").map(|v| v.display()).as_deref(),
            Some("hook")
        );
    }

    #[test]
    fn reminder_propagation_filters_and_rewrites_inherited_copies() {
        let mut all = SystemReminder::new("all reminder", ReminderSource::InPipeline, 1);
        all.propagate = ReminderPropagate::All;
        let mut session = SystemReminder::new("session reminder", ReminderSource::InPipeline, 1);
        session.propagate = ReminderPropagate::Session;
        let mut none = SystemReminder::new("none reminder", ReminderSource::InPipeline, 1);
        none.propagate = ReminderPropagate::None;
        let inherited_session = inherited_reminder_for_handoff(session.clone(), "root-parent");

        let transcript = new_transcript_with_events(
            Some("parent".to_string()),
            Vec::new(),
            None,
            None,
            vec![
                transcript_reminder_event(&all),
                transcript_reminder_event(&session),
                transcript_reminder_event(&none),
                transcript_reminder_event(&inherited_session),
            ],
            Vec::new(),
            None,
        );
        let propagated = reminder_propagation_from_transcript(&transcript, "parent");
        let bodies = propagated
            .iter()
            .map(|reminder| reminder.body.as_str())
            .collect::<Vec<_>>();

        assert_eq!(bodies, vec!["all reminder", "session reminder"]);
        assert!(propagated
            .iter()
            .all(|reminder| reminder.source == ReminderSource::Inherited));
        assert!(propagated
            .iter()
            .all(|reminder| reminder.originating_agent_id.as_deref() == Some("parent")));
    }

    #[test]
    fn lifecycle_event_payloads_round_trip_through_serde() {
        let suspension = Suspension {
            handle: "worker://triage/42".to_string(),
            initiator: SuspensionInitiator::Self_,
            reason: "waiting for approval".to_string(),
            conditions: Some(serde_json::json!({"kind": "approval"})),
            resume_by_mechanism: Some("ResumeBy.trigger".to_string()),
            suspended_at_turn: 5,
            span_id: Some("span-1".to_string()),
        };
        let suspension_json = serde_json::to_value(&suspension).expect("serialize suspension");
        assert_eq!(suspension_json["initiator"], "self");
        let parsed_suspension: Suspension =
            serde_json::from_value(suspension_json).expect("deserialize suspension");
        assert_eq!(parsed_suspension, suspension);

        let resumption = Resumption {
            handle: "worker://triage/42".to_string(),
            initiator: ResumptionInitiator::DrainAgent,
            initiator_id: "session-settle-1".to_string(),
            input: Some(serde_json::json!({"approved": true})),
            input_hash: "sha256:abc".to_string(),
            continue_transcript: true,
            trigger_match: Some(TriggerMatch {
                source: "github".to_string(),
                event_id: "evt-1".to_string(),
                filter_summary: "label matched".to_string(),
            }),
            linked_suspension_span_id: Some("span-1".to_string()),
            resumed_at_turn: 6,
        };
        let resumption_json = serde_json::to_value(&resumption).expect("serialize resumption");
        assert_eq!(resumption_json["initiator"], "drain_agent");
        let parsed_resumption: Resumption =
            serde_json::from_value(resumption_json).expect("deserialize resumption");
        assert_eq!(parsed_resumption, resumption);

        let drain = DrainDecision {
            pipeline_id: "pipeline-1".to_string(),
            item: DrainDecisionItem {
                category: DrainDecisionItemCategory::InFlightLlmCall,
                id: "call-1".to_string(),
                summary: "model call still running".to_string(),
            },
            action: DrainDecisionAction::Wait,
            reason: "allow in-flight call to settle".to_string(),
            decided_at_turn: 7,
            settlement_agent_session_id: "settle-session-1".to_string(),
        };
        let drain_json = serde_json::to_value(&drain).expect("serialize drain decision");
        assert_eq!(drain_json["item"]["category"], "in_flight_llm_call");
        let parsed_drain: DrainDecision =
            serde_json::from_value(drain_json).expect("deserialize drain decision");
        assert_eq!(parsed_drain, drain);
    }

    #[test]
    fn lifecycle_events_carry_standard_envelope_and_typed_slots() {
        let suspension = Suspension {
            handle: "worker://triage/42".to_string(),
            initiator: SuspensionInitiator::Parent,
            reason: "parent paused worker".to_string(),
            conditions: None,
            resume_by_mechanism: None,
            suspended_at_turn: 5,
            span_id: None,
        };
        let suspension_event = transcript_suspension_event(&suspension);
        assert_lifecycle_event_slot(
            &suspension_event,
            SUSPENSION_EVENT_KIND,
            "suspension",
            "parent paused worker",
        );

        let resumption = Resumption {
            handle: "worker://triage/42".to_string(),
            initiator: ResumptionInitiator::Operator,
            initiator_id: "operator-1".to_string(),
            input: None,
            input_hash: "sha256:none".to_string(),
            continue_transcript: false,
            trigger_match: None,
            linked_suspension_span_id: None,
            resumed_at_turn: 6,
        };
        let resumption_event = transcript_resumption_event(&resumption);
        assert_lifecycle_event_slot(
            &resumption_event,
            RESUMPTION_EVENT_KIND,
            "resumption",
            "operator-1",
        );

        let drain = DrainDecision {
            pipeline_id: "pipeline-1".to_string(),
            item: DrainDecisionItem {
                category: DrainDecisionItemCategory::SuspendedSubagent,
                id: "worker://triage/42".to_string(),
                summary: "worker suspended".to_string(),
            },
            action: DrainDecisionAction::Resume,
            reason: "settlement agent can continue it".to_string(),
            decided_at_turn: 7,
            settlement_agent_session_id: "settle-session-1".to_string(),
        };
        let drain_event = transcript_drain_decision_event(&drain);
        assert_lifecycle_event_slot(
            &drain_event,
            DRAIN_DECISION_EVENT_KIND,
            "drain",
            "settlement agent can continue it",
        );
    }

    #[test]
    fn messages_with_lifecycle_kinds_promote_to_typed_events() {
        let cases = [
            (
                SUSPENSION_EVENT_KIND,
                "suspension",
                serde_json::json!({
                    "handle": "worker://triage/42",
                    "initiator": "operator",
                    "reason": "manual pause",
                    "suspended_at_turn": 2
                }),
            ),
            (
                RESUMPTION_EVENT_KIND,
                "resumption",
                serde_json::json!({
                    "handle": "worker://triage/42",
                    "initiator": "parent",
                    "initiator_id": "parent-1",
                    "input_hash": "sha256:none",
                    "continue_transcript": true,
                    "resumed_at_turn": 3
                }),
            ),
            (
                DRAIN_DECISION_EVENT_KIND,
                "drain",
                serde_json::json!({
                    "pipeline_id": "pipeline-1",
                    "item": {
                        "category": "queued_trigger",
                        "id": "evt-1",
                        "summary": "queued trigger"
                    },
                    "action": "defer",
                    "reason": "wait for owner",
                    "decided_at_turn": 4,
                    "settlement_agent_session_id": "settle-session-1"
                }),
            ),
        ];

        for (kind, slot, payload) in cases {
            let event = transcript_event_from_message(&VmValue::Dict(std::sync::Arc::new(
                BTreeMap::from([
                    (
                        "kind".to_string(),
                        VmValue::String(std::sync::Arc::from(kind)),
                    ),
                    (slot.to_string(), crate::stdlib::json_to_vm_value(&payload)),
                ]),
            )));
            let dict = event.as_dict().expect("event is dict");
            assert_eq!(dict.get("kind").map(|v| v.display()).as_deref(), Some(kind));
            assert_eq!(
                dict.get("role").map(|v| v.display()).as_deref(),
                Some("system")
            );
            assert_eq!(
                dict.get("visibility").map(|v| v.display()).as_deref(),
                Some("public")
            );
            assert!(dict.get(slot).and_then(VmValue::as_dict).is_some());
            assert!(dict.get("metadata").and_then(VmValue::as_dict).is_some());
        }
    }

    fn assert_lifecycle_event_slot(event: &VmValue, kind: &str, slot: &str, text: &str) {
        let dict = event.as_dict().expect("event is a dict");
        assert_eq!(dict.get("kind").map(|v| v.display()).as_deref(), Some(kind));
        assert_eq!(
            dict.get("role").map(|v| v.display()).as_deref(),
            Some("system")
        );
        assert_eq!(
            dict.get("visibility").map(|v| v.display()).as_deref(),
            Some("public")
        );
        assert_eq!(dict.get("text").map(|v| v.display()).as_deref(), Some(text));
        assert!(dict.get(slot).and_then(VmValue::as_dict).is_some());
        assert!(dict.get("metadata").and_then(VmValue::as_dict).is_some());
    }
}
