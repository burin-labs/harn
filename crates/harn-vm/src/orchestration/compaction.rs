//! Auto-compaction — transcript size management strategies.

use crate::llm::{vm_call_llm_full, vm_value_to_json};
use crate::value::{VmError, VmValue};

mod classification;
mod config;
mod prompt;
use crate::vm::AsyncBuiltinCtx;
pub use classification::{
    ClassificationApplicationReason, ClassificationChoice, ClassificationConfig,
    ClassificationDecision, ClassificationDecisionReceipt, ClassificationReceipt,
    ClassificationSource, ClassificationStatus,
};
pub use config::{
    compact_strategy_name, parse_compact_strategy, AutoCompactConfig, CompactStrategy,
    CompactionRequestProvenance, CompactionThresholdSource, DEFAULT_RECAP_BUDGET_BYTES,
};
#[cfg(test)]
pub(crate) use harn_orchestration::compaction::RECAP_HEADER_SENTINEL;
use harn_orchestration::compaction::{
    apply_model_visible_policy, is_prior_recap, latest_pinned_indices, message_content_bytes,
    observation_mask_compaction_with_callback, select_compaction_window,
    truncate_compaction_summary, truncate_compaction_summary_with_context, CompactionSummary,
    CLASSIFIED_RECAP_HEADER_SENTINEL,
};
#[doc(inline)]
pub use harn_orchestration::compaction::{
    compaction_policy_metadata_fields, compaction_policy_option_keys,
    compaction_policy_to_vm_value, estimate_message_tokens, microcompact_tool_output,
    microcompact_tool_output_result, parse_compaction_policy_options, CompactionPolicy,
    CompactionRequest, CompactionSourceMeasurement, MicrocompactedToolOutput, RecapMetrics,
};
#[cfg(test)]
pub(crate) use harn_orchestration::compaction::{
    observation_mask_compaction, observation_mask_compaction_for_test, MAX_PINNED_SEGMENTS,
    NO_COMPACT_MARKER,
};
use prompt::render_llm_compaction_prompt;

fn compact_summary_text_from_value(value: &VmValue) -> Result<String, VmError> {
    if let Some(map) = value.as_dict() {
        if let Some(summary) = map.get("summary").or_else(|| map.get("text")) {
            return Ok(summary.display());
        }
    }
    match value {
        VmValue::String(text) => Ok(text.to_string()),
        VmValue::Nil => Ok(String::new()),
        _ => serde_json::to_string_pretty(&vm_value_to_json(value))
            .map_err(|e| VmError::Runtime(format!("custom compactor encode error: {e}"))),
    }
}

async fn llm_compaction_summary(
    old_messages: &[serde_json::Value],
    retained_messages: &[serde_json::Value],
    archived_count: usize,
    llm_opts: &crate::llm::api::LlmCallOptions,
    summarize_prompt: Option<&str>,
    policy: &CompactionPolicy,
) -> Result<CompactionSummary, VmError> {
    let prompt = render_llm_compaction_prompt(
        summarize_prompt,
        old_messages,
        retained_messages,
        archived_count,
        policy,
    )?;
    let compact_opts = llm_opts.isolated_request(prompt, "compaction", "compact")?;
    let result = vm_call_llm_full(&compact_opts).await?;
    let summary = result.text.trim();
    if summary.is_empty() {
        // Bounded retry: one deterministic pass over the same source window.
        // The boundary still checks what that pass carried forward.
        Ok(truncate_compaction_summary_with_context(
            old_messages,
            archived_count,
            true,
        ))
    } else {
        let header = format!("[auto-compacted {archived_count} older messages]\n");
        let scaffold_bytes = header.len();
        Ok(CompactionSummary::new(
            format!("{header}{summary}"),
            scaffold_bytes,
        ))
    }
}

