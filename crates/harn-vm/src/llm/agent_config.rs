//! Agent loop configuration, builtin registration, and result building
//! extracted from `agent.rs` for maintainability.

use std::rc::Rc;
use std::sync::Arc;

use crate::agent_events::{self, AgentEventSink};
use crate::value::VmValue;
use crate::vm::Vm;

use super::agent::run_agent_loop_internal;
use super::agent_observe::{observed_llm_call, LlmRetryConfig};
use super::daemon::{parse_daemon_loop_config, DaemonLoopConfig};
use super::helpers::{
    extract_llm_options, opt_bool, opt_int, opt_str, transcript_event, transcript_to_vm_with_events,
};
use super::tools::build_assistant_response_message;

#[derive(Clone)]
pub struct AgentLoopConfig {
    pub persistent: bool,
    pub max_iterations: usize,
    pub max_nudges: usize,
    pub nudge: Option<String>,
    pub done_sentinel: Option<String>,
    pub break_unless_phase: Option<String>,
    pub tool_retries: usize,
    pub tool_backoff_ms: u64,
    pub tool_format: String,
    /// Auto-compaction config.
    pub auto_compact: Option<crate::orchestration::AutoCompactConfig>,
    /// Capability policy scoped to this agent loop.
    pub policy: Option<crate::orchestration::CapabilityPolicy>,
    /// Declarative approval policy (auto-approve / auto-deny / require host confirmation).
    pub approval_policy: Option<crate::orchestration::ToolApprovalPolicy>,
    /// Daemon mode.
    pub daemon: bool,
    /// Extended daemon lifecycle settings.
    pub daemon_config: DaemonLoopConfig,
    /// LLM call retry count.
    pub llm_retries: usize,
    /// Base backoff in milliseconds between LLM retries.
    pub llm_backoff_ms: u64,
    /// Exit only when verification passes.
    pub exit_when_verified: bool,
    /// Tool loop detection thresholds.
    pub loop_detect_warn: usize,
    pub loop_detect_block: usize,
    pub loop_detect_skip: usize,
    /// Optional few-shot examples for the tool-calling contract.
    pub tool_examples: Option<String>,
    /// Optional turn-shape constraints.
    pub turn_policy: Option<crate::orchestration::TurnPolicy>,
    /// Stop after successful use of named tools.
    pub stop_after_successful_tools: Option<Vec<String>>,
    /// Require successful use of named tools.
    pub require_successful_tools: Option<Vec<String>>,
    /// Stable identifier for this agent-loop session. Events emitted
    /// through the stream are tagged with this id; subscribers key on
    /// it to scope their observation to a specific session.
    pub session_id: String,
    /// Optional sink that receives every `AgentEvent` the turn loop
    /// produces. In addition to this direct sink, any sinks registered
    /// via `agent_subscribe(session_id, closure)` from inside the
    /// pipeline receive the same events (registry is keyed on session id).
    pub event_sink: Option<Arc<dyn AgentEventSink>>,
    /// Optional initial task ledger. When populated (typically from a
    /// prior planning stage's `tasks` array or a caller-supplied
    /// deliverables list), the ledger is rendered into each turn's
    /// prompt and gates `<done>` until resolved. See `llm/ledger.rs`.
    pub task_ledger: crate::llm::ledger::TaskLedger,
    /// Optional Harn closure called after each tool turn. Receives a
    /// dict of turn metadata (`tool_results`, `successful_tool_names`,
    /// `iteration`, ...) and may return:
    /// - `""` / `nil`: no action
    /// - `"some string"`: inject that user message into the transcript
    /// - `true`: stop the stage immediately
    /// - `{message, stop}` dict: both (optional `message`, optional `stop`)
    pub post_turn_callback: Option<crate::value::VmValue>,
}

impl std::fmt::Debug for AgentLoopConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentLoopConfig")
            .field("persistent", &self.persistent)
            .field("max_iterations", &self.max_iterations)
            .field("session_id", &self.session_id)
            .field("event_sink", &self.event_sink.as_ref().map(|_| "..."))
            .finish_non_exhaustive()
    }
}

