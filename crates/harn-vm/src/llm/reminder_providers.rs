//! System reminder provider registry and canonical provider implementations.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use serde_json::Value as JsonValue;

use harn_parser::diagnostic_codes::Code;

use crate::llm::helpers::{ReminderPropagate, ReminderRoleHint, ReminderSource, SystemReminder};
use crate::orchestration::{HookEffect, HookEvent, ReminderSpec};
use crate::value::{VmClosure, VmError, VmValue};

const TOKEN_PRESSURE_ID: &str = "token_pressure";
const IDLE_NUDGE_ID: &str = "idle_nudge";
const TOOL_OUTPUT_TRUNCATED_ID: &str = "tool_output_truncated";
const POST_COMPACT_RECAP_ID: &str = "post_compact_recap";
const RESUME_CONTINUITY_ID: &str = "resume_continuity";

const TOKEN_PRESSURE_EVENTS: &[HookEvent] = &[HookEvent::OnBudgetThreshold];
const IDLE_NUDGE_EVENTS: &[HookEvent] = &[HookEvent::SessionIdle];
const TOOL_OUTPUT_TRUNCATED_EVENTS: &[HookEvent] = &[HookEvent::PostToolUse];
const POST_COMPACT_RECAP_EVENTS: &[HookEvent] = &[HookEvent::PostCompact];
const RESUME_CONTINUITY_EVENTS: &[HookEvent] = &[HookEvent::WorkerResumed];

/// Context passed to reminder providers.
#[derive(Clone, Debug)]
pub struct ProviderContext {
    pub event: HookEvent,
    pub session_id: String,
    pub payload: JsonValue,
    pub options: JsonValue,
}

/// Provider interface for lifecycle-driven system reminders.
pub trait ReminderProvider {
    fn id(&self) -> &'static str;
    fn subscribes_to(&self) -> &'static [HookEvent];
    fn evaluate(&self, ctx: &ProviderContext) -> Option<ReminderSpec>;
}

#[derive(Clone)]
struct VmReminderProvider {
    id: String,
    subscribes_to: Vec<HookEvent>,
    evaluate: Rc<VmClosure>,
}

thread_local! {
    static USER_PROVIDERS: RefCell<Vec<VmReminderProvider>> = const { RefCell::new(Vec::new()) };
}

struct TokenPressureProvider;
struct IdleNudgeProvider;
struct ToolOutputTruncatedProvider;
struct PostCompactRecapProvider;
struct ResumeContinuityProvider;

impl ReminderProvider for TokenPressureProvider {
    fn id(&self) -> &'static str {
        TOKEN_PRESSURE_ID
    }

    fn subscribes_to(&self) -> &'static [HookEvent] {
        TOKEN_PRESSURE_EVENTS
    }

    fn evaluate(&self, ctx: &ProviderContext) -> Option<ReminderSpec> {
        let tokens_used = json_i64(&ctx.payload, "tokens_used")?;
        let context_window = token_pressure_context_window(ctx)?;
        if context_window <= 0 {
            return None;
        }
        let ratio = tokens_used as f64 / context_window as f64;
        let (threshold, severity, preserve_on_compact) = if ratio >= 0.95 {
            (95, "CRITICAL", true)
        } else if ratio >= 0.85 {
            (85, "WARNING", false)
        } else if ratio >= 0.70 {
            (70, "CAUTION", false)
        } else {
            return None;
        };
        let percent = (ratio * 100.0).round() as i64;
        let body = format!(
            "Token pressure {severity}: session has used about {percent}% of the context window ({tokens_used}/{context_window} tokens). Compact or summarize before continuing."
        );
        let mut reminder = provider_reminder(body, TOKEN_PRESSURE_ID, ctx);
        reminder.tags = vec![TOKEN_PRESSURE_ID.to_string()];
        reminder.dedupe_key = Some(TOKEN_PRESSURE_ID.to_string());
        reminder.ttl_turns = Some(2);
        reminder.preserve_on_compact = preserve_on_compact;
        reminder.role_hint = ReminderRoleHint::Developer;
        reminder.body = format!("[{threshold}%] {}", reminder.body);
        Some(reminder)
    }
}

