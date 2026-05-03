//! Agent loop configuration, builtin registration, and result building
//! extracted from `agent.rs` for maintainability.

use std::rc::Rc;
use std::sync::Arc;

use crate::agent_events::AgentEventSink;
use crate::stdlib::harn_entry::{register_harn_async_entrypoints, HarnAsyncEntrypoint};
use crate::value::{VmError, VmValue};
use crate::vm::Vm;

use super::agent::run_agent_loop_internal;
use super::agent_observe::{
    observed_llm_call, LlmRetryConfig, DEFAULT_LLM_CALL_BACKOFF_MS, DEFAULT_LLM_CALL_RETRIES,
};
use super::daemon::{parse_daemon_loop_config, DaemonLoopConfig};
use super::helpers::{
    extract_llm_options, opt_bool, opt_int, opt_str, transcript_event, transcript_to_vm_with_events,
};
use super::tools::build_assistant_response_message;

pub(crate) const DEFAULT_AGENT_LOOP_LLM_RETRIES: usize = 2;
pub(crate) const DEFAULT_MAX_VERIFY_ATTEMPTS: usize = 20;
const DEFAULT_AGENT_LOOP_MAX_ITERATIONS: i64 = 50;
const DEFAULT_AGENT_LOOP_MAX_NUDGES: i64 = 8;
const DEFAULT_AGENT_LOOP_TOOL_RETRIES: i64 = 0;
const DEFAULT_AGENT_LOOP_SCHEMA_RETRIES: i64 = 0;
const HOST_LLM_SESSION_RUN_BUILTIN: &str = "__host_llm_session_run";

const AGENT_STDLIB_ENTRYPOINTS: &[HarnAsyncEntrypoint] = &[
    HarnAsyncEntrypoint::new("agent_loop", "std/agent/loop", "agent_loop"),
    HarnAsyncEntrypoint::new("agent_turn", "std/agent/turn", "agent_turn"),
    HarnAsyncEntrypoint::new("agent_llm_turn", "std/agent/primitives", "agent_llm_turn"),
    HarnAsyncEntrypoint::new(
        "agent_parse_tool_calls",
        "std/agent/primitives",
        "agent_parse_tool_calls",
    ),
    HarnAsyncEntrypoint::new(
        "agent_dispatch_tool_call",
        "std/agent/primitives",
        "agent_dispatch_tool_call",
    ),
    HarnAsyncEntrypoint::new(
        "agent_dispatch_tool_batch",
        "std/agent/primitives",
        "agent_dispatch_tool_batch",
    ),
];

#[derive(Clone, Copy)]
pub(crate) struct AgentLoopProfileDefaults {
    pub max_iterations: i64,
    pub max_nudges: i64,
    pub tool_retries: i64,
    pub llm_retries: i64,
    pub schema_retries: i64,
}

impl AgentLoopProfileDefaults {
    fn for_name(name: &str) -> Option<Self> {
        match name {
            "tool_using" => Some(Self {
                max_iterations: 50,
                max_nudges: 8,
                tool_retries: 0,
                llm_retries: DEFAULT_AGENT_LOOP_LLM_RETRIES as i64,
                schema_retries: 0,
            }),
            "researcher" => Some(Self {
                max_iterations: 30,
                max_nudges: 4,
                tool_retries: 0,
                llm_retries: 2,
                schema_retries: 0,
            }),
            "verifier" => Some(Self {
                max_iterations: 5,
                max_nudges: 0,
                tool_retries: 0,
                llm_retries: 2,
                schema_retries: 3,
            }),
            "completer" => Some(Self {
                max_iterations: 1,
                max_nudges: 0,
                tool_retries: 0,
                llm_retries: 2,
                schema_retries: 0,
            }),
            _ => None,
        }
    }
}

