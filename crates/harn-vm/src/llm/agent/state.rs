//! Long-lived agent-loop state. Extracted from `agent/mod.rs` so the
//! orchestrator can thread its state through phase methods instead of
//! juggling 40+ local bindings. The four RAII drop-guards that used to
//! live inline in `run_agent_loop_internal` now live here as owned
//! fields — their `Drop` impls still pop orchestration stacks and clear
//! session sinks on loop exit (success or error).

use std::rc::Rc;

use crate::llm::daemon::{watch_state, DaemonLoopConfig};
use crate::value::VmError;

use super::super::agent_config::AgentLoopConfig;
use super::super::agent_tools::{
    merge_agent_loop_approval_policy, merge_agent_loop_policy, normalize_native_tools_for_format,
    normalize_tool_choice_for_format, normalize_tool_examples_for_format, ToolCallTracker,
};
use super::super::tools::build_tool_calling_contract_prompt;

/// Resets the iteration counter in the transcript-observer thread-local
/// when the loop exits.
pub(super) struct TranscriptIterationGuard;

impl Drop for TranscriptIterationGuard {
    fn drop(&mut self) {
        crate::llm::agent_observe::set_current_iteration(None);
    }
}

/// Pops the loop-local execution policy off the orchestration stack
/// when the loop exits. Only pops if the loop actually pushed a policy.
pub(super) struct ExecutionPolicyGuard {
    pub(super) active: bool,
}

impl Drop for ExecutionPolicyGuard {
    fn drop(&mut self) {
        if self.active {
            crate::orchestration::pop_execution_policy();
        }
    }
}

/// Pops the loop-local approval policy off the orchestration stack when
/// the loop exits. Only pops if the loop actually pushed a policy.
pub(super) struct ApprovalPolicyGuard {
    pub(super) active: bool,
}

impl Drop for ApprovalPolicyGuard {
    fn drop(&mut self) {
        if self.active {
            crate::orchestration::pop_approval_policy();
        }
    }
}

/// Drops every external sink and closure subscriber registered against
/// this loop's session_id when the loop exits (success or error).
/// Without this, pipeline `agent_subscribe` closures would accumulate
/// across workflow stages — each stage builder calls `agent_subscribe`
/// exactly once for its own session_id, but the registry only cleared
/// when the ACP server explicitly tore a session down. CLI / non-ACP
/// embeddings leaked monotonically.
pub(super) struct SessionSinkGuard {
    pub(super) session_id: String,
}

impl Drop for SessionSinkGuard {
    fn drop(&mut self) {
        if !self.session_id.is_empty() {
            crate::agent_events::clear_session_sinks(&self.session_id);
        }
    }
}

/// Every long-lived local the agent loop carried in
/// `run_agent_loop_internal`, gathered onto a single owned struct so
/// phase methods can take `&mut self` and mutate state in place.
///
/// # Drop ordering
///
/// Rust drops struct fields in FORWARD declaration order. The original
/// function declared its drop guards via `let _x = …` bindings in this
/// sequence:
///
/// ```text
/// let _iteration_guard = …;  // 1st
/// let _sink_guard      = …;  // 2nd
/// let _policy_guard    = …;  // 3rd
/// let _approval_guard  = …;  // 4th
/// ```
///
/// `let` bindings drop in REVERSE declaration order, so the observable
/// drop order was: approval → policy → sink → iteration. To preserve
/// that order with struct fields (forward drop order), the guard fields
/// appear in this struct in the SAME sequence as the observable drop
/// order: `_approval_guard`, `_policy_guard`, `_sink_guard`,
/// `_iteration_guard`. **Do not reorder** without re-auditing the push/
/// pop lifetimes in `crate::orchestration`.
pub(super) struct AgentLoopState {
    pub(super) config: AgentLoopConfig,
    pub(super) session_id: String,

    pub(super) tool_contract_prompt: Option<String>,
    pub(super) base_system: Option<String>,
    pub(super) persistent_system_prompt: Option<String>,
    pub(super) has_tools: bool,

    pub(super) visible_messages: Vec<serde_json::Value>,
    pub(super) recorded_messages: Vec<serde_json::Value>,
    pub(super) transcript_events: Vec<crate::value::VmValue>,
    pub(super) transcript_summary: Option<String>,
    pub(super) total_text: String,
    pub(super) last_iteration_text: String,

    pub(super) task_ledger: crate::llm::ledger::TaskLedger,
    pub(super) ledger_done_rejections: usize,
    pub(super) loop_tracker: ToolCallTracker,
    pub(super) loop_detect_enabled: bool,

    pub(super) total_iterations: usize,
    pub(super) resumed_iterations: usize,
    pub(super) consecutive_text_only: usize,
    pub(super) consecutive_single_tool_turns: usize,
    pub(super) idle_backoff_ms: u64,
    pub(super) last_run_exit_code: Option<i32>,

    pub(super) all_tools_used: Vec<String>,
    pub(super) successful_tools_used: Vec<String>,
    pub(super) rejected_tools: Vec<String>,
    pub(super) deferred_user_messages: Vec<String>,