impl ReminderProvider for IdleNudgeProvider {
    fn id(&self) -> &'static str {
        IDLE_NUDGE_ID
    }

    fn subscribes_to(&self) -> &'static [HookEvent] {
        IDLE_NUDGE_EVENTS
    }

    fn evaluate(&self, ctx: &ProviderContext) -> Option<ReminderSpec> {
        let wake_interval_ms = json_i64(&ctx.payload, "wake_interval_ms").unwrap_or(0);
        let idle_seconds = provider_config_i64(ctx, IDLE_NUDGE_ID, &["idle_seconds", "seconds"])
            .unwrap_or(60)
            .max(1);
        if wake_interval_ms < idle_seconds.saturating_mul(1000) {
            return None;
        }
        let mut reminder = provider_reminder(
            format!(
                "Session has been idle for at least {idle_seconds}s. Re-check recent state before taking the next autonomous action."
            ),
            IDLE_NUDGE_ID,
            ctx,
        );
        reminder.tags = vec!["idle".to_string()];
        reminder.dedupe_key = Some(IDLE_NUDGE_ID.to_string());
        reminder.ttl_turns = Some(1);
        reminder.propagate = ReminderPropagate::None;
        Some(reminder)
    }
}

impl ReminderProvider for ToolOutputTruncatedProvider {
    fn id(&self) -> &'static str {
        TOOL_OUTPUT_TRUNCATED_ID
    }

    fn subscribes_to(&self) -> &'static [HookEvent] {
        TOOL_OUTPUT_TRUNCATED_EVENTS
    }

    fn evaluate(&self, ctx: &ProviderContext) -> Option<ReminderSpec> {
        let result = ctx.payload.get("result").unwrap_or(&JsonValue::Null);
        let truncated = json_bool(&ctx.payload, "truncated")
            .or_else(|| json_bool(result, "truncated"))
            .unwrap_or(false);
        if !truncated {
            return None;
        }
        let tool_name = ctx
            .payload
            .get("tool_name")
            .and_then(JsonValue::as_str)
            .or_else(|| {
                ctx.payload
                    .get("tool")
                    .and_then(|tool| tool.get("name"))
                    .and_then(JsonValue::as_str)
            })
            .unwrap_or("tool");
        let original_size = json_i64(&ctx.payload, "original_size")
            .or_else(|| json_i64(result, "original_size"))
            .unwrap_or(0);
        let mut reminder = provider_reminder(
            format!(
                "Tool output from `{tool_name}` was truncated before it reached the model. Original size: {original_size} bytes/chars."
            ),
            TOOL_OUTPUT_TRUNCATED_ID,
            ctx,
        );
        reminder.tags = vec!["truncation".to_string()];
        reminder.dedupe_key = Some(format!("{TOOL_OUTPUT_TRUNCATED_ID}:{tool_name}"));
        reminder.ttl_turns = Some(1);
        reminder.propagate = ReminderPropagate::None;
        Some(reminder)
    }
}

impl ReminderProvider for PostCompactRecapProvider {
    fn id(&self) -> &'static str {
        POST_COMPACT_RECAP_ID
    }

    fn subscribes_to(&self) -> &'static [HookEvent] {
        POST_COMPACT_RECAP_EVENTS
    }

    fn evaluate(&self, ctx: &ProviderContext) -> Option<ReminderSpec> {
        let archived = json_i64(&ctx.payload, "archived_messages").unwrap_or(0);
        if archived <= 0 {
            return None;
        }
        let summary = ctx
            .payload
            .get("summary")
            .and_then(JsonValue::as_str)
            .unwrap_or("")
            .trim();
        let body = if summary.is_empty() {
            format!(
                "Transcript compacted after archiving {archived} messages. Use the current recap instead of assuming older details remain verbatim."
            )
        } else {
            format!("Transcript compacted. Current recap: {summary}")
        };
        let mut reminder = provider_reminder(body, POST_COMPACT_RECAP_ID, ctx);
        reminder.tags = vec!["recap".to_string()];
        reminder.dedupe_key = Some(POST_COMPACT_RECAP_ID.to_string());
        reminder.ttl_turns = Some(2);
        reminder.preserve_on_compact = false;
        Some(reminder)
    }
}

