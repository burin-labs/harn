//! Token estimation, microcompaction, and transcript auto-compact builtins.

use crate::orchestration::ArtifactRecord;
use crate::stdlib::macros::harn_builtin;
use crate::value::{VmError, VmValue};

use super::super::{parse_artifact_list, parse_context_policy};
use super::convert::to_vm;

/// Select workflow artifacts according to a context policy.
#[harn_builtin(
    sig = "select_artifacts_adaptive(artifacts?: list|nil, policy?: dict|nil) -> list",
    category = "workflow.host"
)]
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

/// Estimate tokens for a list of message objects.
#[harn_builtin(
    sig = "estimate_tokens(messages?: list) -> int",
    category = "workflow.host"
)]
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

/// Compact long tool output with the host microcompaction primitive.
#[harn_builtin(
    sig = "microcompact(text: string, max_chars?: int) -> string",
    category = "workflow.host"
)]
pub(super) fn microcompact_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let text = args.first().map(|a| a.display()).unwrap_or_default();
    let max_chars = args
        .get(1)
        .map(|v| non_negative_usize(v, "microcompact", "max_chars"))
        .transpose()?
        .unwrap_or(20_000);
    Ok(VmValue::String(arcstr::ArcStr::from(
        crate::orchestration::microcompact_tool_output(&text, max_chars),
    )))
}

fn non_negative_usize(value: &VmValue, builtin: &str, key: &str) -> Result<usize, VmError> {
    match value {
        VmValue::Int(n) if *n >= 0 => Ok(*n as usize),
        VmValue::Int(_) => Err(VmError::Runtime(format!("{builtin}: `{key}` must be >= 0"))),
        other => Err(VmError::Runtime(format!(
            "{builtin}: `{key}` must be an int, got {}",
            other.type_name()
        ))),
    }
}

/// Apply the workflow/agent transcript auto-compaction primitive to a message list.
#[harn_builtin(
    sig = "transcript_auto_compact(messages: list, options?: dict|nil) -> list",
    kind = "async",
    category = "workflow.host"
)]
pub(super) async fn transcript_auto_compact_builtin(
    ctx: crate::vm::AsyncBuiltinCtx,
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
    let mut config = crate::orchestration::AutoCompactConfig {
        policy: crate::orchestration::parse_compaction_policy_options(
            options.as_ref(),
            "transcript_auto_compact",
        )?,
        ..Default::default()
    };
    if let Some(v) = options
        .as_ref()
        .and_then(|o| o.get("keep_first"))
        .map(|v| non_negative_usize(v, "transcript_auto_compact", "keep_first"))
        .transpose()?
    {
        config.keep_first = v;
    }
    let threshold = options.as_ref().and_then(|o| {
        o.get("token_threshold")
            .map(|v| ("token_threshold", v))
            .or_else(|| o.get("compact_threshold").map(|v| ("compact_threshold", v)))
    });
    if let Some((key, v)) = threshold {
        config.token_threshold = non_negative_usize(v, "transcript_auto_compact", key)?;
    }
    if let Some(v) = options
        .as_ref()
        .and_then(|o| o.get("tool_output_max_chars"))
        .map(|v| non_negative_usize(v, "transcript_auto_compact", "tool_output_max_chars"))
        .transpose()?
    {
        config.tool_output_max_chars = v;
    }
    if let Some(v) = options
        .as_ref()
        .and_then(|o| o.get("keep_last"))
        .map(|v| non_negative_usize(v, "transcript_auto_compact", "keep_last"))
        .transpose()?
    {
        config.keep_last = v;
    }
    if let Some(v) = options
        .as_ref()
        .and_then(|o| o.get("hard_limit_tokens"))
        .map(|v| non_negative_usize(v, "transcript_auto_compact", "hard_limit_tokens"))
        .transpose()?
    {
        config.hard_limit_tokens = Some(v);
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
            VmValue::String(arcstr::ArcStr::from("")),
            VmValue::Nil,
            args.get(1).cloned().unwrap_or(VmValue::Nil),
        ])?)
    } else {
        None
    };
    let lifecycle =
        crate::orchestration::CompactLifecycle::new(crate::orchestration::CompactMode::Workflow);
    crate::orchestration::run_compaction_lifecycle_with_ctx(
        Some(&ctx),
        &mut messages,
        &mut config,
        llm_opts.as_ref(),
        lifecycle,
    )
    .await?;
    Ok(VmValue::List(std::sync::Arc::new(
        messages
            .iter()
            .map(crate::stdlib::json_to_vm_value)
            .collect(),
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn microcompact_rejects_negative_limit() {
        let mut out = String::new();
        let err = microcompact_builtin(
            &[
                VmValue::String(arcstr::ArcStr::from("hello")),
                VmValue::Int(-1),
            ],
            &mut out,
        )
        .expect_err("negative limits must fail");
        assert!(err.to_string().contains("max_chars"));
    }
}
