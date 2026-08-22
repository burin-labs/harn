//! Turn projection, compaction, tool-format claims, and pre-call bookkeeping.

use super::*;

/// No-op compaction hook; Harn implements compaction via llm_call.
#[harn_builtin(
    exposure = "runtime_internal",
    effects = [],
    sig = "__host_agent_session_compact_if_needed(session_id: string, options: dict) -> dict",
    category = "agent.host",
    runtime_only = true
)]
fn host_agent_session_compact_builtin(
    _args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    Ok(VmValue::Nil)
}

/// Replace the session's transcript message list (used by Harn-driven auto-compact).
#[harn_builtin(
    exposure = "runtime_internal",
    effects = [],
    sig = "__host_agent_session_replace_messages(session_id: string, messages: list, summary?: any) -> nil",
    category = "agent.host",
    runtime_only = true
)]
fn host_agent_session_replace_messages_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let session_id = match args.first() {
        Some(VmValue::String(s)) if !s.is_empty() => s.to_string(),
        _ => {
            return Err(VmError::Runtime(format!(
                "{HOST_SESSION_REPLACE_MESSAGES}: session_id must be a non-empty string"
            )))
        }
    };
    let messages_json: Vec<serde_json::Value> = match args.get(1) {
        Some(VmValue::List(list)) => list.iter().map(vm_to_json).collect(),
        _ => {
            return Err(VmError::Runtime(format!(
                "{HOST_SESSION_REPLACE_MESSAGES}: messages must be a list"
            )))
        }
    };
    let summary = match args.get(2) {
        Some(VmValue::String(s)) if !s.is_empty() => Some(s.to_string()),
        _ => None,
    };
    crate::agent_sessions::replace_messages_with_summary(
        &session_id,
        &messages_json,
        summary.as_deref(),
    )
    .map_err(VmError::Runtime)?;
    Ok(VmValue::Nil)
}

/// Score skills against the current task context.
#[harn_builtin(
    exposure = "runtime_internal",
    effects = [],
    sig = "__host_skill_score(context: dict, registry: dict, options: dict) -> dict",
    kind = "async",
    category = "agent.host",
    runtime_only = true
)]
async fn host_skill_score(
    _ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let context = args.first().cloned().unwrap_or(VmValue::Nil);
    let registry = args.get(1).cloned().unwrap_or(VmValue::Nil);
    let options = args.get(2).cloned().unwrap_or(VmValue::Nil);
    crate::llm::skill_score::score_skill_registry(
        &context,
        &registry,
        &options,
        crate::llm::current_host_bridge(),
    )
    .await
}

/// Pre-call budget projection hook (returns false for now).
#[harn_builtin(
    exposure = "runtime_internal",
    effects = [],
    sig = "__host_agent_budget_pre_call_blocked(session_id: string, envelope: dict) -> bool",
    category = "agent.host",
    runtime_only = true
)]
fn host_agent_budget_pre_call_builtin(
    _args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    Ok(VmValue::Bool(false))
}

/// Record a native→text tool-call fallback as a transcript event and trace counter.
#[harn_builtin(
    exposure = "runtime_internal",
    effects = [],
    sig = "__host_agent_record_native_tool_fallback(session_id: string, payload: dict) -> nil",
    category = "agent.host",
    runtime_only = true
)]
fn host_agent_record_native_tool_fallback_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let session_id = match args.first() {
        Some(VmValue::String(s)) if !s.is_empty() => s.to_string(),
        _ => {
            return Err(VmError::Runtime(format!(
                "{HOST_AGENT_RECORD_NATIVE_TOOL_FALLBACK}: session_id must be a non-empty string"
            )))
        }
    };
    let payload = args.get(1).cloned().unwrap_or(VmValue::Nil);
    let payload_json = vm_to_json(&payload);
    let accepted = payload_json
        .get("accepted")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let policy = payload_json
        .get("policy")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .to_string();
    let fallback_index = payload_json
        .get("fallback_index")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0) as usize;
    let tool_call_count = payload_json
        .get("tool_call_count")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0) as usize;
    let iteration = payload_json
        .get("iteration")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0) as usize;
    crate::llm::trace::emit_agent_event(crate::llm::trace::AgentTraceEvent::NativeToolFallback {
        iteration,
        accepted,
        policy,
        fallback_index,
        tool_call_count,
    });
    let event = crate::llm::helpers::transcript_event(
        "native_tool_fallback",
        "assistant",
        "internal",
        "",
        Some(payload_json),
    );
    crate::agent_sessions::append_event(&session_id, event).map_err(VmError::Runtime)?;
    Ok(VmValue::Nil)
}

