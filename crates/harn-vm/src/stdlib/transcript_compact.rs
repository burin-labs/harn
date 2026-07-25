use crate::llm::helpers::{
    extract_llm_options, is_transcript_value, new_transcript_with_events, project_llm_options,
    transcript_asset_list, transcript_event_with_id, transcript_id, transcript_message_list,
    transcript_summary_text, vm_value_to_json,
};
use crate::orchestration::{
    compact_strategy_name, estimate_message_tokens, run_compaction_lifecycle_with_ctx,
    transcript_compactable_events, AutoCompactConfig, CompactLifecycle, CompactMode,
    CompactStrategy, CompactionPolicy,
};
use crate::stdlib::json_to_vm_value;
use crate::value::{VmError, VmValue};
use crate::vm::Vm;

pub(crate) fn register_transcript_compaction_builtins(vm: &mut Vm) {
    vm.register_async_builtin("transcript_compact", |ctx, args| async move {
        let transcript = args
            .first()
            .and_then(|value| value.as_dict())
            .filter(|_| args.first().is_some_and(is_transcript_value))
            .ok_or_else(|| {
                VmError::Runtime("transcript_compact: first argument must be a transcript".into())
            })?;
        let options = args.get(1).and_then(|value| value.as_dict());
        compact_transcript_impl(&ctx, transcript, options, args.get(1).cloned()).await
    });
}

#[derive(Clone)]
struct TranscriptCompactOptions {
    strategy: CompactStrategy,
    keep_last: usize,
    target_tokens: Option<usize>,
    summarize_prompt: Option<String>,
    summary: Option<String>,
    custom_compactor: Option<VmValue>,
    policy: CompactionPolicy,
    recap_budget_bytes: usize,
}

async fn compact_transcript_impl(
    ctx: &crate::vm::AsyncBuiltinCtx,
    transcript: &crate::value::DictMap,
    options: Option<&crate::value::DictMap>,
    _raw_options: Option<VmValue>,
) -> Result<VmValue, VmError> {
    let provider_options = options
        .map(crate::llm::reminder_providers::options_map_to_json)
        .unwrap_or_else(|| serde_json::json!({}));
    let parsed_options = parse_options(options)?;

    let mut config = AutoCompactConfig {
        keep_last: parsed_options.keep_last,
        compact_strategy: parsed_options.strategy.clone(),
        hard_limit_strategy: parsed_options.strategy.clone(),
        summarize_prompt: parsed_options.summarize_prompt.clone(),
        custom_compactor: parsed_options.custom_compactor.clone(),
        policy: parsed_options.policy.clone(),
        policy_strategy: compact_strategy_name(&parsed_options.strategy).to_string(),
        recap_budget_bytes: parsed_options.recap_budget_bytes,
        ..Default::default()
    };
    if let Some(target_tokens) = parsed_options.target_tokens {
        config.token_threshold = target_tokens;
        config.hard_limit_tokens = Some(target_tokens);
    } else {
        config.token_threshold = 0;
    }

    let original_transcript = VmValue::dict(transcript.clone());
    let mut messages: Vec<serde_json::Value> = transcript_message_list(transcript)?
        .iter()
        .map(vm_value_to_json)
        .collect();
    if let Some(target_tokens) = parsed_options.target_tokens {
        if estimate_message_tokens(&messages) <= target_tokens {
            return Ok(original_transcript);
        }
    }

    let llm_opts = if config.compact_strategy == CompactStrategy::Llm {
        let projected = options
            .map(|options| {
                let mut llm_options = options.clone();
                llm_options.remove("strategy");
                project_llm_options(&llm_options)
            })
            .transpose()?
            .unwrap_or_default();
        Some(extract_llm_options(&[
            VmValue::String(arcstr::ArcStr::from("")),
            VmValue::Nil,
            VmValue::dict(projected),
        ])?)
    } else {
        None
    };

    let transcript_id_value = transcript_id(transcript);
    let session_id = transcript_id_value
        .clone()
        .or_else(crate::llm::current_agent_session_id);
    let extra_events = transcript_compactable_events(transcript);

    let lifecycle = CompactLifecycle::new(CompactMode::Manual)
        .with_session_id(session_id.as_deref())
        .with_transcript_id(transcript_id_value.as_deref())
        .with_reminder_events(extra_events)
        .with_summary_override(parsed_options.summary.clone())
        .with_provider_options(provider_options)
        .with_source_transcript(Some(&original_transcript));

    let Some(outcome) = run_compaction_lifecycle_with_ctx(
        Some(ctx),
        &mut messages,
        &mut config,
        llm_opts.as_ref(),
        lifecycle,
    )
    .await?
    else {
        return Ok(original_transcript);
    };

    let mut assets = transcript_asset_list(transcript)?;
    if let Some(asset) = outcome.snapshot_asset {
        assets.push(asset);
    }
    let mut extra_events = outcome.reminder_report.preserved_events;
    extra_events.push(transcript_event_with_id(
        &outcome.receipt.receipt_id,
        "compaction",
        "system",
        "internal",
        &format!(
            "Transcript compacted via {}",
            compact_strategy_name(&outcome.strategy)
        ),
        Some(outcome.event_metadata),
    ));

    let compacted = new_transcript_with_events(
        transcript_id_value,
        messages.iter().map(json_to_vm_value).collect(),
        merge_summary(transcript_summary_text(transcript), &outcome.summary),
        transcript.get("metadata").cloned(),
        extra_events,
        assets,
        transcript_state(transcript),
    );
    Ok(compacted)
}

