//! Pure helper functions used by the agent loop. Extracted from
//! `agent/mod.rs` so the orchestrator module can stay short and focused
//! on control flow. These helpers have no long-lived state; the
//! thread-local feedback queue and host-bridge slot remain in
//! `agent/mod.rs`.

use std::rc::Rc;

use crate::value::VmError;
use crate::value::VmValue;

use crate::llm::daemon::{persist_snapshot, DaemonLoopConfig, DaemonSnapshot};

pub(crate) fn loop_state_requests_phase_change(text: &str, current_phase: &str) -> bool {
    if current_phase.trim().is_empty() {
        return false;
    }

    let current_phase = current_phase.trim();
    let mut last_next_phase: Option<&str> = None;
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("next_phase:") {
            let phase = rest.trim();
            if !phase.is_empty() {
                last_next_phase = Some(phase);
            }
        }
    }

    last_next_phase.is_some_and(|phase| phase != current_phase)
}

pub(crate) fn should_stop_after_successful_tools(
    tool_results: &[serde_json::Value],
    stop_tools: &[String],
) -> bool {
    has_successful_tools(tool_results, stop_tools)
}

pub(crate) fn has_successful_tools(
    tool_results: &[serde_json::Value],
    tool_names: &[String],
) -> bool {
    tool_results
        .iter()
        .filter(|result| result["status"].as_str() == Some("ok"))
        .filter_map(|result| result["tool_name"].as_str())
        .any(|tool_name| tool_names.iter().any(|wanted| wanted == tool_name))
}

pub(crate) fn prose_char_len(text: &str) -> usize {
    text.trim().chars().count()
}

pub(crate) fn prose_exceeds_budget(
    prose: &str,
    turn_policy: Option<&crate::orchestration::TurnPolicy>,
) -> bool {
    let Some(limit) = turn_policy.and_then(|policy| policy.max_prose_chars) else {
        return false;
    };
    prose_char_len(prose) > limit
}

pub(crate) fn trim_prose_for_history(
    prose: &str,
    turn_policy: Option<&crate::orchestration::TurnPolicy>,
) -> String {
    let trimmed = prose.trim();
    let Some(limit) = turn_policy.and_then(|policy| policy.max_prose_chars) else {
        return trimmed.to_string();
    };
    let chars: Vec<char> = trimmed.chars().collect();
    if chars.len() <= limit {
        return trimmed.to_string();
    }
    let kept: String = chars.into_iter().take(limit).collect();
    format!("{kept}\n\n<assistant prose truncated by turn policy; keep prose brief and act>")
}

/// Hard cap on a single assistant turn replayed in history, measured in
/// characters. Large enough that typical edit heredocs, long reasoning, and
/// bundled tool calls all fit verbatim. Only truly pathological runaway
/// responses exceed this. A LOW cap would thrash provider prompt caches as
/// different turns land on different sides of the threshold; keep it high
/// so the cap is effectively dormant under normal operation.
pub(crate) const ASSISTANT_HISTORY_HARD_CAP_CHARS: usize = 131_072;

/// Content to replay for an assistant turn in conversation history.
///
/// Under the tagged response protocol, `canonical` is the parser's
/// well-formed reconstruction of the turn (tool calls wrapped in
/// `<tool_call>`, prose in `<assistant_prose>`, etc). Replaying the
/// canonical form rather than the raw provider bytes closes the
/// self-poison loop where a turn with leading raw code becomes "what
/// the agent said" on the next turn, teaching the model to repeat the
/// bad shape.
///
/// When canonical is missing (native-format path, no tools, or the
/// response contained nothing parseable), fall back to the raw text.
/// When tool parsing failed, replay a compact placeholder instead —
/// otherwise the next iteration sees its own broken syntax and mutates
/// it further.
///
/// Truncation is deterministic and bounded only by
/// `ASSISTANT_HISTORY_HARD_CAP_CHARS`, keeping prompt-cache prefixes
/// stable across turns.
pub(crate) fn assistant_history_text(
    canonical: Option<&str>,
    raw_text: &str,
    tool_parse_errors: usize,
    tool_calls: &[serde_json::Value],
) -> String {
    if tool_parse_errors > 0 {
        let names: Vec<&str> = tool_calls
            .iter()
            .filter_map(|c| c.get("name").and_then(|n| n.as_str()))
            .collect();
        return format!(
            "<assistant turn partially elided: {} tool call(s) executed successfully ({}), \
             {} malformed tool call(s) rejected. See tool results and parse errors that follow.>",
            tool_calls.len(),
            names.join(", "),
            tool_parse_errors,
        );
    }
    let source = canonical
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| raw_text.trim());
    let chars: Vec<char> = source.chars().collect();
    let total = chars.len();
    if total <= ASSISTANT_HISTORY_HARD_CAP_CHARS {
        return source.to_string();
    }
    let kept: String = chars
        .into_iter()
        .take(ASSISTANT_HISTORY_HARD_CAP_CHARS)
        .collect();
    format!(
        "{kept}\n\n<assistant turn truncated: raw length {total} chars exceeded history cap ({ASSISTANT_HISTORY_HARD_CAP_CHARS})>"
    )
}

