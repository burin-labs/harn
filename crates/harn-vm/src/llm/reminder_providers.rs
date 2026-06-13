//! System reminder provider registry and canonical provider implementations.

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::sync::Arc;

use serde_json::Value as JsonValue;

use harn_parser::diagnostic_codes::Code;

use crate::llm::helpers::{ReminderPropagate, ReminderRoleHint, ReminderSource, SystemReminder};
use crate::orchestration::{HookEffect, HookEvent, ReminderSpec};
use crate::value::{VmClosure, VmError};

const TOKEN_PRESSURE_ID: &str = "token_pressure";
const IDLE_NUDGE_ID: &str = "idle_nudge";
const TOOL_OUTPUT_TRUNCATED_ID: &str = "tool_output_truncated";
const POST_COMPACT_RECAP_ID: &str = "post_compact_recap";
const RESUME_CONTINUITY_ID: &str = "resume_continuity";
const COMPASS_AST_EDITS_ID: &str = "compass_ast_edits";
const PROJECT_FACTS_ID: &str = "project_facts";
const WORKSPACE_ANCHOR_ID: &str = "workspace_anchor";
const GROUNDED_REVIEW_ID: &str = "grounded_review";

const TOKEN_PRESSURE_EVENTS: &[HookEvent] = &[HookEvent::OnBudgetThreshold];
const IDLE_NUDGE_EVENTS: &[HookEvent] = &[HookEvent::SessionIdle];
const TOOL_OUTPUT_TRUNCATED_EVENTS: &[HookEvent] = &[HookEvent::PostToolUse];
const POST_COMPACT_RECAP_EVENTS: &[HookEvent] = &[HookEvent::PostCompact];
const RESUME_CONTINUITY_EVENTS: &[HookEvent] = &[HookEvent::WorkerResumed];
const PROJECT_FACTS_EVENTS: &[HookEvent] = &[HookEvent::SessionStart, HookEvent::OnBudgetThreshold];
const WORKSPACE_ANCHOR_EVENTS: &[HookEvent] =
    &[HookEvent::SessionStart, HookEvent::OnBudgetThreshold];
const COMPASS_AST_EDITS_EVENTS: &[HookEvent] = &[HookEvent::SessionStart, HookEvent::WorkerResumed];
const GROUNDED_REVIEW_EVENTS: &[HookEvent] = &[
    HookEvent::PostToolUse,
    HookEvent::PostStep,
    HookEvent::PostAgentTurn,
];

const PROJECT_FACTS_DEFAULT_NAMESPACE: &str = "project/facts";
const PROJECT_FACTS_DEFAULT_MAX: i64 = 5;
const PROJECT_FACTS_HARD_MAX: i64 = 25;
const PROJECT_FACTS_DEFAULT_MIN_CONFIDENCE: f64 = 0.5;
const PROJECT_FACTS_RECALL_MULTIPLIER: usize = 4;
const PROJECT_FACTS_DEFAULT_QUERY: &str = "project decisions constraints architecture";
const PROJECT_FACTS_REFRESH_RATIO: f64 = 0.70;
const GROUNDED_REVIEW_DEFAULT_MAX_FINDINGS: usize = 6;
const GROUNDED_REVIEW_HARD_MAX_FINDINGS: usize = 20;
const GROUNDED_REVIEW_MAX_TEXT_BYTES: usize = 96 * 1024;
const GROUNDED_REVIEW_SUMMARY_CHARS: usize = 220;

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
    evaluate: Arc<VmClosure>,
}

thread_local! {
    static USER_PROVIDERS: RefCell<Vec<VmReminderProvider>> = const { RefCell::new(Vec::new()) };
}

struct TokenPressureProvider;
struct IdleNudgeProvider;
struct ToolOutputTruncatedProvider;
struct PostCompactRecapProvider;
struct ResumeContinuityProvider;
struct ProjectFactsProvider;
struct WorkspaceAnchorProvider;
struct GroundedReviewProvider;

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

impl ReminderProvider for ProjectFactsProvider {
    fn id(&self) -> &'static str {
        PROJECT_FACTS_ID
    }

    fn subscribes_to(&self) -> &'static [HookEvent] {
        PROJECT_FACTS_EVENTS
    }

    fn evaluate(&self, ctx: &ProviderContext) -> Option<ReminderSpec> {
        let config = provider_config_json(ctx, PROJECT_FACTS_ID);
        let max_facts = provider_facts_max(config);
        if max_facts == 0 {
            return None;
        }
        // OnBudgetThreshold ticks once per LLM call, so only refresh when the
        // session is actually under pressure (default 70% of the window).
        // SessionStart always fires; the dedupe_key collapses any later
        // refreshes into the freshest fact body.
        if ctx.event == HookEvent::OnBudgetThreshold && !under_budget_pressure(ctx, config) {
            return None;
        }
        let min_confidence = provider_facts_min_confidence(config);
        let kind_filter = provider_facts_kind_filter(config);
        let namespace = provider_facts_namespace(config);
        let root =
            crate::stdlib::memory::resolve_memory_root(provider_facts_root(config).as_deref());
        let query = provider_facts_relevance_query(ctx, config);
        let recall_limit =
            (max_facts.saturating_mul(PROJECT_FACTS_RECALL_MULTIPLIER)).max(max_facts);
        let recalled = match crate::stdlib::memory::lexical_recall_fact_values(
            &root,
            &namespace,
            &query,
            recall_limit,
        ) {
            Ok(records) => records,
            Err(_) => return None,
        };
        let mut filtered = Vec::with_capacity(max_facts);
        for value in recalled {
            if let Some(rendered) = format_fact_line(&value, min_confidence, &kind_filter) {
                filtered.push(rendered);
                if filtered.len() >= max_facts {
                    break;
                }
            }
        }
        if filtered.is_empty() {
            return None;
        }
        let header = format!(
            "Project facts recalled from `{namespace}` (top {count}, min confidence {min:.2}):",
            count = filtered.len(),
            min = min_confidence,
        );
        let mut body = header;
        for line in &filtered {
            body.push('\n');
            body.push_str(line);
        }
        let mut reminder = provider_reminder(body, PROJECT_FACTS_ID, ctx);
        reminder.tags = vec![PROJECT_FACTS_ID.to_string()];
        reminder.dedupe_key = Some(PROJECT_FACTS_ID.to_string());
        reminder.ttl_turns = Some(1);
        reminder.preserve_on_compact = false;
        reminder.propagate = ReminderPropagate::Session;
        reminder.role_hint = ReminderRoleHint::System;
        Some(reminder)
    }
}

