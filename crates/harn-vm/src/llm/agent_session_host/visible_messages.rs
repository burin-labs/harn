//! Canonical provider-visible projection of durable session history plus active directives.

use crate::value::{VmError, VmValue};
use crate::vm::Vm;
use harn_builtin_macros::harn_builtin;

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
    // Only a list is a message array; anything else falls back to durable
    // history, exactly as a non-array JSON projection of it would.
    let messages = match args.get(1) {
        Some(VmValue::List(messages)) => messages.as_ref().clone(),
        _ => durable_messages(&session_id),
    };
    Ok(VmValue::list(visible_message_values(&session_id, messages)))
}

/// The provider-visible message list for `messages`, each message carrying
/// its lineage. Operates on the session's VM values in place: only the
/// directive envelope, when one is pending, is built as a new message.
pub(crate) fn visible_message_values(session_id: &str, messages: Vec<VmValue>) -> Vec<VmValue> {
    let reminders = crate::llm::helpers::pending_reminders_from_session(Some(session_id));
    let rendered = crate::llm::helpers::render_pending_reminders(
        &crate::llm::capabilities::Capabilities::default(),
        &reminders,
    );
    let directive_nonce = crate::llm::helpers::directive_nonce_for_session(session_id);
    let source_count = messages.len();
    let mut visible = messages;
    let pending = crate::llm::helpers::uncommitted_directives(&visible, &rendered);
    if let Some(envelope) =
        crate::llm::helpers::directive_envelope_message(&pending, &directive_nonce)
    {
        visible.push(super::json_to_vm(&envelope));
    }
    let compaction_receipt_ref = latest_compaction_receipt_ref(session_id);
    // Each call re-projects the whole transcript, and the durable messages are
    // the same shared values call after call. Reusing the projected message
    // keeps it the same shared value too, so options building downstream
    // converts only the messages that changed.
    VISIBLE_MESSAGE_MEMO.with(|memo| {
        let mut memo = memo.borrow_mut();
        for (position, message) in visible.iter_mut().enumerate() {
            if message.as_dict().is_none() {
                continue;
            }
            let source_index = (position < source_count).then_some(position);
            let input = message.clone();
            *message = memo.project(
                position,
                &input,
                (source_index, compaction_receipt_ref.clone()),
                || with_lineage(&input, source_index, compaction_receipt_ref.as_ref()),
                |_| 1,
            );
        }
    });
    visible
}

/// Projected messages keyed by the source index and compaction receipt the
/// lineage reads.
type VisibleMessageMemo =
    crate::llm::content_hash::IdentitySequenceMemo<(Option<usize>, Option<String>), VmValue>;

