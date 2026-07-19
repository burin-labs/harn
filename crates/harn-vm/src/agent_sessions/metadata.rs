use super::*;

pub fn set_active_skills(id: &str, skills: Vec<String>) {
    SESSIONS.with(|s| {
        if let Some(state) = s.borrow_mut().get_mut(id) {
            state.active_skills = skills;
            state.touch();
        }
    });
}

/// Skills that were active at the end of the previous agent_loop run
/// against this session. Returns an empty vec when the session doesn't
/// exist or nothing was persisted.
pub fn active_skills(id: &str) -> Vec<String> {
    SESSIONS.with(|s| {
        s.borrow()
            .get(id)
            .map(|state| state.active_skills.clone())
            .unwrap_or_default()
    })
}

/// Claim the tool-calling contract for a session.
///
/// The first loop against a named session records its `tool_format`.
/// Later re-entry must use the same format so prompt/history generated
/// under a text contract is never replayed as native, or vice versa.
pub fn claim_tool_format(id: &str, tool_format: &str) -> Result<(), String> {
    let tool_format = tool_format.trim();
    if tool_format.is_empty() {
        return Ok(());
    }
    SESSIONS.with(|s| {
        let mut map = s.borrow_mut();
        let Some(state) = map.get_mut(id) else {
            return Err(format!("agent session '{id}' does not exist"));
        };
        match state.tool_format.as_deref() {
            Some(existing) if existing != tool_format => Err(format!(
                "agent session '{id}' is locked to tool_format='{existing}', but this run requested tool_format='{tool_format}'. Start a new session or fork/reset the transcript before changing tool mode."
            )),
            Some(_) => {
                state.touch();
                Ok(())
            }
            None => {
                state.tool_format = Some(tool_format.to_string());
                state.touch();
                Ok(())
            }
        }
    })
}

pub fn tool_format(id: &str) -> Option<String> {
    SESSIONS.with(|s| {
        s.borrow()
            .get(id)
            .and_then(|state| state.tool_format.clone())
    })
}

pub fn record_system_prompt(id: &str, system_prompt: &str) -> Result<(), String> {
    let system_prompt = system_prompt.trim();
    if system_prompt.is_empty() {
        return Ok(());
    }
    assert_cache_stable_system_prompt(system_prompt);
    SESSIONS.with(|s| {
        let mut map = s.borrow_mut();
        let Some(state) = map.get_mut(id) else {
            return Err(format!("agent session '{id}' does not exist"));
        };
        let changed = state.system_prompt.as_deref() != Some(system_prompt);
        state.system_prompt = Some(system_prompt.to_string());
        let dict = state
            .transcript
            .as_dict()
            .cloned()
            .unwrap_or_else(crate::value::DictMap::new);
        let mut next = dict;
        apply_system_prompt_metadata(&mut next, system_prompt);
        if changed {
            let mut events: Vec<VmValue> = match next.get("events") {
                Some(VmValue::List(list)) => list.iter().cloned().collect(),
                _ => Vec::new(),
            };
            events.push(crate::llm::helpers::transcript_event(
                "system_prompt",
                "system",
                "internal",
                "",
                Some(crate::llm::helpers::system_prompt_event_metadata(
                    system_prompt,
                )),
            ));
            next.insert(
                crate::value::intern_key("events"),
                VmValue::List(std::sync::Arc::new(events)),
            );
        }
        apply_transcript_with_budget(state, VmValue::dict(next), "record_system_prompt")?;
        Ok(())
    })
}

pub fn system_prompt(id: &str) -> Option<String> {
    SESSIONS.with(|s| {
        s.borrow()
            .get(id)
            .and_then(|state| state.system_prompt.clone())
    })
}

#[cfg(debug_assertions)]
pub(super) fn forbidden_workspace_prompt_token(system_prompt: &str) -> Option<&'static str> {
    let mut remaining = system_prompt;
    while let Some(index) = remaining.find("{{") {
        let candidate = remaining[index + 2..].trim_start();
        if candidate.starts_with("workspace_") {
            return Some("workspace_");
        }
        if candidate.starts_with("project_") {
            return Some("project_");
        }
        remaining = candidate;
    }
    None
}