impl ReminderProvider for WorkspaceAnchorProvider {
    fn id(&self) -> &'static str {
        WORKSPACE_ANCHOR_ID
    }

    fn subscribes_to(&self) -> &'static [HookEvent] {
        WORKSPACE_ANCHOR_EVENTS
    }

    fn evaluate(&self, ctx: &ProviderContext) -> Option<ReminderSpec> {
        let anchor = crate::agent_sessions::workspace_anchor(&ctx.session_id)?;
        let mut reminder = provider_reminder(
            format_workspace_anchor_body(&anchor),
            WORKSPACE_ANCHOR_ID,
            ctx,
        );
        reminder.tags = vec![WORKSPACE_ANCHOR_ID.to_string()];
        reminder.dedupe_key = Some(WORKSPACE_ANCHOR_ID.to_string());
        reminder.ttl_turns = Some(1);
        reminder.preserve_on_compact = false;
        reminder.propagate = ReminderPropagate::Session;
        reminder.role_hint = ReminderRoleHint::System;
        Some(reminder)
    }
}

fn format_workspace_anchor_body(anchor: &crate::workspace_anchor::WorkspaceAnchor) -> String {
    let mut body = format!(
        "<workspace-anchor>\nprimary: {}",
        anchor.primary.to_string_lossy()
    );
    if anchor.additional_roots.is_empty() {
        body.push_str("\nadditional_roots: []");
    } else {
        body.push_str("\nadditional_roots:");
        for root in &anchor.additional_roots {
            body.push_str("\n  - ");
            body.push_str(&root.path.to_string_lossy());
            body.push_str(" (mount_mode: ");
            body.push_str(root.mount_mode.as_str());
            body.push_str(", mounted_at: ");
            body.push_str(&root.mounted_at);
            body.push(')');
        }
    }
    body.push_str("\n</workspace-anchor>");
    body
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct GroundedReviewFinding {
    source: String,
    confidence: &'static str,
    provenance: String,
    summary: String,
}

impl ReminderProvider for GroundedReviewProvider {
    fn id(&self) -> &'static str {
        GROUNDED_REVIEW_ID
    }

    fn subscribes_to(&self) -> &'static [HookEvent] {
        GROUNDED_REVIEW_EVENTS
    }

    fn evaluate(&self, ctx: &ProviderContext) -> Option<ReminderSpec> {
        let mut findings = grounded_review_findings(ctx);
        if findings.is_empty() {
            return None;
        }
        let max_findings = grounded_review_max_findings(ctx);
        let truncated_count = findings.len().saturating_sub(max_findings);
        findings.truncate(max_findings);

        let mut reminder = provider_reminder(
            format_grounded_review_body(ctx, &findings, truncated_count),
            GROUNDED_REVIEW_ID,
            ctx,
        );
        reminder.tags = vec![GROUNDED_REVIEW_ID.to_string(), "code_review".to_string()];
        reminder.dedupe_key = Some(grounded_review_dedupe_key(ctx.event, &findings));
        reminder.ttl_turns = Some(2);
        reminder.propagate = ReminderPropagate::None;
        reminder.role_hint = ReminderRoleHint::Developer;
        Some(reminder)
    }
}

fn grounded_review_findings(ctx: &ProviderContext) -> Vec<GroundedReviewFinding> {
    let include_warnings =
        provider_config_bool(ctx, GROUNDED_REVIEW_ID, &["include_warnings"]).unwrap_or(false);
    let text_scan = provider_config_bool(ctx, GROUNDED_REVIEW_ID, &["text_scan"]).unwrap_or(true);
    let mut findings = Vec::new();
    collect_explicit_error_finding(&ctx.payload, ctx.event, &mut findings);
    collect_verifier_signal_findings(&ctx.payload, "$", &mut findings, 0);
    collect_structured_review_findings(&ctx.payload, "$", include_warnings, &mut findings, 0);
    if text_scan {
        collect_text_review_findings(&ctx.payload, ctx.event, &mut findings);
    }
    dedupe_review_findings(findings)
}

fn grounded_review_max_findings(ctx: &ProviderContext) -> usize {
    provider_config_i64(ctx, GROUNDED_REVIEW_ID, &["max_findings", "limit"])
        .unwrap_or(GROUNDED_REVIEW_DEFAULT_MAX_FINDINGS as i64)
        .clamp(1, GROUNDED_REVIEW_HARD_MAX_FINDINGS as i64) as usize
}

fn collect_explicit_error_finding(
    payload: &JsonValue,
    event: HookEvent,
    findings: &mut Vec<GroundedReviewFinding>,
) {
    let status = json_str(payload, "status")
        .or_else(|| json_path_str(payload, &["result", "status"]))
        .map(|value| value.to_ascii_lowercase());
    let ok = json_bool(payload, "ok").or_else(|| json_path_bool(payload, &["result", "ok"]));
    let error_value = payload
        .get("error")
        .or_else(|| json_path(payload, &["result", "error"]))
        .or_else(|| json_path(payload, &["result", "message"]));
    let error_summary = error_value.and_then(error_value_summary);
    let failed_status = status
        .as_deref()
        .is_some_and(|value| matches!(value, "error" | "failed" | "failure"));
    if ok != Some(false) && !failed_status && error_summary.is_none() {
        return;
    }
    let summary = error_summary
        .or_else(|| status.map(|value| format!("status={value}")))
        .unwrap_or_else(|| "runtime reported ok=false".to_string());
    findings.push(GroundedReviewFinding {
        source: "runtime_error".to_string(),
        confidence: "verified",
        provenance: format!("event={}", event.as_str()),
        summary: truncate_review_summary(&summary),
    });
}

fn collect_verifier_signal_findings(
    value: &JsonValue,
    path: &str,
    findings: &mut Vec<GroundedReviewFinding>,
    depth: usize,
) {
    if depth > 8 {
        return;
    }
    match value {
        JsonValue::Object(map) => {
            if let Some(signals) = map.get("verifier_signals").and_then(JsonValue::as_array) {
                for signal in signals {
                    if let Some(finding) = verifier_signal_finding(signal, path) {
                        findings.push(finding);
                    }
                }
            }
            if let Some(outcome) = map
                .get("verifier_outcome")
                .and_then(JsonValue::as_str)
                .map(|value| value.trim().to_ascii_lowercase())
                .filter(|value| !is_accept_signal(value))
            {
                findings.push(GroundedReviewFinding {
                    source: "verifier".to_string(),
                    confidence: "verified",
                    provenance: path.to_string(),
                    summary: format!("verifier outcome: {outcome}"),
                });
            }
            for (key, child) in map {
                let child_path = format!("{path}.{key}");
                collect_verifier_signal_findings(child, &child_path, findings, depth + 1);
            }
        }
        JsonValue::Array(items) => {
            for (idx, child) in items.iter().enumerate() {
                let child_path = format!("{path}[{idx}]");
                collect_verifier_signal_findings(child, &child_path, findings, depth + 1);
            }
        }
        _ => {}
    }
}