async fn custom_compaction_summary(
    ctx: Option<&AsyncBuiltinCtx>,
    old_messages: &[serde_json::Value],
    archived_count: usize,
    callback: &VmValue,
    reminders: &[VmValue],
    policy: &CompactionPolicy,
) -> Result<CompactionSummary, VmError> {
    let Some(VmValue::Closure(closure)) = Some(callback.clone()) else {
        return Err(VmError::Runtime(
            "compact_callback must be a closure when compact_strategy is 'custom'".to_string(),
        ));
    };
    let Some(ctx) = ctx else {
        return Err(VmError::Runtime(
            "custom transcript compaction requires an async builtin VM context".to_string(),
        ));
    };
    let mut vm = ctx.child_vm();
    let messages_vm = VmValue::List(std::sync::Arc::new(
        old_messages
            .iter()
            .map(crate::stdlib::json_to_vm_value)
            .collect(),
    ));
    let result = if policy.has_metadata()
        && (closure.func.params.len() >= 3 || closure.func.has_rest_param)
    {
        let reminders_vm = VmValue::List(std::sync::Arc::new(reminders.to_vec()));
        let policy_vm = compaction_policy_to_vm_value(policy);
        vm.call_closure_pub(&closure, &[messages_vm, reminders_vm, policy_vm])
            .await
    } else if closure.func.params.len() >= 2 || closure.func.has_rest_param {
        let reminders_vm = VmValue::List(std::sync::Arc::new(reminders.to_vec()));
        vm.call_closure_pub(&closure, &[messages_vm, reminders_vm])
            .await
    } else {
        vm.call_closure_pub(&closure, &[messages_vm]).await
    };
    let summary = compact_summary_text_from_value(&result?)?;
    ctx.forward_output(&vm.take_output());
    if summary.trim().is_empty() {
        // Bounded retry, same contract as the LLM strategy.
        Ok(truncate_compaction_summary(old_messages, archived_count))
    } else {
        let header = format!("[auto-compacted {archived_count} older messages]\n");
        let scaffold_bytes = header.len();
        Ok(CompactionSummary::new(
            format!("{header}{summary}"),
            scaffold_bytes,
        ))
    }
}

/// Invoke the mask_callback to get per-message custom masks.
async fn invoke_mask_callback(
    ctx: Option<&AsyncBuiltinCtx>,
    callback: &VmValue,
    old_messages: &[serde_json::Value],
) -> Result<Vec<Option<String>>, VmError> {
    let VmValue::Closure(closure) = callback.clone() else {
        return Err(VmError::Runtime(
            "mask_callback must be a closure".to_string(),
        ));
    };
    let Some(ctx) = ctx else {
        return Err(VmError::Runtime(
            "mask_callback requires an async builtin VM context".to_string(),
        ));
    };
    let mut vm = ctx.child_vm();
    let messages_vm = VmValue::List(std::sync::Arc::new(
        old_messages
            .iter()
            .map(crate::stdlib::json_to_vm_value)
            .collect(),
    ));
    let result = vm.call_closure_pub(&closure, &[messages_vm]).await?;
    ctx.forward_output(&vm.take_output());
    let list = match result {
        VmValue::List(items) => items,
        _ => return Ok(vec![None; old_messages.len()]),
    };
    Ok(list
        .iter()
        .map(|v| match v {
            VmValue::String(s) => Some(s.to_string()),
            VmValue::Nil => None,
            _ => None,
        })
        .collect())
}