pub(crate) fn agent_loop_profile_defaults(
    options: &Option<std::collections::BTreeMap<String, VmValue>>,
    label: &str,
) -> Result<AgentLoopProfileDefaults, crate::value::VmError> {
    let profile = opt_str(options, "profile").unwrap_or_else(|| "tool_using".to_string());
    AgentLoopProfileDefaults::for_name(&profile).ok_or_else(|| {
        crate::value::VmError::Runtime(format!(
            "{label}: profile must be one of tool_using, researcher, verifier, completer; got `{profile}`"
        ))
    })
}

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
    pub schema_retries: usize,
    pub schema_retry_nudge: super::SchemaNudge,
    pub tool_format: String,
    pub native_tool_fallback: crate::orchestration::NativeToolFallbackPolicy,
    /// Auto-compaction config.
    pub auto_compact: Option<crate::orchestration::AutoCompactConfig>,
    /// Capability policy scoped to this agent loop.
    pub policy: Option<crate::orchestration::CapabilityPolicy>,
    /// Command-runner pre/post policy scoped to this agent loop.
    pub command_policy: Option<crate::orchestration::CommandPolicy>,
    /// Dynamic per-agent permission rules, including VM predicates and escalation.
    pub permissions: Option<crate::llm::permissions::DynamicPermissionPolicy>,
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
    /// Optional total token budget for the loop. When exceeded, the
    /// loop exits gracefully with a budget-exhausted status after the
    /// current iteration finishes.
    pub token_budget: Option<i64>,
    /// Optional cost/token budget envelope. Per-call limits are enforced
    /// before each provider call; `total_budget_usd` is enforced across
    /// the loop.
    pub budget: Option<crate::llm::cost::LlmBudgetEnvelope>,
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
    /// Optional initial task ledger. When populated, the ledger is
    /// rendered into each turn's prompt and gates `<done>` until resolved.
    /// Public shorthand normalization is owned by std/agent/options.harn;
    /// this config only carries the typed ledger shape.
    pub task_ledger: crate::llm::ledger::TaskLedger,
    /// Optional Harn closure called after each tool turn. Receives a
    /// dict of turn metadata (`tool_results`, `successful_tool_names`,
    /// `iteration`, ...) and may return:
    /// - `""` / `nil`: no action
    /// - `"some string"`: inject that user message into the transcript
    /// - `true`: stop the stage immediately
    /// - `{message, stop}` dict: both (optional `message`, optional `stop`)
    pub post_turn_callback: Option<crate::value::VmValue>,
    /// Optional Harn closure that fires JUST BEFORE the loop yields
    /// control to the caller — i.e. when a natural stop is about to
    /// happen (sentinel hit, terminal tool, post_turn `{stop: true}`,
    /// max_nudges loop-stuck). The closure receives a dict shaped like
    /// `post_turn_callback`'s payload plus a `stop_reason: string`
    /// field (e.g. `"sentinel"`, `"terminal_tool"`, `"post_turn_stop"`,
    /// `"loop_stuck"`).
    ///
    /// Return values:
    /// - `nil` / `true` / `{confirm: true}` — confirm the stop, loop yields.
    /// - `false` — veto without feedback, loop continues with one more turn.
    /// - `string s` — veto + inject `s` as a `<runtime_feedback>` user note.
    /// - `{message?, inject?, confirm?}` — veto (unless `confirm: true`)
    ///   and inject typed messages and/or a feedback string. Same `inject`
    ///   shape as `post_turn_callback` (list of `{role, content}` dicts).
    ///
    /// Bounded by `max_verify_attempts` to prevent infinite veto loops.
    /// Does NOT fire on budget-exhaustion paths — those represent hard
    /// resource limits, not "is the work actually done" decisions.
    pub verify_completion: Option<crate::value::VmValue>,
    /// Built-in completion judge. When enabled, Harn runs a structured
    /// side-call at natural exit points and interprets `{pass, feedback}`:
    /// pass confirms, failure vetoes and injects feedback. This is the
    /// Harn-owned version of the common "is the agent really done?"
    /// integration hook, so hosts do not need to hand-roll side
    /// transcript mechanics.
    pub verify_completion_judge: Option<super::agent::completion_judge::CompletionJudgeConfig>,
    /// Sentinel-specific completion judge. Unlike `verify_completion_judge`,
    /// this side-call only runs when the loop is about to exit because
    /// the model emitted the configured done sentinel.
    pub done_judge: Option<super::agent::completion_judge::CompletionJudgeConfig>,
    /// Cap on how many times `verify_completion` may veto a stop in a
    /// single loop. Once exceeded, the next break is forced and the
    /// loop exits with `final_status = "verify_exhausted"`. Defaults to
    /// `3` when `verify_completion` is set; ignored otherwise.
    pub max_verify_attempts: usize,
    /// Optional per-call directory for the existing Harn
    /// `llm_transcript.jsonl` sidecar. When unset, the process-level
    /// `HARN_LLM_TRANSCRIPT_DIR` environment variable is still honored.
    pub llm_transcript_dir: Option<String>,
    /// Skill registry (from `skill_registry()` / `skill { }` decls)
    /// exposed to the skill-matching phase. `None` disables skills for
    /// this loop.
    pub skill_registry: Option<VmValue>,
    /// Skill matching configuration (strategy, top_n, sticky).
    pub skill_match: super::agent::SkillMatchConfig,
    /// Working file set fed into `paths:` auto-trigger scoring.
    pub working_files: Vec<String>,
    /// Declarative MCP server configs bootstrapped for this loop.
    pub mcp_servers: Vec<serde_json::Value>,
    /// Live MCP clients created from `mcp_servers` after bootstrap.
    pub(crate) mcp_clients: std::collections::BTreeMap<String, crate::mcp::VmMcpClientHandle>,
    /// Per-agent autonomy budget. When configured, the loop checks the
    /// hourly/daily decision count at entry and short-circuits to a
    /// HITL approval request before doing any LLM work. VM-enforced —
    /// scripts cannot bypass.
    pub autonomy_budget: Option<crate::llm::autonomy_budget::AgentAutonomyBudget>,
}