#[cfg(debug_assertions)]
pub(super) fn assert_cache_stable_system_prompt(system_prompt: &str) {
    if let Some(prefix) = forbidden_workspace_prompt_token(system_prompt) {
        panic!(
            "{CACHE_STABLE_SYSTEM_PROMPT_DIAGNOSTIC}: session system prompts must not interpolate `{{{{{prefix}...` tokens; move workspace/project context into the workspace-anchor reminder"
        );
    }
}

#[cfg(not(debug_assertions))]
pub(super) fn assert_cache_stable_system_prompt(_system_prompt: &str) {}

/// Pin (or clear, with `None`) a model selector on a session. Returns
/// `Ok(true)` when the value actually changed so callers can decide
/// whether to broadcast a notification. The selector is stored verbatim
/// — alias / catalog resolution is the call-site's job.
pub fn set_pinned_model(id: &str, model: Option<String>) -> Result<bool, String> {
    let normalized = model
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    SESSIONS.with(|s| {
        let mut map = s.borrow_mut();
        let Some(state) = map.get_mut(id) else {
            return Err(format!("agent session '{id}' does not exist"));
        };
        let changed = state.pinned_model != normalized;
        state.pinned_model = normalized;
        state.touch();
        Ok(changed)
    })
}

/// Read the session's pinned model selector, if any. Consumed by
/// `vm_resolve_model` as the per-session default when a script-level
/// `llm_call` does not pass `model:` explicitly.
pub fn pinned_model(id: &str) -> Option<String> {
    SESSIONS.with(|s| {
        s.borrow()
            .get(id)
            .and_then(|state| state.pinned_model.clone())
    })
}

/// Pin (or clear) the session-level provider-aware reasoning policy.
pub fn set_pinned_reasoning_policy(id: &str, policy: Option<String>) -> Result<bool, String> {
    let normalized = match policy {
        Some(value) => crate::llm::reasoning_policy::normalize_policy_selector(&value)?,
        None => None,
    };
    SESSIONS.with(|s| {
        let mut map = s.borrow_mut();
        let Some(state) = map.get_mut(id) else {
            return Err(format!("agent session '{id}' does not exist"));
        };
        let changed = state.pinned_reasoning_policy != normalized;
        state.pinned_reasoning_policy = normalized;
        state.touch();
        Ok(changed)
    })
}

/// Read the session's pinned reasoning policy, if any.
pub fn pinned_reasoning_policy(id: &str) -> Option<String> {
    SESSIONS.with(|s| {
        s.borrow()
            .get(id)
            .and_then(|state| state.pinned_reasoning_policy.clone())
    })
}

/// Set (or clear, with `None`) the typed workspace anchor on a session.
/// Returns `Ok(true)` when the value actually changed so callers can
/// decide whether to broadcast `AnchorChanged` notifications.
pub fn set_workspace_anchor(id: &str, anchor: Option<WorkspaceAnchor>) -> Result<bool, String> {
    SESSIONS.with(|s| {
        let mut map = s.borrow_mut();
        let Some(state) = map.get_mut(id) else {
            return Err(format!("agent session '{id}' does not exist"));
        };
        let changed = state.workspace_anchor != anchor;
        state.workspace_anchor = anchor;
        if changed {
            state.redo_stack.clear();
            crate::llm::permissions::clear_session_grants(id);
        }
        state.touch();
        Ok(changed)
    })
}

/// Read the session's typed workspace anchor, if any.
pub fn workspace_anchor(id: &str) -> Option<WorkspaceAnchor> {
    SESSIONS.with(|s| {
        s.borrow()
            .get(id)
            .and_then(|state| state.workspace_anchor.clone())
    })
}