fn verifier_signal_finding(value: &JsonValue, path: &str) -> Option<GroundedReviewFinding> {
    let signal = value
        .get("signal")
        .and_then(JsonValue::as_str)
        .map(|value| value.trim().to_ascii_lowercase())?;
    if is_accept_signal(&signal) {
        return None;
    }
    let kind = value
        .get("kind")
        .and_then(JsonValue::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("verifier");
    let name = value
        .get("name")
        .and_then(JsonValue::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let reason = value
        .get("reason")
        .and_then(JsonValue::as_str)
        .map(|value| clean_inline_text(value.to_string()))
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| format!("{kind} verifier returned {signal}"));
    let provenance = match name {
        Some(name) => format!("{path} name={name} signal={signal}"),
        None => format!("{path} signal={signal}"),
    };
    Some(GroundedReviewFinding {
        source: format!("verifier:{kind}"),
        confidence: "verified",
        provenance,
        summary: truncate_review_summary(&reason),
    })
}

fn collect_structured_review_findings(
    value: &JsonValue,
    path: &str,
    include_warnings: bool,
    findings: &mut Vec<GroundedReviewFinding>,
    depth: usize,
) {
    if depth > 8 {
        return;
    }
    match value {
        JsonValue::Object(map) => {
            for (key, child) in map {
                if let Some(source) = structured_signal_source_for_key(key) {
                    collect_structured_signal_items(
                        child,
                        source,
                        &format!("{path}.{key}"),
                        include_warnings,
                        findings,
                    );
                }
                let child_path = format!("{path}.{key}");
                collect_structured_review_findings(
                    child,
                    &child_path,
                    include_warnings,
                    findings,
                    depth + 1,
                );
            }
        }
        JsonValue::Array(items) => {
            for (idx, child) in items.iter().enumerate() {
                let child_path = format!("{path}[{idx}]");
                collect_structured_review_findings(
                    child,
                    &child_path,
                    include_warnings,
                    findings,
                    depth + 1,
                );
            }
        }
        _ => {}
    }
}

fn structured_signal_source_for_key(key: &str) -> Option<&'static str> {
    match key.to_ascii_lowercase().as_str() {
        "parse_errors" | "parseerrors" => Some("parse_errors"),
        "undefined_names" | "undefinednames" => Some("undefined_names"),
        "diagnostics" | "issues" | "findings" => Some("diagnostic"),
        "errors" | "failures" => Some("error"),
        _ => None,
    }
}

fn collect_structured_signal_items(
    value: &JsonValue,
    default_source: &str,
    path: &str,
    include_warnings: bool,
    findings: &mut Vec<GroundedReviewFinding>,
) {
    match value {
        JsonValue::Array(items) => {
            for (idx, item) in items.iter().enumerate() {
                if let Some(finding) = structured_signal_finding(
                    item,
                    default_source,
                    &format!("{path}[{idx}]"),
                    include_warnings,
                ) {
                    findings.push(finding);
                }
            }
        }
        JsonValue::Object(_) | JsonValue::String(_) => {
            if let Some(finding) =
                structured_signal_finding(value, default_source, path, include_warnings)
            {
                findings.push(finding);
            }
        }
        _ => {}
    }
}

fn structured_signal_finding(
    value: &JsonValue,
    default_source: &str,
    path: &str,
    include_warnings: bool,
) -> Option<GroundedReviewFinding> {
    let summary = match value {
        JsonValue::String(text) => clean_inline_text(text.clone()),
        JsonValue::Object(_) => structured_signal_summary(value)?,
        _ => return None,
    };
    if summary.is_empty()
        || !include_structured_signal(value, default_source, &summary, include_warnings)
    {
        return None;
    }
    Some(GroundedReviewFinding {
        source: structured_signal_source(value, default_source, &summary),
        confidence: "verified",
        provenance: structured_signal_provenance(value, path),
        summary: truncate_review_summary(&summary),
    })
}

fn include_structured_signal(
    value: &JsonValue,
    default_source: &str,
    summary: &str,
    include_warnings: bool,
) -> bool {
    if matches!(default_source, "parse_errors" | "undefined_names") {
        return true;
    }
    if default_source == "error" {
        return !looks_like_clean_error_summary(summary);
    }
    let severity = first_string_field(value, &["severity", "level", "status", "conclusion"])
        .map(|value| value.to_ascii_lowercase());
    if severity
        .as_deref()
        .is_some_and(|value| matches!(value, "error" | "fatal" | "fail" | "failed" | "failure"))
    {
        return true;
    }
    if include_warnings
        && severity
            .as_deref()
            .is_some_and(|value| matches!(value, "warning" | "warn"))
    {
        return true;
    }
    is_strong_review_signal(summary)
}

fn structured_signal_summary(value: &JsonValue) -> Option<String> {
    for key in [
        "message",
        "reason",
        "summary",
        "title",
        "detail",
        "description",
        "text",
    ] {
        if let Some(text) = value
            .get(key)
            .and_then(JsonValue::as_str)
            .map(|value| clean_inline_text(value.to_string()))
            .filter(|value| !value.is_empty())
        {
            return Some(text);
        }
    }
    value.get("error").and_then(error_value_summary)
}

fn structured_signal_source(value: &JsonValue, default_source: &str, summary: &str) -> String {
    let lower = summary.to_ascii_lowercase();
    if lower.contains("undefined name")
        || lower.contains("undefined variable")
        || lower.contains("undefined identifier")
    {
        return "undefined_names".to_string();
    }
    if lower.contains("parse error") || lower.contains("syntax error") {
        return "parse_errors".to_string();
    }
    first_string_field(value, &["source", "kind", "category", "rule"])
        .map(|source| source.trim().replace(' ', "_"))
        .filter(|source| !source.is_empty())
        .unwrap_or_else(|| default_source.to_string())
}

fn structured_signal_provenance(value: &JsonValue, path: &str) -> String {
    let mut parts = vec![path.to_string()];
    if let Some(file) = first_string_field(value, &["file", "path", "uri"]) {
        parts.push(format!("file={file}"));
    }
    if let Some(line) = json_i64(value, "line")
        .or_else(|| json_i64(value, "start_row").map(|value| value + 1))
        .or_else(|| json_path_i64(value, &["span", "start", "line"]))
    {
        parts.push(format!("line={line}"));
    }
    if let Some(column) = json_i64(value, "column")
        .or_else(|| json_i64(value, "start_col").map(|value| value + 1))
        .or_else(|| json_path_i64(value, &["span", "start", "column"]))
    {
        parts.push(format!("column={column}"));
    }
    parts.join(" ")
}

