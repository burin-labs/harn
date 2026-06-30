//! Centralized compaction lifecycle.
//!
//! Every transcript compaction in the runtime — manual `transcript_compact()`,
//! `agent_session_compact()`, `transcript_auto_compact()`, worker-transcript
//! compaction during resume, and host-script-driven auto-compaction — funnels
//! through [`run_compaction_lifecycle`] so the hook contract, reminder
//! lifecycle, and `AgentEvent::TranscriptCompacted` payload are identical
//! regardless of entry point.
//!
//! Lifecycle ordering:
//!
//! 1. Estimate tokens before.
//! 2. Build the `PreCompact` payload.
//! 3. Fire `PreCompact` lifecycle hooks with veto/modify control. `Block`
//!    cancels compaction; `Modify` applies caller-facing overrides
//!    (`keep_last`, `target_tokens`, `strategy`) back to the config.
//! 4. Run the reminder lifecycle (`preserve_on_compact`, `ttl_turns`,
//!    `dedupe_key`) over the caller-supplied reminder events.
//! 5. Invoke [`auto_compact_messages`] to perform the actual compaction.
//! 6. Emit per-reminder lifecycle events (`expired`, `deduped`).
//! 7. Build the `PostCompact` payload with archived count, summary, and
//!    optional snapshot asset id.
//! 8. Fire `PostCompact` lifecycle hooks (non-veto).
//! 9. Re-evaluate registered reminder providers against the post-compact
//!    payload so injected reminders land on the next turn.
//! 10. Emit `AgentEvent::TranscriptCompacted` when the call carries a
//!     `session_id`.

use crate::value::VmDictExt;
use std::collections::BTreeMap;

use serde_json::Value as JsonValue;

use crate::agent_events::AgentEvent;
use crate::llm::api::LlmCallOptions;
use crate::llm::helpers::{
    emit_reminder_lifecycle_event, normalize_transcript_asset, reminder_from_event,
    reminder_lifecycle_payload, replace_reminder_payload, SystemReminder,
    REMINDER_DEDUPED_EVENT_KIND, REMINDER_EXPIRED_EVENT_KIND,
};
use crate::value::{VmError, VmValue};

use super::{
    auto_compact_messages_with_result_with_ctx, compact_strategy_name,
    compaction_policy_metadata_fields, estimate_message_tokens, parse_compact_strategy,
    run_lifecycle_hooks_with_control_with_ctx, run_lifecycle_hooks_with_ctx, AutoCompactConfig,
    CompactStrategy, CompactionPolicy, HookControl, HookEvent,
};

/// Identifies the call-site that initiated compaction. The string form is
/// exposed in hook payloads and `AgentEvent::TranscriptCompacted` so
/// downstream consumers can route user-initiated compactions differently from
/// automatic agent-loop ones.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompactMode {
    /// `transcript_compact()` stdlib builtin (user-initiated, transcript dict in,
    /// transcript dict out).
    Manual,
    /// `agent_session_compact()` stdlib builtin (host-initiated, mutates an
    /// active agent session in place).
    Host,
    /// In-agent-loop automatic compaction emitted by host scripts after the
    /// turn-budget check fires. Mirrors what `host_agent_record_compaction`
    /// historically labelled as `auto`.
    Auto,
    /// `transcript_auto_compact()` workflow builtin operating on a raw message
    /// list with no owning session.
    Workflow,
    /// Worker-transcript compaction during snapshot resume.
    Worker,
    /// Resume-time digest extraction (kept verbatim so the bypass remains
    /// observable). No hooks fire for this mode.
    ResumeDigest,
}