/// Record a transcript compaction as a transcript event and trace counter.
#[harn_builtin(
    exposure = "runtime_internal",
    effects = [],
    sig = "__host_agent_record_compaction(session_id: string, payload: dict) -> nil",
    category = "agent.host",
    runtime_only = true
)]
fn host_agent_record_compaction_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let session_id = match args.first() {
        Some(VmValue::String(s)) if !s.is_empty() => s.to_string(),
        _ => {
            return Err(VmError::Runtime(format!(
                "{HOST_AGENT_RECORD_COMPACTION}: session_id must be a non-empty string"
            )))
        }
    };
    let payload = args.get(1).cloned().unwrap_or(VmValue::Nil);
    let payload_json = vm_to_json(&payload);
    let archived_messages = payload_json
        .get("archived_messages")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0) as usize;
    let new_summary_len = payload_json
        .get("new_summary_len")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0) as usize;
    let iteration = payload_json
        .get("iteration")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0) as usize;
    crate::llm::trace::emit_agent_event(crate::llm::trace::AgentTraceEvent::ContextCompaction {
        archived_messages,
        new_summary_len,
        iteration,
    });
    // Normalize the host-script payload into the one canonical receipt at this
    // builtin boundary, so a `.harn`-driven compaction yields the same unified
    // receipt (and shared id) as the Rust lifecycle paths (harn#4995).
    let receipt =
        crate::orchestration::CompactionReceipt::from_host_payload(&session_id, &payload_json);
    let mut metadata = payload_json;
    if let Some(obj) = metadata.as_object_mut() {
        obj.insert("receipt".to_string(), receipt.to_json());
    }
    let event = crate::llm::helpers::transcript_event_with_id(
        &receipt.receipt_id,
        "compaction",
        "system",
        "internal",
        "",
        Some(metadata),
    );
    crate::agent_sessions::append_event(&session_id, event).map_err(VmError::Runtime)?;
    crate::orchestration::emit_transcript_compacted_event_sync(&session_id, receipt);
    Ok(VmValue::Nil)
}

/// Project the session transcript through a policy, append a
/// transcript.projection event, and return the projected messages
/// with metadata.
#[harn_builtin(
    exposure = "runtime_internal",
    effects = [],
    sig = "__host_agent_session_project_turn(session_id: string, options?: dict|nil) -> dict",
    kind = "async",
    category = "agent.host",
    runtime_only = true
)]
async fn host_agent_session_project_turn(
    ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let session_id = match args.first() {
        Some(VmValue::String(s)) if !s.is_empty() => s.to_string(),
        _ => {
            return Err(VmError::Runtime(format!(
                "{HOST_SESSION_PROJECT_TURN}: session_id must be a non-empty string"
            )))
        }
    };
    let options = args.get(1).cloned().unwrap_or(VmValue::Nil);
    let policy = crate::stdlib::transcript_project::parse_projection_options(&options)?;
    let Some(transcript) = crate::agent_sessions::transcript(&session_id) else {
        return Err(VmError::Runtime(format!(
            "{HOST_SESSION_PROJECT_TURN}: unknown agent session `{session_id}`"
        )));
    };
    let transcript_dict = transcript.as_dict().cloned().unwrap_or_default();
    let result = crate::stdlib::transcript_project::project_transcript(
        Some(&ctx),
        &transcript_dict,
        &policy,
    )
    .await?;
    let event = crate::stdlib::transcript_project::projection_event_value(&result, &policy);
    let _ = crate::agent_sessions::append_event(&session_id, event.clone());
    crate::llm::emit_live_agent_event_with_ctx(
        Some(&ctx),
        &AgentEvent::TranscriptProjected {
            session_id: session_id.clone(),
            policy: policy.kind.as_str().to_string(),
            reason: result.reason.clone(),
            prefix_hash: result.prefix_hash.clone(),
            kept_count: result.kept_indices.len(),
            dropped_count: result.dropped_indices.len(),
            provider_safety_blocked: result.provider_safety_blocked,
            redacted_count: result.redaction_pointers.len(),
            reclaimed_tokens: result.reclaimed_tokens,
            roots_consulted: result.roots_consulted.clone(),
            redaction_pointers: result.redaction_pointers.clone(),
        },
    )
    .await;
    Ok(crate::stdlib::transcript_project::result_to_vm(
        &result, &policy,
    ))
}

/// Claim the session's tool_format contract; rejects mid-session changes.
#[harn_builtin(
    exposure = "runtime_internal",
    effects = [],
    sig = "__host_agent_session_claim_tool_format(session_id: string, tool_format: string) -> dict",
    category = "agent.host",
    runtime_only = true
)]
fn host_agent_session_claim_tool_format_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let session_id = match args.first() {
        Some(VmValue::String(s)) if !s.is_empty() => s.to_string(),
        _ => {
            return Err(VmError::Runtime(format!(
                "{HOST_SESSION_CLAIM_TOOL_FORMAT}: session_id must be a non-empty string"
            )))
        }
    };
    let tool_format = match args.get(1) {
        Some(VmValue::String(s)) if !s.is_empty() => s.to_string(),
        _ => {
            return Err(VmError::Runtime(format!(
                "{HOST_SESSION_CLAIM_TOOL_FORMAT}: tool_format must be a non-empty string"
            )))
        }
    };
    crate::agent_sessions::claim_tool_format(&session_id, &tool_format)
        .map_err(VmError::Runtime)?;
    with_session(&session_id, HOST_SESSION_CLAIM_TOOL_FORMAT, |session| {
        session.tool_mode = tool_format.clone();
        Ok(())
    })?;
    Ok(VmValue::Nil)
}

const TURN_PROJECTION_BUILTINS: &[&VmBuiltinDef] = &[
    &HOST_AGENT_SESSION_COMPACT_BUILTIN_DEF,
    &HOST_AGENT_SESSION_REPLACE_MESSAGES_BUILTIN_DEF,
    &HOST_AGENT_BUDGET_PRE_CALL_BUILTIN_DEF,
    &HOST_AGENT_SESSION_CLAIM_TOOL_FORMAT_BUILTIN_DEF,
    &HOST_AGENT_RECORD_NATIVE_TOOL_FALLBACK_BUILTIN_DEF,
    &HOST_AGENT_RECORD_COMPACTION_BUILTIN_DEF,
    &HOST_SKILL_SCORE_DEF,
    &HOST_AGENT_SESSION_PROJECT_TURN_DEF,
];

pub(super) fn register_turn_projection_primitives(vm: &mut Vm) {
    register_builtin_defs(vm, TURN_PROJECTION_BUILTINS);
}