    pub(super) daemon_state: String,
    pub(super) daemon_snapshot_path: Option<String>,
    pub(super) daemon_watch_state: std::collections::BTreeMap<String, u64>,

    /// Set only by the finalize phase.
    pub(super) final_status: &'static str,

    pub(super) loop_start: std::time::Instant,

    pub(super) bridge: Option<Rc<crate::bridge::HostBridge>>,
    pub(super) tool_format: String,
    pub(super) done_sentinel: String,
    pub(super) break_unless_phase: Option<String>,
    pub(super) max_iterations: usize,
    pub(super) max_nudges: usize,
    pub(super) tool_retries: usize,
    pub(super) tool_backoff_ms: u64,
    pub(super) exit_when_verified: bool,
    pub(super) persistent: bool,
    pub(super) daemon: bool,
    pub(super) auto_compact: Option<crate::orchestration::AutoCompactConfig>,
    pub(super) daemon_config: DaemonLoopConfig,
    pub(super) custom_nudge: Option<String>,

    // Drop guards: see "Drop ordering" on the struct docs.
    pub(super) _approval_guard: ApprovalPolicyGuard,
    pub(super) _policy_guard: ExecutionPolicyGuard,
    pub(super) _sink_guard: SessionSinkGuard,
    pub(super) _iteration_guard: TranscriptIterationGuard,
}