thread_local! {
    static VISIBLE_MESSAGE_MEMO: std::cell::RefCell<VisibleMessageMemo> = const {
        std::cell::RefCell::new(crate::llm::content_hash::IdentitySequenceMemo::new(
            VISIBLE_MESSAGE_MEMO_ENTRIES,
        ))
    };
    #[cfg(test)]
    static VISIBLE_MESSAGE_PROJECTIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

const VISIBLE_MESSAGE_MEMO_ENTRIES: usize = 65_536;

/// Messages [`visible_message_values`] has projected on this thread.
#[cfg(test)]
pub(crate) fn visible_message_projections() -> usize {
    VISIBLE_MESSAGE_PROJECTIONS.with(std::cell::Cell::get)
}

/// `message` with its lineage attached. A pure function of its arguments.
fn with_lineage(
    message: &VmValue,
    source_index: Option<usize>,
    compaction_receipt_ref: Option<&String>,
) -> VmValue {
    #[cfg(test)]
    VISIBLE_MESSAGE_PROJECTIONS.with(|count| count.set(count.get() + 1));
    let Some(fields) = message.as_dict() else {
        return message.clone();
    };
    let mut attached = attached_lineage(
        fields.get(crate::llm::message_lineage::MESSAGE_LINEAGE_KEY),
        source_index,
    );
    attached.message.semantic_kind = semantic_kind(message);
    if attached.message.source_message_index == Some(0) {
        attached.message.compaction_receipt_ref = compaction_receipt_ref.cloned();
        if attached.message.compaction_receipt_ref.is_some() {
            attached.message.semantic_kind =
                crate::llm::message_lineage::MessageSemanticKind::CondensedMemory;
        }
    }
    let lineage = super::json_to_vm(
        &serde_json::to_value(attached).expect("message lineage is JSON representable"),
    );
    let mut next = fields.clone();
    next.insert(
        crate::value::intern_key(crate::llm::message_lineage::MESSAGE_LINEAGE_KEY),
        lineage,
    );
    VmValue::dict_map(next)
}

/// JSON form of [`visible_message_values`] for tests that build messages as JSON.
#[cfg(test)]
pub(crate) fn visible_messages_with_lineage(
    session_id: &str,
    messages: Vec<serde_json::Value>,
) -> Vec<serde_json::Value> {
    let messages = messages.iter().map(super::json_to_vm).collect();
    visible_message_values(session_id, messages)
        .iter()
        .map(super::vm_to_json)
        .collect()
}

fn attached_lineage(
    metadata: Option<&VmValue>,
    source_message_index: Option<usize>,
) -> crate::llm::message_lineage::AttachedMessageLineage {
    use crate::llm::message_lineage::{
        raw_projection, unknown_projection, AttachedMessageLineage, MessageLineageEntry,
    };
    match metadata {
        None => AttachedMessageLineage {
            projection: raw_projection(),
            message: MessageLineageEntry {
                source_message_index,
                ..MessageLineageEntry::default()
            },
        },
        Some(metadata) => {
            serde_json::from_value(super::vm_to_json(metadata)).unwrap_or_else(|_| {
                AttachedMessageLineage {
                    projection: unknown_projection(),
                    message: MessageLineageEntry::default(),
                }
            })
        }
    }
}

fn semantic_kind(message: &VmValue) -> crate::llm::message_lineage::MessageSemanticKind {
    use crate::llm::helpers::MessageView;
    use crate::llm::message_lineage::MessageSemanticKind;
    if crate::llm::helpers::has_directive_commit_metadata(message) {
        return MessageSemanticKind::ContextDirective;
    }
    match message.str_field("role") {
        Some("user") => MessageSemanticKind::User,
        Some("assistant")
            if message
                .list_len_field("tool_calls")
                .is_some_and(|calls| calls > 0) =>
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
    use crate::llm::helpers::MessageView;
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
        if event.str_field("kind") != Some("compaction") {
            return None;
        }
        let receipt = event
            .as_dict()?
            .get("metadata")?
            .as_dict()?
            .get("receipt")?;
        receipt
            .str_field("receipt_id")
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    })
}

/// Commit the active directive envelope into durable session history.
///
/// This is the turn boundary that makes the provider-visible message array
/// append-only: once the envelope is a durable message, every later request
/// re-sends those exact bytes at the same index instead of re-deriving a
/// placement that moves. Directives already present in history are not
/// re-issued, so an unchanged provider firing every turn commits nothing and
/// no earlier turn is ever edited to remove a stale one. The one exception is
/// a history that ends on a withdrawn turn's placeholder: its rejection
/// directive is restated so the next request never ends on an assistant
/// message (see `turn_boundary_directives`).
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
    let withdrawal_reason = crate::agent_sessions::last_withdrawal_reason(&session_id);
    let pending = crate::llm::helpers::turn_boundary_directives(
        &messages,
        &reminders,
        withdrawal_reason.as_deref(),
    );
    let nonce = crate::llm::helpers::directive_nonce_for_session(&session_id);
    let Some(message) = crate::llm::helpers::directive_envelope_message(&pending, &nonce) else {
        return Ok(VmValue::Int(0));
    };
    crate::agent_sessions::inject_message(&session_id, super::json_to_vm(&message))
        .map_err(VmError::Runtime)?;
    Ok(VmValue::Int(pending.len() as i64))
}

fn durable_messages(session_id: &str) -> Vec<VmValue> {
    match crate::agent_sessions::transcript(session_id)
        .as_ref()
        .and_then(|value| super::dict_get(value, "messages"))
    {
        Some(VmValue::List(messages)) => messages.as_ref().clone(),
        _ => Vec::new(),
    }
}

pub(super) fn register_visible_message_primitives(vm: &mut Vm) {
    harn_vm_core::builtin_support::register_module_builtins(vm, module_path!());
}
