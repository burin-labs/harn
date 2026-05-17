use std::collections::BTreeMap;
use std::rc::Rc;

use serde::{Deserialize, Serialize};

use crate::value::{VmError, VmValue};

use super::blocks::{
    default_visibility_for_role, normalize_message_blocks, overall_visibility, render_blocks_text,
};
use super::messages::json_messages_to_vm;
use super::{TRANSCRIPT_ASSET_TYPE, TRANSCRIPT_TYPE, TRANSCRIPT_VERSION};

/// Canonical `kind` for a [`SystemReminder`] transcript event.
pub(crate) const SYSTEM_REMINDER_EVENT_KIND: &str = "system_reminder";

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
}

impl ReminderSource {
    #[allow(dead_code)] // reserved for the R-02+ wire renderers
    pub fn as_str(self) -> &'static str {
        match self {
            Self::StdlibProvider => "stdlib_provider",
            Self::Hook => "hook",
            Self::Bridge => "bridge",
            Self::InPipeline => "in_pipeline",
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
        }
    }
}

pub(crate) fn transcript_message_list(
    transcript: &BTreeMap<String, VmValue>,
) -> Result<Vec<VmValue>, VmError> {
    match transcript.get("messages") {
        Some(VmValue::List(list)) => Ok((**list).clone()),
        Some(_) => Err(VmError::Thrown(VmValue::String(Rc::from(
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
        Some(_) => Err(VmError::Thrown(VmValue::String(Rc::from(
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
        VmValue::String(Rc::from(TRANSCRIPT_TYPE)),
    );
    transcript.insert("version".to_string(), VmValue::Int(TRANSCRIPT_VERSION));
    transcript.insert(
        "id".to_string(),
        VmValue::String(Rc::from(
            id.unwrap_or_else(|| uuid::Uuid::now_v7().to_string()),
        )),
    );
    transcript.insert("messages".to_string(), VmValue::List(Rc::new(messages)));
    transcript.insert("events".to_string(), VmValue::List(Rc::new(events)));
    transcript.insert("assets".to_string(), VmValue::List(Rc::new(assets)));
    if let Some(summary) = summary {
        transcript.insert("summary".to_string(), VmValue::String(Rc::from(summary)));
    }
    if let Some(metadata) = metadata {
        transcript.insert("metadata".to_string(), metadata);
    }
    if let Some(state) = state {
        transcript.insert("state".to_string(), VmValue::String(Rc::from(state)));
    }
    VmValue::Dict(Rc::new(transcript))
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
        VmValue::String(Rc::from(uuid::Uuid::now_v7().to_string())),
    );
    event.insert("kind".to_string(), VmValue::String(Rc::from(kind)));
    event.insert("role".to_string(), VmValue::String(Rc::from(role.as_str())));
    event.insert(
        "visibility".to_string(),
        VmValue::String(Rc::from(visibility)),
    );
    event.insert("text".to_string(), VmValue::String(Rc::from(text)));
    event.insert("blocks".to_string(), VmValue::List(Rc::new(blocks)));
    VmValue::Dict(Rc::new(event))
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
        VmValue::String(Rc::from(uuid::Uuid::now_v7().to_string())),
    );
    event.insert("kind".to_string(), VmValue::String(Rc::from(kind)));
    event.insert("role".to_string(), VmValue::String(Rc::from(role)));
    event.insert(
        "visibility".to_string(),
        VmValue::String(Rc::from(visibility)),
    );
    event.insert("text".to_string(), VmValue::String(Rc::from(text)));
    event.insert(
        "blocks".to_string(),
        VmValue::List(Rc::new(vec![VmValue::Dict(Rc::new(BTreeMap::from([
            ("type".to_string(), VmValue::String(Rc::from("text"))),
            ("text".to_string(), VmValue::String(Rc::from(text))),
            (
                "visibility".to_string(),
                VmValue::String(Rc::from(visibility)),
            ),
        ])))])),
    );
    if let Some(metadata) = metadata {
        event.insert(
            "metadata".to_string(),
            crate::stdlib::json_to_vm_value(&metadata),
        );
    }
    VmValue::Dict(Rc::new(event))
}

pub(crate) fn normalize_transcript_asset(value: &VmValue) -> VmValue {
    let mut asset = value.as_dict().cloned().unwrap_or_default();
    asset.insert(
        "_type".to_string(),
        VmValue::String(Rc::from(TRANSCRIPT_ASSET_TYPE)),
    );
    if !asset.contains_key("id") {
        asset.insert(
            "id".to_string(),
            VmValue::String(Rc::from(uuid::Uuid::now_v7().to_string())),
        );
    }
    if !asset.contains_key("kind") {
        asset.insert("kind".to_string(), VmValue::String(Rc::from("blob")));
    }
    if !asset.contains_key("visibility") {
        asset.insert(
            "visibility".to_string(),
            VmValue::String(Rc::from("internal")),
        );
    }
    if value.as_dict().is_none() {
        asset.insert(
            "storage".to_string(),
            VmValue::Dict(Rc::new(BTreeMap::from([(
                "path".to_string(),
                VmValue::String(Rc::from(value.display())),
            )]))),
        );
    }
    VmValue::Dict(Rc::new(asset))
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
    VmValue::Dict(Rc::new(event))
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

fn reminder_from_vm_value(value: &VmValue) -> SystemReminder {
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
    }
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
                VmValue::String(Rc::from(SYSTEM_REMINDER_EVENT_KIND)),
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
        let event = transcript_event_from_message(&VmValue::Dict(Rc::new(dict)));
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
}