/// Rewrite each tool-result message in `messages` whose content exceeds
/// `config.tool_output_max_chars`, using `config.compress_callback` when set
/// (and a VM context is available) else the deterministic
/// [`microcompact_tool_output`]. Only the `content` text is replaced; the
/// message's `role`/`tool_call_id` are left untouched so tool-call pairing is
/// preserved. A `tool_output_max_chars` of 0 disables the pass.
async fn clamp_tool_outputs(
    ctx: Option<&AsyncBuiltinCtx>,
    messages: &mut [serde_json::Value],
    config: &AutoCompactConfig,
) -> Result<(), VmError> {
    if config.tool_output_max_chars == 0 {
        return Ok(());
    }
    // Exempt the most-recent pinned tool-outputs (those carrying the host's
    // `[no-compact]` marker) from length-clamping so the agent's live file view
    // stays intact. Bounded to the latest MAX_PINNED_SEGMENTS so older pinned
    // snapshots in the kept window still clamp and can't blow the budget.
    let pinned = latest_pinned_indices(messages.iter(), |msg| {
        if msg.get("role").and_then(|role| role.as_str()) == Some("tool") {
            msg.get("content").and_then(|content| content.as_str())
        } else {
            None
        }
    });
    for (idx, message) in messages.iter_mut().enumerate() {
        if message.get("role").and_then(|role| role.as_str()) != Some("tool") {
            continue;
        }
        let Some(content) = message.get("content").and_then(|content| content.as_str()) else {
            continue;
        };
        if content.len() <= config.tool_output_max_chars {
            continue;
        }
        if pinned.contains(&idx) {
            continue;
        }
        let content = content.to_string();
        let replacement = match (config.compress_callback.as_ref(), ctx) {
            (Some(callback), Some(ctx)) => {
                invoke_compress_callback(ctx, callback, &content, config.tool_output_max_chars)
                    .await?
            }
            _ => microcompact_tool_output(&content, config.tool_output_max_chars),
        };
        message["content"] = serde_json::Value::String(replacement);
    }
    Ok(())
}

/// Invoke `compress_callback(content, max_chars)` to replace one oversized
/// tool-output body, mirroring [`invoke_mask_callback`]'s child-VM closure
/// invocation. A non-string return falls back to the deterministic primitive.
async fn invoke_compress_callback(
    ctx: &AsyncBuiltinCtx,
    callback: &VmValue,
    content: &str,
    max_chars: usize,
) -> Result<String, VmError> {
    let VmValue::Closure(closure) = callback.clone() else {
        return Err(VmError::Runtime(
            "compress_callback must be a closure".to_string(),
        ));
    };
    let mut vm = ctx.child_vm();
    let args = [
        VmValue::String(arcstr::ArcStr::from(content)),
        VmValue::Int(max_chars as i64),
    ];
    let result = vm.call_closure_pub(&closure, &args).await?;
    ctx.forward_output(&vm.take_output());
    match result {
        VmValue::String(text) => Ok(text.to_string()),
        _ => Ok(microcompact_tool_output(content, max_chars)),
    }
}

#[derive(Clone, Copy)]
struct CompactionStrategyInputs<'a> {
    ctx: Option<&'a AsyncBuiltinCtx>,
    strategy: &'a CompactStrategy,
    old_messages: &'a [serde_json::Value],
    retained_messages: &'a [serde_json::Value],
    archived_count: usize,
    llm_opts: Option<&'a crate::llm::api::LlmCallOptions>,
    custom_compactor: Option<&'a VmValue>,
    custom_compactor_reminders: &'a [VmValue],
    mask_callback: Option<&'a VmValue>,
    summarize_prompt: Option<&'a str>,
    policy: &'a CompactionPolicy,
    recap_budget_bytes: usize,
}

