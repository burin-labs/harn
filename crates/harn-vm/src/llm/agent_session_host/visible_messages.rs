//! Canonical provider-visible projection of durable session history plus active directives.

use crate::stdlib::macros::{harn_builtin, register_builtin_defs, VmBuiltinDef};
use crate::value::{VmError, VmValue};
use crate::vm::Vm;

/// Return exactly the message view passed to model callers.
///
/// The envelope is committed to durable history at the turn boundary that
/// emits it, so this projection adds only directives no committed message
/// carries yet.
#[harn_builtin(
    exposure = "runtime_internal",
    effects = [],
    sig = "__host_agent_session_visible_messages(session_id: string, messages?: list|nil) -> list",
    category = "agent.host",
    runtime_only = true
)]
fn host_agent_session_visible_messages_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let session_id = args.first().map(VmValue::display).unwrap_or_default();
    let messages = args
        .get(1)
        .filter(|value| !matches!(value, VmValue::Nil))
        .map(super::vm_to_json)
        .and_then(|value| value.as_array().cloned())
        .unwrap_or_else(|| durable_messages(&session_id));
    Ok(super::json_to_vm(&serde_json::Value::Array(
        visible_messages_with_lineage(&session_id, messages),
    )))
}

pub(crate) fn visible_messages_with_lineage(
    session_id: &str,
    messages: Vec<serde_json::Value>,
) -> Vec<serde_json::Value> {
    let reminders = crate::llm::helpers::pending_reminders_from_session(Some(session_id));
    let rendered = crate::llm::helpers::render_pending_reminders(
        &crate::llm::capabilities::Capabilities::default(),
        &reminders,
    );
    let source_count = messages.len();
    let mut visible = crate::llm::helpers::apply_rendered_reminder_messages(messages, &rendered);
    let compaction_receipt_ref = latest_compaction_receipt_ref(session_id);
    for (position, message) in visible.iter_mut().enumerate() {
        let semantic_kind = semantic_kind(message);
        let existing = message
            .get(crate::llm::message_lineage::MESSAGE_LINEAGE_KEY)
            .cloned()
            .and_then(|value| serde_json::from_value(value).ok());
        let mut attached: crate::llm::message_lineage::AttachedMessageLineage = existing
            .unwrap_or_else(|| crate::llm::message_lineage::AttachedMessageLineage {
                projection: crate::llm::message_lineage::raw_projection(),
                message: crate::llm::message_lineage::MessageLineageEntry {
                    source_message_index: (position < source_count).then_some(position),
                    ..crate::llm::message_lineage::MessageLineageEntry::default()
                },
            });
        attached.message.semantic_kind = semantic_kind;
        if attached.message.source_message_index == Some(0) {
            attached.message.compaction_receipt_ref = compaction_receipt_ref.clone();
            if attached.message.compaction_receipt_ref.is_some() {
                attached.message.semantic_kind =
                    crate::llm::message_lineage::MessageSemanticKind::CondensedMemory;
            }
        }
        if let Some(object) = message.as_object_mut() {
            object.insert(
                crate::llm::message_lineage::MESSAGE_LINEAGE_KEY.to_string(),
                serde_json::to_value(attached).expect("message lineage is JSON representable"),
            );
        }
    }
    visible
}

fn semantic_kind(message: &serde_json::Value) -> crate::llm::message_lineage::MessageSemanticKind {
    use crate::llm::message_lineage::MessageSemanticKind;
    if crate::llm::helpers::has_directive_commit_metadata(message) {
        return MessageSemanticKind::ContextDirective;
    }
    match message.get("role").and_then(serde_json::Value::as_str) {
        Some("user") => MessageSemanticKind::User,
        Some("assistant")
            if message
                .get("tool_calls")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|calls| !calls.is_empty()) =>
        {
            MessageSemanticKind::AssistantToolCall
        }
        Some("assistant") => MessageSemanticKind::Assistant,
        Some("tool" | "tool_result") => MessageSemanticKind::ToolResult,
        Some("system" | "developer") => MessageSemanticKind::Instruction,
        _ => MessageSemanticKind::Unknown,
    }
}

fn latest_compaction_receipt_ref(session_id: &str) -> Option<String> {
    let transcript = crate::agent_sessions::transcript(session_id)?;
    let transcript = transcript.as_dict()?;
    match transcript.get("summary") {
        Some(VmValue::String(summary)) if !summary.is_empty() => {}
        _ => return None,
    }
    let VmValue::List(events) = transcript.get("events")? else {
        return None;
    };
    events.iter().rev().find_map(|event| {
        let event = super::vm_to_json(event);
        (event.get("kind").and_then(serde_json::Value::as_str) == Some("compaction"))
            .then(|| {
                event
                    .pointer("/metadata/receipt/receipt_id")
                    .and_then(serde_json::Value::as_str)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string)
            })
            .flatten()
    })
}

/// Commit the active directive envelope into durable session history.
///
/// This is the turn boundary that makes the provider-visible message array
/// append-only: once the envelope is a durable message, every later request
/// re-sends those exact bytes at the same index instead of re-deriving a
/// placement that moves. Directives already present in history are not
/// re-issued, so an unchanged provider firing every turn commits nothing and
/// no earlier turn is ever edited to remove a stale one.
///
/// Returns the number of directives committed, which is zero when nothing is
/// pending or when everything pending is already in history.
#[harn_builtin(
    exposure = "runtime_internal",
    effects = [],
    sig = "__host_agent_session_commit_directives(session_id: string) -> int",
    category = "agent.host",
    runtime_only = true
)]
fn host_agent_session_commit_directives_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let session_id = args.first().map(VmValue::display).unwrap_or_default();
    if session_id.is_empty() {
        return Ok(VmValue::Int(0));
    }
    let messages = durable_messages(&session_id);
    let reminders = crate::llm::helpers::pending_reminders_from_session(Some(&session_id));
    let capabilities = crate::llm::capabilities::Capabilities::default();
    let rendered = crate::llm::helpers::render_pending_reminders(&capabilities, &reminders);
    let pending = crate::llm::helpers::uncommitted_directives(&messages, &rendered);
    let Some(message) = crate::llm::helpers::directive_envelope_message(&pending) else {
        return Ok(VmValue::Int(0));
    };
    crate::agent_sessions::inject_message(&session_id, super::json_to_vm(&message))
        .map_err(VmError::Runtime)?;
    Ok(VmValue::Int(pending.len() as i64))
}

fn durable_messages(session_id: &str) -> Vec<serde_json::Value> {
    crate::agent_sessions::transcript(session_id)
        .as_ref()
        .and_then(|value| super::dict_get(value, "messages"))
        .map(super::vm_to_json)
        .and_then(|value| value.as_array().cloned())
        .unwrap_or_default()
}

const VISIBLE_MESSAGE_BUILTINS: &[&VmBuiltinDef] = &[
    &HOST_AGENT_SESSION_VISIBLE_MESSAGES_BUILTIN_DEF,
    &HOST_AGENT_SESSION_COMMIT_DIRECTIVES_BUILTIN_DEF,
];

pub(super) fn register_visible_message_primitives(vm: &mut Vm) {
    register_builtin_defs(vm, VISIBLE_MESSAGE_BUILTINS);
}