/// Outcome of `reanchor_session`: previous + new anchor and whether the
/// swap actually moved anything. Callers use `changed` to suppress
/// no-op transcript / live events.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReanchorOutcome {
    pub previous: Option<WorkspaceAnchor>,
    pub current: WorkspaceAnchor,
    pub changed: bool,
}

/// Atomically swap the session's primary anchor + emit the canonical
/// `AnchorChanged` transcript event and live `AgentEvent::AnchorChanged`
/// notification (#2218). Clears session-scoped permission grants so
/// stale anchor-based decisions don't leak into the next turn.
pub fn reanchor_session(
    id: &str,
    new_anchor: WorkspaceAnchor,
    carry_transcript: bool,
    compacted: bool,
    reason: Option<String>,
) -> Result<ReanchorOutcome, String> {
    let outcome = SESSIONS.with(|s| {
        let mut map = s.borrow_mut();
        let Some(state) = map.get_mut(id) else {
            return Err(format!("agent session '{id}' does not exist"));
        };
        let previous = state.workspace_anchor.clone();
        let changed = previous.as_ref() != Some(&new_anchor);
        state.workspace_anchor = Some(new_anchor.clone());
        if changed {
            crate::llm::permissions::clear_session_grants(id);
        }
        state.touch();
        Ok(ReanchorOutcome {
            previous,
            current: new_anchor,
            changed,
        })
    })?;
    if !outcome.changed {
        return Ok(outcome);
    }
    let previous_json = outcome.previous.as_ref().map(WorkspaceAnchor::to_json);
    let current_json = outcome.current.to_json();
    let event_metadata = serde_json::json!({
        "previous": previous_json,
        "current": current_json,
        "carry_transcript": carry_transcript,
        "compacted": compacted,
        "reason": reason,
    });
    let event = crate::llm::helpers::transcript_event(
        "AnchorChanged",
        "system",
        "internal",
        "",
        Some(event_metadata),
    );
    let _ = append_event(id, event);
    crate::llm::emit_live_agent_event_sync(&crate::agent_events::AgentEvent::AnchorChanged {
        session_id: id.to_string(),
        previous: previous_json,
        current: current_json,
        carry_transcript,
        compacted,
        reason,
    });
    Ok(outcome)
}

/// Set session-local workspace defaults. Returns `Ok(true)` when the
/// policy changed.
pub fn set_workspace_policy(id: &str, policy: WorkspacePolicy) -> Result<bool, String> {
    SESSIONS.with(|s| {
        let mut map = s.borrow_mut();
        let Some(state) = map.get_mut(id) else {
            return Err(format!("agent session '{id}' does not exist"));
        };
        let changed = state.workspace_policy != policy;
        state.workspace_policy = policy;
        if changed {
            state.redo_stack.clear();
        }
        state.touch();
        Ok(changed)
    })
}

/// Read the session's workspace policy, if the session exists.
pub fn workspace_policy(id: &str) -> Option<WorkspacePolicy> {
    SESSIONS.with(|s| {
        s.borrow()
            .get(id)
            .map(|state| state.workspace_policy.clone())
    })
}