impl ReminderProvider for ResumeContinuityProvider {
    fn id(&self) -> &'static str {
        RESUME_CONTINUITY_ID
    }

    fn subscribes_to(&self) -> &'static [HookEvent] {
        RESUME_CONTINUITY_EVENTS
    }

    fn evaluate(&self, ctx: &ProviderContext) -> Option<ReminderSpec> {
        let reason = json_path_str(&ctx.payload, &["suspension", "reason"])
            .or_else(|| json_str(&ctx.payload, "reason"))
            .map(clean_inline_text)
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "unspecified".to_string());
        let suspended_at_turn = json_i64(&ctx.payload, "suspended_at_turn")
            .or_else(|| json_path_i64(&ctx.payload, &["suspension", "suspended_at_turn"]))
            .unwrap_or(0);
        let input_presentation = if json_bool(&ctx.payload, "input_present").unwrap_or(false) {
            let input = json_str(&ctx.payload, "input_rendered")
                .or_else(|| json_path_str(&ctx.payload, &["resume", "input_rendered"]))
                .map(clean_inline_text)
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| "<unrenderable input>".to_string());
            format!("The resumer provided input: {input}")
        } else {
            "No input was provided".to_string()
        };
        let mut body = format!(
            "You were suspended at turn {suspended_at_turn} for reason: '{reason}'. {}. {input_presentation}.",
            resume_cause(ctx),
        );
        if !json_bool(&ctx.payload, "continue_transcript").unwrap_or(true) {
            if let Some(digest) = json_str(&ctx.payload, "digest")
                .or_else(|| json_path_str(&ctx.payload, &["resume", "digest"]))
                .map(clean_inline_text)
                .filter(|value| !value.is_empty())
            {
                body.push_str(" Pre-suspend conversation digest: ");
                body.push_str(&digest);
            }
        }

        let mut reminder = provider_reminder(body, RESUME_CONTINUITY_ID, ctx);
        reminder.tags = vec![RESUME_CONTINUITY_ID.to_string()];
        reminder.dedupe_key = Some(RESUME_CONTINUITY_ID.to_string());
        reminder.ttl_turns = Some(1);
        reminder.preserve_on_compact = true;
        reminder.propagate = ReminderPropagate::None;
        reminder.role_hint = ReminderRoleHint::System;
        Some(reminder)
    }
}

pub fn parse_provider_event(name: &str) -> Result<HookEvent, String> {
    match name.trim() {
        "PostToolUse" | "post_tool_use" => Ok(HookEvent::PostToolUse),
        "OnBudgetThreshold" | "on_budget_threshold" => Ok(HookEvent::OnBudgetThreshold),
        "SessionIdle" | "session_idle" => Ok(HookEvent::SessionIdle),
        "PostCompact" | "post_compact" => Ok(HookEvent::PostCompact),
        "WorkerSpawned" | "worker_spawned" => Ok(HookEvent::WorkerSpawned),
        "WorkerProgressed" | "worker_progressed" => Ok(HookEvent::WorkerProgressed),
        "WorkerWaitingForInput" | "worker_waiting_for_input" => {
            Ok(HookEvent::WorkerWaitingForInput)
        }
        "WorkerSuspended" | "worker_suspended" => Ok(HookEvent::WorkerSuspended),
        "WorkerResumed" | "worker_resumed" => Ok(HookEvent::WorkerResumed),
        "WorkerCompleted" | "worker_completed" => Ok(HookEvent::WorkerCompleted),
        "WorkerFailed" | "worker_failed" => Ok(HookEvent::WorkerFailed),
        "WorkerCancelled" | "worker_cancelled" => Ok(HookEvent::WorkerCancelled),
        other => HookEvent::parse_session_event(other)
            .map_err(|_| format!("unknown reminder provider event `{other}`")),
    }
}

pub fn register_vm_provider(
    id: impl Into<String>,
    subscribes_to: Vec<HookEvent>,
    evaluate: Rc<VmClosure>,
) {
    let id = id.into();
    USER_PROVIDERS.with(|providers| {
        let mut providers = providers.borrow_mut();
        providers.retain(|provider| provider.id != id);
        providers.push(VmReminderProvider {
            id,
            subscribes_to,
            evaluate,
        });
    });
}

pub fn clear_reminder_providers() {
    USER_PROVIDERS.with(|providers| providers.borrow_mut().clear());
}