fn collect_text_review_findings(
    payload: &JsonValue,
    event: HookEvent,
    findings: &mut Vec<GroundedReviewFinding>,
) {
    let tool_name = json_str(payload, "tool_name")
        .or_else(|| json_path_str(payload, &["tool", "name"]))
        .unwrap_or_else(|| "unknown".to_string());
    let command = review_payload_command(payload).unwrap_or_default();
    let mut text_blocks = Vec::new();
    collect_text_field(payload, &["result", "text"], &mut text_blocks);
    collect_text_field(payload, &["result", "rendered_result"], &mut text_blocks);
    collect_text_field(payload, &["rendered_result"], &mut text_blocks);
    collect_text_field(payload, &["observation"], &mut text_blocks);
    collect_text_field(payload, &["error"], &mut text_blocks);
    for text in text_blocks {
        let text = strip_ansi_codes(&truncate_bytes(&text, GROUNDED_REVIEW_MAX_TEXT_BYTES));
        if !looks_like_verifier_output(&tool_name, &command, &text) {
            continue;
        }
        let source = classify_text_review_source(&tool_name, &command, &text);
        for line in text.lines().filter_map(review_failure_line) {
            findings.push(GroundedReviewFinding {
                source: source.clone(),
                confidence: "verified",
                provenance: format!(
                    "event={} tool={}{}",
                    event.as_str(),
                    tool_name,
                    command
                        .is_empty()
                        .then(String::new)
                        .unwrap_or_else(|| format!(
                            " command={}",
                            truncate_review_summary(&command)
                        ))
                ),
                summary: line,
            });
            if findings.len() >= GROUNDED_REVIEW_HARD_MAX_FINDINGS {
                return;
            }
        }
    }
}

fn review_payload_command(payload: &JsonValue) -> Option<String> {
    for path in [
        &["tool", "args", "cmd"][..],
        &["tool", "args", "command"][..],
        &["args", "cmd"][..],
        &["args", "command"][..],
        &["command"][..],
        &["cmd"][..],
    ] {
        if let Some(command) = json_path(payload, path).and_then(command_value_string) {
            return Some(command);
        }
    }
    None
}

fn command_value_string(value: &JsonValue) -> Option<String> {
    match value {
        JsonValue::String(command) => Some(clean_inline_text(command.clone())),
        JsonValue::Array(items) => {
            let parts = items
                .iter()
                .filter_map(JsonValue::as_str)
                .map(str::trim)
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>();
            (!parts.is_empty()).then(|| parts.join(" "))
        }
        _ => None,
    }
}

fn collect_text_field(payload: &JsonValue, path: &[&str], out: &mut Vec<String>) {
    if let Some(text) = json_path(payload, path)
        .and_then(JsonValue::as_str)
        .map(str::to_string)
        .filter(|value| !value.trim().is_empty())
    {
        out.push(text);
    }
}

fn looks_like_verifier_output(tool_name: &str, command: &str, text: &str) -> bool {
    let probe = format!("{tool_name} {command}").to_ascii_lowercase();
    const COMMAND_MARKERS: &[&str] = &[
        "cargo check",
        "cargo test",
        "cargo nextest",
        "cargo clippy",
        "cargo build",
        "harn check",
        "harn lint",
        "harn test",
        "make test",
        "make all",
        "make conformance",
        "make lint",
        "make fmt",
        "npm test",
        "npm run test",
        "npm run lint",
        "npm run build",
        "pnpm test",
        "pnpm lint",
        "yarn test",
        "pytest",
        "go test",
        "swift test",
        "swift build",
        "tsc",
        "eslint",
        "ruff",
        "mypy",
    ];
    if COMMAND_MARKERS.iter().any(|marker| probe.contains(marker)) {
        return true;
    }
    let lower = text.to_ascii_lowercase();
    is_strong_review_signal(&lower)
        || lower.contains("error[e")
        || lower.contains("test result: failed")
        || lower.contains("failed tests/")
}

fn classify_text_review_source(tool_name: &str, command: &str, text: &str) -> String {
    let probe = format!("{tool_name} {command} {text}").to_ascii_lowercase();
    if probe.contains("parse error") || probe.contains("syntaxerror") {
        "parse_errors".to_string()
    } else if probe.contains("undefined name")
        || probe.contains("undefined variable")
        || probe.contains("undefined identifier")
    {
        "undefined_names".to_string()
    } else if probe.contains("lint")
        || probe.contains("clippy")
        || probe.contains("eslint")
        || probe.contains("ruff")
    {
        "lint".to_string()
    } else if probe.contains("test") || probe.contains("pytest") || probe.contains("nextest") {
        "test".to_string()
    } else if probe.contains("check")
        || probe.contains("build")
        || probe.contains("tsc")
        || probe.contains("mypy")
    {
        "typecheck".to_string()
    } else {
        "verifier_output".to_string()
    }
}

fn review_failure_line(line: &str) -> Option<String> {
    let cleaned = clean_inline_text(line.to_string());
    if cleaned.is_empty() {
        return None;
    }
    let lower = cleaned.to_ascii_lowercase();
    if lower.contains("0 failed")
        || lower.contains("test result: ok")
        || lower.contains("no parse error")
        || lower.contains("no syntax error")
        || lower.contains("no undefined name")
        || lower.contains("no undefined variable")
        || lower.contains("no undefined identifier")
    {
        return None;
    }
    let matched = lower.starts_with("error:")
        || lower.contains(" error:")
        || lower.contains("error[")
        || lower.contains("syntaxerror:")
        || lower.contains("typeerror:")
        || lower.contains("referenceerror:")
        || lower.contains("nameerror:")
        || lower.contains("parseerror:")
        || lower.contains("parse error")
        || lower.contains("syntax error")
        || lower.contains("undefined name")
        || lower.contains("undefined variable")
        || lower.contains("undefined identifier")
        || lower.contains("cannot find")
        || lower.contains("not found in this scope")
        || lower.contains("test result: failed")
        || lower.starts_with("failed ")
        || lower.contains(" failed tests/")
        || lower.starts_with("--- fail:")
        || lower.contains("panicked at")
        || lower.contains("compilation failed")
        || lower.contains("build failed")
        || (lower.contains("harn-") && lower.contains(" error "));
    matched.then(|| truncate_review_summary(&cleaned))
}

fn is_strong_review_signal(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    lower.contains("parse error")
        || lower.contains("syntax error")
        || lower.contains("undefined name")
        || lower.contains("undefined variable")
        || lower.contains("undefined identifier")
        || lower.contains("not found in this scope")
        || lower.contains("test result: failed")
        || lower.contains("panicked at")
}

fn looks_like_clean_error_summary(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    lower == "no errors"
        || lower == "0 errors"
        || lower.contains("0 errors")
        || lower.contains("no errors found")
}

fn is_accept_signal(value: &str) -> bool {
    matches!(
        value,
        "accept" | "accepted" | "pass" | "passed" | "ok" | "success" | "succeeded"
    )
}

fn first_string_field(value: &JsonValue, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        value
            .get(*key)
            .and_then(JsonValue::as_str)
            .map(str::to_string)
    })
}