/// Validate and mount an additional workspace root on an anchored
/// session. When the path is already mounted, updates its mount mode
/// in place and refreshes its `mounted_at` timestamp.
pub fn add_workspace_root(
    id: &str,
    root: &str,
    mount_mode: Option<MountMode>,
    reason: Option<String>,
) -> Result<String, String> {
    let normalized_root = validate_workspace_root_path(root)?;
    let mounted_at = crate::orchestration::now_rfc3339();
    SESSIONS.with(|s| {
        let mut map = s.borrow_mut();
        let Some(state) = map.get_mut(id) else {
            return Err(format!("agent session '{id}' does not exist"));
        };
        let default_mount_mode = state.workspace_policy.default_mount_mode;
        let Some(anchor) = state.workspace_anchor.as_mut() else {
            return Err(format!("agent session '{id}' has no workspace anchor"));
        };
        let resolved_mount_mode = mount_mode.unwrap_or(default_mount_mode);
        if let Some(existing) = anchor
            .additional_roots
            .iter_mut()
            .find(|entry| entry.path == normalized_root)
        {
            let changed =
                existing.mount_mode != resolved_mount_mode || existing.mounted_at != mounted_at;
            existing.mount_mode = resolved_mount_mode;
            existing.mounted_at = mounted_at.clone();
            if changed {
                state.redo_stack.clear();
            }
        } else {
            anchor.additional_roots.push(MountedRoot {
                path: normalized_root.clone(),
                mount_mode: resolved_mount_mode,
                mounted_at: mounted_at.clone(),
            });
            state.redo_stack.clear();
        }
        let event = crate::llm::helpers::transcript_event(
            "RootMounted",
            "system",
            "internal",
            "",
            Some(serde_json::json!({
                "path": normalized_root.to_string_lossy(),
                "mount_mode": resolved_mount_mode.as_str(),
                "mounted_at": mounted_at.clone(),
                "reason": reason,
            })),
        );
        append_event_to_state(state, event, "add_workspace_root")?;
        crate::llm::permissions::clear_session_grants(id);
        state.touch();
        Ok(mounted_at.clone())
    })
}

/// Remove one mounted root from an anchored session. Returns whether an
/// existing mount entry was deleted. Removing an absent root is a no-op.
pub fn remove_workspace_root(id: &str, root: &str) -> Result<bool, String> {
    let normalized_root = normalize_workspace_root_path(root);
    SESSIONS.with(|s| {
        let mut map = s.borrow_mut();
        let Some(state) = map.get_mut(id) else {
            return Err(format!("agent session '{id}' does not exist"));
        };
        let Some(anchor) = state.workspace_anchor.as_mut() else {
            return Err(format!("agent session '{id}' has no workspace anchor"));
        };
        let before = anchor.additional_roots.len();
        anchor
            .additional_roots
            .retain(|entry| entry.path != normalized_root);
        let removed = anchor.additional_roots.len() != before;
        if removed {
            state.redo_stack.clear();
            crate::llm::permissions::clear_session_grants(id);
        }
        state.touch();
        Ok(removed)
    })
}

pub fn list_workspace_roots(id: &str) -> Result<(PathBuf, Vec<MountedRoot>), String> {
    SESSIONS.with(|s| {
        let map = s.borrow();
        let Some(state) = map.get(id) else {
            return Err(format!("agent session '{id}' does not exist"));
        };
        let Some(anchor) = state.workspace_anchor.as_ref() else {
            return Err(format!("agent session '{id}' has no workspace anchor"));
        };
        Ok((anchor.primary.clone(), anchor.additional_roots.clone()))
    })
}

pub(super) fn validate_workspace_root_path(root: &str) -> Result<PathBuf, String> {
    let normalized = normalize_workspace_root_path(root);
    let canonical = std::fs::canonicalize(&normalized)
        .map_err(|error| format!("workspace root '{root}' must exist and be readable: {error}"))?;
    let metadata = std::fs::metadata(&canonical)
        .map_err(|error| format!("workspace root '{root}' must exist and be readable: {error}"))?;
    if !metadata.is_dir() {
        return Err(format!("workspace root '{root}' must be a directory"));
    }
    std::fs::read_dir(&canonical)
        .map_err(|error| format!("workspace root '{root}' must be readable: {error}"))?;
    Ok(canonical)
}

pub(super) fn normalize_workspace_root_path(root: &str) -> PathBuf {
    let absolute = crate::stdlib::process::normalize_context_path(Path::new(root));
    std::fs::canonicalize(&absolute).unwrap_or(absolute)
}

pub(super) fn empty_transcript(id: &str) -> VmValue {
    use crate::llm::helpers::new_transcript_with;
    new_transcript_with(Some(id.to_string()), Vec::new(), None, None)
}

