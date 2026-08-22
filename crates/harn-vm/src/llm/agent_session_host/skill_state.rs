//! Active skill state and durable skill lifecycle events.

use super::*;

/// Replace the session's active skill list.
#[harn_builtin(
    exposure = "runtime_internal",
    effects = [],
    sig = "__host_agent_session_set_active_skills(session_id: string, skills: list) -> nil",
    category = "agent.host",
    runtime_only = true
)]
fn host_agent_session_set_active_skills_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let session_id = args.first().map(|v| v.display()).unwrap_or_default();
    let skills_value = args.get(1).cloned().unwrap_or(VmValue::Nil);
    let ids: Vec<String> = list_items(&skills_value)
        .iter()
        .filter_map(|v| dict_get(v, "id").map(|v| v.display()))
        .collect();
    with_session(&session_id, HOST_SESSION_SET_ACTIVE_SKILLS, |session| {
        session.active_skills = ids.clone();
        Ok(())
    })?;
    crate::agent_sessions::set_active_skills(&session_id, ids);
    Ok(VmValue::Nil)
}

/// Return the session's active skill list.
#[harn_builtin(
    exposure = "runtime_internal",
    effects = [],
    sig = "__host_agent_session_active_skills(session_id: string) -> list",
    category = "agent.host",
    runtime_only = true
)]
fn host_agent_session_active_skills_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let session_id = args.first().map(|v| v.display()).unwrap_or_default();
    let ids = with_session(&session_id, HOST_SESSION_ACTIVE_SKILLS, |session| {
        Ok(session.active_skills.clone())
    })?;
    let list = ids
        .into_iter()
        .map(|id| {
            let mut entry = crate::value::DictMap::new();
            entry.put_str("id", id);
            VmValue::dict(entry)
        })
        .collect();
    Ok(VmValue::List(std::sync::Arc::new(list)))
}

/// Append a skill lifecycle event and notify live agent-event sinks.
#[harn_builtin(
    exposure = "runtime_internal",
    effects = [],
    sig = "__host_agent_session_record_skill_event(session_id: string, kind: string, metadata: dict) -> nil",
    category = "agent.host",
    runtime_only = true
)]
fn host_agent_session_record_skill_event_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let session_id = args.first().map(|v| v.display()).unwrap_or_default();
    let kind = args.get(1).map(|v| v.display()).unwrap_or_default();
    let metadata = args.get(2).cloned().unwrap_or(VmValue::Nil);
    if session_id.is_empty() || kind.is_empty() {
        return Ok(VmValue::Nil);
    }
    let metadata_json = vm_to_json(&metadata);
    let text = metadata_json
        .get("name")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    let event = crate::llm::helpers::transcript_event(
        &kind,
        "system",
        "internal",
        &text,
        Some(metadata_json.clone()),
    );
    crate::agent_sessions::append_event(&session_id, event).map_err(VmError::Runtime)?;

    let name = metadata_json
        .get("name")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    let iteration = metadata_json
        .get("iteration")
        .and_then(|value| value.as_u64())
        .unwrap_or(0) as usize;
    match kind.as_str() {
        "skill_activated" if !name.is_empty() => {
            let reason = metadata_json
                .get("trigger")
                .or_else(|| metadata_json.get("reason"))
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .to_string();
            crate::llm::agent_runtime::emit_agent_event_sync(&AgentEvent::SkillActivated {
                session_id,
                skill_name: name,
                iteration,
                reason,
            });
        }
        "skill_deactivated" if !name.is_empty() => {
            crate::llm::agent_runtime::emit_agent_event_sync(&AgentEvent::SkillDeactivated {
                session_id,
                skill_name: name,
                iteration,
            });
        }
        "skill_scope_tools" if !name.is_empty() => {
            let allowed_tools = metadata_json
                .get("allowed_tools")
                .and_then(|value| value.as_array())
                .map(|values| {
                    values
                        .iter()
                        .filter_map(|value| value.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            crate::llm::agent_runtime::emit_agent_event_sync(&AgentEvent::SkillScopeTools {
                session_id,
                skill_name: name,
                allowed_tools,
            });
        }
        "skill_narrow" => {
            let string_list = |key: &str| {
                metadata_json
                    .get(key)
                    .and_then(|value| value.as_array())
                    .map(|values| {
                        values
                            .iter()
                            .filter_map(|value| value.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default()
            };
            let reason = metadata_json
                .get("reason")
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .to_string();
            crate::llm::agent_runtime::emit_agent_event_sync(&AgentEvent::SkillNarrow {
                session_id,
                reason,
                removed_tools: string_list("removed_tools"),
                remaining_tools: string_list("remaining_tools"),
                policy: metadata_json
                    .get("policy")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null),
                removed_tool_details: metadata_json
                    .get("removed_tool_details")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null),
                kept_tool_details: metadata_json
                    .get("kept_tool_details")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null),
            });
        }
        _ => {}
    }
    Ok(VmValue::Nil)
}

const SKILL_STATE_BUILTINS: &[&VmBuiltinDef] = &[
    &HOST_AGENT_SESSION_SET_ACTIVE_SKILLS_BUILTIN_DEF,
    &HOST_AGENT_SESSION_ACTIVE_SKILLS_BUILTIN_DEF,
    &HOST_AGENT_SESSION_RECORD_SKILL_EVENT_BUILTIN_DEF,
];

pub(super) fn register_skill_state_primitives(vm: &mut Vm) {
    register_builtin_defs(vm, SKILL_STATE_BUILTINS);
}
