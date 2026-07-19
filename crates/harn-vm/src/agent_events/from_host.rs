//! Typed deserialization of host-emitted agent events.
//!
//! `__host_agent_emit_event` receives an untyped `(event_type, payload)`
//! pair from the Harn agent loop and turns it into a typed [`AgentEvent`].
//! Historically this lived in a ~570-line hand-written
//! `match event_type.as_str()` in `llm::agent_session_host` that
//! re-derived, field by field, the shape the `AgentEvent` enum already
//! declares via its `serde` derives.
//!
//! [`AgentEvent::from_host_payload`] replaces that with a typed
//! `serde_json::from_value::<AgentEvent>` path. The payload keys already
//! match the enum's snake_case field names, so most event types
//! deserialize directly once the `type` tag and `session_id` are injected.
//! Only three classes of arm need bespoke handling:
//!
//! 1. **Special arms** ([`from_host_special`]) where the host `event_type`
//!    does not map 1:1 onto a variant's fields — the whole payload becomes
//!    one field (`loop_stuck`, `cache_hit`, …), or a nudge `event_type`
//!    collapses onto a synthesized `FeedbackInjected`.
//! 2. **Field defaults** ([`apply_host_payload_defaults`]) for the handful
//!    of genuinely-optional payload fields the old match defaulted to a
//!    non-serde-default value (`ToolCall.status` → `pending`,
//!    `progress_reported.replace` → `true`, the container fields that
//!    default to `[]`/`{}`, …), plus the bare-string `executor` alias
//!    normalization the internally-tagged [`super::ToolExecutor`] can't
//!    parse on its own. Required scalars the loop always emits are left to
//!    serde (a malformed emit surfaces a loud error instead of a silent
//!    zero-fill).
//! 3. **Ambient audit** — `tool_call` / `tool_call_update` take their
//!    `audit` from the active mutation session, never the payload.
//!
//! The set of accepted `event_type` strings is an explicit allowlist
//! ([`from_host_special`] + [`HOST_DESERIALIZE_EVENT_TYPES`]) so the
//! accept/reject boundary is byte-identical to the retired match — many
//! `AgentEvent` variants (`worker_update`, `handoff`, `artifact`, …) are
//! constructed elsewhere and are *not* emittable through this host path.

use serde_json::{Map, Value};

use crate::value::VmError;

use super::AgentEvent;

const HOST_AGENT_EMIT_EVENT: &str = "__host_agent_emit_event";
const NO_PROGRESS_STREAK_NUDGE_FALLBACK: &str =
    "No progress was detected. Use the next turn to make concrete task progress or explain the remaining blocker.";

/// `event_type` strings that deserialize directly into an [`AgentEvent`]
/// variant once normalized. Kept as an explicit allowlist so this host
/// path accepts exactly the set the retired hand-match accepted and
/// rejects every other variant (which is constructed on non-host paths).
const HOST_DESERIALIZE_EVENT_TYPES: &[&str] = &[
    "tool_call",
    "tool_call_update",
    "iteration_start",
    "iteration_end",
    "judge_decision",
    "step_judge_decision",
    "structural_validator_decision",
    "scope_classifier_verdict",
    "input_guardrail_verdict",
    "missing_tool_call_verdict",
    "budget_exhausted",
    "budget_circuit_breaker",
    "progress_reported",
    "tool_search_query",
    "tool_search_result",
    "skill_narrow",
    "loop_control_decision",
    "capability_gap",
    "tool_format_override",
    "tool_call_audit",
    "loop_checkpoint",
];

impl AgentEvent {
    /// Build a typed [`AgentEvent`] from a host `emit_event` call.
    ///
    /// Mirrors the accept/reject boundary and per-field defaults of the
    /// retired `build_agent_event` hand-match exactly; unsupported
    /// `event_type` values return a `Runtime` error.
    pub fn from_host_payload(
        session_id: &str,
        event_type: &str,
        payload: &Value,
    ) -> Result<AgentEvent, VmError> {
        if let Some(event) = from_host_special(session_id, event_type, payload) {
            return Ok(event);
        }
        from_host_generic(session_id, event_type, payload)
    }
}