fn error_value_summary(value: &JsonValue) -> Option<String> {
    match value {
        JsonValue::String(text) => Some(clean_inline_text(text.clone())),
        JsonValue::Object(_) => {
            first_string_field(value, &["message", "reason", "summary", "text"])
        }
        _ => None,
    }
    .filter(|value| !value.is_empty())
}

fn truncate_review_summary(value: &str) -> String {
    let cleaned = clean_inline_text(value.to_string());
    truncate_chars(&cleaned, GROUNDED_REVIEW_SUMMARY_CHARS)
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    let mut chars = value.chars();
    let mut out = String::new();
    for _ in 0..max_chars {
        let Some(ch) = chars.next() else {
            return value.to_string();
        };
        out.push(ch);
    }
    if chars.next().is_some() {
        out.push_str("...");
    }
    out
}

fn truncate_bytes(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_string();
    }
    let mut end = max_bytes;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...", &value[..end])
}

fn strip_ansi_codes(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            for next in chars.by_ref() {
                if next.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(ch);
        }
    }
    out
}

fn dedupe_review_findings(findings: Vec<GroundedReviewFinding>) -> Vec<GroundedReviewFinding> {
    let mut seen = BTreeSet::new();
    let mut deduped = Vec::new();
    for finding in findings {
        let key = format!(
            "{}\n{}\n{}",
            finding.source, finding.provenance, finding.summary
        );
        if seen.insert(key) {
            deduped.push(finding);
        }
    }
    deduped
}

fn grounded_review_dedupe_key(event: HookEvent, findings: &[GroundedReviewFinding]) -> String {
    use sha2::Digest as _;

    let mut hasher = sha2::Sha256::new();
    hasher.update(event.as_str().as_bytes());
    for finding in findings {
        hasher.update(b"\0");
        hasher.update(finding.source.as_bytes());
        hasher.update(b"\0");
        hasher.update(finding.provenance.as_bytes());
        hasher.update(b"\0");
        hasher.update(finding.summary.as_bytes());
    }
    let digest = hex::encode(hasher.finalize());
    format!("{GROUNDED_REVIEW_ID}:{}:{}", event.as_str(), &digest[..16])
}

fn format_grounded_review_body(
    ctx: &ProviderContext,
    findings: &[GroundedReviewFinding],
    truncated_count: usize,
) -> String {
    let mut body = format!(
        "Grounded adversarial review: {} concrete verifier/runtime signal(s) were observed at {}.",
        findings.len(),
        ctx.event.as_str(),
    );
    body.push_str(
        " Treat these as advisory evidence; inspect the cited source, fix the root cause, and rerun the relevant verifier.",
    );
    body.push_str(" Do not apply critique-only rewrites without a concrete failing signal.");
    for finding in findings {
        body.push_str("\n- [");
        body.push_str(finding.confidence);
        body.push(':');
        body.push_str(&finding.source);
        body.push_str("] ");
        body.push_str(&finding.summary);
        body.push_str(" (provenance: ");
        body.push_str(&finding.provenance);
        body.push(')');
    }
    if truncated_count > 0 {
        body.push_str(&format!(
            "\n- [{GROUNDED_REVIEW_ID}] {truncated_count} additional signal(s) omitted; inspect the tool output for the full set."
        ));
    }
    body
}

pub fn register_vm_provider(
    id: impl Into<String>,
    subscribes_to: Vec<HookEvent>,
    evaluate: Arc<VmClosure>,
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
    vm_ctx: Option<&crate::vm::AsyncBuiltinCtx>,
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
        match evaluate_vm_provider(vm_ctx, &provider, &ctx).await {
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

/// Burin compass (B.9, #2521): steer the agent loop toward the
/// AST-precise edit primitives over freeform text edits.
///
/// The `edit_*` stdlib surface (`edit_apply_node`, `edit_rename_symbol`,
/// the structured refactors, `edit_dry_run`) is the moat tool for the
/// "measure twice, cut once" burin metaphor — but it only pays off when
/// agents reach for it first. Left to defaults, the path of least
/// resistance is a freeform `str_replace`/text patch, the brittle
/// string-collision failure mode the AST tools exist to kill. This
/// provider injects a standing reminder at session start (and on resume)
/// that inverts the default: structural is the expected tool, freeform is
/// the conscious fallback. When enabled it always fires (the steer is
/// relevant to any code-editing session) and preserves across compaction
/// so the guidance persists through a long session.
///
/// It is registered in `canonical_providers` but ships **opt-in**: it is
/// deliberately absent from the default-enabled set
/// (`canonical_provider_ids`), so a session activates it explicitly with
/// `reminders: {providers: ["compass_ast_edits"]}`. The other canonical
/// providers are conditional (they stay silent without relevant data); an
/// unconditional default-on steer would instead fire on every agent loop,
/// including sub-agent and one-shot loops that never edit code. Flipping
/// it on by default, alongside the tool-rewrite router, is tracked as
/// follow-up #2612.
struct CompassAstEditsProvider;

const COMPASS_AST_EDITS_BODY: &str = "When editing source files, prefer the AST-precise edit \
tools over freeform text edits: `edit_apply_node` / `edit_insert_at_anchor` for node-level \
changes, `edit_rename_symbol` for safe cross-file renames, and the structured refactors \
(`edit_extract_function`, `edit_change_signature`, `edit_inline`, `edit_move_decl`, …) for \
compound changes. Preview any plan with `edit_dry_run` before committing. Reach for \
`edit_safe_text_patch` only when the language has no grammar support — the structural tools \
update semantic neighbours (callers, imports) that a string replace would silently miss.";

impl ReminderProvider for CompassAstEditsProvider {
    fn id(&self) -> &'static str {
        COMPASS_AST_EDITS_ID
    }

    fn subscribes_to(&self) -> &'static [HookEvent] {
        COMPASS_AST_EDITS_EVENTS
    }

    fn evaluate(&self, ctx: &ProviderContext) -> Option<ReminderSpec> {
        let mut reminder = provider_reminder(
            COMPASS_AST_EDITS_BODY.to_string(),
            COMPASS_AST_EDITS_ID,
            ctx,
        );
        reminder.tags = vec![COMPASS_AST_EDITS_ID.to_string()];
        // One steer per session; the dedupe key suppresses a second
        // emission if both SessionStart and a later resume fire.
        reminder.dedupe_key = Some(COMPASS_AST_EDITS_ID.to_string());
        reminder.ttl_turns = None;
        reminder.preserve_on_compact = true;
        reminder.propagate = ReminderPropagate::Session;
        reminder.role_hint = ReminderRoleHint::System;
        Some(reminder)
    }
}