impl CompactMode {
    pub fn as_str(self) -> &'static str {
        match self {
            CompactMode::Manual => "manual",
            CompactMode::Host => "host",
            CompactMode::Auto => "auto",
            CompactMode::Workflow => "workflow",
            CompactMode::Worker => "worker",
            CompactMode::ResumeDigest => "resume_digest",
        }
    }

    /// Session-level `PreCompact` / `PostCompact` hooks fire only for
    /// modes that operate against an owning agent session. The other
    /// modes are utility wrappers around raw message lists or worker
    /// transcripts — their callers (e.g. the `.harn` agent loop)
    /// orchestrate the session-level hook firing separately, so the
    /// lifecycle path here must stay silent to avoid double-dispatch.
    pub fn fires_hooks(self) -> bool {
        match self {
            CompactMode::Manual | CompactMode::Host | CompactMode::Auto => true,
            CompactMode::Workflow | CompactMode::Worker | CompactMode::ResumeDigest => false,
        }
    }
}

/// Identifies why compaction fired. This is separate from [`CompactMode`]:
/// mode describes the caller surface, while trigger explains the pressure
/// that made the caller compact.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompactionTrigger {
    Manual,
    Threshold,
    BudgetPressure,
}

impl CompactionTrigger {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Threshold => "threshold",
            Self::BudgetPressure => "budget_pressure",
        }
    }
}

/// Per-call inputs that travel with a compaction request through the
/// lifecycle. Stored as references to keep allocations down on the hot path.
pub struct CompactLifecycle<'a> {
    pub session_id: Option<&'a str>,
    pub transcript_id: Option<&'a str>,
    pub mode: CompactMode,
    pub trigger: CompactionTrigger,
    pub fire_hooks: bool,
    /// Reminder events from the source transcript that should pass through
    /// the `preserve_on_compact` / `ttl_turns` / `dedupe_key` lifecycle
    /// before being re-attached to the compacted transcript.
    pub reminder_events: Vec<VmValue>,
    /// Caller-supplied summary override. When `Some`, replaces the
    /// `auto_compact_messages` output before the post-compact payload is
    /// assembled. Used by `transcript_compact()` to support pre-computed
    /// summaries.
    pub summary_override: Option<String>,
    /// Provider options forwarded to `evaluate_and_inject` so registered
    /// providers see the same shape the caller observed.
    pub provider_options: JsonValue,
    /// Optional source-transcript value used to build a pre-compaction
    /// snapshot asset. Paths that don't have a transcript dict (e.g.,
    /// `transcript_auto_compact()` on a raw list) leave this `None` and
    /// the post-compact payload omits `snapshot_asset_id`.
    pub source_transcript: Option<&'a VmValue>,
    /// Whether to invoke the registered reminder providers after the
    /// post-compact hook chain. Only meaningful when `session_id` is set.
    pub evaluate_providers: bool,
}

impl<'a> CompactLifecycle<'a> {
    pub fn new(mode: CompactMode) -> Self {
        let trigger = match mode {
            CompactMode::Manual | CompactMode::Host | CompactMode::ResumeDigest => {
                CompactionTrigger::Manual
            }
            CompactMode::Auto | CompactMode::Workflow | CompactMode::Worker => {
                CompactionTrigger::Threshold
            }
        };
        Self {
            session_id: None,
            transcript_id: None,
            mode,
            trigger,
            fire_hooks: mode.fires_hooks(),
            reminder_events: Vec::new(),
            summary_override: None,
            provider_options: JsonValue::Object(serde_json::Map::new()),
            source_transcript: None,
            evaluate_providers: true,
        }
    }

    pub fn with_session_id(mut self, session_id: Option<&'a str>) -> Self {
        self.session_id = session_id;
        self
    }

    pub fn with_transcript_id(mut self, transcript_id: Option<&'a str>) -> Self {
        self.transcript_id = transcript_id;
        self
    }

    pub fn with_trigger(mut self, trigger: CompactionTrigger) -> Self {
        self.trigger = trigger;
        self
    }

    pub fn with_hook_dispatch(mut self, fire_hooks: bool) -> Self {
        self.fire_hooks = fire_hooks;
        self
    }

    pub fn with_reminder_events(mut self, events: Vec<VmValue>) -> Self {
        self.reminder_events = events;
        self
    }

    pub fn with_summary_override(mut self, summary: Option<String>) -> Self {
        self.summary_override = summary;
        self
    }

    pub fn with_provider_options(mut self, options: JsonValue) -> Self {
        self.provider_options = options;
        self
    }