pub(crate) fn agent_loop_result_from_llm(
    result: &super::api::LlmResult,
    opts: super::api::LlmCallOptions,
) -> serde_json::Value {
    let mut transcript_messages = opts.messages.clone();
    transcript_messages.push(build_assistant_response_message(
        &result.text,
        &result.blocks,
        &result.tool_calls,
        result.thinking.as_deref(),
        &opts.provider,
    ));
    let mut events = vec![transcript_event(
        "provider_payload",
        "assistant",
        "internal",
        "",
        Some(serde_json::json!({
            "model": result.model.clone(),
            "input_tokens": result.input_tokens,
            "output_tokens": result.output_tokens,
            "tool_calls": result.tool_calls.clone(),
        })),
    )];
    if let Some(thinking) = result.thinking.clone() {
        if !thinking.is_empty() {
            events.push(transcript_event(
                "private_reasoning",
                "assistant",
                "private",
                &thinking,
                None,
            ));
        }
    }
    serde_json::json!({
        "status": "done",
        "text": result.text,
        "visible_text": result.text,
        "private_reasoning": result.thinking,
        "iterations": 1,
        "duration_ms": 0,
        "tools_used": [],
        "transcript": super::helpers::vm_value_to_json(&transcript_to_vm_with_events(
            opts.transcript_id,
            opts.transcript_summary,
            opts.transcript_metadata,
            &transcript_messages,
            events,
            Vec::new(),
            Some("active"),
        )),
    })
}

/// Assemble the user-facing result dict for `llm_call` from a raw `LlmResult`.
pub(crate) fn build_llm_call_result(
    result: &super::api::LlmResult,
    opts: &super::api::LlmCallOptions,
) -> VmValue {
    use super::api::vm_build_llm_result;
    use super::helpers::{expects_structured_output, extract_json};
    use crate::stdlib::json_to_vm_value;

    let mut transcript_messages = opts.messages.clone();
    transcript_messages.push(build_assistant_response_message(
        &result.text,
        &result.blocks,
        &result.tool_calls,
        result.thinking.as_deref(),
        &opts.provider,
    ));
    let mut extra_events = vec![transcript_event(
        "provider_payload",
        "assistant",
        "internal",
        "",
        Some(serde_json::json!({
            "model": result.model.clone(),
            "input_tokens": result.input_tokens,
            "output_tokens": result.output_tokens,
            "tool_calls": result.tool_calls.clone(),
        })),
    )];
    if let Some(thinking) = result.thinking.clone() {
        if !thinking.is_empty() {
            extra_events.push(transcript_event(
                "private_reasoning",
                "assistant",
                "private",
                &thinking,
                None,
            ));
        }
    }
    let transcript = transcript_to_vm_with_events(
        opts.transcript_id.clone(),
        opts.transcript_summary.clone(),
        opts.transcript_metadata.clone(),
        &transcript_messages,
        extra_events,
        Vec::new(),
        Some("active"),
    );

    if expects_structured_output(opts) {
        let json_str = extract_json(&result.text);
        let parsed = serde_json::from_str::<serde_json::Value>(&json_str)
            .ok()
            .map(|jv| json_to_vm_value(&jv));
        return vm_build_llm_result(result, parsed, Some(transcript), opts.tools.as_ref());
    }

    vm_build_llm_result(result, None, Some(transcript), opts.tools.as_ref())
}

