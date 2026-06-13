use crate::value::VmDictExt;
use std::collections::BTreeMap;

use super::super::parse_context_policy;
use super::WorkerCarryPolicy;
use crate::orchestration::{select_artifacts, ArtifactRecord, CapabilityPolicy, ContextPolicy};
use crate::stdlib::options::{ErrorKind, OptionsParser};
use crate::value::{VmError, VmValue};

const SPAWN_AGENT_FN: &str = "spawn_agent";

fn default_worker_carry_policy() -> WorkerCarryPolicy {
    WorkerCarryPolicy {
        artifact_mode: "inherit".to_string(),
        transcript_mode: "inherit".to_string(),
        context_policy: ContextPolicy::default(),
        resume_workflow: true,
        persist_state: true,
        retriggerable: false,
        policy: None,
    }
}

fn display_non_empty(value: Option<&VmValue>) -> Option<String> {
    match value {
        None | Some(VmValue::Nil) => None,
        Some(value) => {
            let rendered = value.display();
            if rendered.is_empty() {
                None
            } else {
                Some(rendered)
            }
        }
    }
}

pub(in crate::stdlib::agents) fn parse_worker_carry_policy(
    dict: &crate::value::DictMap,
) -> Result<WorkerCarryPolicy, VmError> {
    let mut parent = OptionsParser::new(SPAWN_AGENT_FN, dict, ErrorKind::Runtime);
    let Some(carry) = parent.optional_dict("carry")? else {
        return Ok(default_worker_carry_policy());
    };

    let mut parser = OptionsParser::new(SPAWN_AGENT_FN, carry, ErrorKind::Runtime);
    let artifacts_alias = parser.raw("artifacts");
    let transcript_alias = parser.raw("transcript");
    let context_policy_value = parser.raw("context_policy");
    let artifact_mode = display_non_empty(parser.raw("artifact_mode").or(artifacts_alias))
        .unwrap_or_else(|| "inherit".to_string());
    let transcript_mode = parser
        .raw("transcript_mode")
        .or(transcript_alias)
        .map(parse_transcript_mode)
        .transpose()?
        .unwrap_or_else(|| "inherit".to_string());
    let context_policy = parse_context_policy(
        context_policy_value.or_else(|| artifacts_alias.filter(|value| value.as_dict().is_some())),
    )?;
    let resume_workflow = parser.bool_or("resume_workflow", true)?;
    let persist_state = parser.bool_or("persist_state", true)?;
    let retriggerable = parser.bool_or("retriggerable", false)?;
    parser.finish_strict(&["policy", "tools"])?;

    Ok(WorkerCarryPolicy {
        artifact_mode,
        transcript_mode,
        context_policy,
        resume_workflow,
        persist_state,
        retriggerable,
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
            "{SPAWN_AGENT_FN}: carry.transcript_mode must be one of inherit, fork, reset, compact; got `{mode}`"
        ))),
    }
}

pub(super) fn parse_worker_policy_value(value: &VmValue) -> Result<CapabilityPolicy, VmError> {
    let json = crate::llm::helpers::vm_value_to_json(value);
    serde_json::from_value(json)
        .map_err(|e| VmError::Runtime(format!("{SPAWN_AGENT_FN}: policy parse error: {e}")))
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
            return Err(VmError::Runtime(format!(
                "{SPAWN_AGENT_FN}: tools shorthand must be a list of strings"
            )))
        }
    };
    let mut allowed = Vec::new();
    for tool in tools.iter() {
        let name = match tool {
            VmValue::String(text) => text.trim().to_string(),
            _ => {
                return Err(VmError::Runtime(format!(
                    "{SPAWN_AGENT_FN}: tools shorthand must be a list of strings"
                )))
            }
        };
        if !name.is_empty() && !allowed.contains(&name) {
            allowed.push(name);
        }
    }
    if allowed.is_empty() {
        return Err(VmError::Runtime(format!(
            "{SPAWN_AGENT_FN}: tools shorthand must include at least one tool name"
        )));
    }
    Ok(Some(CapabilityPolicy {
        tools: allowed,
        ..Default::default()
    }))
}

pub(super) fn resolve_worker_policy(
    dict: &crate::value::DictMap,
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
                .map_err(|e| VmError::Runtime(format!("{SPAWN_AGENT_FN}: {e}")))?,
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
                VmError::Runtime(format!("{SPAWN_AGENT_FN}: {e}"))
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
    next.put_str("id", new_id);
    if let Some(parent_id) = parent_id.filter(|value| !value.is_empty()) {
        let metadata = match next.get("metadata") {
            Some(VmValue::Dict(metadata)) => {
                let mut metadata = metadata.as_ref().clone();
                metadata.put_str("parent_transcript_id", parent_id);
                VmValue::dict(metadata)
            }
            _ => VmValue::dict(BTreeMap::from([(
                "parent_transcript_id".to_string(),
                VmValue::String(std::sync::Arc::from(parent_id)),
            )])),
        };
        next.insert("metadata".to_string(), metadata);
    }
    VmValue::dict(next)
}

pub(in super::super) async fn compact_worker_transcript(
    ctx: &crate::vm::AsyncBuiltinCtx,
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
    let mut config = crate::orchestration::AutoCompactConfig {
        token_threshold: 1,
        keep_last: 2,
        compact_strategy: crate::orchestration::CompactStrategy::Truncate,
        hard_limit_tokens: None,
        policy_strategy: crate::orchestration::compact_strategy_name(
            &crate::orchestration::CompactStrategy::Truncate,
        )
        .to_string(),
        ..Default::default()
    };
    let reminder_events = crate::orchestration::transcript_compactable_events(dict);
    let transcript_id_value = crate::llm::helpers::transcript_id(dict);
    let lifecycle =
        crate::orchestration::CompactLifecycle::new(crate::orchestration::CompactMode::Worker)
            .with_transcript_id(transcript_id_value.as_deref())
            .with_reminder_events(reminder_events)
            .with_source_transcript(Some(&transcript));
    let Some(outcome) = crate::orchestration::run_compaction_lifecycle_with_ctx(
        Some(ctx),
        &mut messages,
        &mut config,
        None,
        lifecycle,
    )
    .await?
    else {
        return Ok(transcript);
    };

    let vm_messages = messages
        .iter()
        .map(crate::stdlib::json_to_vm_value)
        .collect::<Vec<_>>();
    let mut events = crate::llm::helpers::transcript_events_from_messages(&vm_messages);
    events.extend(outcome.reminder_report.preserved_events);
    let mut next = dict.clone();
    next.insert(
        "messages".to_string(),
        VmValue::List(std::sync::Arc::new(vm_messages)),
    );
    next.insert(
        "events".to_string(),
        VmValue::List(std::sync::Arc::new(events)),
    );
    next.put_str("summary", outcome.summary);
    Ok(VmValue::dict(next))
}