    pub fn with_source_transcript(mut self, transcript: Option<&'a VmValue>) -> Self {
        self.source_transcript = transcript;
        self
    }

    pub fn with_evaluate_providers(mut self, evaluate: bool) -> Self {
        self.evaluate_providers = evaluate;
        self
    }
}

/// Result of a successful compaction. Returned to callers so they can
/// finalize their own persistence (transcript dict assembly, agent-session
/// replacement, snapshot recording). The messages themselves are mutated in
/// place on the caller's `Vec` so a no-op return (`Ok(None)`) leaves them
/// unchanged for downstream code that always writes the messages back.
pub struct CompactionOutcome {
    pub summary: String,
    pub archived_messages: usize,
    pub estimated_tokens_before: usize,
    pub estimated_tokens_after: usize,
    pub reminder_report: ReminderCompactReport,
    /// Snapshot asset built from the caller-supplied source transcript.
    /// `None` when no source transcript was provided.
    pub snapshot_asset: Option<VmValue>,
    /// `snapshot_asset.id` extracted for inclusion in event payloads.
    pub snapshot_asset_id: Option<String>,
    /// Engine strategy actually used (after honoring any PreCompact `Modify`).
    pub strategy: CompactStrategy,
    /// User-facing policy label resolved on the config.
    pub policy_strategy: String,
    /// `metadata` block ready to attach to the persisted transcript
    /// `"compaction"` event. Includes policy fields + reminder counts.
    pub event_metadata: JsonValue,
}

#[derive(Clone, Debug)]
pub struct TranscriptCompactedEventMetrics {
    pub archived_messages: usize,
    pub estimated_tokens_before: usize,
    pub estimated_tokens_after: usize,
    pub snapshot_asset_id: Option<String>,
}

/// Reminder-lifecycle bookkeeping produced before the compaction runs and
/// consumed by both the persisted transcript and the AgentEvent payload.
#[derive(Debug, Default)]
pub struct ReminderCompactReport {
    /// Non-reminder events plus reminders flagged `preserve_on_compact`.
    /// Callers re-attach these to the compacted transcript.
    pub preserved_events: Vec<VmValue>,
    /// Reminder values handed to `custom_compactor` callbacks so user
    /// scripts can fold pending reminders into their summarization output.
    pub custom_reminders: Vec<VmValue>,
    /// Reminders whose `ttl_turns` reached zero this compaction.
    pub expired: Vec<SystemReminder>,
    /// Reminders that were folded into the compacted summary because
    /// they had no `preserve_on_compact` flag.
    pub compacted: Vec<SystemReminder>,
    /// Reminders dropped because a newer reminder with the same
    /// `dedupe_key` was retained.
    pub deduped: Vec<ReminderDedupeRecord>,
    /// Count of reminders whose `ttl_turns` were decremented (still alive).
    pub decremented_count: usize,
    /// Count of reminders that carried `preserve_on_compact = true`.
    pub preserved_count: usize,
}

#[derive(Clone, Debug)]
pub struct ReminderDedupeRecord {
    pub replaced_id: String,
    pub replacing_id: String,
    pub dedupe_key: String,
}

/// Run a transcript compaction through the canonical lifecycle. The
/// `messages` vec is mutated in place by [`auto_compact_messages_with_result`]; on a
/// `Ok(None)` return it is left untouched so callers that always write
/// messages back (e.g. `transcript_auto_compact()`) can do so unconditionally.
///
/// `Ok(None)` means no compaction happened — either the messages were
/// already under threshold, a PreCompact hook returned `Block`, or
/// `auto_compact_messages_with_result` itself decided there was nothing to do.
pub(crate) async fn run_compaction_lifecycle(
    messages: &mut Vec<JsonValue>,
    config: &mut AutoCompactConfig,
    llm_opts: Option<&LlmCallOptions>,
    lifecycle: CompactLifecycle<'_>,
) -> Result<Option<CompactionOutcome>, VmError> {
    run_compaction_lifecycle_with_ctx(None, messages, config, llm_opts, lifecycle).await
}