/// Parse the `done_sentinel` option into the `Option<String>` shape
/// `AgentLoopConfig` carries. See the call site for the accepted
/// shapes — this helper exists so the `bool` discriminator can be
/// distinguished from the legacy `string` path (a `display()`-based
/// parse would wrongly coerce `false` to `"false"`).
///
/// Returns:
/// - `Ok(None)` — option missing or `nil`; state.rs picks the
///   tool_format-aware default.
/// - `Ok(Some("##DONE##"))` — `true` (explicit default).
/// - `Ok(Some(""))` — `false` (explicit disable). Detection sites
///   guard with `!is_empty()`, so empty == disabled.
/// - `Ok(Some(s))` — any other string (custom sentinel).
pub(crate) fn parse_done_sentinel_option(
    options: &Option<std::collections::BTreeMap<String, VmValue>>,
) -> Result<Option<String>, crate::value::VmError> {
    let raw = match options.as_ref().and_then(|o| o.get("done_sentinel")) {
        Some(value) => value,
        None => return Ok(None),
    };
    match raw {
        VmValue::Nil => Ok(None),
        VmValue::Bool(true) => Ok(Some("##DONE##".to_string())),
        VmValue::Bool(false) => Ok(Some(String::new())),
        VmValue::String(s) => Ok(Some(s.to_string())),
        other => Err(crate::value::VmError::Thrown(VmValue::String(Rc::from(
            format!(
                "agent_loop: `done_sentinel` must be a string, bool, or nil; \
                 got {}",
                other.type_name()
            )
            .as_str(),
        )))),
    }
}

/// Parse a closure-shaped option (e.g. `post_turn_callback`,
/// `verify_completion`). Missing / `nil` returns `Ok(None)`. A closure
/// returns `Ok(Some(value))`. Any other shape errors out instead of
/// silently dropping — agent_loop callbacks are too easy to typo for a
/// permissive parse.
pub(crate) fn parse_closure_option(
    options: &Option<std::collections::BTreeMap<String, VmValue>>,
    name: &str,
) -> Result<Option<VmValue>, crate::value::VmError> {
    let raw = match options.as_ref().and_then(|o| o.get(name)) {
        Some(value) => value,
        None => return Ok(None),
    };
    match raw {
        VmValue::Nil => Ok(None),
        VmValue::Closure(_) => Ok(Some(raw.clone())),
        other => Err(crate::value::VmError::Thrown(VmValue::String(Rc::from(
            format!(
                "agent_loop: `{name}` must be a closure or nil; got {}",
                other.type_name()
            )
            .as_str(),
        )))),
    }
}