pub async fn evaluate_and_inject(
    event: HookEvent,
    session_id: &str,
    payload: JsonValue,
    options: JsonValue,
) -> Result<JsonValue, VmError> {
    if session_id.trim().is_empty() || !crate::agent_sessions::exists(session_id) {
        return Ok(serde_json::json!({"reports": [], "fired_count": 0}));
    }
    let payload = normalize_payload(event, session_id, payload);
    let ctx = ProviderContext {
        event,
        session_id: session_id.to_string(),
        payload,
        options,
    };
    let user_providers = USER_PROVIDERS.with(|providers| providers.borrow().clone());
    let enabled = enabled_provider_ids(&ctx.options, &user_providers);
    if enabled.is_empty() {
        return Ok(serde_json::json!({"reports": [], "fired_count": 0}));
    }

    let mut reports = Vec::new();
    for provider in canonical_providers() {
        if !enabled.contains(provider.id()) || !subscribes_to(provider.subscribes_to(), event) {
            continue;
        }
        let reminder = provider.evaluate(&ctx);
        emit_provider_evaluated(&ctx, provider.id(), reminder.is_some(), None);
        if let Some(reminder) = reminder {
            reports.push(inject_report(session_id, provider.id(), reminder)?);
        }
    }

    for provider in user_providers {
        if !enabled.contains(provider.id.as_str()) || !subscribes_to(&provider.subscribes_to, event)
        {
            continue;
        }
        match evaluate_vm_provider(&provider, &ctx).await {
            Ok(reminders) => {
                emit_provider_evaluated(&ctx, &provider.id, !reminders.is_empty(), None);
                for reminder in reminders {
                    reports.push(inject_report(session_id, &provider.id, reminder)?);
                }
            }
            Err(error) => {
                emit_provider_evaluated(&ctx, &provider.id, false, Some(error.to_string()));
                return Err(error);
            }
        }
    }

    Ok(serde_json::json!({
        "fired_count": reports.len(),
        "reports": reports,
    }))
}

fn canonical_providers() -> [&'static dyn ReminderProvider; 5] {
    [
        &TokenPressureProvider,
        &IdleNudgeProvider,
        &ToolOutputTruncatedProvider,
        &PostCompactRecapProvider,
        &ResumeContinuityProvider,
    ]
}

fn subscribes_to(events: &[HookEvent], event: HookEvent) -> bool {
    events.contains(&event)
}

fn normalize_payload(event: HookEvent, session_id: &str, payload: JsonValue) -> JsonValue {
    let mut payload = match payload {
        JsonValue::Object(map) => JsonValue::Object(map),
        _ => JsonValue::Object(serde_json::Map::new()),
    };
    if let JsonValue::Object(map) = &mut payload {
        map.entry("event".to_string())
            .or_insert_with(|| JsonValue::String(event.as_str().to_string()));
        map.entry("session".to_string()).or_insert_with(|| {
            serde_json::json!({
                "id": session_id,
            })
        });
        map.entry("session_id".to_string())
            .or_insert_with(|| JsonValue::String(session_id.to_string()));
    }
    payload
}

fn enabled_provider_ids(
    options: &JsonValue,
    user_providers: &[VmReminderProvider],
) -> BTreeSet<String> {
    let reminders = options.get("reminders").unwrap_or(&JsonValue::Null);
    if reminders.as_bool() == Some(false)
        || reminders
            .get("enabled")
            .and_then(JsonValue::as_bool)
            .is_some_and(|enabled| !enabled)
    {
        return BTreeSet::new();
    }

    let mut enabled: BTreeSet<String> = canonical_provider_ids()
        .into_iter()
        .map(str::to_string)
        .collect();
    for provider in user_providers {
        enabled.insert(provider.id.clone());
    }
    if let Some(providers) = reminders.get("providers").and_then(JsonValue::as_array) {
        for provider in providers {
            let Some(raw) = provider
                .as_str()
                .map(str::trim)
                .filter(|raw| !raw.is_empty())
            else {
                continue;
            };
            if let Some(id) = raw.strip_prefix('-') {
                enabled.remove(id);
            } else {
                enabled.insert(raw.to_string());
            }
        }
    }
    enabled
}