/// Arms whose host payload is not a 1:1 field mapping onto the variant:
/// the whole payload becomes a single field, a couple of fields are
/// derived, or a nudge `event_type` collapses onto `FeedbackInjected`.
/// Returns `None` for everything else so the caller falls through to the
/// generic deserialize path.
fn from_host_special(session_id: &str, event_type: &str, payload: &Value) -> Option<AgentEvent> {
    let sid = || session_id.to_string();
    let feedback = |kind: &str, content: String| AgentEvent::FeedbackInjected {
        session_id: sid(),
        kind: kind.to_string(),
        content,
        streak: None,
    };
    let feedback_with_streak =
        |kind: &str, content: String, streak: Option<usize>| AgentEvent::FeedbackInjected {
            session_id: sid(),
            kind: kind.to_string(),
            content,
            streak,
        };
    let feedback_content = |fallback: String| {
        first_non_empty_string(payload, &["content", "message", "text"]).unwrap_or(fallback)
    };
    let event = match event_type {
        "typed_checkpoint" => AgentEvent::TypedCheckpoint {
            session_id: sid(),
            checkpoint: payload.clone(),
        },
        "loop_stuck" => AgentEvent::LoopStuckSignal {
            session_id: sid(),
            payload: payload.clone(),
        },
        "reserved_terminal_verify" => AgentEvent::ReservedTerminalVerify {
            session_id: sid(),
            payload: payload.clone(),
        },
        "agent_loop_stall_warning" => AgentEvent::AgentLoopStallWarning {
            session_id: sid(),
            warning: payload.clone(),
        },
        "cache_hit" => AgentEvent::CacheHit {
            session_id: sid(),
            key: obj_string(payload, "key"),
            backend: obj_string(payload, "backend"),
            namespace: obj_string(payload, "namespace"),
            payload: payload.clone(),
        },
        "cache_miss" => AgentEvent::CacheMiss {
            session_id: sid(),
            key: obj_string(payload, "key"),
            backend: obj_string(payload, "backend"),
            namespace: obj_string(payload, "namespace"),
            payload: payload.clone(),
        },
        "agent_scratchpad_reorganization" => {
            let mut details = payload.clone();
            if let Some(object) = details.as_object_mut() {
                object.remove("iteration");
                object.remove("status");
            }
            AgentEvent::AgentScratchpadReorganization {
                session_id: sid(),
                iteration: obj_usize(payload, "iteration"),
                status: obj_string(payload, "status"),
                details,
            }
        }
        // Read-only stance lifecycle (std/agent/stance). The four stdlib
        // event names map onto one typed variant distinguished by `phase`
        // so trace consumers match on a single event type.
        "stance_armed"
        | "stance_write_access_granted"
        | "stance_write_access_denied"
        | "stance_disarmed" => {
            let allowed_tools = payload
                .get("allowed_tools")
                .and_then(Value::as_array)
                .map(|values| {
                    values
                        .iter()
                        .filter_map(|value| value.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            AgentEvent::StanceTransition {
                session_id: sid(),
                phase: event_type
                    .strip_prefix("stance_")
                    .unwrap_or(event_type)
                    .to_string(),
                escape_tool: obj_string(payload, "escape_tool"),
                allowed_tools,
                justification: obj_string(payload, "justification"),
                consent: obj_string(payload, "consent"),
                reason: obj_string(payload, "reason"),
            }
        }
        // Engine-side corrective nudges (see the retired match's doc
        // comments): each surfaces to operators on the FeedbackInjected
        // stream with a synthesized `kind` and a derived `content`.
        "completion_confirmation_nudge" => feedback(
            "completion_confirmation_nudge",
            feedback_content(obj_string(payload, "visible_text_prefix")),
        ),
        "fenced_call_attempt_nudge" => feedback(
            "fenced_call_attempt_nudge",
            feedback_content(obj_string(payload, "fence")),
        ),
        "missing_tool_call_nudge" => feedback(
            "missing_tool_call_nudge",
            feedback_content(obj_string(payload, "tool")),
        ),
        "no_progress_streak_nudge" => feedback_with_streak(
            "no_progress_streak_nudge",
            feedback_content(NO_PROGRESS_STREAK_NUDGE_FALLBACK.to_string()),
            feedback_streak(payload),
        ),
        "tool_call_blank_name_dropped" => feedback(
            "tool_call_blank_name_dropped",
            feedback_content(obj_usize(payload, "dropped_count").to_string()),
        ),
        "llm_auto_continue" => feedback(
            "llm_auto_continue",
            feedback_content(format!(
                "{}->{} (attempt {}/{})",
                obj_usize(payload, "previous_max_tokens"),
                obj_usize(payload, "raised_max_tokens"),
                obj_usize(payload, "attempt"),
                obj_usize(payload, "max_continuations"),
            )),
        ),
        "context_overflow_recovery" => feedback(
            "context_overflow_recovery",
            feedback_content(format!(
                "attempt {}/{} archived {} messages",
                obj_usize(payload, "attempt"),
                obj_usize(payload, "max_recoveries"),
                obj_usize(payload, "archived_messages"),
            )),
        ),
        _ => return None,
    };
    Some(event)
}

fn first_non_empty_string(payload: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        payload
            .get(*key)
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(str::to_string)
    })
}

fn feedback_streak(payload: &Value) -> Option<usize> {
    let streak = obj_usize(payload, "streak").max(obj_usize(payload, "turns_since_progress"));
    (streak > 0).then_some(streak)
}

/// Generic path: allowlist-check, normalize the payload to match the
/// enum's serde shape, deserialize, then override the ambient `audit`
/// for the two tool-call variants.
fn from_host_generic(
    session_id: &str,
    event_type: &str,
    payload: &Value,
) -> Result<AgentEvent, VmError> {
    if !HOST_DESERIALIZE_EVENT_TYPES.contains(&event_type) {
        return Err(VmError::Runtime(format!(
            "{HOST_AGENT_EMIT_EVENT}: unsupported event type `{event_type}`"
        )));
    }
    let mut obj = match payload {
        Value::Object(map) => map.clone(),
        _ => Map::new(),
    };
    apply_host_payload_defaults(event_type, &mut obj)?;
    obj.insert("type".to_string(), Value::String(event_type.to_string()));
    obj.insert(
        "session_id".to_string(),
        Value::String(session_id.to_string()),
    );
    let mut event: AgentEvent = serde_json::from_value(Value::Object(obj)).map_err(|error| {
        VmError::Runtime(format!(
            "{HOST_AGENT_EMIT_EVENT}: invalid `{event_type}` payload: {error}"
        ))
    })?;
    // `tool_call` / `tool_call_update` carry the mutation-session audit
    // context active at emit time, never a payload-supplied value.
    if let AgentEvent::ToolCall { audit, .. } | AgentEvent::ToolCallUpdate { audit, .. } =
        &mut event
    {
        *audit = crate::orchestration::current_mutation_session();
    }
    Ok(event)
}

/// Fill in the required-field defaults the retired hand-match applied that
/// differ from serde's own missing-field behavior (serde already defaults
/// missing `Option<T>` fields to `None`, so only non-`Option` required
/// fields with a non-zero/non-empty default need help here).
fn apply_host_payload_defaults(
    event_type: &str,
    obj: &mut Map<String, Value>,
) -> Result<(), VmError> {
    match event_type {
        "tool_call" => {
            obj.remove("audit"); // sourced from the ambient mutation session
            set_default(obj, "status", Value::String("pending".to_string()));
            set_default(obj, "raw_input", Value::Null);
        }
        "tool_call_update" => {
            obj.remove("audit"); // sourced from the ambient mutation session
            set_default(obj, "status", Value::String("in_progress".to_string()));
            normalize_executor(obj)?;
        }
        "iteration_end" => set_default(obj, "iteration_info", Value::Null),
        "progress_reported" => {
            set_default(obj, "entries", Value::Array(Vec::new()));
            set_default(obj, "replace", Value::Bool(true));
            set_default(obj, "metadata", Value::Object(Map::new()));
        }
        "tool_search_query" => set_default(obj, "query", Value::Null),
        "tool_search_result" => set_default(obj, "promoted", Value::Array(Vec::new())),
        "skill_narrow" => {
            set_default(obj, "removed_tools", Value::Array(Vec::new()));
            set_default(obj, "remaining_tools", Value::Array(Vec::new()));
        }
        "tool_call_audit" => set_default(obj, "audit", Value::Null),
        _ => {}
    }
    Ok(())
}

/// Normalize a bare-string `executor` into the object form
/// [`super::ToolExecutor`]'s internally-tagged `Deserialize` expects,
/// preserving the retired match's alias set. Non-string values (absent,
/// `null`, or an already-structured `mcp_server` object) are left for
/// serde to handle.
fn normalize_executor(obj: &mut Map<String, Value>) -> Result<(), VmError> {
    let raw = match obj.get("executor") {
        Some(Value::String(value)) => value.clone(),
        _ => return Ok(()),
    };
    let kind = match raw.trim() {
        "" => {
            obj.remove("executor");
            return Ok(());
        }
        "harn" | "harn_builtin" => "harn_builtin",
        "host" | "host_bridge" => "host_bridge",
        "provider" | "provider_native" => "provider_native",
        other => {
            return Err(VmError::Runtime(format!(
                "{HOST_AGENT_EMIT_EVENT}: invalid tool executor `{other}`"
            )));
        }
    };
    let mut executor = Map::new();
    executor.insert("kind".to_string(), Value::String(kind.to_string()));
    obj.insert("executor".to_string(), Value::Object(executor));
    Ok(())
}

fn set_default(obj: &mut Map<String, Value>, key: &str, value: Value) {
    obj.entry(key).or_insert(value);
}

fn obj_string(payload: &Value, key: &str) -> String {
    payload
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

fn obj_usize(payload: &Value, key: &str) -> usize {
    payload.get(key).and_then(Value::as_u64).unwrap_or(0) as usize
}
