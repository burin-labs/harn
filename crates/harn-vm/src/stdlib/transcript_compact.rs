use std::collections::BTreeMap;
use std::rc::Rc;

use crate::agent_events::AgentEvent;
use crate::llm::helpers::{
    extract_llm_options, is_transcript_value, new_transcript_with_events,
    normalize_transcript_asset, transcript_asset_list, transcript_event, transcript_id,
    transcript_message_list, transcript_summary_text, vm_value_to_json,
};
use crate::orchestration::{
    auto_compact_messages, compact_strategy_name, estimate_message_tokens, AutoCompactConfig,
    CompactStrategy,
};
use crate::stdlib::json_to_vm_value;
use crate::value::{VmError, VmValue};
use crate::vm::Vm;

pub(crate) fn register_transcript_compaction_builtins(vm: &mut Vm) {
    vm.register_async_builtin("transcript_compact", |args| async move {
        let transcript = args
            .first()
            .and_then(|value| value.as_dict())
            .filter(|_| args.first().is_some_and(is_transcript_value))
            .ok_or_else(|| {
                VmError::Runtime("transcript_compact: first argument must be a transcript".into())
            })?;
        let options = args.get(1).and_then(|value| value.as_dict());
        compact_transcript_impl(transcript, options, args.get(1).cloned()).await
    });
}

#[derive(Clone)]
struct TranscriptCompactOptions {
    strategy: CompactStrategy,
    keep_last: usize,
    target_tokens: Option<usize>,
    summarize_prompt: Option<String>,
    summary: Option<String>,
}

async fn compact_transcript_impl(
    transcript: &BTreeMap<String, VmValue>,
    options: Option<&BTreeMap<String, VmValue>>,
    raw_options: Option<VmValue>,
) -> Result<VmValue, VmError> {
    let options = parse_options(options)?;
    let mut config = AutoCompactConfig {
        keep_last: options.keep_last,
        compact_strategy: options.strategy.clone(),
        hard_limit_strategy: options.strategy.clone(),
        summarize_prompt: options.summarize_prompt.clone(),
        ..Default::default()
    };
    if let Some(target_tokens) = options.target_tokens {
        config.token_threshold = target_tokens;
        config.hard_limit_tokens = Some(target_tokens);
    } else {
        config.token_threshold = 0;
    }

    let original_transcript = VmValue::Dict(Rc::new(transcript.clone()));
    let mut messages: Vec<serde_json::Value> = transcript_message_list(transcript)?
        .iter()
        .map(vm_value_to_json)
        .collect();
    let estimated_tokens_before = estimate_message_tokens(&messages);
    if options
        .target_tokens
        .is_some_and(|target_tokens| estimated_tokens_before <= target_tokens)
    {
        return Ok(original_transcript);
    }

    let llm_opts = if config.compact_strategy == CompactStrategy::Llm {
        Some(extract_llm_options(&[
            VmValue::String(Rc::from("")),
            VmValue::Nil,
            raw_options.unwrap_or(VmValue::Nil),
        ])?)
    } else {
        None
    };

    let original_message_count = messages.len();
    let Some(summary) = auto_compact_messages(&mut messages, &config, llm_opts.as_ref()).await?
    else {
        return Ok(original_transcript);
    };
    let summary = options.summary.clone().unwrap_or(summary);
    let estimated_tokens_after = estimate_message_tokens(&messages);
    let archived_messages = original_message_count
        .saturating_sub(messages.len())
        .saturating_add(1);
    let snapshot_asset = build_snapshot_asset(
        &original_transcript,
        &options,
        archived_messages,
        estimated_tokens_before,
        estimated_tokens_after,
    );
    let snapshot_asset_id = snapshot_asset
        .as_dict()
        .and_then(|dict| dict.get("id"))
        .map(|value| value.display())
        .unwrap_or_default();
    let mut assets = transcript_asset_list(transcript)?;
    assets.push(snapshot_asset);

    let metadata = serde_json::json!({
        "mode": "manual",
        "strategy": compact_strategy_name(&options.strategy),
        "keep_last": options.keep_last,
        "target_tokens": options.target_tokens,
        "archived_messages": archived_messages,
        "estimated_tokens_before": estimated_tokens_before,
        "estimated_tokens_after": estimated_tokens_after,
        "snapshot_asset_id": snapshot_asset_id,
    });
    let mut extra_events = transcript_extra_events(transcript);
    extra_events.push(transcript_event(
        "compaction",
        "system",
        "internal",
        &format!(
            "Transcript compacted via {}",
            compact_strategy_name(&options.strategy)
        ),
        Some(metadata.clone()),
    ));

    if let Some(session_id) =
        transcript_id(transcript).or_else(crate::llm::current_agent_session_id)
    {
        crate::llm::emit_live_agent_event(&AgentEvent::TranscriptCompacted {
            session_id,
            mode: "manual".to_string(),
            strategy: compact_strategy_name(&options.strategy).to_string(),
            archived_messages,
            estimated_tokens_before,
            estimated_tokens_after,
            snapshot_asset_id: Some(snapshot_asset_id.clone()),
        })
        .await;
    }

    let compacted = new_transcript_with_events(
        transcript_id(transcript),
        messages.iter().map(json_to_vm_value).collect(),
        merge_summary(transcript_summary_text(transcript), &summary),
        transcript.get("metadata").cloned(),
        extra_events,
        assets,
        transcript_state(transcript),
    );
    Ok(compacted)
}