fn canonical_provider_ids() -> [&'static str; 5] {
    [
        TOKEN_PRESSURE_ID,
        IDLE_NUDGE_ID,
        TOOL_OUTPUT_TRUNCATED_ID,
        POST_COMPACT_RECAP_ID,
        RESUME_CONTINUITY_ID,
    ]
}

async fn evaluate_vm_provider(
    provider: &VmReminderProvider,
    ctx: &ProviderContext,
) -> Result<Vec<ReminderSpec>, VmError> {
    let Some(mut vm) = crate::vm::clone_async_builtin_child_vm() else {
        return Err(VmError::Runtime(
            "register_reminder_provider: evaluate requires an async builtin VM context".to_string(),
        ));
    };
    let arg = crate::stdlib::json_to_vm_value(&serde_json::json!({
        "event": ctx.event.as_str(),
        "session": {"id": ctx.session_id},
        "session_id": ctx.session_id,
        "payload": ctx.payload,
        "options": ctx.options,
        "config": provider_config_json(ctx, &provider.id).cloned().unwrap_or(JsonValue::Null),
    }));
    let raw = vm.call_closure_pub(&provider.evaluate, &[arg]).await?;
    let effects = crate::orchestration::parse_hook_effects(ctx.event, &raw).map_err(|error| {
        let message = error.to_string();
        if message.contains("HARN-RMD-") {
            error
        } else {
            VmError::Runtime(format!(
                "{}: provider `{}` returned malformed ReminderSpec: {message}",
                Code::ReminderProviderMalformedSpec.as_str(),
                provider.id
            ))
        }
    })?;
    let fired_at_turn = fired_at_turn(ctx);
    let mut reminders = Vec::new();
    for effect in effects {
        match effect {
            HookEffect::Reminder(mut reminder) => {
                reminder.source = ReminderSource::StdlibProvider;
                reminder.fired_at_turn = fired_at_turn;
                reminders.push(reminder);
            }
        }
    }
    Ok(reminders)
}

fn inject_report(
    session_id: &str,
    provider_id: &str,
    reminder: ReminderSpec,
) -> Result<JsonValue, VmError> {
    let report =
        crate::agent_sessions::inject_reminder(session_id, reminder).map_err(VmError::Runtime)?;
    Ok(serde_json::json!({
        "provider": provider_id,
        "reminder_id": report.reminder_id,
        "deduped_count": report.deduped_count,
    }))
}

fn emit_provider_evaluated(
    ctx: &ProviderContext,
    provider_id: &str,
    fired: bool,
    error: Option<String>,
) {
    let mut payload = serde_json::json!({
        "session_id": &ctx.session_id,
        "provider_id": provider_id,
        "event": ctx.event.as_str(),
        "fired": fired,
    });
    if let Some(error) = error {
        if let Some(obj) = payload.as_object_mut() {
            obj.insert("error".to_string(), JsonValue::String(error));
        }
    }
    crate::llm::helpers::emit_reminder_lifecycle_event(
        crate::llm::helpers::REMINDER_PROVIDER_EVALUATED_EVENT_KIND,
        payload,
    );
}

fn provider_reminder(
    body: impl Into<String>,
    provider_id: &str,
    ctx: &ProviderContext,
) -> SystemReminder {
    let mut reminder =
        SystemReminder::new(body, ReminderSource::StdlibProvider, fired_at_turn(ctx));
    reminder.tags = vec![provider_id.to_string()];
    reminder.dedupe_key = Some(provider_id.to_string());
    reminder.propagate = ReminderPropagate::Session;
    reminder.role_hint = ReminderRoleHint::System;
    reminder
}

fn fired_at_turn(ctx: &ProviderContext) -> i64 {
    json_i64(&ctx.payload, "iteration")
        .or_else(|| json_i64(&ctx.payload, "turn"))
        .unwrap_or(0)
}

fn token_pressure_context_window(ctx: &ProviderContext) -> Option<i64> {
    json_i64(&ctx.payload, "context_window")
        .or_else(|| provider_config_i64(ctx, TOKEN_PRESSURE_ID, &["context_window"]))
        .or_else(|| json_i64(&ctx.options, "context_window"))
        .or_else(|| json_i64(&ctx.options, "max_context_tokens"))
        .or_else(|| model_context_window(ctx))
}