pub(crate) async fn run_compaction_lifecycle_with_ctx(
    ctx: Option<&crate::vm::AsyncBuiltinCtx>,
    messages: &mut Vec<JsonValue>,
    config: &mut AutoCompactConfig,
    llm_opts: Option<&LlmCallOptions>,
    mut lifecycle: CompactLifecycle<'_>,
) -> Result<Option<CompactionOutcome>, VmError> {
    // Move `reminder_events` out up front so subsequent reads of
    // `lifecycle` don't trip the partial-move check.
    let reminder_events = std::mem::take(&mut lifecycle.reminder_events);

    let estimated_tokens_before = estimate_message_tokens(messages);
    let original_message_count = messages.len();

    let fires_hooks = lifecycle.fire_hooks;

    if fires_hooks {
        let pre_payload = build_hook_payload(
            HookEvent::PreCompact,
            &lifecycle,
            config,
            HookPayloadStage::Pre {
                message_count: original_message_count,
                estimated_tokens_before,
            },
        );
        match run_lifecycle_hooks_with_control_with_ctx(ctx, HookEvent::PreCompact, &pre_payload)
            .await?
        {
            HookControl::Block { .. } => return Ok(None),
            HookControl::Modify { payload } => apply_pre_modify_overrides(config, &payload)?,
            HookControl::Allow | HookControl::Decision { .. } => {}
        }
    }

    let reminder_report = compact_reminder_events(reminder_events);
    config.custom_compactor_reminders = reminder_report.custom_reminders.clone();

    let Some(compact_result) =
        auto_compact_messages_with_result_with_ctx(ctx, messages, config, llm_opts).await?
    else {
        return Ok(None);
    };
    let engine_strategy = compact_result.strategy;
    let raw_summary = compact_result.summary;
    let summary = lifecycle.summary_override.clone().unwrap_or(raw_summary);

    if fires_hooks {
        emit_reminder_lifecycle_records(lifecycle.transcript_id, &reminder_report);
    }

    let estimated_tokens_after = estimate_message_tokens(messages);
    let archived_messages = original_message_count
        .saturating_sub(messages.len())
        .saturating_add(1);

    let snapshot_asset = lifecycle.source_transcript.map(|transcript| {
        build_snapshot_asset(
            transcript,
            config,
            &engine_strategy,
            archived_messages,
            estimated_tokens_before,
            estimated_tokens_after,
        )
    });
    let snapshot_asset_id = snapshot_asset.as_ref().map(snapshot_asset_id_of);
    let event_metrics = TranscriptCompactedEventMetrics {
        archived_messages,
        estimated_tokens_before,
        estimated_tokens_after,
        snapshot_asset_id: snapshot_asset_id.clone(),
    };

    let event_metadata = build_event_metadata(
        &lifecycle,
        config,
        &event_metrics,
        &reminder_report,
        &summary,
        &engine_strategy,
    );

    if fires_hooks {
        let post_payload = build_hook_payload(
            HookEvent::PostCompact,
            &lifecycle,
            config,
            HookPayloadStage::Post {
                original_message_count,
                remaining_messages: messages.len(),
                archived_messages,
                estimated_tokens_before,
                estimated_tokens_after,
                summary: &summary,
                snapshot_asset_id: snapshot_asset_id.as_deref(),
                reminder_report: &reminder_report,
            },
        );
        run_lifecycle_hooks_with_ctx(ctx, HookEvent::PostCompact, &post_payload).await?;

        if let Some(session_id) = lifecycle.session_id {
            emit_transcript_compacted_event(
                ctx,
                session_id,
                lifecycle.mode,
                lifecycle.trigger.as_str(),
                config,
                event_metrics.clone(),
            )
            .await;
            if lifecycle.evaluate_providers {
                let _ = crate::llm::reminder_providers::evaluate_and_inject(
                    ctx,
                    HookEvent::PostCompact,
                    session_id,
                    post_payload,
                    lifecycle.provider_options.clone(),
                )
                .await;
            }
        }
    }

    Ok(Some(CompactionOutcome {
        summary,
        archived_messages,
        estimated_tokens_before,
        estimated_tokens_after,
        reminder_report,
        snapshot_asset,
        snapshot_asset_id,
        strategy: engine_strategy,
        policy_strategy: config.policy_strategy.clone(),
        event_metadata,
    }))
}