pub fn register_agent_loop_with_bridge(vm: &mut Vm, bridge: Rc<crate::bridge::HostBridge>) {
    let b = bridge;
    super::agent::install_current_host_bridge(b.clone());
    vm.register_async_builtin("agent_loop", move |args| {
        let captured_bridge = b.clone();
        async move {
            std::mem::drop(captured_bridge);
            let options = args.get(2).and_then(|a| a.as_dict()).cloned();
            let max_iterations = opt_int(&options, "max_iterations").unwrap_or(50) as usize;
            let persistent = opt_bool(&options, "persistent");
            let max_nudges = opt_int(&options, "max_nudges").unwrap_or(8) as usize;
            let custom_nudge = opt_str(&options, "nudge");
            let tool_retries = opt_int(&options, "tool_retries").unwrap_or(0) as usize;
            let tool_backoff_ms = opt_int(&options, "tool_backoff_ms").unwrap_or(1000) as u64;
            let tool_format = opt_str(&options, "tool_format").unwrap_or_else(|| {
                let opts = extract_llm_options(&args).ok();
                let model = opts.as_ref().map(|o| o.model.as_str()).unwrap_or("");
                let provider = opts.as_ref().map(|o| o.provider.as_str()).unwrap_or("");
                crate::llm_config::default_tool_format(model, provider)
            });
            let done_sentinel = opt_str(&options, "done_sentinel");
            let break_unless_phase = opt_str(&options, "break_unless_phase");
            let session_id = opt_str(&options, "session_id")
                .unwrap_or_else(|| format!("agent_session_{}", uuid::Uuid::now_v7()));
            let daemon = opt_bool(&options, "daemon");
            let auto_compact = if opt_bool(&options, "auto_compact") {
                let mut ac = crate::orchestration::AutoCompactConfig::default();
                let user_specified_threshold = opt_int(&options, "compact_threshold").is_some();
                if let Some(v) = opt_int(&options, "compact_threshold") {
                    ac.token_threshold = v as usize;
                }
                if let Some(v) = opt_int(&options, "tool_output_max_chars") {
                    ac.tool_output_max_chars = v as usize;
                }
                if let Some(v) = opt_int(&options, "compact_keep_last") {
                    ac.keep_last = v as usize;
                }
                if let Some(strategy) = opt_str(&options, "compact_strategy") {
                    ac.compact_strategy = crate::orchestration::parse_compact_strategy(&strategy)?;
                }
                if let Some(v) = opt_int(&options, "hard_limit_tokens") {
                    ac.hard_limit_tokens = Some(v as usize);
                }
                if let Some(strategy) = opt_str(&options, "hard_limit_strategy") {
                    ac.hard_limit_strategy =
                        crate::orchestration::parse_compact_strategy(&strategy)?;
                }
                if let Some(callback) = options.as_ref().and_then(|o| o.get("mask_callback")) {
                    ac.mask_callback = Some(callback.clone());
                }
                if let Some(callback) = options.as_ref().and_then(|o| o.get("compact_callback")) {
                    ac.custom_compactor = Some(callback.clone());
                    if !options
                        .as_ref()
                        .is_some_and(|o| o.contains_key("compact_strategy"))
                    {
                        ac.compact_strategy = crate::orchestration::CompactStrategy::Custom;
                    }
                }
                if let Some(callback) = options.as_ref().and_then(|o| o.get("compress_callback")) {
                    ac.compress_callback = Some(callback.clone());
                }
                {
                    let probe_opts = extract_llm_options(&args)?;
                    let user_specified_hard_limit =
                        opt_int(&options, "hard_limit_tokens").is_some();
                    crate::llm::api::adapt_auto_compact_to_provider(
                        &mut ac,
                        user_specified_threshold,
                        user_specified_hard_limit,
                        &probe_opts.provider,
                        &probe_opts.model,
                        &probe_opts.api_key,
                    )
                    .await;
                }
                Some(ac)
            } else {
                None
            };
            let policy = options.as_ref().and_then(|o| o.get("policy")).map(|v| {
                let json = crate::llm::helpers::vm_value_to_json(v);
                serde_json::from_value::<crate::orchestration::CapabilityPolicy>(json)
                    .unwrap_or_default()
            });
            let approval_policy =
                options
                    .as_ref()
                    .and_then(|o| o.get("approval_policy"))
                    .map(|v| {
                        let json = crate::llm::helpers::vm_value_to_json(v);
                        serde_json::from_value::<crate::orchestration::ToolApprovalPolicy>(json)
                            .unwrap_or_default()
                    });
            let daemon_config = parse_daemon_loop_config(options.as_ref());
            let turn_policy = options
                .as_ref()
                .and_then(|o| o.get("turn_policy"))
                .map(|v| {
                    let json = crate::llm::helpers::vm_value_to_json(v);
                    serde_json::from_value::<crate::orchestration::TurnPolicy>(json)
                        .unwrap_or_default()
                });
            let mut opts = extract_llm_options(&args)?;
            let result = run_agent_loop_internal(
                &mut opts,
                AgentLoopConfig {
                    persistent,
                    max_iterations,
                    max_nudges,
                    nudge: custom_nudge,
                    done_sentinel,
                    break_unless_phase,
                    tool_retries,
                    tool_backoff_ms,
                    tool_format,
                    auto_compact,
                    policy,
                    approval_policy,
                    daemon,
                    daemon_config,
                    llm_retries: opt_int(&options, "llm_retries").unwrap_or(4) as usize,
                    llm_backoff_ms: opt_int(&options, "llm_backoff_ms").unwrap_or(2000) as u64,
                    exit_when_verified: opt_bool(&options, "exit_when_verified"),
                    loop_detect_warn: opt_int(&options, "loop_detect_warn").unwrap_or(2) as usize,
                    loop_detect_block: opt_int(&options, "loop_detect_block").unwrap_or(3) as usize,
                    loop_detect_skip: opt_int(&options, "loop_detect_skip").unwrap_or(4) as usize,
                    tool_examples: opt_str(&options, "tool_examples"),
                    turn_policy,
                    stop_after_successful_tools: crate::llm::helpers::opt_str_list(
                        &options,
                        "stop_after_successful_tools",
                    ),
                    require_successful_tools: crate::llm::helpers::opt_str_list(
                        &options,
                        "require_successful_tools",
                    ),
                    session_id,
                    event_sink: None,
                    task_ledger: parse_task_ledger_from_options(&options),
                    post_turn_callback: options
                        .as_ref()
                        .and_then(|o| o.get("post_turn_callback"))
                        .filter(|v| matches!(v, crate::value::VmValue::Closure(_)))
                        .cloned(),
                },
            )
            .await?;
            Ok(crate::stdlib::json_to_vm_value(&result))
        }
    });
}

