//! Agent-loop tests split by topic.
//!
//! Shared helpers live here so the submodules can stay focused on one
//! concern each via `use super::*;`.

pub(super) use super::helpers::{
    action_turn_nudge, assistant_history_text, has_successful_tools,
    loop_state_requests_phase_change, prose_exceeds_budget, sentinel_without_action_nudge,
    should_stop_after_successful_tools, trim_prose_for_history,
};
pub(super) use super::run_agent_loop_internal;
pub(super) use crate::bridge::HostBridge;
pub(super) use crate::llm::agent_config::{build_llm_call_result, AgentLoopConfig};
pub(super) use crate::llm::agent_observe::{
    extract_retry_after_ms, observed_llm_call, LlmRetryConfig,
};
pub(super) use crate::llm::agent_tools::{
    merge_agent_loop_policy, normalize_native_tools_for_format, normalize_tool_choice_for_format,
    normalize_tool_examples_for_format, required_tool_choice_for_provider,
};
pub(super) use crate::llm::api::{LlmCallOptions, LlmResult};
pub(super) use crate::llm::daemon::{persist_snapshot, DaemonLoopConfig, DaemonSnapshot};
pub(super) use crate::llm::mock::{get_llm_mock_calls, reset_llm_mock_state};
pub(super) use crate::orchestration::{pop_execution_policy, push_execution_policy, TurnPolicy};
pub(super) use crate::tool_annotations::{ToolAnnotations, ToolKind};
pub(super) use crate::value::{VmError, VmValue};
pub(super) use serde_json::json;
pub(super) use std::rc::Rc;
pub(super) use std::sync::atomic::AtomicBool;
pub(super) use std::sync::{Arc, Mutex};

mod native_tool_fallback;
mod phase_history;
mod policy;
mod prompts;
mod result_build;
mod retry_after;
mod tool_classification;
mod tool_dispatch_categories;
mod tool_durations;
mod tool_format;
mod transcript;

pub(super) fn base_opts(messages: Vec<serde_json::Value>) -> LlmCallOptions {
    LlmCallOptions {
        provider: "mock".to_string(),
        model: "mock".to_string(),
        api_key: String::new(),
        route_policy: crate::llm::api::LlmRoutePolicy::Manual,
        fallback_chain: Vec::new(),
        routing_decision: None,
        session_id: None,
        messages,
        system: None,
        transcript_summary: None,
        max_tokens: 128,
        temperature: None,
        top_p: None,
        top_k: None,
        stop: None,
        seed: None,
        frequency_penalty: None,
        presence_penalty: None,
        response_format: None,
        json_schema: None,
        output_schema: None,
        output_validation: None,
        thinking: None,
        tools: None,
        native_tools: None,
        tool_choice: None,
        tool_search: None,
        cache: false,
        timeout: None,
        idle_timeout: None,
        stream: true,
        provider_overrides: None,
        prefill: None,
        structural_experiment: None,
        applied_structural_experiment: None,
    }
}

pub(super) fn base_agent_config() -> AgentLoopConfig {
    AgentLoopConfig {
        persistent: false,
        max_iterations: 1,
        max_nudges: 1,
        nudge: None,
        done_sentinel: None,
        break_unless_phase: None,
        tool_retries: 0,
        tool_backoff_ms: 1,
        tool_format: "text".to_string(),
        native_tool_fallback: crate::orchestration::NativeToolFallbackPolicy::Allow,
        auto_compact: None,
        policy: None,
        command_policy: None,
        permissions: None,
        approval_policy: None,
        daemon: false,
        daemon_config: DaemonLoopConfig::default(),
        llm_retries: 0,
        llm_backoff_ms: 1,
        token_budget: None,
        exit_when_verified: false,
        loop_detect_warn: 0,
        loop_detect_block: 0,
        loop_detect_skip: 0,
        tool_examples: None,
        turn_policy: None,
        stop_after_successful_tools: None,
        require_successful_tools: None,
        session_id: "test_session".to_string(),
        event_sink: None,
        task_ledger: Default::default(),
        post_turn_callback: None,
        skill_registry: None,
        skill_match: Default::default(),
        working_files: Vec::new(),
    }
}

/// Mutex protecting the HARN_LLM_TRANSCRIPT_DIR env var so transcript
/// tests in this module don't race each other and end up writing to a
/// neighbour's temp dir.
pub(super) fn transcript_env_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| std::sync::Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}