pub(super) fn clone_transcript_with_id(transcript: &VmValue, new_id: &str) -> VmValue {
    let Some(dict) = transcript.as_dict() else {
        return empty_transcript(new_id);
    };
    let mut next = dict.clone();
    next.put_str("id", new_id);
    VmValue::dict(next)
}

pub(super) fn clone_transcript_with_parent(transcript: &VmValue, parent_id: &str) -> VmValue {
    let Some(dict) = transcript.as_dict() else {
        return transcript.clone();
    };
    let mut next = dict.clone();
    let metadata = match next.get("metadata") {
        Some(VmValue::Dict(metadata)) => {
            let mut metadata = metadata.as_ref().clone();
            metadata.put_str("parent_session_id", parent_id);
            VmValue::dict(metadata)
        }
        _ => VmValue::dict(BTreeMap::from([(
            "parent_session_id".to_string(),
            VmValue::String(arcstr::ArcStr::from(parent_id.to_string())),
        )])),
    };
    next.insert(crate::value::intern_key("metadata"), metadata);
    VmValue::dict(next)
}

pub(super) fn apply_system_prompt_metadata(next: &mut crate::value::DictMap, system_prompt: &str) {
    let mut metadata = match next.get("metadata") {
        Some(VmValue::Dict(metadata)) => metadata.as_ref().clone(),
        _ => crate::value::DictMap::new(),
    };
    metadata.insert(
        crate::value::intern_key("system_prompt"),
        crate::stdlib::json_to_vm_value(&crate::llm::helpers::system_prompt_metadata(
            system_prompt,
        )),
    );
    next.insert(
        crate::value::intern_key("metadata"),
        VmValue::dict(metadata),
    );
}

pub(super) fn transcript_with_session_metadata(
    transcript: VmValue,
    state: &SessionState,
) -> VmValue {
    let Some(dict) = transcript.as_dict() else {
        return transcript;
    };
    let mut next = dict.clone();
    let mut metadata = match next.get("metadata") {
        Some(VmValue::Dict(metadata)) => metadata.as_ref().clone(),
        _ => crate::value::DictMap::new(),
    };
    if let Some(tool_format) = state.tool_format.as_ref() {
        metadata.put_str("tool_format", tool_format.clone());
        metadata.insert(
            crate::value::intern_key("tool_mode_locked"),
            VmValue::Bool(true),
        );
    }
    if let Some(system_prompt) = state.system_prompt.as_ref() {
        metadata.insert(
            crate::value::intern_key("system_prompt"),
            crate::stdlib::json_to_vm_value(&crate::llm::helpers::system_prompt_metadata(
                system_prompt,
            )),
        );
    }
    if let Some(actor_chain) = state.actor_chain.as_ref() {
        metadata.insert(
            crate::value::intern_key("actor_chain"),
            actor_chain.to_vm_value(),
        );
    } else {
        metadata.remove("actor_chain");
    }
    if let Some(anchor) = state.workspace_anchor.as_ref() {
        metadata.insert(
            crate::value::intern_key(WORKSPACE_ANCHOR_METADATA_KEY),
            anchor.to_vm_value(),
        );
    } else {
        metadata.remove(WORKSPACE_ANCHOR_METADATA_KEY);
    }
    if let Some(scratchpad) = state.scratchpad.as_ref() {
        metadata.insert(
            crate::value::intern_key("agent_scratchpad"),
            scratchpad.clone(),
        );
        metadata.insert(
            crate::value::intern_key("agent_scratchpad_version"),
            VmValue::Int(state.scratchpad_version as i64),
        );
    } else {
        metadata.remove("agent_scratchpad");
        metadata.remove("agent_scratchpad_version");
    }
    if let Some(last_action) = state.last_transcript_budget_action.as_ref() {
        let usage = transcript_usage(
            &VmValue::dict(next.clone()),
            state.transcript_budget_policy.max_approx_bytes.is_some(),
        );
        metadata.insert(
            crate::value::intern_key("transcript_budget"),
            crate::stdlib::json_to_vm_value(&serde_json::json!({
                "policy": transcript_budget_policy_json(&state.transcript_budget_policy.normalized()),
                "usage": transcript_budget_usage_json(&usage),
                "last_action": last_action,
            })),
        );
    }
    if !metadata.is_empty() {
        next.insert(
            crate::value::intern_key("metadata"),
            VmValue::dict(metadata),
        );
    }
    VmValue::dict(next)
}