/// Apply a single compaction strategy to a list of archived messages. Returns
/// the summary text plus [`RecapMetrics`] for the observation-mask strategy
/// (the only strategy that spends a recap budget); other strategies return
/// `None`.
async fn apply_compaction_strategy(
    input: CompactionStrategyInputs<'_>,
) -> Result<(CompactionSummary, Option<RecapMetrics>), VmError> {
    let CompactionStrategyInputs {
        strategy,
        old_messages,
        retained_messages,
        archived_count,
        llm_opts,
        custom_compactor,
        custom_compactor_reminders,
        mask_callback,
        summarize_prompt,
        policy,
        recap_budget_bytes,
        ctx,
    } = input;
    match strategy {
        CompactStrategy::Truncate => Ok((
            truncate_compaction_summary(old_messages, archived_count),
            None,
        )),
        CompactStrategy::Llm => llm_compaction_summary(
            old_messages,
            retained_messages,
            archived_count,
            llm_opts.ok_or_else(|| {
                VmError::Runtime(
                    "LLM transcript compaction requires active LLM call options".to_string(),
                )
            })?,
            summarize_prompt,
            policy,
        )
        .await
        .map(|summary| (summary, None)),
        CompactStrategy::Custom => custom_compaction_summary(
            ctx,
            old_messages,
            archived_count,
            custom_compactor.ok_or_else(|| {
                VmError::Runtime(
                    "compact_callback is required when compact_strategy is 'custom'".to_string(),
                )
            })?,
            custom_compactor_reminders,
            policy,
        )
        .await
        .map(|summary| (summary, None)),
        CompactStrategy::ObservationMask => {
            let mask_results = if let Some(cb) = mask_callback {
                Some(invoke_mask_callback(ctx, cb, old_messages).await?)
            } else {
                None
            };
            let (summary, metrics) = observation_mask_compaction_with_callback(
                old_messages,
                archived_count,
                mask_results.as_deref(),
                recap_budget_bytes,
            );
            Ok((summary, Some(metrics)))
        }
        CompactStrategy::Classify => Err(VmError::Runtime(
            "classify is a primary compaction strategy, not a positional fallback".into(),
        )),
    }
}

async fn apply_compaction_strategy_with_fallback(
    input: CompactionStrategyInputs<'_>,
    fallback_strategy: Option<&CompactStrategy>,
) -> Result<(CompactionSummary, CompactStrategy, Option<RecapMetrics>), VmError> {
    match apply_compaction_strategy(input).await {
        Ok((summary, metrics)) => Ok((summary, input.strategy.clone(), metrics)),
        Err(primary_error) => {
            if primary_error.is_uncatchable_control_flow()
                || crate::value::error_to_category(&primary_error)
                    == crate::value::ErrorCategory::Cancelled
            {
                return Err(primary_error);
            }
            let Some(fallback) = fallback_strategy.filter(|fallback| *fallback != input.strategy)
            else {
                return Err(primary_error);
            };
            let fallback_input = CompactionStrategyInputs {
                strategy: fallback,
                ..input
            };
            apply_compaction_strategy(fallback_input)
                .await
                .map(|(summary, metrics)| (summary, fallback.clone(), metrics))
        }
    }
}

#[derive(Debug)]
pub(crate) struct AutoCompactResult {
    pub summary: String,
    pub strategy: CompactStrategy,
    pub recap_metrics: Option<RecapMetrics>,
    pub classification: Option<Box<ClassificationReceipt>>,
    /// Typed source-window/summary measurement for this compaction. Always
    /// populated on this path, so a `None` field downstream means the
    /// measurement did not travel, never that it read zero.
    pub measurement: CompactionSourceMeasurement,
}

/// Auto-compact a message list in place using two-tier compaction.
#[cfg(test)]
pub(crate) async fn auto_compact_messages_with_result(
    messages: &mut Vec<serde_json::Value>,
    config: &AutoCompactConfig,
    llm_opts: Option<&crate::llm::api::LlmCallOptions>,
) -> Result<Option<AutoCompactResult>, VmError> {
    let Some((result, candidate)) =
        prepare_compaction_messages_with_ctx(None, messages, config, llm_opts).await?
    else {
        return Ok(None);
    };
    *messages = candidate;
    Ok(Some(result))
}

