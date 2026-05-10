//! Token estimation, microcompaction, and transcript auto-compact builtins.

use std::rc::Rc;

use crate::orchestration::ArtifactRecord;
use crate::value::{VmError, VmValue};

use super::super::{parse_artifact_list, parse_context_policy};
use super::convert::to_vm;

pub(super) fn select_artifacts_adaptive_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let artifacts_val = args.first().cloned().unwrap_or(VmValue::Nil);
    let policy_val = args.get(1).cloned().unwrap_or(VmValue::Nil);
    let artifacts: Vec<ArtifactRecord> = parse_artifact_list(Some(&artifacts_val))?;
    let policy: crate::orchestration::ContextPolicy = parse_context_policy(Some(&policy_val))?;
    let selected = crate::orchestration::select_artifacts_adaptive(artifacts, &policy);
    to_vm(&selected)
}

pub(super) fn estimate_tokens_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let messages: Vec<serde_json::Value> = args
        .first()
        .and_then(|a| match a {
            VmValue::List(list) => Some(
                list.iter()
                    .map(crate::llm::helpers::vm_value_to_json)
                    .collect(),
            ),
            _ => None,
        })
        .unwrap_or_default();
    let tokens = crate::orchestration::estimate_message_tokens(&messages);
    Ok(VmValue::Int(tokens as i64))
}

pub(super) fn microcompact_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let text = args.first().map(|a| a.display()).unwrap_or_default();
    let max_chars = args
        .get(1)
        .and_then(|v| match v {
            VmValue::Int(n) => Some(*n as usize),
            _ => None,
        })
        .unwrap_or(20_000);
    Ok(VmValue::String(Rc::from(
        crate::orchestration::microcompact_tool_output(&text, max_chars),
    )))
}

pub(super) async fn transcript_auto_compact_builtin(
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let mut messages: Vec<serde_json::Value> = match args.first() {
        Some(VmValue::List(list)) => list
            .iter()
            .map(crate::llm::helpers::vm_value_to_json)
            .collect(),
        _ => {
            return Err(VmError::Runtime(
                "transcript_auto_compact: first argument must be a message list".to_string(),
            ))
        }
    };
    let options = args.get(1).and_then(|v| v.as_dict()).cloned();
    let mut config = crate::orchestration::AutoCompactConfig::default();
    if let Some(v) = options
        .as_ref()
        .and_then(|o| o.get("keep_first"))
        .and_then(|v| v.as_int())
    {
        config.keep_first = v.max(0) as usize;
    }
    let threshold = options.as_ref().and_then(|o| {
        o.get("token_threshold")
            .or_else(|| o.get("compact_threshold"))
            .and_then(|v| v.as_int())
    });
    if let Some(v) = threshold {
        config.token_threshold = v.max(0) as usize;
    }
    if let Some(v) = options
        .as_ref()
        .and_then(|o| o.get("tool_output_max_chars"))
        .and_then(|v| v.as_int())
    {
        config.tool_output_max_chars = v.max(0) as usize;
    }
    if let Some(v) = options
        .as_ref()
        .and_then(|o| o.get("keep_last"))
        .and_then(|v| v.as_int())
    {
        config.keep_last = v.max(0) as usize;
    }
    if let Some(v) = options
        .as_ref()
        .and_then(|o| o.get("hard_limit_tokens"))
        .and_then(|v| v.as_int())
    {
        config.hard_limit_tokens = Some(v.max(0) as usize);
    }
    if let Some(strategy) = options
        .as_ref()
        .and_then(|o| o.get("compact_strategy"))
        .map(|v| v.display())
    {
        config.compact_strategy = crate::orchestration::parse_compact_strategy(&strategy)?;
    }
    if let Some(strategy) = options
        .as_ref()
        .and_then(|o| o.get("hard_limit_strategy"))
        .map(|v| v.display())
    {
        config.hard_limit_strategy = crate::orchestration::parse_compact_strategy(&strategy)?;
    }
    if let Some(prompt) = options
        .as_ref()
        .and_then(|o| o.get("summarize_prompt"))
        .map(|v| v.display())
    {
        if !prompt.is_empty() {
            config.summarize_prompt = Some(prompt);
        }
    }
    if let Some(callback) = options.as_ref().and_then(|o| o.get("compact_callback")) {
        config.custom_compactor = Some(callback.clone());
        if !options
            .as_ref()
            .is_some_and(|o| o.contains_key("compact_strategy"))
        {
            config.compact_strategy = crate::orchestration::CompactStrategy::Custom;
        }
    }
    let llm_opts = if config.compact_strategy == crate::orchestration::CompactStrategy::Llm {
        Some(crate::llm::extract_llm_options(&[
            VmValue::String(Rc::from("")),
            VmValue::Nil,
            args.get(1).cloned().unwrap_or(VmValue::Nil),
        ])?)
    } else {
        None
    };
    crate::orchestration::auto_compact_messages(&mut messages, &config, llm_opts.as_ref()).await?;
    Ok(VmValue::List(Rc::new(
        messages
            .iter()
            .map(crate::stdlib::json_to_vm_value)
            .collect(),
    )))
}