pub(crate) fn parse_command_policy_from_options(
    options: &Option<std::collections::BTreeMap<String, VmValue>>,
    label: &str,
) -> Result<Option<crate::orchestration::CommandPolicy>, crate::value::VmError> {
    let direct = options.as_ref().and_then(|o| o.get("command_policy"));
    let nested = options
        .as_ref()
        .and_then(|o| o.get("policy"))
        .and_then(|value| value.as_dict())
        .and_then(|policy| policy.get("command_policy"));
    crate::orchestration::parse_command_policy_value(direct.or(nested), label)
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
            "thinking_summary": result.thinking_summary,
            "cost_usd": crate::llm::cost::calculate_cost_for_provider(
                &result.provider,
                &result.model,
                result.input_tokens,
                result.output_tokens,
            ),
            "route_policy": opts.route_policy.as_label(),
            "routing_decision": opts.routing_decision.as_ref(),
            "structural_experiment": opts.applied_structural_experiment.as_ref(),
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
    if let Some(summary) = result.thinking_summary.clone() {
        if !summary.is_empty() {
            events.push(transcript_event(
                "thinking_summary",
                "assistant",
                "private",
                &summary,
                None,
            ));
        }
    }
    serde_json::json!({
        "status": "done",
        "text": result.text,
        "visible_text": result.text,
        "private_reasoning": result.thinking,
        "thinking_summary": result.thinking_summary,
        "llm": {
            "iterations": 1,
            "duration_ms": 0,
            "input_tokens": result.input_tokens,
            "output_tokens": result.output_tokens,
        },
        "tools": {
            "calls": [],
            "successful": [],
            "rejected": [],
            "mode": "",
        },
        "transcript": super::helpers::vm_value_to_json(&transcript_to_vm_with_events(
            None,
            opts.transcript_summary,
            None,
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
            "thinking_summary": result.thinking_summary,
            "structural_experiment": opts.applied_structural_experiment.as_ref(),
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
    if let Some(summary) = result.thinking_summary.clone() {
        if !summary.is_empty() {
            extra_events.push(transcript_event(
                "thinking_summary",
                "assistant",
                "private",
                &summary,
                None,
            ));
        }
    }
    let transcript = transcript_to_vm_with_events(
        None,
        opts.transcript_summary.clone(),
        None,
        &transcript_messages,
        extra_events,
        Vec::new(),
        Some("active"),
    );

    if expects_structured_output(opts) {
        let parsed = structured_output_candidates(result, opts.tools.as_ref())
            .into_iter()
            .find_map(|candidate| {
                let json_str = extract_json(&candidate);
                serde_json::from_str::<serde_json::Value>(&json_str)
                    .ok()
                    .map(|jv| json_to_vm_value(&jv))
            });
        return vm_build_llm_result(result, parsed, Some(transcript), opts.tools.as_ref());
    }

    vm_build_llm_result(result, None, Some(transcript), opts.tools.as_ref())
}

fn structured_output_candidates(
    result: &super::api::LlmResult,
    tools: Option<&crate::value::VmValue>,
) -> Vec<String> {
    let mut candidates = Vec::new();
    push_structured_output_candidate(&mut candidates, result.text.trim().to_string());

    let public_blocks = result
        .blocks
        .iter()
        .filter(|block| {
            block.get("type").and_then(|value| value.as_str()) == Some("output_text")
                && block.get("visibility").and_then(|value| value.as_str()) != Some("private")
        })
        .filter_map(|block| block.get("text").and_then(|value| value.as_str()))
        .collect::<String>();
    push_structured_output_candidate(&mut candidates, public_blocks.trim().to_string());

    for call in &result.tool_calls {
        if let Some(arguments) = call.get("arguments") {
            if let Ok(serialized) = serde_json::to_string(arguments) {
                push_structured_output_candidate(&mut candidates, serialized);
            }
        }
    }

    let derived = candidates.clone();
    for candidate in derived {
        let parsed = crate::llm::tools::parse_text_tool_calls_with_tools(&candidate, tools);
        if !parsed.prose.is_empty() {
            push_structured_output_candidate(&mut candidates, parsed.prose.trim().to_string());
        }
    }

    candidates
}

fn push_structured_output_candidate(candidates: &mut Vec<String>, candidate: String) {
    if candidate.is_empty() || candidates.iter().any(|existing| existing == &candidate) {
        return;
    }
    candidates.push(candidate);
}

async fn run_llm_turn_loop_driver(args: Vec<VmValue>) -> Result<VmValue, VmError> {
    let options = args.get(2).and_then(|a| a.as_dict()).cloned();
    // Public agent_loop policy defaults are composed by std/agent/options.harn.
    // The hidden host primitive keeps only defensive fallbacks for direct callers.
    let max_iterations =
        opt_int(&options, "max_iterations").unwrap_or(DEFAULT_AGENT_LOOP_MAX_ITERATIONS) as usize;
    let persistent = opt_bool(&options, "persistent");
    let max_nudges =
        opt_int(&options, "max_nudges").unwrap_or(DEFAULT_AGENT_LOOP_MAX_NUDGES) as usize;
    let custom_nudge = opt_str(&options, "nudge");
    let tool_retries =
        opt_int(&options, "tool_retries").unwrap_or(DEFAULT_AGENT_LOOP_TOOL_RETRIES) as usize;
    let schema_retries =
        opt_int(&options, "schema_retries").unwrap_or(DEFAULT_AGENT_LOOP_SCHEMA_RETRIES) as usize;
    let tool_backoff_ms = opt_int(&options, "tool_backoff_ms").unwrap_or(1000) as u64;
    let tool_format = opt_str(&options, "tool_format").unwrap_or_else(|| {
        let opts = extract_llm_options(&args).ok();
        let model = opts.as_ref().map(|o| o.model.as_str()).unwrap_or("");
        let provider = opts.as_ref().map(|o| o.provider.as_str()).unwrap_or("");
        crate::llm_config::default_tool_format(model, provider)
    });
    let native_tool_fallback = opt_str(&options, "native_tool_fallback")
        .map(|value| {
            crate::orchestration::NativeToolFallbackPolicy::parse(&value).ok_or_else(|| {
                crate::value::VmError::Runtime(format!(
                    "agent_loop: native_tool_fallback must be one of allow, allow_once, reject; got `{value}`"
                ))
            })
        })
        .transpose()?
        .unwrap_or_default();
    let done_sentinel = parse_done_sentinel_option(&options)?;
    let break_unless_phase = opt_str(&options, "break_unless_phase");
    let session_id = opt_str(&options, "session_id")
        .or_else(crate::agent_sessions::current_session_id)
        .unwrap_or_else(|| format!("agent_session_{}", uuid::Uuid::now_v7()));
    let daemon = opt_bool(&options, "daemon");
    let auto_compact = crate::llm::resolve_agent_loop_auto_compact(&args, &options).await?;
    let policy = options.as_ref().and_then(|o| o.get("policy")).map(|v| {
        let json = crate::llm::helpers::vm_value_to_json(v);
        serde_json::from_value::<crate::orchestration::CapabilityPolicy>(json).unwrap_or_default()
    });
    let command_policy = parse_command_policy_from_options(&options, "agent_loop")?;
    let approval_policy = options
        .as_ref()
        .and_then(|o| o.get("approval_policy"))
        .map(|v| {
            let json = crate::llm::helpers::vm_value_to_json(v);
            serde_json::from_value::<crate::orchestration::ToolApprovalPolicy>(json)
                .unwrap_or_default()
        });
    let permissions = crate::llm::permissions::parse_dynamic_permission_policy(
        options.as_ref().and_then(|o| o.get("permissions")),
        "agent_loop",
    )?;
    let daemon_config = parse_daemon_loop_config(options.as_ref());
    let turn_policy = options
        .as_ref()
        .and_then(|o| o.get("turn_policy"))
        .map(|v| {
            let json = crate::llm::helpers::vm_value_to_json(v);
            serde_json::from_value::<crate::orchestration::TurnPolicy>(json).unwrap_or_default()
        });
    let (skill_registry, skill_match, working_files) = super::agent::parse_skill_config(&options);
    let mcp_servers = super::agent::parse_mcp_server_specs(&options)?;
    let autonomy_budget = crate::llm::autonomy_budget::parse_autonomy_budget(
        options.as_ref(),
        &session_id,
        "agent_loop",
    )?;
    let mut opts = extract_llm_options(&args)?;
    let budget = opts.budget.clone();
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
            schema_retries,
            schema_retry_nudge: super::parse_schema_nudge(&options),
            tool_format,
            native_tool_fallback,
            auto_compact,
            policy,
            command_policy,
            permissions,
            approval_policy,
            daemon,
            daemon_config,
            llm_retries: opt_int(&options, "llm_retries")
                .unwrap_or(DEFAULT_AGENT_LOOP_LLM_RETRIES as i64) as usize,
            llm_backoff_ms: opt_int(&options, "llm_backoff_ms").unwrap_or(2000) as u64,
            token_budget: opt_int(&options, "token_budget"),
            budget,
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
            post_turn_callback: parse_closure_option(&options, "post_turn_callback")?,
            verify_completion: parse_closure_option(&options, "verify_completion")?,
            verify_completion_judge: super::agent::completion_judge::parse_completion_judge_option(
                &options,
            )?,
            done_judge: super::agent::completion_judge::parse_done_judge_option(&options)?,
            max_verify_attempts: opt_int(&options, "max_verify_attempts")
                .filter(|n| *n >= 0)
                .map(|n| n as usize)
                .unwrap_or(DEFAULT_MAX_VERIFY_ATTEMPTS),
            llm_transcript_dir: opt_str(&options, "llm_transcript_dir"),
            skill_registry,
            skill_match,
            working_files,
            mcp_servers,
            mcp_clients: Default::default(),
            autonomy_budget,
        },
    )
    .await?;
    Ok(crate::stdlib::json_to_vm_value(&result))
}

fn register_llm_turn_loop_driver(vm: &mut Vm, bridge: Option<Rc<crate::bridge::HostBridge>>) {
    if let Some(b) = bridge.as_ref() {
        super::agent::install_current_host_bridge(b.clone());
    }
    vm.register_async_builtin(HOST_LLM_SESSION_RUN_BUILTIN, move |args| {
        let captured_bridge = bridge.clone();
        Box::pin(async move {
            std::mem::drop(captured_bridge);
            run_llm_turn_loop_driver(args).await
        })
    });
}

pub(crate) fn register_agent_loop(vm: &mut Vm) {
    register_llm_turn_loop_driver(vm, None);
    register_harn_async_entrypoints(vm, AGENT_STDLIB_ENTRYPOINTS);
}

pub fn register_agent_loop_with_bridge(vm: &mut Vm, bridge: Rc<crate::bridge::HostBridge>) {
    register_llm_turn_loop_driver(vm, Some(bridge));
    register_harn_async_entrypoints(vm, AGENT_STDLIB_ENTRYPOINTS);
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
        crate::agent_sessions::append_subscriber(&session_id, callback);
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

fn parse_task_ledger_from_options(
    options: &Option<std::collections::BTreeMap<String, VmValue>>,
) -> crate::llm::ledger::TaskLedger {
    let Some(opts) = options.as_ref() else {
        return crate::llm::ledger::TaskLedger::default();
    };
    if let Some(explicit) = opts.get("task_ledger") {
        let json = crate::llm::helpers::vm_value_to_json(explicit);
        if let Ok(parsed) = serde_json::from_value::<crate::llm::ledger::TaskLedger>(json) {
            return parsed;
        }
    }
    crate::llm::ledger::TaskLedger::default()
}

/// Register a bridge-aware `llm_call` that emits call_start/call_end notifications.
pub fn register_llm_call_with_bridge(vm: &mut Vm, bridge: Rc<crate::bridge::HostBridge>) {
    let b = bridge;
    vm.register_async_builtin("llm_call", move |args| {
        let bridge = b.clone();
        async move {
            let mut opts = extract_llm_options(&args)?;
            let options = args.get(2).and_then(|a| a.as_dict()).cloned();
            let user_visible = opt_bool(&options, "user_visible");
            // Match the non-bridge `llm_call` default (see
            // `crate::llm::execute_llm_call`): fail fast unless the caller
            // opts into transient HTTP/provider retries.
            let retry_config = LlmRetryConfig {
                retries: opt_int(&options, "llm_retries")
                    .unwrap_or(DEFAULT_LLM_CALL_RETRIES as i64)
                    .max(0) as usize,
                backoff_ms: opt_int(&options, "llm_backoff_ms")
                    .unwrap_or(DEFAULT_LLM_CALL_BACKOFF_MS as i64)
                    .max(0) as u64,
            };
            let _ =
                crate::llm::structural_experiments::apply_structural_experiment(&mut opts, None)
                    .await?;

            let result = observed_llm_call(
                &opts,
                opt_str(&options, "tool_format").as_deref(),
                Some(&bridge),
                &retry_config,
                None,
                user_visible,
                true,
                // Direct `llm_call` host invocations are not part of an
                // agent loop, so the streaming candidate detector
                // (harn#692) doesn't fire here.
                None,
            )
            .await?;

            Ok(build_llm_call_result(&result, &opts))
        }
    });
}

/// Register bridge-aware `llm_call_structured` / `llm_call_structured_safe`.
/// The bridge path still runs the schema-retry loop locally so the
/// throws-on-exhausted-retries contract matches the non-bridge path;
/// the bridge receives per-attempt call_start/call_end notifications
/// identically to the plain `llm_call` bridge variant. Paired with
/// `register_llm_call_with_bridge` in the ACP setup.
pub fn register_llm_call_structured_with_bridge(
    vm: &mut Vm,
    bridge: Rc<crate::bridge::HostBridge>,
) {
    let b1 = bridge.clone();
    vm.register_async_builtin("llm_call_structured", move |args| {
        let bridge = b1.clone();
        async move {
            let rewritten = crate::llm::rewrite_structured_args(args)?;
            let opts = extract_llm_options(&rewritten)?;
            let options = rewritten.get(2).and_then(|a| a.as_dict()).cloned();
            let response = crate::llm::execute_llm_call(opts, options, Some(&bridge)).await?;
            Ok(crate::llm::extract_structured_data(response))
        }
    });
    let b2 = bridge.clone();
    vm.register_async_builtin("llm_call_structured_safe", move |args| {
        let bridge = b2.clone();
        async move {
            let rewritten = match crate::llm::rewrite_structured_args(args) {
                Ok(v) => v,
                Err(err) => return Ok(crate::llm::structured_safe_envelope_err(&err)),
            };
            let opts = match extract_llm_options(&rewritten) {
                Ok(opts) => opts,
                Err(err) => return Ok(crate::llm::structured_safe_envelope_err(&err)),
            };
            let options = rewritten.get(2).and_then(|a| a.as_dict()).cloned();
            match crate::llm::execute_llm_call(opts, options, Some(&bridge)).await {
                Ok(response) => Ok(crate::llm::structured_safe_envelope_ok(
                    crate::llm::extract_structured_data(response),
                )),
                Err(err) => Ok(crate::llm::structured_safe_envelope_err(&err)),
            }
        }
    });
    let b3 = bridge;
    vm.register_async_builtin("llm_call_structured_result", move |args| {
        let bridge = b3.clone();
        async move {
            crate::llm::structured_envelope::llm_call_structured_result_impl(args, Some(&bridge))
                .await
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn structured_output_candidates_include_tool_call_arguments() {
        let result = crate::llm::api::LlmResult {
            text: String::new(),
            tool_calls: vec![serde_json::json!({
                "id": "call_1",
                "type": "tool_call",
                "name": "json_response",
                "arguments": {"answer": "ok"},
            })],
            input_tokens: 1,
            output_tokens: 1,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            model: "claude-sonnet-4-6".to_string(),
            provider: "anthropic".to_string(),
            thinking: None,
            thinking_summary: None,
            stop_reason: None,
            blocks: Vec::new(),
        };

        let candidates = structured_output_candidates(&result, None);

        assert!(candidates
            .iter()
            .any(|candidate| candidate == r#"{"answer":"ok"}"#));
    }
}