/// Emit `AgentEvent::TranscriptCompacted` with the shared payload shape.
/// Exposed for the host-script `host_agent_record_compaction` builtin which
/// records compactions performed entirely from `.harn` code; lifecycle
/// callers reach this through [`run_compaction_lifecycle`].
pub async fn emit_transcript_compacted_event(
    ctx: Option<&crate::vm::AsyncBuiltinCtx>,
    session_id: &str,
    mode: CompactMode,
    reason: &str,
    config: &AutoCompactConfig,
    metrics: TranscriptCompactedEventMetrics,
) {
    crate::llm::emit_live_agent_event_with_ctx(
        ctx,
        &AgentEvent::TranscriptCompacted {
            session_id: session_id.to_string(),
            mode: mode.as_str().to_string(),
            reason: reason.to_string(),
            strategy: config.policy_strategy.clone(),
            archived_messages: metrics.archived_messages,
            estimated_tokens_before: metrics.estimated_tokens_before,
            estimated_tokens_after: metrics.estimated_tokens_after,
            snapshot_asset_id: metrics.snapshot_asset_id,
            instruction_mode: Some(config.policy.instruction_mode().to_string()),
            instruction_source: config.policy.instruction_source().map(str::to_string),
            compaction_policy: config.policy.metadata_json(),
        },
    )
    .await;
}

/// Synchronous variant of [`emit_transcript_compacted_event`]. Used by
/// `host_agent_record_compaction` which runs in a sync builtin context and
/// can't `.await` directly.
pub fn emit_transcript_compacted_event_sync(
    session_id: &str,
    mode: CompactMode,
    reason: String,
    policy: &CompactionPolicy,
    policy_strategy: String,
    metrics: TranscriptCompactedEventMetrics,
) {
    crate::llm::emit_live_agent_event_sync(&AgentEvent::TranscriptCompacted {
        session_id: session_id.to_string(),
        mode: mode.as_str().to_string(),
        reason,
        strategy: policy_strategy,
        archived_messages: metrics.archived_messages,
        estimated_tokens_before: metrics.estimated_tokens_before,
        estimated_tokens_after: metrics.estimated_tokens_after,
        snapshot_asset_id: metrics.snapshot_asset_id,
        instruction_mode: Some(policy.instruction_mode().to_string()),
        instruction_source: policy.instruction_source().map(str::to_string),
        compaction_policy: policy.metadata_json(),
    });
}

// ---------------------------------------------------------------------------
// Internal payload + reminder helpers shared by stdlib builtins and the
// agent-session host.
// ---------------------------------------------------------------------------

enum HookPayloadStage<'a> {
    Pre {
        message_count: usize,
        estimated_tokens_before: usize,
    },
    Post {
        original_message_count: usize,
        remaining_messages: usize,
        archived_messages: usize,
        estimated_tokens_before: usize,
        estimated_tokens_after: usize,
        summary: &'a str,
        snapshot_asset_id: Option<&'a str>,
        reminder_report: &'a ReminderCompactReport,
    },
}

