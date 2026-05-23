use std::collections::BTreeMap;
use std::rc::Rc;

use crate::agent_events::AgentEvent;
use crate::llm::helpers::{
    emit_reminder_lifecycle_event, extract_llm_options, is_transcript_value,
    new_transcript_with_events, normalize_transcript_asset, reminder_from_event,
    reminder_lifecycle_payload, replace_reminder_payload, transcript_asset_list, transcript_event,
    transcript_id, transcript_message_list, transcript_summary_text, vm_value_to_json,
    SystemReminder, REMINDER_DEDUPED_EVENT_KIND, REMINDER_EXPIRED_EVENT_KIND,
};
use crate::orchestration::{
    auto_compact_messages, compact_strategy_name, estimate_message_tokens, AutoCompactConfig,
    CompactStrategy, CompactionPolicy,
};
use crate::orchestration::{HookControl, HookEvent};
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
    custom_compactor: Option<VmValue>,
    policy: CompactionPolicy,
}

async fn compact_transcript_impl(
    transcript: &BTreeMap<String, VmValue>,
    options: Option<&BTreeMap<String, VmValue>>,
    raw_options: Option<VmValue>,
) -> Result<VmValue, VmError> {
    let provider_options = options
        .map(crate::llm::reminder_providers::options_map_to_json)
        .unwrap_or_else(|| serde_json::json!({}));
    let options = parse_options(options)?;
    let mut config = AutoCompactConfig {
        keep_last: options.keep_last,
        compact_strategy: options.strategy.clone(),
        hard_limit_strategy: options.strategy.clone(),
        summarize_prompt: options.summarize_prompt.clone(),
        custom_compactor: options.custom_compactor.clone(),
        policy: options.policy.clone(),
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
    let compact_session_id =
        transcript_id(transcript).or_else(crate::llm::current_agent_session_id);
    let pre_payload = compact_hook_payload(
        HookEvent::PreCompact,
        compact_session_id.as_deref(),
        &options,
        messages.len(),
        None,
        None,
        estimated_tokens_before,
        None,
        None,
        None,
        None,
    );
    if let HookControl::Block { .. } =
        crate::orchestration::run_lifecycle_hooks_with_control(HookEvent::PreCompact, &pre_payload)
            .await?
    {
        return Ok(original_transcript);
    }
    let reminder_report = compact_reminder_events(transcript_extra_events(transcript));
    config.custom_compactor_reminders = reminder_report.custom_reminders.clone();

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
    emit_reminder_compact_lifecycle(transcript_id(transcript), &reminder_report);
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
    let post_payload = compact_hook_payload(
        HookEvent::PostCompact,
        compact_session_id.as_deref(),
        &options,
        original_message_count,
        Some(messages.len()),
        Some(archived_messages),
        estimated_tokens_before,
        Some(estimated_tokens_after),
        Some(&summary),
        Some(&snapshot_asset_id),
        Some(&reminder_report),
    );
    let mut assets = transcript_asset_list(transcript)?;
    assets.push(snapshot_asset);

    let mut metadata = serde_json::json!({
        "mode": "manual",
        "strategy": compact_strategy_name(&options.strategy),
        "keep_last": options.keep_last,
        "target_tokens": options.target_tokens,
        "archived_messages": archived_messages,
        "estimated_tokens_before": estimated_tokens_before,
        "estimated_tokens_after": estimated_tokens_after,
        "snapshot_asset_id": snapshot_asset_id,
        "reminders_decremented": reminder_report.decremented_count,
        "reminders_expired": reminder_report.expired.len(),
        "reminders_deduped": reminder_report.deduped.len(),
        "reminders_preserved": reminder_report.preserved_count,
    });
    add_compaction_policy_metadata(&mut metadata, &options.policy);
    let mut extra_events = reminder_report.preserved_events;
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

    crate::orchestration::run_lifecycle_hooks(HookEvent::PostCompact, &post_payload).await?;

    if let Some(session_id) = compact_session_id.as_deref() {
        crate::llm::emit_live_agent_event(&AgentEvent::TranscriptCompacted {
            session_id: session_id.to_string(),
            mode: "manual".to_string(),
            strategy: compact_strategy_name(&options.strategy).to_string(),
            archived_messages,
            estimated_tokens_before,
            estimated_tokens_after,
            snapshot_asset_id: Some(snapshot_asset_id.clone()),
            instruction_mode: Some(options.policy.instruction_mode().to_string()),
            instruction_source: options.policy.instruction_source().map(str::to_string),
            compaction_policy: options.policy.metadata_json(),
        })
        .await;

        let _ = crate::llm::reminder_providers::evaluate_and_inject(
            HookEvent::PostCompact,
            session_id,
            post_payload,
            provider_options,
        )
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

#[allow(clippy::too_many_arguments)]
fn compact_hook_payload(
    event: HookEvent,
    session_id: Option<&str>,
    options: &TranscriptCompactOptions,
    message_count: usize,
    remaining_messages: Option<usize>,
    archived_messages: Option<usize>,
    estimated_tokens_before: usize,
    estimated_tokens_after: Option<usize>,
    summary: Option<&str>,
    snapshot_asset_id: Option<&str>,
    reminder_report: Option<&ReminderCompactReport>,
) -> serde_json::Value {
    let session_id = session_id.unwrap_or_default();
    let mut payload = serde_json::json!({
        "event": event.as_str(),
        "session": {"id": session_id},
        "session_id": session_id,
        "strategy": compact_strategy_name(&options.strategy),
        "engine_strategy": compact_strategy_name(&options.strategy),
        "keep_last": options.keep_last,
        "target_tokens": options.target_tokens,
        "message_count": message_count,
        "estimated_tokens_before": estimated_tokens_before,
    });
    let Some(map) = payload.as_object_mut() else {
        return payload;
    };
    for (key, value) in crate::orchestration::compaction_policy_metadata_fields(&options.policy) {
        map.insert(key.to_string(), value);
    }
    if let Some(value) = remaining_messages {
        map.insert("remaining_messages".to_string(), serde_json::json!(value));
    }
    if let Some(value) = archived_messages {
        map.insert("archived_messages".to_string(), serde_json::json!(value));
    }
    if let Some(value) = estimated_tokens_after {
        map.insert(
            "estimated_tokens_after".to_string(),
            serde_json::json!(value),
        );
    }
    if let Some(summary) = summary {
        map.insert("summary".to_string(), serde_json::json!(summary));
        map.insert(
            "new_summary_len".to_string(),
            serde_json::json!(summary.len()),
        );
    }
    if let Some(snapshot_asset_id) = snapshot_asset_id {
        map.insert(
            "snapshot_asset_id".to_string(),
            serde_json::json!(snapshot_asset_id),
        );
    }
    if let Some(report) = reminder_report {
        map.insert(
            "reminders_decremented".to_string(),
            serde_json::json!(report.decremented_count),
        );
        map.insert(
            "reminders_expired".to_string(),
            serde_json::json!(report.expired.len()),
        );
        map.insert(
            "reminders_deduped".to_string(),
            serde_json::json!(report.deduped.len()),
        );
        map.insert(
            "reminders_preserved".to_string(),
            serde_json::json!(report.preserved_count),
        );
    }
    payload
}

#[derive(Debug, Default)]
struct ReminderCompactReport {
    preserved_events: Vec<VmValue>,
    custom_reminders: Vec<VmValue>,
    expired: Vec<SystemReminder>,
    compacted: Vec<SystemReminder>,
    deduped: Vec<ReminderDedupeRecord>,
    decremented_count: usize,
    preserved_count: usize,
}

#[derive(Clone, Debug)]
struct ReminderDedupeRecord {
    replaced_id: String,
    replacing_id: String,
    dedupe_key: String,
}

enum CompactEvent {
    Other(VmValue),
    Reminder {
        event: VmValue,
        reminder: SystemReminder,
        reminder_index: usize,
    },
}

fn compact_reminder_events(extra_events: Vec<VmValue>) -> ReminderCompactReport {
    let mut events = Vec::with_capacity(extra_events.len());
    let mut reminders = Vec::new();
    let mut expired = Vec::new();
    let mut decremented_count = 0;

    for event in extra_events {
        let Some(reminder) = reminder_from_event(&event) else {
            events.push(CompactEvent::Other(event));
            continue;
        };

        let (event, reminder) = match reminder.ttl_turns {
            Some(ttl) if ttl <= 1 => {
                expired.push(reminder);
                continue;
            }
            Some(ttl) => {
                let mut updated = reminder;
                updated.ttl_turns = Some(ttl - 1);
                decremented_count += 1;
                (replace_reminder_payload(&event, &updated), updated)
            }
            None => (event, reminder),
        };

        let reminder_index = reminders.len();
        reminders.push(reminder.clone());
        events.push(CompactEvent::Reminder {
            event,
            reminder,
            reminder_index,
        });
    }

    let mut newest_by_dedupe_key = BTreeMap::new();
    for (index, reminder) in reminders.iter().enumerate() {
        if let Some(dedupe_key) = reminder.dedupe_key.as_deref() {
            newest_by_dedupe_key.insert(dedupe_key.to_string(), index);
        }
    }

    let mut kept_reminders = Vec::new();
    let mut preserved_events = Vec::new();
    let mut compacted = Vec::new();
    let mut deduped = Vec::new();
    let mut preserved_count = 0;

    for event in events {
        match event {
            CompactEvent::Other(event) => preserved_events.push(event),
            CompactEvent::Reminder {
                event,
                reminder,
                reminder_index,
            } => {
                let keep = reminder
                    .dedupe_key
                    .as_deref()
                    .and_then(|key| newest_by_dedupe_key.get(key))
                    .is_none_or(|newest| *newest == reminder_index);
                if !keep {
                    let replacing_id = reminder
                        .dedupe_key
                        .as_deref()
                        .and_then(|key| newest_by_dedupe_key.get(key))
                        .and_then(|index| reminders.get(*index))
                        .map(|newest| newest.id.clone())
                        .unwrap_or_default();
                    deduped.push(ReminderDedupeRecord {
                        replaced_id: reminder.id.clone(),
                        replacing_id,
                        dedupe_key: reminder.dedupe_key.clone().unwrap_or_default(),
                    });
                    continue;
                }

                kept_reminders.push(crate::stdlib::json_to_vm_value(
                    &serde_json::to_value(&reminder).unwrap_or(serde_json::Value::Null),
                ));
                if reminder.preserve_on_compact {
                    preserved_count += 1;
                    preserved_events.push(event);
                } else {
                    compacted.push(reminder);
                }
            }
        }
    }

    ReminderCompactReport {
        preserved_events,
        custom_reminders: kept_reminders,
        expired,
        compacted,
        deduped,
        decremented_count,
        preserved_count,
    }
}

fn emit_reminder_compact_lifecycle(transcript_id: Option<String>, report: &ReminderCompactReport) {
    for reminder in &report.expired {
        let mut payload = reminder_lifecycle_payload(transcript_id.as_deref(), reminder);
        if let Some(obj) = payload.as_object_mut() {
            obj.insert(
                "transcript_id".to_string(),
                serde_json::json!(&transcript_id),
            );
            obj.insert(
                "reason".to_string(),
                serde_json::Value::String("ttl".to_string()),
            );
            obj.insert(
                "ttl_turns_before".to_string(),
                serde_json::json!(&reminder.ttl_turns),
            );
            obj.insert("expired_at_turn".to_string(), serde_json::Value::Null);
            obj.insert(
                "expired_at_boundary".to_string(),
                serde_json::Value::String("pre_compact".to_string()),
            );
            obj.insert(
                "phase".to_string(),
                serde_json::Value::String("pre_compact".to_string()),
            );
        }
        emit_reminder_lifecycle_event(REMINDER_EXPIRED_EVENT_KIND, payload);
    }

    for reminder in &report.compacted {
        let mut payload = reminder_lifecycle_payload(transcript_id.as_deref(), reminder);
        if let Some(obj) = payload.as_object_mut() {
            obj.insert(
                "transcript_id".to_string(),
                serde_json::json!(&transcript_id),
            );
            obj.insert(
                "reason".to_string(),
                serde_json::Value::String("compaction".to_string()),
            );
            obj.insert(
                "expired_at_boundary".to_string(),
                serde_json::Value::String("pre_compact".to_string()),
            );
            obj.insert(
                "phase".to_string(),
                serde_json::Value::String("pre_compact".to_string()),
            );
        }
        emit_reminder_lifecycle_event(REMINDER_EXPIRED_EVENT_KIND, payload);
    }

    if !report.deduped.is_empty() {
        let dropped_reminder_ids = report
            .deduped
            .iter()
            .map(|record| record.replaced_id.clone())
            .collect::<Vec<_>>();
        emit_reminder_lifecycle_event(
            REMINDER_DEDUPED_EVENT_KIND,
            serde_json::json!({
                "transcript_id": &transcript_id,
                "boundary": "pre_compact",
                "replaced_id": report.deduped.first().map(|record| &record.replaced_id),
                "replacing_id": report.deduped.first().map(|record| &record.replacing_id),
                "dedupe_key": report.deduped.first().map(|record| &record.dedupe_key),
                "replaced_ids": &dropped_reminder_ids,
                "dropped_reminder_ids": &dropped_reminder_ids,
                "dropped_count": dropped_reminder_ids.len(),
            }),
        );
    }
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
        custom_compactor: None,
        policy: crate::orchestration::parse_compaction_policy_options(
            options,
            "transcript_compact",
        )?,
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

fn add_compaction_policy_metadata(metadata: &mut serde_json::Value, policy: &CompactionPolicy) {
    let Some(map) = metadata.as_object_mut() else {
        return;
    };
    for (key, value) in crate::orchestration::compaction_policy_metadata_fields(policy) {
        map.insert(key.to_string(), value);
    }
}

fn build_snapshot_asset(
    transcript: &VmValue,
    options: &TranscriptCompactOptions,
    archived_messages: usize,
    estimated_tokens_before: usize,
    estimated_tokens_after: usize,
) -> VmValue {
    let mut asset_metadata = BTreeMap::from([
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
        (
            "instruction_mode".to_string(),
            VmValue::String(Rc::from(options.policy.instruction_mode())),
        ),
    ]);
    if let Some(policy_json) = options.policy.metadata_json() {
        asset_metadata.insert(
            "compaction_policy".to_string(),
            crate::stdlib::json_to_vm_value(&policy_json),
        );
    }
    if let Some(source) = options.policy.instruction_source() {
        asset_metadata.insert(
            "instruction_source".to_string(),
            VmValue::String(Rc::from(source)),
        );
    }
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
            VmValue::Dict(Rc::new(asset_metadata)),
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