pub(crate) fn action_turn_nudge(
    tool_format: &str,
    has_tools: bool,
    turn_policy: Option<&crate::orchestration::TurnPolicy>,
    prose_too_long: bool,
) -> Option<String> {
    let policy = turn_policy?;
    if !policy.require_action_or_yield {
        return None;
    }
    let prose_clause = if let Some(limit) = policy.max_prose_chars {
        format!("Keep prose to at most {limit} visible characters, then")
    } else {
        "Keep prose brief, then".to_string()
    };
    let emphasis = if prose_too_long {
        " Your last response spent too much budget on prose."
    } else {
        ""
    };
    let completion_clause = if has_tools && policy.allow_done_sentinel && tool_format == "native" {
        "either make concrete progress with the provider tool channel, switch phase, or include `##DONE##` exactly once if the task is genuinely complete."
    } else if has_tools && policy.allow_done_sentinel {
        "either make concrete progress with a well-formed <tool_call> block, switch phase, or emit <user_response>final answer</user_response> plus a <done> block if the task is genuinely complete."
    } else if has_tools {
        "either make concrete progress with a well-formed <tool_call> block or switch phase if the workflow allows it."
    } else if policy.allow_done_sentinel {
        "either make concrete progress in your reply, switch phase, or include `##DONE##` exactly once if the task is genuinely complete."
    } else {
        "either make concrete progress in your reply or switch phase if the workflow allows it."
    };
    let mode_clause = if has_tools && tool_format == "native" {
        " Use the provider tool channel only; handwritten tool-call text is invalid in this transcript."
    } else {
        ""
    };
    Some(format!(
        "{prose_clause} {completion_clause}{emphasis}{mode_clause}"
    ))
}

pub(crate) async fn inject_queued_user_messages(
    bridge: Option<&Rc<crate::bridge::HostBridge>>,
    messages: &mut Vec<serde_json::Value>,
    checkpoint: crate::bridge::DeliveryCheckpoint,
) -> Result<Vec<crate::bridge::QueuedUserMessage>, VmError> {
    let Some(bridge) = bridge else {
        return Ok(Vec::new());
    };
    let queued = bridge.take_queued_user_messages_for(checkpoint).await;
    for message in &queued {
        messages.push(serde_json::json!({
            "role": "user",
            "content": message.content.clone(),
        }));
    }
    Ok(queued)
}

pub(crate) fn append_message_to_contexts(
    visible_messages: &mut Vec<serde_json::Value>,
    recorded_messages: &mut Vec<serde_json::Value>,
    message: serde_json::Value,
) {
    crate::llm::agent_observe::emit_message_event(&message);
    visible_messages.push(message.clone());
    recorded_messages.push(message);
}

pub(crate) fn append_host_messages_to_recorded(
    recorded_messages: &mut Vec<serde_json::Value>,
    queued_messages: &[crate::bridge::QueuedUserMessage],
) {
    for message in queued_messages {
        recorded_messages.push(serde_json::json!({
            "role": "user",
            "content": message.content.clone(),
        }));
    }
}

pub(crate) fn runtime_feedback_message(
    kind: &str,
    content: impl Into<String>,
) -> serde_json::Value {
    let content = content.into();
    serde_json::json!({
        "role": "user",
        "content": format!(
            "<runtime_feedback kind=\"{}\">\n{}\n</runtime_feedback>",
            escape_runtime_feedback_kind(kind),
            content.trim(),
        ),
    })
}