pub fn register_agent_subscribe(vm: &mut Vm) {
    vm.register_builtin("agent_subscribe", |args, _out| {
        let session_id = match args.first() {
            Some(VmValue::String(s)) => s.to_string(),
            _ => {
                return Err(crate::value::VmError::Runtime(
                    "agent_subscribe(session_id, callback): session_id must be a string".into(),
                ))
            }
        };
        let callback = args.get(1).cloned().ok_or_else(|| {
            crate::value::VmError::Runtime(
                "agent_subscribe(session_id, callback): callback closure required".into(),
            )
        })?;
        if !matches!(callback, VmValue::Closure(_)) {
            return Err(crate::value::VmError::Runtime(
                "agent_subscribe(session_id, callback): callback must be a closure".into(),
            ));
        }
        agent_events::register_closure_subscriber(session_id, callback);
        Ok(VmValue::Nil)
    });
}

pub fn register_agent_inject_feedback(vm: &mut Vm) {
    vm.register_builtin("agent_inject_feedback", |args, _out| {
        let session_id =
            match args.first() {
                Some(VmValue::String(s)) => s.to_string(),
                _ => return Err(crate::value::VmError::Runtime(
                    "agent_inject_feedback(session_id, kind, content): session_id must be a string"
                        .into(),
                )),
            };
        let kind = match args.get(1) {
            Some(VmValue::String(s)) => s.to_string(),
            _ => {
                return Err(crate::value::VmError::Runtime(
                    "agent_inject_feedback(session_id, kind, content): kind must be a string"
                        .into(),
                ))
            }
        };
        let content =
            match args.get(2) {
                Some(VmValue::String(s)) => s.to_string(),
                _ => return Err(crate::value::VmError::Runtime(
                    "agent_inject_feedback(session_id, kind, content): content must be a string"
                        .into(),
                )),
            };
        super::agent::push_pending_feedback(&session_id, &kind, &content);
        Ok(VmValue::Nil)
    });
}

/// Extract an initial task ledger from agent_loop options. Accepts:
///
/// - `task_ledger: { root_task, deliverables: [...], rationale, ... }` verbatim
/// - `deliverables: ["task A", "task B"]` as shorthand for seeding
/// - `root_task: "..."` standalone to record the original user ask
///
/// Unrecognised shapes fall through to an empty ledger (the loop runs
/// un-gated, which is correct for trivial one-shots).
fn parse_task_ledger_from_options(
    options: &Option<std::collections::BTreeMap<String, VmValue>>,
) -> crate::llm::ledger::TaskLedger {
    use crate::llm::ledger::{Deliverable, DeliverableStatus, TaskLedger};

    let Some(opts) = options.as_ref() else {
        return TaskLedger::default();
    };
    if let Some(explicit) = opts.get("task_ledger") {
        let json = crate::llm::helpers::vm_value_to_json(explicit);
        if let Ok(parsed) = serde_json::from_value::<TaskLedger>(json) {
            return parsed;
        }
    }
    let mut ledger = TaskLedger::default();
    if let Some(VmValue::String(s)) = opts.get("root_task") {
        ledger.root_task = s.trim().to_string();
    }
    if let Some(deliverables) = opts.get("deliverables").and_then(|v| match v {
        VmValue::List(items) => Some(items.clone()),
        _ => None,
    }) {
        for (idx, item) in deliverables.iter().enumerate() {
            let text = item.display().trim().to_string();
            if text.is_empty() {
                continue;
            }
            ledger.deliverables.push(Deliverable {
                id: format!("deliverable-{}", idx + 1),
                text,
                status: DeliverableStatus::Open,
                note: None,
            });
        }
    }
    ledger
}

/// Register a bridge-aware `llm_call` that emits call_start/call_end notifications.
pub fn register_llm_call_with_bridge(vm: &mut Vm, bridge: Rc<crate::bridge::HostBridge>) {
    let b = bridge;
    vm.register_async_builtin("llm_call", move |args| {
        let bridge = b.clone();
        async move {
            let opts = extract_llm_options(&args)?;
            let options = args.get(2).and_then(|a| a.as_dict()).cloned();
            let user_visible = opt_bool(&options, "user_visible");
            let retry_config = LlmRetryConfig {
                retries: opt_int(&options, "llm_retries").unwrap_or(0) as usize,
                backoff_ms: opt_int(&options, "llm_backoff_ms").unwrap_or(2000) as u64,
            };

            let result = observed_llm_call(
                &opts,
                opt_str(&options, "tool_format").as_deref(),
                Some(&bridge),
                &retry_config,
                None,
                user_visible,
                true,
            )
            .await?;

            Ok(build_llm_call_result(&result, &opts))
        }
    });
}
