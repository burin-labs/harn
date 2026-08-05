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
//! [`HOST_EVENT_POLICIES`] is the single registry for this boundary. It owns
//! both which `event_type` strings may enter through the host path and whether
//! each accepted event is copied into the live transcript journal. Many
//! `AgentEvent` variants (`worker_update`, `handoff`, `artifact`, …) are
//! constructed elsewhere and are *not* emittable through this host path.

use serde_json::{Map, Value};

use crate::value::VmError;

use super::AgentEvent;

const HOST_AGENT_EMIT_EVENT: &str = "__host_agent_emit_event";
const NO_PROGRESS_STREAK_NUDGE_FALLBACK: &str =
    "No progress was detected. Use the next turn to make concrete task progress or explain the remaining blocker.";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HostTranscriptRole {
    Assistant,
    Tool,
}

impl HostTranscriptRole {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Assistant => "assistant",
            Self::Tool => "tool",
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct HostEventPolicy {
    event_type: &'static str,
    transcript_role: Option<HostTranscriptRole>,
}

const fn host_event(
    event_type: &'static str,
    transcript_role: Option<HostTranscriptRole>,
) -> HostEventPolicy {
    HostEventPolicy {
        event_type,
        transcript_role,
    }
}

const ASSISTANT: Option<HostTranscriptRole> = Some(HostTranscriptRole::Assistant);
const TOOL: Option<HostTranscriptRole> = Some(HostTranscriptRole::Tool);

/// The one policy registry for events entering through `agent_emit_event`.
///
/// A registry row authorizes host deserialization. `transcript_role` controls
/// whether the same accepted payload is copied into the durable live-session
/// journal. Keeping both decisions together prevents an event from appearing
/// registered at its stdlib call site while being silently absent from one of
/// the runtime's observation surfaces.
const HOST_EVENT_POLICIES: &[HostEventPolicy] = &[
    host_event("tool_call", ASSISTANT),
    host_event("tool_call_update", ASSISTANT),
    host_event("iteration_start", None),
    host_event("iteration_end", None),
    host_event("judge_decision", None),
    host_event("step_judge_decision", None),
    host_event("structural_validator_decision", None),
    host_event("scope_classifier_verdict", None),
    host_event("input_guardrail_verdict", None),
    host_event("missing_tool_call_verdict", None),
    host_event("require_successful_tools_violation", ASSISTANT),
    host_event("final_wrapup", ASSISTANT),
    host_event("pack_thinking_stripped", ASSISTANT),
    host_event("self_consistency_tie", ASSISTANT),
    host_event("code_librarian_query_nl_fallback", ASSISTANT),
    host_event("budget_exhausted", ASSISTANT),
    host_event("budget_circuit_breaker", ASSISTANT),
    host_event("progress_reported", None),
    host_event("tool_search_query", ASSISTANT),
    host_event("tool_search_result", TOOL),
    host_event("skill_narrow", ASSISTANT),
    host_event("loop_control_decision", None),
    host_event("capability_gap", None),
    host_event("tool_format_override", ASSISTANT),
    host_event("tool_call_audit", TOOL),
    host_event("tool_batch_disposition", TOOL),
    host_event("loop_checkpoint", ASSISTANT),
    // The loud-boundary funnel (harn#5142). Registered so a `.harn` boundary
    // reports a drop through the same typed event as the Rust funnel.
    host_event("boundary_failure", None),
    host_event("typed_checkpoint", ASSISTANT),
    host_event("model_job", TOOL),
    host_event("loop_stuck", ASSISTANT),
    host_event("reserved_terminal_verify", ASSISTANT),
    host_event("agent_loop_stall_warning", ASSISTANT),
    host_event("cache_hit", None),
    host_event("cache_miss", None),
    // `std/llm` handler telemetry. Every one of these shipped in the embedded
    // stdlib while this registry refused it, so the events were emitted and
    // dropped; the drift check in `from_host_tests` is what keeps the two
    // halves together from here.
    host_event("llm_call_log", None),
    host_event("llm_routing_decision", None),
    host_event("llm_fallback_attempt", None),
    host_event("llm_shadow_diff", None),
    host_event("semantic_cache_hit", None),
    host_event("semantic_cache_miss", None),
    host_event("agent_scratchpad_reorganization", None),
    host_event("stance_armed", None),
    host_event("stance_write_access_granted", None),
    host_event("stance_write_access_denied", None),
    host_event("stance_disarmed", None),
    host_event("completion_confirmation_nudge", None),
    host_event("fenced_call_attempt_nudge", None),
    host_event("missing_tool_call_nudge", None),
    host_event("no_progress_streak_nudge", None),
    host_event("tool_call_blank_name_dropped", None),
    host_event("llm_auto_continue", None),
    host_event("context_overflow_recovery", ASSISTANT),
];

/// Every `event_type` this boundary accepts, for the drift check that keeps
/// the embedded stdlib's emitters and this registry in sync.
#[cfg(test)]
pub(super) fn registered_host_event_types() -> impl Iterator<Item = &'static str> {
    HOST_EVENT_POLICIES.iter().map(|policy| policy.event_type)
}

