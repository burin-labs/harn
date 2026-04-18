use std::collections::BTreeMap;

use super::super::parse_context_policy;
use super::WorkerCarryPolicy;
use crate::orchestration::{select_artifacts, ArtifactRecord, CapabilityPolicy, ContextPolicy};
use crate::value::{VmError, VmValue};

pub(super) fn parse_worker_carry_policy(
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
    let context_policy = parse_context_policy(carry.get("context_policy").or_else(|| {
        carry
            .get("artifacts")
            .filter(|value| value.as_dict().is_some())
    }))?;

    Ok(WorkerCarryPolicy {
        artifact_mode,
        context_policy,
        resume_workflow: !matches!(carry.get("resume_workflow"), Some(VmValue::Bool(false))),
        persist_state: !matches!(carry.get("persist_state"), Some(VmValue::Bool(false))),
        policy: None,
    })
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