pub(crate) async fn prepare_compaction_messages_with_ctx(
    ctx: Option<&AsyncBuiltinCtx>,
    messages: &[serde_json::Value],
    config: &AutoCompactConfig,
    llm_opts: Option<&crate::llm::api::LlmCallOptions>,
) -> Result<Option<(AutoCompactResult, Vec<serde_json::Value>)>, VmError> {
    if config.compact_strategy == CompactStrategy::Classify && config.classification.is_none() {
        return Err(VmError::Runtime(
            "classify requires a classification policy".into(),
        ));
    }
    if config.hard_limit_strategy == CompactStrategy::Classify
        || config.fallback_strategy == Some(CompactStrategy::Classify)
    {
        return Err(VmError::Runtime(
            "classify cannot be a hard-limit or fallback strategy".into(),
        ));
    }
    if config.token_threshold > 0 && estimate_message_tokens(messages) <= config.token_threshold {
        return Ok(None);
    }
    let Some(window) = select_compaction_window(messages, config.keep_first, config.keep_last)
    else {
        return Ok(None);
    };
    // Mutating the caller before an awaited strategy succeeds loses source
    // context on refusal, cancellation, or a dropped future. Allocate only
    // after compaction is admitted, then publish the complete candidate once.
    let mut candidate = messages.to_vec();
    let result = Box::pin(compact_selected_window(
        ctx,
        &mut candidate,
        config,
        llm_opts,
        window.start,
        window.end,
    ))
    .await?;
    Ok(Some((result, candidate)))
}