fn host_event_policy(event_type: &str) -> Option<&'static HostEventPolicy> {
    HOST_EVENT_POLICIES
        .iter()
        .find(|policy| policy.event_type == event_type)
}

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
        if host_event_policy(event_type).is_none() {
            return Err(reject(
                session_id,
                format!("unsupported event type `{event_type}`"),
                payload,
            ));
        }
        if let Some(event) = from_host_special(session_id, event_type, payload) {
            return Ok(event);
        }
        from_host_generic(session_id, event_type, payload)
    }

    pub(crate) fn host_transcript_role(event_type: &str) -> Option<HostTranscriptRole> {
        host_event_policy(event_type).and_then(|policy| policy.transcript_role)
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
        "model_job" => AgentEvent::ModelJob {
            session_id: sid(),
            event: payload.clone(),
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
        "llm_call_log" => AgentEvent::LlmCallLog {
            session_id: sid(),
            model: obj_string(payload, "model"),
            provider: obj_string(payload, "provider"),
            status: obj_string(payload, "status"),
            latency_ms: obj_usize(payload, "latency_ms"),
            iteration: obj_usize(payload, "iteration"),
            attempt: obj_usize(payload, "attempt"),
            payload: payload.clone(),
        },
        "llm_routing_decision" => AgentEvent::LlmRoutingDecision {
            session_id: sid(),
            route_index: obj_i64(payload, "route_index"),
            route_name: obj_string(payload, "route_name"),
            used_default: obj_bool(payload, "used_default"),
            payload: payload.clone(),
        },
        "llm_fallback_attempt" => AgentEvent::LlmFallbackAttempt {
            session_id: sid(),
            fallback_index: obj_usize(payload, "fallback_index"),
            fallback_total: obj_usize(payload, "fallback_total"),
            ok: obj_bool(payload, "ok"),
            status: obj_string(payload, "status"),
            payload: payload.clone(),
        },
        "llm_shadow_diff" => AgentEvent::LlmShadowDiff {
            session_id: sid(),
            primary_ok: obj_bool(payload, "primary_ok"),
            shadow_ok: obj_bool(payload, "shadow_ok"),
            primary_status: obj_string(payload, "primary_status"),
            shadow_status: obj_string(payload, "shadow_status"),
            primary_len: obj_usize(payload, "primary_len"),
            shadow_len: obj_usize(payload, "shadow_len"),
            payload: payload.clone(),
        },
        "semantic_cache_hit" => AgentEvent::SemanticCacheHit {
            session_id: sid(),
            similarity: obj_f64(payload, "similarity"),
            provider: obj_string(payload, "provider"),
            model: obj_string(payload, "model"),
            payload: payload.clone(),
        },
        "semantic_cache_miss" => AgentEvent::SemanticCacheMiss {
            session_id: sid(),
            nearest_similarity: obj_f64(payload, "nearest_similarity"),
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
        reject(
            session_id,
            format!("invalid `{event_type}` payload: {error}"),
            payload,
        )
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

/// Refuse a host event, loudly.
///
/// The `VmError` alone was not enough: every stdlib emit site wraps
/// `agent_emit_event` in `try { }` and discards the result, so a rejected
/// event type or a malformed payload used to vanish without a trace — the
/// runtime's own event bus had a silent boundary in it. The rejection now also
/// goes out through the loud-boundary funnel (harn#5142), which is not
/// swallowable by a caller's `try`, so a `.harn` boundary reporting through a
/// name nobody registered is visible instead of merely ineffective.
fn reject(session_id: &str, detail: String, payload: &Value) -> VmError {
    crate::boundary::BoundaryFailure::new(
        crate::boundary::BoundaryId::HostEventIngest,
        crate::boundary::BoundaryFailureKind::Unrecognized,
        detail.clone(),
    )
    .in_session(session_id)
    .with_excerpt(&payload.to_string())
    .report();
    VmError::Runtime(format!("{HOST_AGENT_EMIT_EVENT}: {detail}"))
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
        // `owner` is derived from `kind`, never supplied: one attribution rule
        // for the Rust funnel and the `.harn` boundaries alike. A payload that
        // tries to set it is overruled rather than trusted.
        "boundary_failure" => {
            let owner = obj
                .get("kind")
                .and_then(Value::as_str)
                .and_then(|kind| {
                    serde_json::from_value::<crate::boundary::BoundaryFailureKind>(Value::String(
                        kind.to_string(),
                    ))
                    .ok()
                })
                .map(|kind| kind.owner())
                .unwrap_or("harness");
            obj.insert("owner".to_string(), Value::String(owner.to_string()));
        }
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

fn obj_bool(payload: &Value, key: &str) -> bool {
    payload.get(key).and_then(Value::as_bool).unwrap_or(false)
}

fn obj_f64(payload: &Value, key: &str) -> f64 {
    payload.get(key).and_then(Value::as_f64).unwrap_or(0.0)
}

/// A route index is `-1` when the router fell through to its default, so this
/// one cannot borrow [`obj_usize`].
fn obj_i64(payload: &Value, key: &str) -> i64 {
    payload.get(key).and_then(Value::as_i64).unwrap_or(0)
}