fn parse_options(
    options: Option<&BTreeMap<String, VmValue>>,
) -> Result<TranscriptCompactOptions, VmError> {
    let mut parsed = TranscriptCompactOptions {
        strategy: CompactStrategy::ObservationMask,
        keep_last: AutoCompactConfig::default().keep_last,
        target_tokens: None,
        summarize_prompt: None,
        summary: None,
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
    if parsed.strategy == CompactStrategy::Custom {
        return Err(VmError::Runtime(
            "transcript_compact: strategy 'custom' is not supported; use 'llm', 'truncate', or 'observation_mask'"
                .into(),
        ));
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
    if parsed.summarize_prompt.is_some() && parsed.strategy != CompactStrategy::Llm {
        return Err(VmError::Runtime(
            "transcript_compact: summarize_prompt is only supported with strategy 'llm'".into(),
        ));
    }
    Ok(parsed)
}

fn build_snapshot_asset(
    transcript: &VmValue,
    options: &TranscriptCompactOptions,
    archived_messages: usize,
    estimated_tokens_before: usize,
    estimated_tokens_after: usize,
) -> VmValue {
    let asset = VmValue::Dict(Rc::new(BTreeMap::from([
        (
            "id".to_string(),
            VmValue::String(Rc::from(format!(
                "compaction-source-{}",
                uuid::Uuid::now_v7()
            ))),
        ),
        (
            "kind".to_string(),
            VmValue::String(Rc::from("compaction_source_transcript")),
        ),
        (
            "title".to_string(),
            VmValue::String(Rc::from("Pre-compaction transcript")),
        ),
        (
            "visibility".to_string(),
            VmValue::String(Rc::from("internal")),
        ),
        ("data".to_string(), transcript.clone()),
        (
            "metadata".to_string(),
            VmValue::Dict(Rc::new(BTreeMap::from([
                (
                    "strategy".to_string(),
                    VmValue::String(Rc::from(compact_strategy_name(&options.strategy))),
                ),
                (
                    "archived_messages".to_string(),
                    VmValue::Int(archived_messages as i64),
                ),
                (
                    "estimated_tokens_before".to_string(),
                    VmValue::Int(estimated_tokens_before as i64),
                ),
                (
                    "estimated_tokens_after".to_string(),
                    VmValue::Int(estimated_tokens_after as i64),
                ),
            ]))),
        ),
    ])));
    normalize_transcript_asset(&asset)
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

fn transcript_extra_events(transcript: &BTreeMap<String, VmValue>) -> Vec<VmValue> {
    transcript
        .get("events")
        .and_then(|events| match events {
            VmValue::List(list) => Some(
                list.iter()
                    .filter(|event| {
                        event
                            .as_dict()
                            .and_then(|dict| dict.get("kind"))
                            .map(|value| value.display())
                            .is_some_and(|kind| kind != "message" && kind != "tool_result")
                    })
                    .cloned()
                    .collect(),
            ),
            _ => None,
        })
        .unwrap_or_default()
}

fn transcript_state(transcript: &BTreeMap<String, VmValue>) -> Option<&str> {
    transcript.get("state").and_then(|value| match value {
        VmValue::String(text) if !text.is_empty() => Some(text.as_ref()),
        _ => None,
    })
}