/// Materialize the externally-visible view of a session: the transcript
/// plus session-level metadata (lineage, pinned model, scratchpad, live
/// clients, checkpoint counts, ...).
///
/// `state.subscribers` (closure callbacks registered for agent-loop
/// events) are intentionally omitted: they are ephemeral by design,
/// owned by the current-thread agent-loop worker, and are re-registered
/// by the host when a persisted session is reopened. Serializing them
/// would at best persist a useless display-string — see
/// `crate::llm::vm_value_to_json_strict` for the seams that do guard
/// against that class of silent corruption.
pub(super) fn session_snapshot(state: &SessionState) -> VmValue {
    let transcript = transcript_with_session_metadata(state.transcript.clone(), state);
    let Some(dict) = transcript.as_dict() else {
        return state.transcript.clone();
    };
    let mut next = dict.clone();
    let length = next
        .get("messages")
        .and_then(|value| match value {
            VmValue::List(list) => Some(list.len() as i64),
            _ => None,
        })
        .unwrap_or(0);
    next.insert(crate::value::intern_key("length"), VmValue::Int(length));
    next.put_str("created_at", state.created_at.clone());
    next.insert(
        crate::value::intern_key("parent_id"),
        state
            .parent_id
            .as_ref()
            .map(|id| VmValue::String(arcstr::ArcStr::from(id.clone())))
            .unwrap_or(VmValue::Nil),
    );
    next.insert(
        crate::value::intern_key("child_ids"),
        VmValue::List(std::sync::Arc::new(
            state
                .child_ids
                .iter()
                .cloned()
                .map(|id| VmValue::String(arcstr::ArcStr::from(id)))
                .collect(),
        )),
    );
    next.insert(
        crate::value::intern_key("branched_at_event_index"),
        state
            .branched_at_event_index
            .map(|index| VmValue::Int(index as i64))
            .unwrap_or(VmValue::Nil),
    );
    next.insert(
        crate::value::intern_key("system_prompt"),
        state
            .system_prompt
            .as_ref()
            .map(|prompt| VmValue::String(arcstr::ArcStr::from(prompt.clone())))
            .unwrap_or(VmValue::Nil),
    );
    next.insert(
        crate::value::intern_key("tool_format"),
        state
            .tool_format
            .as_ref()
            .map(|format| VmValue::String(arcstr::ArcStr::from(format.clone())))
            .unwrap_or(VmValue::Nil),
    );
    next.insert(
        crate::value::intern_key("pinned_model"),
        state
            .pinned_model
            .as_ref()
            .map(|model| VmValue::String(arcstr::ArcStr::from(model.clone())))
            .unwrap_or(VmValue::Nil),
    );
    next.insert(
        crate::value::intern_key("pinned_reasoning_policy"),
        state
            .pinned_reasoning_policy
            .as_ref()
            .map(|policy| VmValue::String(arcstr::ArcStr::from(policy.clone())))
            .unwrap_or(VmValue::Nil),
    );
    next.insert(
        crate::value::intern_key("actor_chain"),
        state
            .actor_chain
            .as_ref()
            .map(ActorChain::to_vm_value)
            .unwrap_or(VmValue::Nil),
    );
    next.insert(
        crate::value::intern_key("scratchpad"),
        state.scratchpad.clone().unwrap_or(VmValue::Nil),
    );
    next.insert(
        crate::value::intern_key("scratchpad_version"),
        VmValue::Int(state.scratchpad_version as i64),
    );
    next.insert(
        crate::value::intern_key("workspace_anchor"),
        state
            .workspace_anchor
            .as_ref()
            .map(WorkspaceAnchor::to_vm_value)
            .unwrap_or(VmValue::Nil),
    );
    next.insert(
        crate::value::intern_key("workspace_policy"),
        state.workspace_policy.to_vm_value(),
    );
    next.insert(
        crate::value::intern_key("live_clients"),
        crate::stdlib::json_to_vm_value(&serde_json::Value::Array(
            state.live_clients.values().map(live_client_json).collect(),
        )),
    );
    next.insert(
        crate::value::intern_key("live_controller_id"),
        state
            .live_controller_id
            .as_ref()
            .map(|id| VmValue::String(arcstr::ArcStr::from(id.clone())))
            .unwrap_or(VmValue::Nil),
    );
    next.insert(
        crate::value::intern_key("completed_turn_checkpoint_count"),
        VmValue::Int(state.completed_turn_checkpoints.len() as i64),
    );
    next.insert(
        crate::value::intern_key("redo_checkpoint_count"),
        VmValue::Int(state.redo_stack.len() as i64),
    );
    VmValue::dict(next)
}

