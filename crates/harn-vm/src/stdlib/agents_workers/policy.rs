use std::collections::BTreeMap;

use super::super::parse_context_policy;
use super::WorkerCarryPolicy;
use crate::orchestration::{select_artifacts, ArtifactRecord, CapabilityPolicy, ContextPolicy};
use crate::value::{VmError, VmValue};

pub(in crate::stdlib::agents) fn parse_worker_carry_policy(
    dict: &BTreeMap<String, VmValue>,
) -> Result<WorkerCarryPolicy, VmError> {
    let carry = dict
        .get("carry")
        .and_then(|value| value.as_dict())
        .cloned()
        .unwrap_or_default();
    let artifact_mode = carry
        .get("artifact_mode")
        .or_else(|| carry.get("artifacts"))
        .map(|value| value.display())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "inherit".to_string());
    let transcript_mode = carry
        .get("transcript_mode")
        .or_else(|| carry.get("transcript"))
        .map(parse_transcript_mode)
        .transpose()?
        .unwrap_or_else(|| "inherit".to_string());
    let context_policy = parse_context_policy(carry.get("context_policy").or_else(|| {
        carry
            .get("artifacts")
            .filter(|value| value.as_dict().is_some())
    }))?;

    Ok(WorkerCarryPolicy {
        artifact_mode,
        transcript_mode,
        context_policy,
        resume_workflow: !matches!(carry.get("resume_workflow"), Some(VmValue::Bool(false))),
        persist_state: !matches!(carry.get("persist_state"), Some(VmValue::Bool(false))),
        retriggerable: matches!(carry.get("retriggerable"), Some(VmValue::Bool(true))),
        policy: None,
    })
}

pub(super) fn parse_transcript_mode(value: &VmValue) -> Result<String, VmError> {
    let mode = match value {
        VmValue::String(text) => text.trim().to_string(),
        VmValue::Dict(dict) => dict
            .get("mode")
            .map(|value| value.display())
            .unwrap_or_default()
            .trim()
            .to_string(),
        _ => value.display().trim().to_string(),
    };
    match mode.as_str() {
        "inherit" | "fork" | "reset" | "compact" => Ok(mode),
        _ => Err(VmError::Runtime(format!(
            "spawn_agent: carry.transcript_mode must be one of inherit, fork, reset, compact; got `{mode}`"
        ))),
    }
}

pub(super) fn parse_worker_policy_value(value: &VmValue) -> Result<CapabilityPolicy, VmError> {
    let json = crate::llm::helpers::vm_value_to_json(value);
    serde_json::from_value(json)
        .map_err(|e| VmError::Runtime(format!("spawn_agent: policy parse error: {e}")))
}

pub(super) fn worker_policy_value(value: Option<&VmValue>) -> Option<&VmValue> {
    value.filter(|value| !matches!(value, VmValue::Nil))
}

fn parse_worker_tools_policy(value: Option<&VmValue>) -> Result<Option<CapabilityPolicy>, VmError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let tools = match value {
        VmValue::List(list) => list,
        _ => {
            return Err(VmError::Runtime(
                "spawn_agent: tools shorthand must be a list of strings".to_string(),
            ))
        }
    };
    let mut allowed = Vec::new();
    for tool in tools.iter() {
        let name = match tool {
            VmValue::String(text) => text.trim().to_string(),
            _ => {
                return Err(VmError::Runtime(
                    "spawn_agent: tools shorthand must be a list of strings".to_string(),
                ))
            }
        };
        if !name.is_empty() && !allowed.contains(&name) {
            allowed.push(name);
        }
    }
    if allowed.is_empty() {
        return Err(VmError::Runtime(
            "spawn_agent: tools shorthand must include at least one tool name".to_string(),
        ));
    }
    Ok(Some(CapabilityPolicy {
        tools: allowed,
        ..Default::default()
    }))
}

pub(super) fn resolve_worker_policy(
    dict: &BTreeMap<String, VmValue>,
) -> Result<Option<CapabilityPolicy>, VmError> {
    let carry = dict
        .get("carry")
        .and_then(|value| value.as_dict())
        .cloned()
        .unwrap_or_default();
    let explicit = carry
        .get("policy")
        .or_else(|| dict.get("policy"))
        .filter(|value| !matches!(value, VmValue::Nil))
        .map(parse_worker_policy_value)
        .transpose()?;
    let tools = parse_worker_tools_policy(carry.get("tools").or_else(|| dict.get("tools")))?;
    let requested = match (explicit, tools) {
        (Some(policy), Some(tool_policy)) => Some(
            policy
                .intersect(&tool_policy)
                .map_err(|e| VmError::Runtime(format!("spawn_agent: {e}")))?,
        ),
        (Some(policy), None) => Some(policy),
        (None, Some(tool_policy)) => Some(tool_policy),
        (None, None) => None,
    };
    resolve_inherited_worker_policy(requested)
}