fn parse_options(
    options: Option<&crate::value::DictMap>,
) -> Result<TranscriptCompactOptions, VmError> {
    let mut parsed = TranscriptCompactOptions {
        strategy: CompactStrategy::ObservationMask,
        keep_last: AutoCompactConfig::default().keep_last,
        target_tokens: None,
        summarize_prompt: None,
        summary: None,
        custom_compactor: None,
        policy: crate::orchestration::parse_compaction_policy_options(
            options,
            "transcript_compact",
        )?,
        recap_budget_bytes: crate::orchestration::AutoCompactConfig::default().recap_budget_bytes,
    };
    if let Some(value) = options
        .and_then(|dict| {
            dict.get("strategy")
                .or_else(|| dict.get("compact_strategy"))
        })
        .and_then(|value| match value {
            VmValue::String(text) => Some(text.as_ref()),
            _ => None,
        })
    {
        parsed.strategy = crate::orchestration::parse_compact_strategy(value)?;
    }
    if let Some(value) = options
        .and_then(|dict| dict.get("keep_last"))
        .and_then(|value| value.as_int())
    {
        if value < 0 {
            return Err(VmError::Runtime(
                "transcript_compact: keep_last must be >= 0".into(),
            ));
        }
        parsed.keep_last = value as usize;
    }
    if let Some(value) = options
        .and_then(|dict| dict.get("target_tokens"))
        .and_then(|value| value.as_int())
    {
        if value < 0 {
            return Err(VmError::Runtime(
                "transcript_compact: target_tokens must be >= 0".into(),
            ));
        }
        parsed.target_tokens = Some(value as usize);
    }
    if let Some(value) = options
        .and_then(|dict| dict.get("recap_budget_bytes"))
        .and_then(|value| value.as_int())
    {
        if value < 0 {
            return Err(VmError::Runtime(
                "transcript_compact: recap_budget_bytes must be >= 0".into(),
            ));
        }
        parsed.recap_budget_bytes = value as usize;
    }
    parsed.summarize_prompt = options
        .and_then(|dict| dict.get("summarize_prompt"))
        .and_then(|value| match value {
            VmValue::String(text) if !text.trim().is_empty() => Some(text.to_string()),
            _ => None,
        });
    parsed.summary = options
        .and_then(|dict| dict.get("summary"))
        .and_then(|value| match value {
            VmValue::String(text) if !text.trim().is_empty() => Some(text.to_string()),
            _ => None,
        });
    if let Some(value) = options
        .and_then(|dict| dict.get("custom_compactor"))
        .cloned()
    {
        if !matches!(value, VmValue::Closure(_)) {
            return Err(VmError::Runtime(
                "transcript_compact: custom_compactor must be a closure".into(),
            ));
        }
        parsed.custom_compactor = Some(value);
    }
    if parsed.strategy == CompactStrategy::Custom && parsed.custom_compactor.is_none() {
        return Err(VmError::Runtime(
            "transcript_compact: custom_compactor is required with strategy 'custom'".into(),
        ));
    }
    if parsed.summarize_prompt.is_some() && parsed.strategy != CompactStrategy::Llm {
        return Err(VmError::Runtime(
            "transcript_compact: summarize_prompt is only supported with strategy 'llm'".into(),
        ));
    }
    Ok(parsed)
}

fn merge_summary(existing: Option<String>, next: &str) -> Option<String> {
    if next.trim().is_empty() {
        return existing;
    }
    match existing {
        Some(existing) if !existing.trim().is_empty() && existing.trim() != next.trim() => {
            Some(format!("{existing}\n\n{next}"))
        }
        Some(existing) if !existing.trim().is_empty() => Some(existing),
        _ => Some(next.to_string()),
    }
}

fn transcript_state(transcript: &crate::value::DictMap) -> Option<&str> {
    transcript.get("state").and_then(|value| match value {
        VmValue::String(text) if !text.is_empty() => Some(text.as_ref()),
        _ => None,
    })
}