fn build_hook_payload(
    event: HookEvent,
    lifecycle: &CompactLifecycle<'_>,
    config: &AutoCompactConfig,
    stage: HookPayloadStage<'_>,
) -> JsonValue {
    let session_id = lifecycle.session_id.unwrap_or_default();
    let strategy = compact_strategy_name(&config.compact_strategy);
    let mut payload = serde_json::json!({
        "event": event.as_str(),
        "session": {"id": session_id},
        "session_id": session_id,
        "mode": lifecycle.mode.as_str(),
        "reason": lifecycle.trigger.as_str(),
        "strategy": strategy,
        "engine_strategy": strategy,
        "keep_last": config.keep_last,
        "target_tokens": serde_json::Value::Null,
    });
    if config.token_threshold > 0 {
        payload["target_tokens"] = serde_json::json!(config.token_threshold);
    }
    let Some(map) = payload.as_object_mut() else {
        return payload;
    };
    for (key, value) in compaction_policy_metadata_fields(&config.policy) {
        map.insert(key.to_string(), value);
    }
    match stage {
        HookPayloadStage::Pre {
            message_count,
            estimated_tokens_before,
        } => {
            map.insert(
                "message_count".to_string(),
                serde_json::json!(message_count),
            );
            map.insert(
                "estimated_tokens_before".to_string(),
                serde_json::json!(estimated_tokens_before),
            );
        }
        HookPayloadStage::Post {
            original_message_count,
            remaining_messages,
            archived_messages,
            estimated_tokens_before,
            estimated_tokens_after,
            summary,
            snapshot_asset_id,
            reminder_report,
        } => {
            map.insert(
                "message_count".to_string(),
                serde_json::json!(original_message_count),
            );
            map.insert(
                "remaining_messages".to_string(),
                serde_json::json!(remaining_messages),
            );
            map.insert(
                "archived_messages".to_string(),
                serde_json::json!(archived_messages),
            );
            map.insert(
                "estimated_tokens_before".to_string(),
                serde_json::json!(estimated_tokens_before),
            );
            map.insert(
                "estimated_tokens_after".to_string(),
                serde_json::json!(estimated_tokens_after),
            );
            map.insert("summary".to_string(), serde_json::json!(summary));
            map.insert(
                "new_summary_len".to_string(),
                serde_json::json!(summary.len()),
            );
            if let Some(id) = snapshot_asset_id {
                map.insert("snapshot_asset_id".to_string(), serde_json::json!(id));
            }
            map.insert(
                "reminders_decremented".to_string(),
                serde_json::json!(reminder_report.decremented_count),
            );
            map.insert(
                "reminders_expired".to_string(),
                serde_json::json!(reminder_report.expired.len()),
            );
            map.insert(
                "reminders_deduped".to_string(),
                serde_json::json!(reminder_report.deduped.len()),
            );
            map.insert(
                "reminders_preserved".to_string(),
                serde_json::json!(reminder_report.preserved_count),
            );
        }
    }
    payload
}

fn apply_pre_modify_overrides(
    config: &mut AutoCompactConfig,
    payload: &JsonValue,
) -> Result<(), VmError> {
    let Some(map) = payload.as_object() else {
        return Ok(());
    };
    if let Some(value) = map.get("keep_last").and_then(JsonValue::as_u64) {
        config.keep_last = value as usize;
    }
    if let Some(value) = map.get("target_tokens").and_then(JsonValue::as_u64) {
        config.token_threshold = value as usize;
        config.hard_limit_tokens = Some(value as usize);
    }
    if let Some(value) = map.get("strategy").or_else(|| map.get("engine_strategy")) {
        if let Some(name) = value.as_str() {
            let strategy = parse_compact_strategy(name)?;
            config.policy_strategy = compact_strategy_name(&strategy).to_string();
            config.compact_strategy = strategy;
        }
    }
    Ok(())
}

fn build_event_metadata(
    lifecycle: &CompactLifecycle<'_>,
    config: &AutoCompactConfig,
    metrics: &TranscriptCompactedEventMetrics,
    reminder_report: &ReminderCompactReport,
    summary: &str,
    engine_strategy: &CompactStrategy,
) -> JsonValue {
    let mut metadata = serde_json::json!({
        "mode": lifecycle.mode.as_str(),
        "reason": lifecycle.trigger.as_str(),
        "strategy": config.policy_strategy,
        "engine_strategy": compact_strategy_name(engine_strategy),
        "keep_last": config.keep_last,
        "target_tokens": (config.token_threshold > 0).then_some(config.token_threshold),
        "archived_messages": metrics.archived_messages,
        "estimated_tokens_before": metrics.estimated_tokens_before,
        "estimated_tokens_after": metrics.estimated_tokens_after,
        "new_summary_len": summary.len(),
        "snapshot_asset_id": metrics.snapshot_asset_id.as_deref(),
        "reminders_decremented": reminder_report.decremented_count,
        "reminders_expired": reminder_report.expired.len(),
        "reminders_deduped": reminder_report.deduped.len(),
        "reminders_preserved": reminder_report.preserved_count,
    });
    if let Some(map) = metadata.as_object_mut() {
        for (key, value) in compaction_policy_metadata_fields(&config.policy) {
            map.insert(key.to_string(), value);
        }
    }
    metadata
}