pub(super) fn update_lineage(
    map: &mut HashMap<String, SessionState>,
    parent_id: &str,
    child_id: &str,
    branched_at_event_index: Option<usize>,
) {
    let old_parent_id = map.get(child_id).and_then(|child| child.parent_id.clone());
    if let Some(old_parent_id) = old_parent_id.filter(|old_parent_id| old_parent_id != parent_id) {
        if let Some(old_parent) = map.get_mut(&old_parent_id) {
            old_parent.child_ids.retain(|id| id != child_id);
            old_parent.touch();
        }
    }
    if let Some(parent) = map.get_mut(parent_id) {
        parent.touch();
        if !parent.child_ids.iter().any(|id| id == child_id) {
            parent.child_ids.push(child_id.to_string());
        }
    }
    if let Some(child) = map.get_mut(child_id) {
        child.touch();
        child.parent_id = Some(parent_id.to_string());
        child.branched_at_event_index = branched_at_event_index;
        child.transcript = clone_transcript_with_parent(&child.transcript, parent_id);
    }
}

pub(super) fn branch_event_index(transcript: &VmValue, keep_first: usize) -> usize {
    if keep_first == 0 {
        return 0;
    }
    let Some(dict) = transcript.as_dict() else {
        return keep_first;
    };
    let Some(VmValue::List(events)) = dict.get("events") else {
        return keep_first;
    };
    event_prefix_len_for_messages(events, keep_first)
}

pub(super) fn event_kind(event: &VmValue) -> Option<String> {
    event
        .as_dict()
        .and_then(|dict| dict.get("kind"))
        .map(VmValue::display)
}

pub(super) fn event_id(event: &VmValue) -> Option<String> {
    event
        .as_dict()
        .and_then(|dict| dict.get("id"))
        .map(VmValue::display)
}

pub(super) fn is_turn_event(event: &VmValue) -> bool {
    matches!(
        event_kind(event).as_deref(),
        Some("message" | "tool_result")
    )
}

pub(super) fn event_prefix_len_for_messages(events: &[VmValue], keep_first: usize) -> usize {
    if keep_first == 0 {
        return 0;
    }
    let mut retained_messages = 0usize;
    for (index, event) in events.iter().enumerate() {
        if is_turn_event(event) {
            retained_messages += 1;
            if retained_messages == keep_first {
                return index + 1;
            }
        }
    }
    events.len()
}

pub(super) fn turn_event_id_for_count(events: &[VmValue], keep_first: usize) -> Option<String> {
    if keep_first == 0 {
        return None;
    }
    let mut retained_messages = 0usize;
    for event in events {
        if is_turn_event(event) {
            retained_messages += 1;
            if retained_messages == keep_first {
                return event_id(event);
            }
        }
    }
    None
}