pub(crate) fn escape_runtime_feedback_kind(kind: &str) -> String {
    kind.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

pub(crate) fn build_agent_system_prompt(
    base_system: Option<&str>,
    tool_prompt: Option<&str>,
    persistent_prompt: Option<&str>,
) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(base) = base_system {
        let trimmed = base.trim();
        if !trimmed.is_empty() {
            parts.push(trimmed.to_string());
        }
    }
    if let Some(tool_prompt) = tool_prompt {
        let trimmed = tool_prompt.trim();
        if !trimmed.is_empty() {
            parts.push(trimmed.to_string());
        }
    }
    if let Some(persistent_prompt) = persistent_prompt {
        let trimmed = persistent_prompt.trim();
        if !trimmed.is_empty() {
            parts.push(trimmed.to_string());
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n\n"))
    }
}

pub(crate) async fn maybe_auto_compact_agent_messages(
    opts: &crate::llm::api::LlmCallOptions,
    auto_compact: &Option<crate::orchestration::AutoCompactConfig>,
    visible_messages: &mut Vec<serde_json::Value>,
    transcript_summary: &mut Option<String>,
) -> Result<(), VmError> {
    if let Some(ac) = auto_compact {
        let approx_tokens = crate::orchestration::estimate_message_tokens(visible_messages);
        if approx_tokens >= ac.token_threshold {
            let mut compact_opts = opts.clone();
            compact_opts.messages = visible_messages.clone();
            let original_message_count = visible_messages.len();
            if let Some(summary) = crate::orchestration::auto_compact_messages(
                visible_messages,
                ac,
                Some(&compact_opts),
            )
            .await?
            {
                let estimated_tokens_after =
                    crate::orchestration::estimate_message_tokens(visible_messages);
                let archived_messages = original_message_count
                    .saturating_sub(visible_messages.len())
                    .saturating_add(1);
                if let Some(session_id) = super::current_agent_session_id() {
                    super::emit_agent_event(
                        &crate::agent_events::AgentEvent::TranscriptCompacted {
                            session_id,
                            mode: "auto".to_string(),
                            strategy: ac.policy_strategy.clone(),
                            archived_messages,
                            estimated_tokens_before: approx_tokens,
                            estimated_tokens_after,
                            snapshot_asset_id: None,
                        },
                    )
                    .await;
                }
                let merged = match transcript_summary.take() {
                    Some(existing) if !existing.is_empty() => {
                        format!("{existing}\n\n{summary}")
                    }
                    _ => summary,
                };
                *transcript_summary = Some(merged);
            }
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn daemon_snapshot_from_state(
    daemon_state: &str,
    visible_messages: &[serde_json::Value],
    recorded_messages: &[serde_json::Value],
    transcript_summary: &Option<String>,
    transcript_events: &[VmValue],
    total_text: &str,
    last_iteration_text: &str,
    all_tools_used: &[String],
    rejected_tools: &[String],
    deferred_user_messages: &[String],
    total_iterations: usize,
    idle_backoff_ms: u64,
    last_run_exit_code: Option<i32>,
    watch_state_map: &std::collections::BTreeMap<String, u64>,
) -> DaemonSnapshot {
    DaemonSnapshot {
        daemon_state: daemon_state.to_string(),
        visible_messages: visible_messages.to_vec(),
        recorded_messages: recorded_messages.to_vec(),
        transcript_summary: transcript_summary.clone(),
        transcript_events: transcript_events
            .iter()
            .map(crate::llm::helpers::vm_value_to_json)
            .collect(),
        total_text: total_text.to_string(),
        last_iteration_text: last_iteration_text.to_string(),
        all_tools_used: all_tools_used.to_vec(),
        rejected_tools: rejected_tools.to_vec(),
        deferred_user_messages: deferred_user_messages.to_vec(),
        total_iterations,
        idle_backoff_ms,
        last_run_exit_code,
        watch_state: watch_state_map.clone(),
        ..Default::default()
    }
}

pub(crate) fn maybe_persist_daemon_snapshot(
    config: &DaemonLoopConfig,
    snapshot: &DaemonSnapshot,
) -> Result<Option<String>, VmError> {
    let Some(path) = config.effective_persist_path() else {
        return Ok(None);
    };
    persist_snapshot(path, snapshot).map(Some)
}

/// Typed message a `post_turn_callback` can request the loop to inject
/// before the next turn. Unlike the legacy `message: string` path
/// (which always wraps as a `<runtime_feedback>` user note), `inject`
/// entries pass through verbatim so judges can write `assistant`,
/// `system`, or `tool_result` rows when the audit trail benefits from
/// the explicit role.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PostTurnInject {
    pub(crate) role: String,
    pub(crate) content: String,
}

/// Full parsed verdict from a `post_turn_callback` return value.
/// `message` and `stop` preserve the legacy contract; `injects` is the
/// new judge-pattern surface that lets a callback push typed messages
/// onto the next turn without going through the
/// `<runtime_feedback>`-wrapped `message` slot.
///
/// `injects` is intentionally `Vec` rather than `Option<Vec>` — an
/// empty vector means "no inject," which folds the no-op case into the
/// happy path.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct PostTurnVerdict {
    pub(crate) message: Option<String>,
    pub(crate) stop: bool,
    pub(crate) injects: Vec<PostTurnInject>,
}

/// Parse a `post_turn_callback` return value into the full verdict
/// (message + stop + typed inject list). Backward-compatible with the
/// historical legacy shapes (string / bool / `{message, stop}` dict):
///
/// - `nil` / empty string → empty verdict
/// - string → `{message: <s>}`
/// - `true` / `false` → `{stop: <b>}`
/// - dict with `message`, `stop`, and the new optional `inject` key —
///   `inject` accepts either a single message dict or a list of
///   message dicts, each with `{role: string, content: string}`. Roles
///   outside the canonical set (`user`, `assistant`, `system`,
///   `tool_result`) are silently dropped, matching
///   `agent_session_inject`'s contract — judges should never sneak
///   undocumented roles past the message log.
pub(crate) fn interpret_post_turn_callback_verdict(value: &VmValue) -> PostTurnVerdict {
    match value {
        VmValue::Nil => PostTurnVerdict::default(),
        VmValue::Bool(b) => PostTurnVerdict {
            stop: *b,
            ..PostTurnVerdict::default()
        },
        VmValue::String(s) => PostTurnVerdict {
            message: (!s.is_empty()).then(|| s.to_string()),
            ..PostTurnVerdict::default()
        },
        VmValue::Dict(dict) => {
            let message = dict.get("message").and_then(|v| match v {
                VmValue::String(s) if !s.is_empty() => Some(s.to_string()),
                _ => None,
            });
            let stop = dict
                .get("stop")
                .and_then(|v| match v {
                    VmValue::Bool(b) => Some(*b),
                    _ => None,
                })
                .unwrap_or(false);
            let injects = dict
                .get("inject")
                .map(parse_post_turn_injects)
                .unwrap_or_default();
            PostTurnVerdict {
                message,
                stop,
                injects,
            }
        }
        _ => PostTurnVerdict::default(),
    }
}

fn parse_post_turn_injects(value: &VmValue) -> Vec<PostTurnInject> {
    match value {
        VmValue::Dict(_) => parse_post_turn_inject_one(value).into_iter().collect(),
        VmValue::List(list) => list.iter().filter_map(parse_post_turn_inject_one).collect(),
        _ => Vec::new(),
    }
}

fn parse_post_turn_inject_one(value: &VmValue) -> Option<PostTurnInject> {
    let dict = match value {
        VmValue::Dict(d) => d,
        _ => return None,
    };
    let role = match dict.get("role")? {
        VmValue::String(s) => s.to_string(),
        _ => return None,
    };
    if !matches!(
        role.as_str(),
        "user" | "assistant" | "system" | "tool_result"
    ) {
        return None;
    }
    let content = match dict.get("content")? {
        VmValue::String(s) if !s.is_empty() => s.to_string(),
        _ => return None,
    };
    Some(PostTurnInject { role, content })
}

/// Decision returned by a `verify_completion` closure. The hook fires
/// once at every natural break path (sentinel, terminal tool,
/// post_turn `{stop: true}`, loop_stuck) and gets to either confirm
/// the stop (loop yields to caller) or veto with feedback (loop runs
/// one more turn). `Vetoed` carries the feedback the loop should push
/// onto messages before the next turn.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum VerifyDecision {
    Confirmed,
    Vetoed {
        message: Option<String>,
        injects: Vec<PostTurnInject>,
    },
}

/// Parse the value returned by a `verify_completion` closure into a
/// stop / veto decision. Accepted shapes (loose by design — judges
/// shouldn't crash the loop on a malformed return):
///
/// - `nil` / `true` / `{confirm: true}` → confirm the stop.
/// - `false` → veto without feedback.
/// - `string s` (non-empty) → veto + inject `s` as a runtime_feedback note.
/// - dict with `confirm: true` (any other keys ignored) → confirm.
/// - dict with `message?` (string) and/or `inject?` (dict | list of
///   dicts, same shape as `post_turn_callback`'s `inject` key) →
///   veto with the provided feedback.
/// - any other shape → confirm (safe default — the loop's existing
///   stop decision is honored).
pub(crate) fn interpret_verify_completion_verdict(value: &VmValue) -> VerifyDecision {
    match value {
        VmValue::Nil => VerifyDecision::Confirmed,
        VmValue::Bool(true) => VerifyDecision::Confirmed,
        VmValue::Bool(false) => VerifyDecision::Vetoed {
            message: None,
            injects: Vec::new(),
        },
        VmValue::String(s) => {
            if s.is_empty() {
                VerifyDecision::Confirmed
            } else {
                VerifyDecision::Vetoed {
                    message: Some(s.to_string()),
                    injects: Vec::new(),
                }
            }
        }
        VmValue::Dict(dict) => {
            let confirm = dict
                .get("confirm")
                .and_then(|v| match v {
                    VmValue::Bool(b) => Some(*b),
                    _ => None,
                })
                .unwrap_or(false);
            if confirm {
                return VerifyDecision::Confirmed;
            }
            let message = dict.get("message").and_then(|v| match v {
                VmValue::String(s) if !s.is_empty() => Some(s.to_string()),
                _ => None,
            });
            let injects = dict
                .get("inject")
                .map(parse_post_turn_injects)
                .unwrap_or_default();
            VerifyDecision::Vetoed { message, injects }
        }
        _ => VerifyDecision::Confirmed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn dict(entries: Vec<(&str, VmValue)>) -> VmValue {
        let mut map = BTreeMap::new();
        for (k, v) in entries {
            map.insert(k.to_string(), v);
        }
        VmValue::Dict(Rc::new(map))
    }

    fn vm_string(s: &str) -> VmValue {
        VmValue::String(Rc::from(s))
    }

    #[test]
    fn legacy_nil_and_empty_string_are_no_ops() {
        assert_eq!(
            interpret_post_turn_callback_verdict(&VmValue::Nil),
            PostTurnVerdict::default()
        );
        assert_eq!(
            interpret_post_turn_callback_verdict(&vm_string("")),
            PostTurnVerdict::default()
        );
    }

    #[test]
    fn legacy_string_becomes_message() {
        let v = interpret_post_turn_callback_verdict(&vm_string("redo: missing tests"));
        assert_eq!(v.message.as_deref(), Some("redo: missing tests"));
        assert!(!v.stop);
        assert!(v.injects.is_empty());
    }

    #[test]
    fn legacy_bool_controls_stop() {
        let stop = interpret_post_turn_callback_verdict(&VmValue::Bool(true));
        assert!(stop.stop);
        assert!(stop.message.is_none());
        let go = interpret_post_turn_callback_verdict(&VmValue::Bool(false));
        assert!(!go.stop);
    }

    #[test]
    fn legacy_dict_message_and_stop_round_trip() {
        let v = interpret_post_turn_callback_verdict(&dict(vec![
            ("message", vm_string("looks good")),
            ("stop", VmValue::Bool(true)),
        ]));
        assert_eq!(v.message.as_deref(), Some("looks good"));
        assert!(v.stop);
    }

    #[test]
    fn inject_accepts_single_dict() {
        let v = interpret_post_turn_callback_verdict(&dict(vec![(
            "inject",
            dict(vec![
                ("role", vm_string("system")),
                ("content", vm_string("you have 1 turn left")),
            ]),
        )]));
        assert_eq!(v.injects.len(), 1);
        assert_eq!(v.injects[0].role, "system");
        assert_eq!(v.injects[0].content, "you have 1 turn left");
    }

    #[test]
    fn inject_accepts_list_of_dicts_and_filters_invalid() {
        let v = interpret_post_turn_callback_verdict(&dict(vec![(
            "inject",
            VmValue::List(Rc::new(vec![
                dict(vec![
                    ("role", vm_string("user")),
                    ("content", vm_string("a")),
                ]),
                // Unknown role — drop silently per agent_session_inject contract.
                dict(vec![
                    ("role", vm_string("evil")),
                    ("content", vm_string("b")),
                ]),
                // Missing content — drop.
                dict(vec![("role", vm_string("user"))]),
                // Empty content — drop (trim happens here, not later).
                dict(vec![
                    ("role", vm_string("user")),
                    ("content", vm_string("")),
                ]),
                dict(vec![
                    ("role", vm_string("assistant")),
                    ("content", vm_string("c")),
                ]),
            ])),
        )]));
        let kept: Vec<_> = v
            .injects
            .iter()
            .map(|i| (i.role.as_str(), i.content.as_str()))
            .collect();
        assert_eq!(kept, vec![("user", "a"), ("assistant", "c")]);
    }

    #[test]
    fn inject_message_and_stop_compose() {
        let v = interpret_post_turn_callback_verdict(&dict(vec![
            (
                "inject",
                dict(vec![
                    ("role", vm_string("system")),
                    ("content", vm_string("graded: redo step 2")),
                ]),
            ),
            ("message", vm_string("nudge")),
            ("stop", VmValue::Bool(false)),
        ]));
        assert_eq!(v.injects.len(), 1);
        assert_eq!(v.message.as_deref(), Some("nudge"));
        assert!(!v.stop);
    }

    #[test]
    fn inject_with_unrecognized_top_value_is_silent_no_op() {
        let v = interpret_post_turn_callback_verdict(&dict(vec![("inject", VmValue::Int(7))]));
        assert!(v.injects.is_empty());
    }

    #[test]
    fn verify_nil_and_true_confirm_the_stop() {
        assert_eq!(
            interpret_verify_completion_verdict(&VmValue::Nil),
            VerifyDecision::Confirmed
        );
        assert_eq!(
            interpret_verify_completion_verdict(&VmValue::Bool(true)),
            VerifyDecision::Confirmed
        );
        assert_eq!(
            interpret_verify_completion_verdict(&dict(vec![("confirm", VmValue::Bool(true))])),
            VerifyDecision::Confirmed
        );
    }

    #[test]
    fn verify_false_vetoes_without_feedback() {
        let d = interpret_verify_completion_verdict(&VmValue::Bool(false));
        match d {
            VerifyDecision::Vetoed { message, injects } => {
                assert!(message.is_none());
                assert!(injects.is_empty());
            }
            _ => panic!("expected Vetoed, got {d:?}"),
        }
    }

    #[test]
    fn verify_string_vetoes_with_feedback() {
        let d = interpret_verify_completion_verdict(&vm_string("not yet — call grep first"));
        match d {
            VerifyDecision::Vetoed { message, injects } => {
                assert_eq!(message.as_deref(), Some("not yet — call grep first"));
                assert!(injects.is_empty());
            }
            _ => panic!("expected Vetoed, got {d:?}"),
        }
    }

    #[test]
    fn verify_dict_with_message_and_inject_vetoes() {
        let d = interpret_verify_completion_verdict(&dict(vec![
            ("message", vm_string("redo")),
            (
                "inject",
                dict(vec![
                    ("role", vm_string("system")),
                    ("content", vm_string("you missed step 3")),
                ]),
            ),
        ]));
        match d {
            VerifyDecision::Vetoed { message, injects } => {
                assert_eq!(message.as_deref(), Some("redo"));
                assert_eq!(injects.len(), 1);
                assert_eq!(injects[0].role, "system");
                assert_eq!(injects[0].content, "you missed step 3");
            }
            _ => panic!("expected Vetoed, got {d:?}"),
        }
    }

    #[test]
    fn verify_explicit_confirm_wins_over_message() {
        // `confirm: true` short-circuits — message/inject are ignored.
        let d = interpret_verify_completion_verdict(&dict(vec![
            ("confirm", VmValue::Bool(true)),
            ("message", vm_string("ignored")),
        ]));
        assert_eq!(d, VerifyDecision::Confirmed);
    }

    #[test]
    fn verify_unrecognized_shape_falls_back_to_confirm() {
        // Safe default: keep the loop's existing stop decision rather
        // than vetoing on a malformed return.
        let d = interpret_verify_completion_verdict(&VmValue::Int(42));
        assert_eq!(d, VerifyDecision::Confirmed);
    }
}