fn model_context_window(ctx: &ProviderContext) -> Option<i64> {
    let model = ctx
        .payload
        .get("model")
        .and_then(JsonValue::as_str)
        .or_else(|| ctx.options.get("model").and_then(JsonValue::as_str))?;
    let resolved = crate::llm_config::resolve_model_info(model);
    crate::llm_config::model_catalog_entry(&resolved.id)
        .or_else(|| crate::llm_config::model_catalog_entry(model))
        .and_then(|entry| {
            entry
                .runtime_context_window
                .or(Some(entry.context_window))
                .map(|window| window as i64)
        })
}

fn resume_cause(ctx: &ProviderContext) -> String {
    let initiator = json_path_str(&ctx.payload, &["resume", "initiator"])
        .or_else(|| json_str(&ctx.payload, "initiator"))
        .unwrap_or_else(|| "operator".to_string());
    match initiator.as_str() {
        "parent" => "You have been resumed by your parent agent".to_string(),
        "timeout" => "Your timeout elapsed; resuming with summary".to_string(),
        "triggered" | "trigger" => {
            let trigger_id = json_path_str(&ctx.payload, &["resume", "trigger", "id"])
                .or_else(|| json_path_str(&ctx.payload, &["trigger", "id"]))
                .or_else(|| json_str(&ctx.payload, "trigger_id"))
                .unwrap_or_else(|| "unknown".to_string());
            let event_id = json_path_str(&ctx.payload, &["resume", "trigger", "event_id"])
                .or_else(|| json_path_str(&ctx.payload, &["trigger", "event_id"]))
                .or_else(|| json_str(&ctx.payload, "event_id"))
                .unwrap_or_else(|| "unknown".to_string());
            format!("You have been resumed by trigger {trigger_id} matching event {event_id}")
        }
        _ => "You have been resumed by the operator".to_string(),
    }
}

fn clean_inline_text(value: String) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn provider_config_i64(ctx: &ProviderContext, provider_id: &str, keys: &[&str]) -> Option<i64> {
    let config = provider_config_json(ctx, provider_id)?;
    for key in keys {
        if let Some(value) = json_i64(config, key) {
            return Some(value);
        }
    }
    None
}

fn provider_config_json<'a>(ctx: &'a ProviderContext, provider_id: &str) -> Option<&'a JsonValue> {
    ctx.options
        .get("reminders")
        .and_then(|reminders| reminders.get("config"))
        .and_then(|config| config.get(provider_id))
        .or_else(|| {
            ctx.options
                .get("reminders")
                .and_then(|reminders| reminders.get(provider_id))
        })
}

fn json_i64(value: &JsonValue, key: &str) -> Option<i64> {
    value.get(key).and_then(|value| {
        value
            .as_i64()
            .or_else(|| value.as_u64().and_then(|value| i64::try_from(value).ok()))
            .or_else(|| value.as_f64().map(|value| value as i64))
    })
}

fn json_path<'a>(value: &'a JsonValue, path: &[&str]) -> Option<&'a JsonValue> {
    let mut current = value;
    for key in path {
        current = current.get(*key)?;
    }
    Some(current)
}

fn json_str(value: &JsonValue, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(JsonValue::as_str)
        .map(str::to_string)
}

fn json_path_str(value: &JsonValue, path: &[&str]) -> Option<String> {
    json_path(value, path)
        .and_then(JsonValue::as_str)
        .map(str::to_string)
}

fn json_path_i64(value: &JsonValue, path: &[&str]) -> Option<i64> {
    json_path(value, path).and_then(|value| {
        value
            .as_i64()
            .or_else(|| value.as_u64().and_then(|value| i64::try_from(value).ok()))
            .or_else(|| value.as_f64().map(|value| value as i64))
    })
}

fn json_bool(value: &JsonValue, key: &str) -> Option<bool> {
    value.get(key).and_then(JsonValue::as_bool)
}

pub(crate) fn options_map_to_json(options: &BTreeMap<String, VmValue>) -> JsonValue {
    JsonValue::Object(
        options
            .iter()
            .map(|(key, value)| (key.clone(), crate::llm::helpers::vm_value_to_json(value)))
            .collect(),
    )
}