fn canonical_providers() -> [&'static dyn ReminderProvider; 9] {
    [
        &TokenPressureProvider,
        &IdleNudgeProvider,
        &ToolOutputTruncatedProvider,
        &PostCompactRecapProvider,
        &ResumeContinuityProvider,
        &ProjectFactsProvider,
        &WorkspaceAnchorProvider,
        &GroundedReviewProvider,
        &CompassAstEditsProvider,
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

    // Default-enabled canonical providers. Opt-in-only providers (e.g. the
    // burin compass) are intentionally excluded here and activate through an
    // explicit `reminders.providers: ["<id>"]` entry handled below.
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

/// Ids of the canonical providers that are enabled by default. The burin
/// compass (`COMPASS_AST_EDITS_ID`) is intentionally NOT listed: it is a
/// registered canonical provider (see `canonical_providers`) but ships
/// opt-in, activated per session via `reminders: {providers: [...]}`.
fn canonical_provider_ids() -> [&'static str; 8] {
    [
        TOKEN_PRESSURE_ID,
        IDLE_NUDGE_ID,
        TOOL_OUTPUT_TRUNCATED_ID,
        POST_COMPACT_RECAP_ID,
        RESUME_CONTINUITY_ID,
        PROJECT_FACTS_ID,
        WORKSPACE_ANCHOR_ID,
        GROUNDED_REVIEW_ID,
    ]
}

async fn evaluate_vm_provider(
    vm_ctx: Option<&crate::vm::AsyncBuiltinCtx>,
    provider: &VmReminderProvider,
    ctx: &ProviderContext,
) -> Result<Vec<ReminderSpec>, VmError> {
    let Some(mut vm) = vm_ctx.map(crate::vm::AsyncBuiltinCtx::child_vm) else {
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

fn provider_config_bool(ctx: &ProviderContext, provider_id: &str, keys: &[&str]) -> Option<bool> {
    let config = provider_config_json(ctx, provider_id)?;
    keys.iter().find_map(|key| json_bool(config, key))
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

fn json_path_bool(value: &JsonValue, path: &[&str]) -> Option<bool> {
    json_path(value, path).and_then(JsonValue::as_bool)
}

fn json_bool(value: &JsonValue, key: &str) -> Option<bool> {
    value.get(key).and_then(JsonValue::as_bool)
}

pub(crate) fn options_map_to_json(options: &crate::value::DictMap) -> JsonValue {
    JsonValue::Object(
        options
            .iter()
            .map(|(key, value)| (key.clone(), crate::llm::helpers::vm_value_to_json(value)))
            .collect(),
    )
}

fn provider_facts_max(config: Option<&JsonValue>) -> usize {
    let raw = config
        .and_then(|cfg| json_i64(cfg, "max_facts"))
        .or_else(|| config.and_then(|cfg| json_i64(cfg, "limit")))
        .unwrap_or(PROJECT_FACTS_DEFAULT_MAX);
    raw.clamp(0, PROJECT_FACTS_HARD_MAX) as usize
}

fn provider_facts_min_confidence(config: Option<&JsonValue>) -> f64 {
    config
        .and_then(|cfg| cfg.get("min_confidence"))
        .and_then(JsonValue::as_f64)
        .filter(|value| value.is_finite())
        .map(|value| value.clamp(0.0, 1.0))
        .unwrap_or(PROJECT_FACTS_DEFAULT_MIN_CONFIDENCE)
}

fn provider_facts_namespace(config: Option<&JsonValue>) -> String {
    if let Some(namespace) = config
        .and_then(|cfg| json_str(cfg, "namespace"))
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    {
        return namespace;
    }
    match config
        .and_then(|cfg| json_str(cfg, "scope"))
        .map(|value| value.trim().to_ascii_lowercase())
        .as_deref()
    {
        Some("workspace") => "workspace/facts".to_string(),
        Some("user") => "user/facts".to_string(),
        _ => PROJECT_FACTS_DEFAULT_NAMESPACE.to_string(),
    }
}

fn provider_facts_root(config: Option<&JsonValue>) -> Option<String> {
    config
        .and_then(|cfg| json_str(cfg, "root"))
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn provider_facts_kind_filter(config: Option<&JsonValue>) -> Vec<String> {
    let Some(raw) = config.and_then(|cfg| cfg.get("kind_filter").or_else(|| cfg.get("kinds")))
    else {
        return Vec::new();
    };
    let mut out = Vec::new();
    match raw {
        JsonValue::String(value) => {
            push_kind_filter(&mut out, value);
        }
        JsonValue::Array(items) => {
            for item in items {
                if let Some(value) = item.as_str() {
                    push_kind_filter(&mut out, value);
                }
            }
        }
        _ => {}
    }
    out
}

fn push_kind_filter(out: &mut Vec<String>, raw: &str) {
    let normalized = raw.trim().to_ascii_lowercase().replace('-', "_");
    if !normalized.is_empty() && !out.iter().any(|existing| existing == &normalized) {
        out.push(normalized);
    }
}

fn provider_facts_relevance_query(ctx: &ProviderContext, config: Option<&JsonValue>) -> String {
    if let Some(query) = config
        .and_then(|cfg| json_str(cfg, "relevance_query"))
        .or_else(|| config.and_then(|cfg| json_str(cfg, "query")))
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    {
        return query;
    }
    let mut parts: Vec<String> = Vec::new();
    if let Some(task) = json_str(&ctx.payload, "task")
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    {
        parts.push(task);
    }
    if let Some(project) = json_path_str(&ctx.payload, &["session", "project_name"])
        .or_else(|| json_path_str(&ctx.payload, &["project", "name"]))
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    {
        parts.push(project);
    }
    if parts.is_empty() {
        PROJECT_FACTS_DEFAULT_QUERY.to_string()
    } else {
        parts.join(" ")
    }
}

fn format_fact_line(value: &JsonValue, min_confidence: f64, kinds: &[String]) -> Option<String> {
    let claim = value.get("claim").and_then(JsonValue::as_str)?.trim();
    if claim.is_empty() {
        return None;
    }
    let kind = value
        .get("kind")
        .and_then(JsonValue::as_str)
        .unwrap_or("claim");
    if !kinds.is_empty() && !kinds.iter().any(|allowed| allowed == kind) {
        return None;
    }
    let confidence = value
        .get("confidence")
        .and_then(JsonValue::as_f64)
        .filter(|value| value.is_finite())
        .unwrap_or(0.0);
    if confidence < min_confidence {
        return None;
    }
    let evidence_ref = value
        .get("evidence")
        .and_then(JsonValue::as_array)
        .and_then(|items| items.iter().find_map(format_evidence_ref));
    let claim = clean_inline_text(claim.to_string());
    let mut line = format!("- [{kind} · confidence {confidence:.2}] {claim}");
    if let Some(reference) = evidence_ref {
        line.push_str(" (evidence: ");
        line.push_str(&reference);
        line.push(')');
    }
    Some(line)
}

fn format_evidence_ref(item: &JsonValue) -> Option<String> {
    let kind = item.get("kind").and_then(JsonValue::as_str)?;
    let reference = item.get("ref").and_then(JsonValue::as_str)?.trim();
    if reference.is_empty() {
        return None;
    }
    Some(format!("{kind}:{reference}"))
}

fn under_budget_pressure(ctx: &ProviderContext, config: Option<&JsonValue>) -> bool {
    let ratio = config
        .and_then(|cfg| cfg.get("refresh_ratio"))
        .and_then(JsonValue::as_f64)
        .filter(|value| value.is_finite() && (0.0..=1.0).contains(value))
        .unwrap_or(PROJECT_FACTS_REFRESH_RATIO);
    let Some(tokens_used) = json_i64(&ctx.payload, "tokens_used") else {
        return false;
    };
    let Some(window) = token_pressure_context_window(ctx) else {
        return false;
    };
    if window <= 0 {
        return false;
    }
    tokens_used as f64 / window as f64 >= ratio
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ctx(event: HookEvent, payload: JsonValue, options: JsonValue) -> ProviderContext {
        ProviderContext {
            event,
            session_id: "session-1".to_string(),
            payload,
            options,
        }
    }

    #[test]
    fn compass_steers_to_ast_edits_and_survives_compaction() {
        let spec = CompassAstEditsProvider
            .evaluate(&ctx(HookEvent::SessionStart, json!({}), JsonValue::Null))
            .expect("compass fires on session start");
        // The reminder's `id` is an auto-generated per-instance UUID; the
        // provider identity is carried by `dedupe_key` / `tags` (matching
        // the other canonical providers, e.g. the workspace anchor test).
        assert_eq!(spec.dedupe_key.as_deref(), Some(COMPASS_AST_EDITS_ID));
        assert_eq!(spec.tags, vec![COMPASS_AST_EDITS_ID.to_string()]);
        assert!(spec.body.contains("edit_apply_node"));
        assert!(spec.body.contains("edit_rename_symbol"));
        assert!(spec.body.contains("edit_dry_run"));
        // The steer must outlive a context compaction, otherwise a long
        // session silently reverts to freeform edits after the first squash.
        assert!(spec.preserve_on_compact);
    }

    #[test]
    fn compass_subscribes_to_session_lifecycle_only() {
        let events = CompassAstEditsProvider.subscribes_to();
        assert!(events.contains(&HookEvent::SessionStart));
        assert!(events.contains(&HookEvent::WorkerResumed));
        // Not per-tool-call — that would spam the transcript on every edit.
        assert!(!events.contains(&HookEvent::PostToolUse));
    }

    #[test]
    fn project_facts_max_clamps_to_hard_cap() {
        assert_eq!(provider_facts_max(Some(&json!({"max_facts": 7}))), 7);
        assert_eq!(provider_facts_max(Some(&json!({"max_facts": 1000}))), 25);
        assert_eq!(provider_facts_max(Some(&json!({"max_facts": -3}))), 0);
        assert_eq!(provider_facts_max(None), 5);
    }

    #[test]
    fn project_facts_namespace_respects_scope_aliases() {
        assert_eq!(
            provider_facts_namespace(Some(&json!({"scope": "workspace"}))),
            "workspace/facts"
        );
        assert_eq!(
            provider_facts_namespace(Some(&json!({"scope": "USER"}))),
            "user/facts"
        );
        assert_eq!(
            provider_facts_namespace(Some(&json!({"namespace": "custom/facts"}))),
            "custom/facts"
        );
        assert_eq!(provider_facts_namespace(None), "project/facts");
    }

    #[test]
    fn project_facts_kind_filter_normalizes_inputs() {
        assert_eq!(
            provider_facts_kind_filter(Some(
                &json!({"kind_filter": ["Decision", "decision", "Constraint"]})
            )),
            vec!["decision", "constraint"]
        );
        assert_eq!(
            provider_facts_kind_filter(Some(&json!({"kinds": "hypothesis"}))),
            vec!["hypothesis"]
        );
        assert!(provider_facts_kind_filter(None).is_empty());
    }

    #[test]
    fn relevance_query_falls_back_to_task_then_default() {
        let ctx_task = ctx(
            HookEvent::SessionStart,
            json!({"task": "wire up the new provider"}),
            JsonValue::Null,
        );
        assert_eq!(
            provider_facts_relevance_query(&ctx_task, None),
            "wire up the new provider"
        );

        let ctx_empty = ctx(HookEvent::SessionStart, json!({}), JsonValue::Null);
        assert_eq!(
            provider_facts_relevance_query(&ctx_empty, None),
            PROJECT_FACTS_DEFAULT_QUERY
        );

        let ctx_query = ctx(HookEvent::SessionStart, json!({}), JsonValue::Null);
        assert_eq!(
            provider_facts_relevance_query(
                &ctx_query,
                Some(&json!({"relevance_query": "architecture"}))
            ),
            "architecture"
        );
    }

    #[test]
    fn format_fact_line_renders_canonical_shape() {
        let value = json!({
            "schema": "harn.fact.v1",
            "kind": "decision",
            "claim": "Wire the memory recall sync helper.",
            "confidence": 0.83,
            "evidence": [{"kind": "file_range", "ref": "memory.rs:441"}],
        });
        let line = format_fact_line(&value, 0.5, &[]).expect("line renders");
        assert_eq!(
            line,
            "- [decision · confidence 0.83] Wire the memory recall sync helper. \
             (evidence: file_range:memory.rs:441)"
        );
    }

    #[test]
    fn format_fact_line_filters_low_confidence_and_kind_mismatch() {
        let value = json!({
            "kind": "hypothesis",
            "claim": "Speculative.",
            "confidence": 0.3,
        });
        assert!(format_fact_line(&value, 0.5, &[]).is_none());
        let value_ok = json!({
            "kind": "hypothesis",
            "claim": "Speculative.",
            "confidence": 0.6,
        });
        assert!(format_fact_line(&value_ok, 0.5, &["decision".to_string()]).is_none());
        assert!(format_fact_line(&value_ok, 0.5, &["hypothesis".to_string()]).is_some());
    }

    #[test]
    fn under_budget_pressure_threshold_gate() {
        let payload = json!({"tokens_used": 75, "context_window": 100});
        let high = ctx(HookEvent::OnBudgetThreshold, payload, JsonValue::Null);
        assert!(under_budget_pressure(&high, None));

        let payload_low = json!({"tokens_used": 50, "context_window": 100});
        let low = ctx(HookEvent::OnBudgetThreshold, payload_low, JsonValue::Null);
        assert!(!under_budget_pressure(&low, None));

        let payload_missing = json!({"tokens_used": 50});
        let missing = ctx(
            HookEvent::OnBudgetThreshold,
            payload_missing,
            JsonValue::Null,
        );
        assert!(!under_budget_pressure(&missing, None));

        let custom_ratio = json!({"refresh_ratio": 0.9});
        let payload_mid = json!({"tokens_used": 80, "context_window": 100});
        let mid = ctx(HookEvent::OnBudgetThreshold, payload_mid, JsonValue::Null);
        assert!(!under_budget_pressure(&mid, Some(&custom_ratio)));
    }

    #[test]
    fn workspace_anchor_provider_formats_primary_and_mounted_roots() {
        crate::agent_sessions::reset_session_store();
        let session_id =
            crate::agent_sessions::open_or_create(Some("workspace-anchor-reminder".into()));
        crate::agent_sessions::set_workspace_anchor(
            &session_id,
            Some(crate::workspace_anchor::WorkspaceAnchor {
                primary: std::path::PathBuf::from("/workspace/main"),
                additional_roots: vec![crate::workspace_anchor::MountedRoot {
                    path: std::path::PathBuf::from("/workspace/lib"),
                    mount_mode: crate::workspace_anchor::MountMode::Extend,
                    mounted_at: "2026-05-24T00:00:01Z".to_string(),
                }],
                anchored_at: "2026-05-24T00:00:00Z".to_string(),
            }),
        )
        .expect("set anchor");
        let ctx = ProviderContext {
            event: HookEvent::SessionStart,
            session_id,
            payload: json!({}),
            options: JsonValue::Null,
        };

        let reminder = WorkspaceAnchorProvider
            .evaluate(&ctx)
            .expect("workspace reminder");

        assert_eq!(reminder.dedupe_key.as_deref(), Some("workspace_anchor"));
        assert_eq!(reminder.ttl_turns, Some(1));
        assert!(reminder.body.contains("<workspace-anchor>"));
        assert!(reminder.body.contains("primary: /workspace/main"));
        assert!(reminder
            .body
            .contains("/workspace/lib (mount_mode: extend, mounted_at: 2026-05-24T00:00:01Z)"));
    }

    #[test]
    fn grounded_review_provider_fires_on_verified_tool_failure() {
        let payload = json!({
            "tool_name": "exec_command",
            "tool": {"name": "exec_command", "args": {"cmd": "cargo check -p demo"}},
            "result": {
                "text": "error[E0425]: cannot find value `missing` in this scope\n --> src/lib.rs:2:5\n"
            },
            "iteration": 2,
        });
        let reminder = GroundedReviewProvider
            .evaluate(&ctx(HookEvent::PostToolUse, payload, JsonValue::Null))
            .expect("verified compiler output should fire");

        assert_eq!(reminder.tags[0], GROUNDED_REVIEW_ID);
        assert_eq!(reminder.ttl_turns, Some(2));
        assert_eq!(reminder.propagate, ReminderPropagate::None);
        assert_eq!(reminder.role_hint, ReminderRoleHint::Developer);
        assert!(reminder
            .dedupe_key
            .as_deref()
            .is_some_and(|key| key.starts_with("grounded_review:PostToolUse:")));
        assert!(reminder.body.contains("verified:typecheck"));
        assert!(reminder.body.contains("cannot find value `missing`"));
        assert!(reminder.body.contains("command=cargo check -p demo"));
    }

    #[test]
    fn grounded_review_provider_surfaces_verifier_and_undefined_name_signals() {
        let payload = json!({
            "verifier_signals": [
                {
                    "name": "lint gate",
                    "kind": "lint",
                    "signal": "refine",
                    "reason": "lint: forbidden pattern matched: unwrap\\(\\)",
                },
                {"name": "typecheck", "kind": "typecheck", "signal": "accept"},
            ],
            "result": {
                "diagnostics": [
                    {
                        "message": "undefined name `missing_symbol`",
                        "name": "missing_symbol",
                        "line": 7,
                    },
                ],
            },
        });
        let reminder = GroundedReviewProvider
            .evaluate(&ctx(HookEvent::PostAgentTurn, payload, JsonValue::Null))
            .expect("verifier and undefined-name findings should fire");

        assert!(reminder.body.contains("verified:verifier:lint"));
        assert!(reminder.body.contains("forbidden pattern matched"));
        assert!(reminder.body.contains("verified:undefined_names"));
        assert!(reminder.body.contains("line=7"));
        assert!(
            !reminder.body.contains("typecheck verifier returned accept"),
            "accepted verifier signals should not become reminders"
        );
    }

    #[test]
    fn grounded_review_gold_set_stays_precision_first() {
        #[derive(Clone, Copy, Eq, PartialEq)]
        enum Label {
            RealDefect,
            StyleNit,
            FalseAlarm,
        }

        let cases = [
            (
                Label::RealDefect,
                json!({
                    "tool_name": "exec_command",
                    "tool": {"name": "exec_command", "args": {"cmd": "cargo test -p demo"}},
                    "result": {"text": "test result: FAILED. 0 passed; 1 failed\n"},
                }),
            ),
            (
                Label::RealDefect,
                json!({
                    "result": {
                        "parse_errors": [
                            {"message": "syntax error: expected expression", "line": 3},
                        ],
                    },
                }),
            ),
            (
                Label::StyleNit,
                json!({
                    "tool_name": "exec_command",
                    "tool": {"name": "exec_command", "args": {"cmd": "cargo check -p demo"}},
                    "result": {"text": "warning: unused variable: `scratch`\n"},
                }),
            ),
            (
                Label::FalseAlarm,
                json!({
                    "tool_name": "exec_command",
                    "tool": {"name": "exec_command", "args": {"cmd": "cargo test -p demo"}},
                    "result": {"text": "test result: ok. 12 passed; 0 failed\n"},
                }),
            ),
            (
                Label::FalseAlarm,
                json!({
                    "tool_name": "exec_command",
                    "tool": {"name": "exec_command", "args": {"cmd": "harn check demo.harn"}},
                    "result": {"text": "demo.harn: ok, no parse error\n"},
                }),
            ),
        ];

        let mut true_positives = 0;
        let mut false_positives = 0;
        let mut non_defect_cases = 0;
        for (label, payload) in cases {
            let fired = GroundedReviewProvider
                .evaluate(&ctx(HookEvent::PostToolUse, payload, JsonValue::Null))
                .is_some();
            match (label, fired) {
                (Label::RealDefect, true) => true_positives += 1,
                (Label::RealDefect, false) => panic!("missed real-defect gold case"),
                (Label::StyleNit | Label::FalseAlarm, true) => false_positives += 1,
                (Label::StyleNit | Label::FalseAlarm, false) => non_defect_cases += 1,
            }
        }

        let precision = true_positives as f64 / (true_positives + false_positives) as f64;
        let regression_rate = false_positives as f64 / (false_positives + non_defect_cases) as f64;
        assert_eq!(precision, 1.0);
        assert_eq!(regression_rate, 0.0);
    }
}