enum CompactEvent {
    Other(VmValue),
    Reminder {
        event: VmValue,
        reminder: SystemReminder,
        reminder_index: usize,
    },
}

/// Process a list of reminder events through the canonical lifecycle:
/// expire by TTL, decrement remaining TTLs, dedupe by `dedupe_key`, and
/// retain `preserve_on_compact` reminders for re-attachment.
pub fn compact_reminder_events(extra_events: Vec<VmValue>) -> ReminderCompactReport {
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
                    &serde_json::to_value(&reminder).unwrap_or(JsonValue::Null),
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

fn emit_reminder_lifecycle_records(transcript_id: Option<&str>, report: &ReminderCompactReport) {
    for reminder in &report.expired {
        let mut payload = reminder_lifecycle_payload(transcript_id, reminder);
        if let Some(obj) = payload.as_object_mut() {
            obj.insert(
                "transcript_id".to_string(),
                serde_json::json!(transcript_id),
            );
            obj.insert("reason".to_string(), JsonValue::String("ttl".to_string()));
            obj.insert(
                "ttl_turns_before".to_string(),
                serde_json::json!(reminder.ttl_turns),
            );
            obj.insert("expired_at_turn".to_string(), JsonValue::Null);
            obj.insert(
                "expired_at_boundary".to_string(),
                JsonValue::String("pre_compact".to_string()),
            );
            obj.insert(
                "phase".to_string(),
                JsonValue::String("pre_compact".to_string()),
            );
        }
        emit_reminder_lifecycle_event(REMINDER_EXPIRED_EVENT_KIND, payload);
    }

    for reminder in &report.compacted {
        let mut payload = reminder_lifecycle_payload(transcript_id, reminder);
        if let Some(obj) = payload.as_object_mut() {
            obj.insert(
                "transcript_id".to_string(),
                serde_json::json!(transcript_id),
            );
            obj.insert(
                "reason".to_string(),
                JsonValue::String("compaction".to_string()),
            );
            obj.insert(
                "expired_at_boundary".to_string(),
                JsonValue::String("pre_compact".to_string()),
            );
            obj.insert(
                "phase".to_string(),
                JsonValue::String("pre_compact".to_string()),
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
                "transcript_id": transcript_id,
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

fn build_snapshot_asset(
    transcript: &VmValue,
    config: &AutoCompactConfig,
    engine_strategy: &CompactStrategy,
    archived_messages: usize,
    estimated_tokens_before: usize,
    estimated_tokens_after: usize,
) -> VmValue {
    let mut asset_metadata = BTreeMap::from([
        (
            "strategy".to_string(),
            VmValue::String(arcstr::ArcStr::from(compact_strategy_name(engine_strategy))),
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
            VmValue::String(arcstr::ArcStr::from(config.policy.instruction_mode())),
        ),
    ]);
    if let Some(policy_json) = config.policy.metadata_json() {
        asset_metadata.insert(
            "compaction_policy".to_string(),
            crate::stdlib::json_to_vm_value(&policy_json),
        );
    }
    if let Some(source) = config.policy.instruction_source() {
        asset_metadata.put_str("instruction_source", source);
    }
    let asset = VmValue::dict(BTreeMap::from([
        (
            "id".to_string(),
            VmValue::String(arcstr::ArcStr::from(format!(
                "compaction-source-{}",
                uuid::Uuid::now_v7()
            ))),
        ),
        (
            "kind".to_string(),
            VmValue::String(arcstr::ArcStr::from("compaction_source_transcript")),
        ),
        (
            "title".to_string(),
            VmValue::String(arcstr::ArcStr::from("Pre-compaction transcript")),
        ),
        (
            "visibility".to_string(),
            VmValue::String(arcstr::ArcStr::from("internal")),
        ),
        ("data".to_string(), transcript.clone()),
        ("metadata".to_string(), VmValue::dict(asset_metadata)),
    ]));
    normalize_transcript_asset(&asset)
}

fn snapshot_asset_id_of(asset: &VmValue) -> String {
    asset
        .as_dict()
        .and_then(|dict| dict.get("id"))
        .map(|value| value.display())
        .unwrap_or_default()
}

/// Extract the events from a transcript-shaped dict that should be routed
/// through [`run_compaction_lifecycle`] (everything except `message` and
/// `tool_result` events). This is the canonical filter used by every
/// transcript-having compaction caller — keeping it in one place stops the
/// trivial-but-load-bearing filter list from drifting per-callsite.
pub fn transcript_compactable_events(transcript: &crate::value::DictMap) -> Vec<VmValue> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::helpers::{ReminderPropagate, ReminderRoleHint, ReminderSource};
    use crate::value::VmDictExt;

    fn reminder_event_value(body: &str, preserve: bool, ttl: Option<i64>) -> VmValue {
        let reminder = SystemReminder {
            id: format!("rem-{}", uuid::Uuid::now_v7()),
            tags: Vec::new(),
            dedupe_key: None,
            ttl_turns: ttl,
            preserve_on_compact: preserve,
            propagate: ReminderPropagate::Session,
            role_hint: ReminderRoleHint::System,
            source: ReminderSource::StdlibProvider,
            body: body.to_string(),
            fired_at_turn: 0,
            originating_agent_id: None,
        };
        let reminder_value =
            crate::stdlib::json_to_vm_value(&serde_json::to_value(&reminder).unwrap());
        let mut event = BTreeMap::new();
        event.put_str("kind", "system_reminder");
        event.put_str("role", "system");
        event.insert("reminder".to_string(), reminder_value);
        VmValue::dict(event)
    }

    #[test]
    fn preserve_on_compact_reminder_survives_lifecycle() {
        let preserved = reminder_event_value("keep me", true, None);
        let droppable = reminder_event_value("drop me", false, None);
        let report = compact_reminder_events(vec![preserved, droppable]);
        assert_eq!(report.preserved_count, 1);
        assert_eq!(report.compacted.len(), 1);
        assert_eq!(report.preserved_events.len(), 1);
        assert!(report.preserved_events.iter().any(|event| {
            event
                .as_dict()
                .and_then(|dict| dict.get("reminder"))
                .and_then(|reminder| reminder.as_dict())
                .and_then(|reminder| reminder.get("body"))
                .map(|body| body.display())
                .is_some_and(|body| body == "keep me")
        }));
    }

    #[test]
    fn ttl_one_reminder_expires_during_lifecycle() {
        let ttl_one = reminder_event_value("ephemeral", false, Some(1));
        let report = compact_reminder_events(vec![ttl_one]);
        assert_eq!(report.expired.len(), 1);
        assert_eq!(report.preserved_count, 0);
    }

    #[test]
    fn ttl_above_one_decrements_and_keeps() {
        let ttl_three = reminder_event_value("keep ttl", false, Some(3));
        let report = compact_reminder_events(vec![ttl_three]);
        assert_eq!(report.decremented_count, 1);
        assert_eq!(report.preserved_events.len(), 0);
        assert_eq!(report.compacted.len(), 1);
    }

    #[test]
    fn fires_hooks_only_for_session_owning_modes() {
        // Session-aware entry points fire hooks.
        assert!(CompactMode::Manual.fires_hooks());
        assert!(CompactMode::Host.fires_hooks());
        assert!(CompactMode::Auto.fires_hooks());
        // Utility paths stay silent so callers (`.harn` agent loop,
        // worker resume) can orchestrate session-level hooks
        // themselves without double-dispatch.
        assert!(!CompactMode::Workflow.fires_hooks());
        assert!(!CompactMode::Worker.fires_hooks());
        assert!(!CompactMode::ResumeDigest.fires_hooks());
    }
}