async fn compact_selected_window(
    ctx: Option<&AsyncBuiltinCtx>,
    messages: &mut Vec<serde_json::Value>,
    config: &AutoCompactConfig,
    llm_opts: Option<&crate::llm::api::LlmCallOptions>,
    compact_start: usize,
    split_at: usize,
) -> Result<AutoCompactResult, VmError> {
    let classification_anchor = if config.compact_strategy == CompactStrategy::Classify {
        classification::latest_user_anchor(messages)
    } else {
        String::new()
    };
    let old_messages: Vec<_> = messages.drain(compact_start..split_at).collect();
    let archived_count = old_messages.len();

    // Clamp oversized tool-result bodies in the *kept* window so the live
    // context honors the policy's `tool_output_max_chars` (and the
    // `compress_callback` override), not just the archived/summarized window.
    // Runs before the hard-limit estimate so tier-2 escalation
    // keys off the post-clamp size. Only the text body is rewritten; `role`
    // and `tool_call_id` are preserved so tool_call/tool_result pairing stays
    // intact.
    clamp_tool_outputs(ctx, messages, config).await?;

    let mut classification = None;
    let (mut summary, mut strategy, mut recap_metrics) =
        if config.compact_strategy == CompactStrategy::Classify {
            let classified = Box::pin(classification::classify_window(
                classification::ClassificationInputs {
                    ctx: ctx.ok_or_else(|| {
                        VmError::Runtime("classify requires an async VM context".into())
                    })?,
                    config: config.classification.as_deref().ok_or_else(|| {
                        VmError::Runtime("classify requires a classification policy".into())
                    })?,
                    archived: &old_messages,
                    retained: messages,
                    anchor: &classification_anchor,
                    first_index: compact_start,
                    budget_bytes: config.recap_budget_bytes,
                    active: llm_opts,
                    summarize_prompt: config.summarize_prompt.as_deref(),
                    policy: &config.policy,
                },
            ))
            .await?;
            let strategy = if classified.receipt.status == ClassificationStatus::Fallback {
                CompactStrategy::ObservationMask
            } else {
                CompactStrategy::Classify
            };
            classification = Some(classified.receipt);
            (classified.summary, strategy, classified.fallback_recap)
        } else {
            apply_compaction_strategy_with_fallback(
                CompactionStrategyInputs {
                    ctx,
                    strategy: &config.compact_strategy,
                    old_messages: &old_messages,
                    retained_messages: messages.as_slice(),
                    archived_count,
                    llm_opts,
                    custom_compactor: config.custom_compactor.as_ref(),
                    custom_compactor_reminders: &config.custom_compactor_reminders,
                    mask_callback: config.mask_callback.as_ref(),
                    summarize_prompt: config.summarize_prompt.as_deref(),
                    policy: &config.policy,
                    recap_budget_bytes: config.recap_budget_bytes,
                },
                config.fallback_strategy.as_ref(),
            )
            .await?
        };

    // A positional hard-limit pass may not undo keep/reword decisions or the
    // confidence floor. The classification receipt reports an unmet budget.
    if let Some(hard_limit) = config
        .hard_limit_tokens
        .filter(|_| classification.is_none())
    {
        let summary_msg = serde_json::json!({"role": "user", "content": &summary.text});
        let mut estimate_msgs = vec![summary_msg];
        estimate_msgs.extend_from_slice(messages.as_slice());
        let estimated = estimate_message_tokens(&estimate_msgs);
        if estimated > hard_limit {
            let tier1_as_messages = vec![serde_json::json!({
                "role": "user",
                "content": summary.text,
            })];
            let (hard_limit_summary, hard_limit_strategy, hard_limit_metrics) =
                apply_compaction_strategy_with_fallback(
                    CompactionStrategyInputs {
                        ctx,
                        strategy: &config.hard_limit_strategy,
                        old_messages: &tier1_as_messages,
                        retained_messages: messages.as_slice(),
                        archived_count,
                        llm_opts,
                        custom_compactor: config.custom_compactor.as_ref(),
                        custom_compactor_reminders: &config.custom_compactor_reminders,
                        mask_callback: None,
                        summarize_prompt: config.summarize_prompt.as_deref(),
                        policy: &config.policy,
                        recap_budget_bytes: config.recap_budget_bytes,
                    },
                    config.fallback_strategy.as_ref(),
                )
                .await?;
            summary = hard_limit_summary;
            strategy = hard_limit_strategy;
            // Tier-2 re-summarized the tier-1 recap; its metrics (if any)
            // describe the delivered body, so they supersede tier-1's.
            recap_metrics = hard_limit_metrics.or(recap_metrics);
        }
    }

    // Measure the source window against what the summary actually carried,
    // BEFORE the model-visible directives and the repair ledger are appended:
    // both are generated scaffold, and counting them would let a summary that
    // preserved nothing look substantial.
    let source_bytes: usize = old_messages.iter().map(message_content_bytes).sum();
    let measurement = CompactionSourceMeasurement {
        source_message_count: Some(archived_count),
        source_bytes: Some(source_bytes),
        summary_bytes: Some(summary.text.len()),
        carried_source_bytes: Some(summary.carried_source_bytes()),
    };

    // A non-empty source window whose summary carried zero source bytes is a
    // failed compaction, not a small one. Every summarizer has already taken its
    // one bounded deterministic retry by this point, so this is the terminal
    // path: refuse the candidate without publishing it, rather than replacing
    // real context with a bare header.
    if measurement.discarded_source_context() {
        return Err(VmError::Runtime(format!(
            "transcript compaction refused: the summary carried 0 of {source_bytes} source \
             bytes across {archived_count} archived messages (summary was {} bytes, all \
             generated scaffold); the source context was preserved",
            summary.text.len()
        )));
    }

    let summary = super::repair_ledger::append_repair_ledger_to_summary(
        apply_model_visible_policy(summary.text, &config.policy),
        &old_messages,
    );

    messages.insert(
        compact_start,
        serde_json::json!({
            "role": "user",
            "content": summary,
        }),
    );
    Ok(AutoCompactResult {
        summary,
        strategy,
        recap_metrics,
        classification,
        measurement,
    })
}

/// Auto-compact a message list in place using two-tier compaction.
#[cfg(test)]
pub(crate) async fn auto_compact_messages(
    messages: &mut Vec<serde_json::Value>,
    config: &AutoCompactConfig,
    llm_opts: Option<&crate::llm::api::LlmCallOptions>,
) -> Result<Option<String>, VmError> {
    Ok(
        auto_compact_messages_with_result(messages, config, llm_opts)
            .await?
            .map(|result| result.summary),
    )
}

#[cfg(test)]
#[path = "compaction/tests.rs"]
mod tests;