impl AgentLoopState {
    /// Build the loop state from a fresh `AgentLoopConfig`, mutating
    /// `opts` in place to normalize the native-tool channel before the
    /// first LLM call.
    pub(super) fn new(
        opts: &mut crate::llm::api::LlmCallOptions,
        config: AgentLoopConfig,
    ) -> Result<Self, VmError> {
        // Fresh transcript segment per top-level loop: reset dedup so the
        // first call re-emits system_prompt + tool_schemas and message
        // events carry meaningful iteration indices.
        crate::llm::agent_observe::reset_transcript_dedup();

        let _iteration_guard = TranscriptIterationGuard;

        let bridge = super::current_host_bridge();
        let max_iterations = config.max_iterations;
        let persistent = config.persistent;
        let max_nudges = config.max_nudges;
        let custom_nudge = config.nudge.clone();
        let done_sentinel = config
            .done_sentinel
            .clone()
            .unwrap_or_else(|| "##DONE##".to_string());
        let break_unless_phase = config.break_unless_phase.clone();
        let tool_retries = config.tool_retries;
        let tool_backoff_ms = config.tool_backoff_ms;
        let tool_format = config.tool_format.clone();
        let session_id = config.session_id.clone();
        let _sink_guard = SessionSinkGuard {
            session_id: session_id.clone(),
        };

        let auto_compact = config.auto_compact.clone();
        let daemon = config.daemon;
        let daemon_config = config.daemon_config.clone();
        let exit_when_verified = config.exit_when_verified;
        let last_run_exit_code: Option<i32> = None;

        let loop_detect_enabled = config.loop_detect_warn > 0;
        let loop_tracker = ToolCallTracker::new(
            config.loop_detect_warn,
            config.loop_detect_block,
            config.loop_detect_skip,
        );

        let effective_policy = merge_agent_loop_policy(config.policy.clone())?;

        // Intersect with outer workflow/worker ceiling so nested loops
        // never widen permissions.
        if let Some(ref policy) = effective_policy {
            crate::orchestration::push_execution_policy(policy.clone());
        }
        let _policy_guard = ExecutionPolicyGuard {
            active: effective_policy.is_some(),
        };

        let effective_approval_policy =
            merge_agent_loop_approval_policy(config.approval_policy.clone());
        if let Some(ref policy) = effective_approval_policy {
            crate::orchestration::push_approval_policy(policy.clone());
        }
        let _approval_guard = ApprovalPolicyGuard {
            active: effective_approval_policy.is_some(),
        };

        let tools_owned = opts.tools.clone();
        let tools_val = tools_owned.as_ref();
        opts.native_tools =
            normalize_native_tools_for_format(&tool_format, opts.native_tools.clone());
        opts.tool_choice = normalize_tool_choice_for_format(
            &opts.provider,
            &tool_format,
            opts.native_tools.as_deref(),
            opts.tool_choice.clone(),
            config.turn_policy.as_ref(),
        );
        let native_tools_for_prompt = opts.native_tools.clone();
        let rendered_schemas =
            crate::llm::tools::collect_tool_schemas(tools_val, native_tools_for_prompt.as_deref());
        let has_tools = !rendered_schemas.is_empty();
        let base_system = opts.system.clone();
        let tool_examples =
            normalize_tool_examples_for_format(&tool_format, config.tool_examples.clone());
        let tool_contract_prompt = if has_tools {
            Some(build_tool_calling_contract_prompt(
                tools_val,
                native_tools_for_prompt.as_deref(),
                &tool_format,
                config
                    .turn_policy
                    .as_ref()
                    .is_some_and(|policy| policy.require_action_or_yield),
                tool_examples.as_deref(),
            ))
        } else {
            None
        };

        let allow_done_sentinel = config
            .turn_policy
            .as_ref()
            .map(|policy| policy.allow_done_sentinel)
            .unwrap_or(true);
        let persistent_system_prompt = if persistent {
            if exit_when_verified {
                if allow_done_sentinel {
                    Some(format!(
                        "\n\nKeep working until the task is complete. Take action with tool calls — \
                         do not stop to explain. Emit `<done>{done_sentinel}</done>` only after a \
                         passing verification run."
                    ))
                } else {
                    Some(
                        "\n\nKeep working until the task is complete. Take action with tool calls — \
                         do not stop to explain."
                            .to_string(),
                    )
                }
            } else if allow_done_sentinel {
                Some(format!(
                    "\n\nIMPORTANT: You MUST keep working until the task is complete. \
                     Do NOT stop to explain or summarize — take action with tool calls. \
                     When the requested work is complete, emit `<done>{done_sentinel}</done>` \
                     as its own top-level block."
                ))
            } else {
                Some(
                    "\n\nIMPORTANT: You MUST keep working until the task is complete. \
                     Do NOT stop to explain or summarize — take action with tool calls."
                        .to_string(),
                )
            }
        } else {
            None
        };
        let mut visible_messages = opts.messages.clone();
        let mut recorded_messages = opts.messages.clone();
        // Emit `message` events for the initial payload so transcript
        // replayers / LoRA corpus extractors see the opening context.
        for message in &opts.messages {
            crate::llm::agent_observe::emit_message_event(message);
        }

        let mut total_text = String::new();
        let mut last_iteration_text = String::new();
        let consecutive_text_only = 0usize;
        let consecutive_single_tool_turns = 0usize;
        let task_ledger = config.task_ledger.clone();
        let ledger_done_rejections = 0usize;
        let mut all_tools_used: Vec<String> = Vec::new();
        let successful_tools_used: Vec<String> = Vec::new();
        let mut rejected_tools: Vec<String> = Vec::new();
        let mut deferred_user_messages: Vec<String> = Vec::new();
        let mut total_iterations = 0usize;
        let final_status = "done";
        let mut transcript_summary = opts.transcript_summary.clone();
        let loop_start = std::time::Instant::now();
        let mut transcript_events: Vec<crate::value::VmValue> = Vec::new();
        let mut idle_backoff_ms = 100u64;
        let mut daemon_state = if daemon {
            "active".to_string()
        } else {
            "done".to_string()
        };
        let mut daemon_snapshot_path: Option<String> = None;
        let mut daemon_watch_state = watch_state(&daemon_config.watch_paths);
        let mut resumed_iterations = 0usize;
        let mut last_run_exit_code = last_run_exit_code;

        if daemon {
            if let Some(path) = daemon_config.resume_path.as_deref() {
                let snapshot = crate::llm::daemon::load_snapshot(path)?;
                daemon_state = snapshot.daemon_state.clone();
                visible_messages = snapshot.visible_messages;
                recorded_messages = snapshot.recorded_messages;
                transcript_summary = snapshot.transcript_summary;
                transcript_events = snapshot
                    .transcript_events
                    .iter()
                    .map(crate::stdlib::json_to_vm_value)
                    .collect();
                total_text = snapshot.total_text;
                last_iteration_text = snapshot.last_iteration_text;
                all_tools_used = snapshot.all_tools_used;
                rejected_tools = snapshot.rejected_tools;
                deferred_user_messages = snapshot.deferred_user_messages;
                resumed_iterations = snapshot.total_iterations;
                total_iterations = resumed_iterations;
                idle_backoff_ms = snapshot.idle_backoff_ms.max(1);
                last_run_exit_code = snapshot.last_run_exit_code;
                daemon_watch_state = if snapshot.watch_state.is_empty() {
                    watch_state(&daemon_config.watch_paths)
                } else {
                    snapshot.watch_state
                };
                daemon_snapshot_path = Some(path.to_string());
            } else if let Some(path) = daemon_config.effective_persist_path() {
                daemon_snapshot_path = Some(path.to_string());
            }
        }

        Ok(Self {
            config,
            session_id,
            tool_contract_prompt,
            base_system,
            persistent_system_prompt,
            has_tools,
            visible_messages,
            recorded_messages,
            transcript_events,
            transcript_summary,
            total_text,
            last_iteration_text,
            task_ledger,
            ledger_done_rejections,
            loop_tracker,
            loop_detect_enabled,
            total_iterations,
            resumed_iterations,
            consecutive_text_only,
            consecutive_single_tool_turns,
            idle_backoff_ms,
            last_run_exit_code,
            all_tools_used,
            successful_tools_used,
            rejected_tools,
            deferred_user_messages,
            daemon_state,
            daemon_snapshot_path,
            daemon_watch_state,
            final_status,
            loop_start,
            bridge,
            tool_format,
            done_sentinel,
            break_unless_phase,
            max_iterations,
            max_nudges,
            tool_retries,
            tool_backoff_ms,
            exit_when_verified,
            persistent,
            daemon,
            auto_compact,
            daemon_config,
            custom_nudge,
            _approval_guard,
            _policy_guard,
            _sink_guard,
            _iteration_guard,
        })
    }
}