pub(in super::super) fn resolve_inherited_worker_policy(
    requested: Option<CapabilityPolicy>,
) -> Result<Option<CapabilityPolicy>, VmError> {
    let parent = crate::orchestration::current_execution_policy();
    match (parent, requested) {
        (Some(parent), Some(requested)) => {
            Ok(Some(parent.intersect(&requested).map_err(|e| {
                VmError::Runtime(format!("spawn_agent: {e}"))
            })?))
        }
        (Some(parent), None) => Ok(Some(parent)),
        (None, Some(requested)) => Ok(Some(requested)),
        (None, None) => Ok(None),
    }
}

pub(in super::super) fn apply_worker_artifact_policy(
    artifacts: &[ArtifactRecord],
    policy: &WorkerCarryPolicy,
) -> Vec<ArtifactRecord> {
    if policy.artifact_mode == "none" {
        return Vec::new();
    }
    if policy.context_policy == ContextPolicy::default() {
        return artifacts.to_vec();
    }
    select_artifacts(artifacts.to_vec(), &policy.context_policy)
}

pub(in super::super) fn apply_worker_transcript_policy(
    transcript: Option<VmValue>,
    policy: &WorkerCarryPolicy,
) -> Result<Option<VmValue>, VmError> {
    match policy.transcript_mode.as_str() {
        "reset" => Ok(None),
        "fork" => Ok(transcript.map(fork_worker_transcript)),
        "inherit" | "compact" | "" => Ok(transcript),
        other => Err(VmError::Runtime(format!(
            "worker transcript policy: unknown transcript_mode `{other}`"
        ))),
    }
}

fn fork_worker_transcript(transcript: VmValue) -> VmValue {
    let Some(dict) = transcript.as_dict() else {
        return transcript;
    };
    let parent_id = dict.get("id").map(|value| value.display());
    let mut next = dict.clone();
    let new_id = uuid::Uuid::now_v7().to_string();
    next.insert("id".to_string(), VmValue::String(std::rc::Rc::from(new_id)));
    if let Some(parent_id) = parent_id.filter(|value| !value.is_empty()) {
        let metadata = match next.get("metadata") {
            Some(VmValue::Dict(metadata)) => {
                let mut metadata = metadata.as_ref().clone();
                metadata.insert(
                    "parent_transcript_id".to_string(),
                    VmValue::String(std::rc::Rc::from(parent_id)),
                );
                VmValue::Dict(std::rc::Rc::new(metadata))
            }
            _ => VmValue::Dict(std::rc::Rc::new(BTreeMap::from([(
                "parent_transcript_id".to_string(),
                VmValue::String(std::rc::Rc::from(parent_id)),
            )]))),
        };
        next.insert("metadata".to_string(), metadata);
    }
    VmValue::Dict(std::rc::Rc::new(next))
}

pub(in super::super) async fn compact_worker_transcript(
    transcript: VmValue,
) -> Result<VmValue, VmError> {
    let Some(dict) = transcript.as_dict() else {
        return Ok(transcript);
    };
    let original_messages = crate::llm::helpers::transcript_message_list(dict)?;
    let mut messages = original_messages
        .iter()
        .map(crate::llm::helpers::vm_value_to_json)
        .collect::<Vec<_>>();
    let config = crate::orchestration::AutoCompactConfig {
        keep_last: 2,
        compact_strategy: crate::orchestration::CompactStrategy::Truncate,
        hard_limit_tokens: None,
        ..Default::default()
    };
    let Some(summary) =
        crate::orchestration::auto_compact_messages(&mut messages, &config, None).await?
    else {
        return Ok(transcript);
    };

    let vm_messages = messages
        .iter()
        .map(crate::stdlib::json_to_vm_value)
        .collect::<Vec<_>>();
    let original_message_event_count =
        crate::llm::helpers::transcript_events_from_messages(&original_messages).len();
    let mut events = crate::llm::helpers::transcript_events_from_messages(&vm_messages);
    if let Some(VmValue::List(original_events)) = dict.get("events") {
        events.extend(
            original_events
                .iter()
                .skip(original_message_event_count)
                .cloned(),
        );
    }
    let mut next = dict.clone();
    next.insert(
        "messages".to_string(),
        VmValue::List(std::rc::Rc::new(vm_messages.clone())),
    );
    next.insert(
        "events".to_string(),
        VmValue::List(std::rc::Rc::new(events)),
    );
    next.insert(
        "summary".to_string(),
        VmValue::String(std::rc::Rc::from(summary)),
    );
    Ok(VmValue::Dict(std::rc::Rc::new(next)))
}
