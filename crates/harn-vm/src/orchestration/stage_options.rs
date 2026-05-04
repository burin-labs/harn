use std::collections::BTreeMap;

use serde::Deserialize;

use super::WorkflowNode;
use crate::value::{VmError, VmValue};

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct WorkflowStageAgentOptions {
    pub run_agent_loop: bool,
    pub tool_format: String,
    pub llm_options: BTreeMap<String, serde_json::Value>,
    pub agent_loop_options: BTreeMap<String, serde_json::Value>,
}

impl WorkflowStageAgentOptions {
    pub fn llm_options_vm_dict(&self) -> BTreeMap<String, VmValue> {
        json_map_to_vm_dict(&self.llm_options)
    }

    pub fn agent_loop_options_vm_dict(&self) -> BTreeMap<String, VmValue> {
        json_map_to_vm_dict(&self.agent_loop_options)
    }
}

pub async fn prepare_workflow_stage_agent_options(
    node: &WorkflowNode,
    session_id: &str,
    has_tools: bool,
) -> Result<WorkflowStageAgentOptions, VmError> {
    let model_policy = serde_json::to_value(&node.model_policy).map_err(|error| {
        VmError::Runtime(format!("workflow stage model_policy encode error: {error}"))
    })?;
    let payload = serde_json::json!({
        "kind": node.kind,
        "mode": node.mode,
        "has_tools": has_tools,
        "session_id": session_id,
        "done_sentinel": node.done_sentinel,
        "exit_when_verified": node.exit_when_verified,
        "model_policy": model_policy,
        "host": {
            "env_tool_format": non_empty_env("HARN_AGENT_TOOL_FORMAT"),
            "default_tool_format": default_stage_tool_format(&node.model_policy),
        },
    });
    let prepared = super::call_workflow_stdlib_function(
        "std/workflow/options",
        "workflow_stage_agent_options",
        &[crate::stdlib::json_to_vm_value(&payload)],
    )
    .await?;
    let prepared = crate::llm::vm_value_to_json(&prepared);
    let prepared: WorkflowStageAgentOptions =
        serde_json::from_value(prepared).map_err(|error| {
            VmError::Runtime(format!(
                "workflow_stage_agent_options returned invalid shape: {error}"
            ))
        })?;
    if prepared.tool_format.trim().is_empty() {
        return Err(VmError::Runtime(
            "workflow_stage_agent_options returned empty tool_format".to_string(),
        ));
    }
    Ok(prepared)
}

fn json_map_to_vm_dict(map: &BTreeMap<String, serde_json::Value>) -> BTreeMap<String, VmValue> {
    map.iter()
        .map(|(key, value)| (key.clone(), crate::stdlib::json_to_vm_value(value)))
        .collect()
}

fn non_empty_env(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn default_stage_tool_format(model_policy: &super::ModelPolicy) -> String {
    let model = model_policy
        .model
        .as_ref()
        .filter(|value| !value.trim().is_empty())
        .cloned()
        .or_else(|| non_empty_env("HARN_LLM_MODEL"))
        .unwrap_or_default();
    let provider = model_policy
        .provider
        .as_ref()
        .filter(|value| !value.trim().is_empty())
        .cloned()
        .or_else(|| non_empty_env("HARN_LLM_PROVIDER"))
        .unwrap_or_default();
    crate::llm_config::default_tool_format(&model, &provider)
}
