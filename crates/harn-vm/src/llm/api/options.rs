//! Option and payload types for `llm_call`: `LlmCallOptions`,
//! `LlmRequestPayload`, plus the `tool_search` / `thinking` sub-configs.

use crate::value::VmValue;

/// Sender for streaming text deltas from an in-flight LLM call.
pub(crate) type DeltaSender = tokio::sync::mpsc::UnboundedSender<String>;

/// Provider-agnostic reasoning effort.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum ReasoningEffort {
    None,
    Minimal,
    Low,
    Medium,
    High,
    XHigh,
}

impl ReasoningEffort {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            ReasoningEffort::None => "none",
            ReasoningEffort::Minimal => "minimal",
            ReasoningEffort::Low => "low",
            ReasoningEffort::Medium => "medium",
            ReasoningEffort::High => "high",
            ReasoningEffort::XHigh => "xhigh",
        }
    }
}

/// Typed `llm_call(..., { thinking: ... })` configuration.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub(crate) enum ThinkingConfig {
    #[default]
    Disabled,
    Enabled {
        budget_tokens: Option<u32>,
    },
    Adaptive,
    Effort {
        level: ReasoningEffort,
    },
}

impl ThinkingConfig {
    pub(crate) fn is_disabled(&self) -> bool {
        matches!(
            self,
            Self::Disabled
                | Self::Effort {
                    level: ReasoningEffort::None
                }
        )
    }

    pub(crate) fn is_enabled(&self) -> bool {
        !self.is_disabled()
    }
}

/// Provider-agnostic structured-output request shape parsed at the script
/// boundary. Providers translate this into their native wire format.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum OutputFormat {
    #[default]
    Text,
    JsonObject,
    JsonSchema {
        schema: serde_json::Value,
        strict: bool,
    },
}

impl OutputFormat {
    pub(crate) fn is_structured(&self) -> bool {
        !matches!(self, Self::Text)
    }

    pub(crate) fn schema(&self) -> Option<&serde_json::Value> {
        match self {
            Self::JsonSchema { schema, .. } => Some(schema),
            _ => None,
        }
    }
}

/// Which tool-search variant to use. BM25/regex match the provider-native
/// variants; hybrid is a Harn client-mode scorer.
/// Scripts write the lower-case short name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ToolSearchVariant {
    /// BM25 / natural-language queries. Default when the user wrote just
    /// `tool_search: true` or omitted the variant.
    Bm25,
    /// Python-regex queries (more precise, less ergonomic).
    Regex,
    /// Client-side reciprocal-rank fusion over BM25 and weighted field matches.
    Hybrid,
}

impl ToolSearchVariant {
    pub(crate) fn as_short(self) -> &'static str {
        match self {
            ToolSearchVariant::Bm25 => "bm25",
            ToolSearchVariant::Regex => "regex",
            ToolSearchVariant::Hybrid => "hybrid",
        }
    }
}

/// How to resolve `tool_search` against the active provider.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ToolSearchMode {
    /// Auto-select: native if the provider supports it, client-executed
    /// fallback otherwise. Default.
    Auto,
    /// Force the provider's native mechanism; error if unsupported.
    Native,
    /// Force the Harn stdlib client-executed fallback even when native is
    /// available.
    Client,
}

/// Provider API surface for a call. OpenAI-compatible providers default to
/// chat completions; the native OpenAI Responses path is explicit because it
/// has different request, tool, and transcript semantics.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LlmApiMode {
    ChatCompletions,
    Responses,
}

impl LlmApiMode {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::ChatCompletions => "chat_completions",
            Self::Responses => "responses",
        }
    }
}

/// User-facing tool_search configuration. Parsed from the `tool_search`
/// option on `llm_call` / `agent_loop`. Absent means no deferred-loading
/// machinery is engaged — tools ship eagerly as always.
#[derive(Clone, Debug)]
pub(crate) struct ToolSearchConfig {
    pub variant: ToolSearchVariant,
    pub mode: ToolSearchMode,
}

impl ToolSearchConfig {
    /// Default when the user writes `tool_search: true` with no detail.
    pub(crate) fn default_bm25_auto() -> Self {
        Self {
            variant: ToolSearchVariant::Bm25,
            mode: ToolSearchMode::Auto,
        }
    }
}

/// First-class cost/latency routing policy for `llm_call`.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum LlmRoutePolicy {
    /// Keep the historical provider/model resolution path.
    Manual,
    /// Pin to a concrete alias, model id, or `provider:model` selector.
    Always(String),
    /// Pick the lowest-cost available candidate at or above the requested
    /// quality tier.
    CheapestOverQuality(String),
    /// Pick the lowest-latency available candidate at or above the requested
    /// quality tier.
    FastestOverQuality(String),
    /// Pick from an explicit ordered candidate list. The strategy is
    /// `prefer_order`, `cheapest_first`, or `fastest_first`.
    PreferenceList {
        targets: Vec<String>,
        strategy: String,
    },
}

impl LlmRoutePolicy {
    pub(crate) fn as_label(&self) -> String {
        match self {
            Self::Manual => "manual".to_string(),
            Self::Always(target) => format!("always({target})"),
            Self::CheapestOverQuality(target) => format!("cheapest_over_quality({target})"),
            Self::FastestOverQuality(target) => format!("fastest_over_quality({target})"),
            Self::PreferenceList { targets, strategy } => {
                format!("preference_list({strategy}: {})", targets.join(","))
            }
        }
    }
}

/// One considered route in a routing decision.
#[derive(Clone, Debug, serde::Serialize, PartialEq)]
pub(crate) struct LlmRouteAlternative {
    pub provider: String,
    pub model: String,
    pub quality_tier: String,
    pub available: bool,
    pub selected: bool,
    pub cost_per_1k_in: Option<f64>,
    pub cost_per_1k_out: Option<f64>,
    pub latency_p50_ms: Option<u64>,
    pub reason: String,
}

/// Serializable route decision kept for transcript/replay fidelity and portal
/// post-hoc scoring.
#[derive(Clone, Debug, serde::Serialize, PartialEq)]
pub(crate) struct LlmRoutingDecision {
    pub policy: String,
    pub requested_quality: Option<String>,
    pub selected_provider: String,
    pub selected_model: String,
    pub alternatives: Vec<LlmRouteAlternative>,
}

#[derive(Clone, Debug, serde::Serialize, PartialEq)]
pub(crate) struct LlmRouteFallback {
    pub provider: String,
    pub model: String,
}

/// Reminder lifecycle metadata carried to the transport boundary.
///
/// Option extraction decides how each reminder renders for the selected
/// route; the API layer emits the lifecycle record only once preflight
/// gates have passed and a request is about to be served.
#[derive(Clone, Debug)]
pub(crate) struct ReminderLifecycleEmission {
    pub session_id: Option<String>,
    pub turn_number: i64,
    pub reminder_id: String,
    pub tags: Vec<String>,
    pub body: String,
    pub dedupe_key: Option<String>,
    pub source: String,
    pub role_hint: String,
    pub rendered_role: String,
    pub ttl_turns: Option<i64>,
    pub propagate: String,
    pub originating_agent_id: Option<String>,
}

impl ReminderLifecycleEmission {
    pub(crate) fn emit(&self) {
        let payload = serde_json::json!({
            "session_id": &self.session_id,
            "turn_number": self.turn_number,
            "reminder_id": &self.reminder_id,
            "tags": &self.tags,
            "dedupe_key": &self.dedupe_key,
            "source": &self.source,
            "role_hint": &self.role_hint,
            "rendered_role": &self.rendered_role,
            "ttl_turns": &self.ttl_turns,
            "propagate": &self.propagate,
            "originating_agent_id": &self.originating_agent_id,
        });
        crate::llm::helpers::emit_reminder_lifecycle_event(
            crate::llm::helpers::REMINDER_FIRED_EVENT_KIND,
            payload,
        );

        if let Some(session_id) = self.session_id.as_deref().filter(|id| !id.is_empty()) {
            crate::llm::agent_runtime::emit_agent_event_sync(
                &crate::agent_events::AgentEvent::ReminderEmitted {
                    session_id: session_id.to_string(),
                    reminder_id: self.reminder_id.clone(),
                    tags: self.tags.clone(),
                    body: self.body.clone(),
                    role_hint: self.role_hint.clone(),
                    rendered_role: self.rendered_role.clone(),
                    source: self.source.clone(),
                    ttl_turns: self.ttl_turns,
                },
            );
        }
    }
}

/// All options for an LLM API call, extracted once from user-facing args.
#[derive(Clone)]
pub(crate) struct LlmCallOptions {
    // --- Routing ---
    pub provider: String,
    pub model: String,
    pub api_key: String,
    pub api_mode: LlmApiMode,
    pub route_policy: LlmRoutePolicy,
    pub fallback_chain: Vec<String>,
    pub route_fallbacks: Vec<LlmRouteFallback>,
    pub routing_decision: Option<LlmRoutingDecision>,
    /// First-class routing policy handle (from `routing_policy({...})`).
    /// When set, `llm_call` dispatches the request through the
    /// failover-aware executor in [`crate::llm::routing`] instead of
    /// the single-shot `execute_llm_call` path. The `provider`/`model`
    /// fields above still hold the "first link" defaults so callers
    /// that ignore the policy (e.g. transcript builders) see a
    /// coherent placeholder.
    pub routing_policy: Option<std::rc::Rc<crate::llm::routing::RoutingPolicyConfig>>,

    // --- Observability ---
    /// Agent session id, when this call is driven from `run_agent_loop_internal`.
    /// Forwarded to the SSE transport so streaming native tool-call deltas
    /// (#693) can emit `AgentEvent::ToolCall` / `AgentEvent::ToolCallUpdate`
    /// against the right session even before the dispatch phase fires its
    /// own lifecycle events. `None` for raw `llm_call(...)` invocations
    /// from script context — those have no agent session to attach to.
    pub session_id: Option<String>,
    /// Agent-loop reminder provider configuration. Parsed for option-table
    /// visibility and transcript/replay fidelity; raw `llm_call(...)` does
    /// not evaluate reminder providers.
    #[allow(dead_code)]
    pub reminders: Option<serde_json::Value>,
    /// Pending reminder lifecycle records for this call. The API layer
    /// emits them after preflight validation succeeds and before the
    /// request is served by a mock, fake, replay, or real provider.
    pub reminder_lifecycle: Vec<ReminderLifecycleEmission>,

    // --- Conversation ---
    pub messages: Vec<serde_json::Value>,
    pub system: Option<String>,
    /// Optional short summary string prepended to the system prompt.
    /// Populated by auto-compaction at mid-loop boundaries; callers
    /// typically leave this `None`.
    pub transcript_summary: Option<String>,

    // --- Generation ---
    pub max_tokens: i64,
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub top_k: Option<i64>,
    pub logprobs: bool,
    pub top_logprobs: Option<i64>,
    pub stop: Option<Vec<String>>,
    pub seed: Option<i64>,
    pub frequency_penalty: Option<f64>,
    pub presence_penalty: Option<f64>,

    // --- Structured output ---
    pub output_format: OutputFormat,
    /// Legacy compatibility mirror for older internals and replay hashes.
    pub response_format: Option<String>,
    /// Legacy compatibility mirror for older internals and replay hashes.
    pub json_schema: Option<serde_json::Value>,
    pub output_schema: Option<serde_json::Value>,
    pub output_validation: Option<String>,
    /// Abort the streaming response when partial JSON can no longer
    /// satisfy `output_schema`. Defaults to `true` when `output_schema`
    /// is present; opt out with `schema_stream_abort: false` to wait
    /// for the full malformed response (and rely on `schema_retries`
    /// to recover post-hoc).
    pub schema_stream_abort: bool,

    // --- Thinking ---
    pub thinking: ThinkingConfig,
    /// Anthropic beta features to request via the `anthropic-beta`
    /// header. Transport applies this only on Anthropic-style routes.
    pub anthropic_beta_features: Vec<String>,
    /// True when the call explicitly asks for vision or contains image blocks.
    pub vision: bool,

    // --- Tools ---
    pub tools: Option<VmValue>,
    pub native_tools: Option<Vec<serde_json::Value>>,
    /// Provider-native Responses tools that Harn does not execute locally,
    /// such as OpenAI hosted web/file/code-interpreter tools or remote MCP.
    pub provider_tools: Vec<serde_json::Value>,
    pub tool_choice: Option<serde_json::Value>,
    /// Progressive-disclosure configuration. When set, the options
    /// extractor resolves this against the active provider's capability
    /// matrix and, for native-supporting providers, prepends a
    /// `tool_search_tool_*_20251119` meta-tool to `native_tools`. For
    /// client-executed mode this carries the config forward into the
    /// agent-loop fallback. See [`ToolSearchConfig`].
    #[allow(dead_code)] // consumed by the options extractor; persisted for transcript /
    // replay fidelity and the client-executed agent loop
    pub tool_search: Option<ToolSearchConfig>,

    // --- Caching ---
    pub cache: bool,

    // --- Transport ---
    pub timeout: Option<u64>,
    /// Per-chunk idle timeout for streaming responses (seconds).
    pub idle_timeout: Option<u64>,
    /// When true, use streaming SSE transport (token-by-token deltas).
    /// When false, use synchronous request/response. Default: true.
    pub stream: bool,

    // --- Provider-specific overrides ---
    pub provider_overrides: Option<serde_json::Value>,
    /// OpenAI Responses conversation state. When present, the provider links
    /// the request to its prior response instead of Harn replaying every turn.
    pub previous_response_id: Option<String>,
    /// OpenAI Responses persistence flag.
    pub store: Option<bool>,
    /// OpenAI Responses background execution flag.
    pub background: Option<bool>,
    /// OpenAI Responses truncation/compaction policy.
    pub truncation: Option<String>,
    /// OpenAI Responses standalone compaction endpoint flag.
    pub compact: Option<bool>,
    /// OpenAI Responses include list for provider-side metadata expansion.
    pub include: Option<Vec<String>>,
    /// OpenAI Responses limit for provider-executed tool calls.
    pub max_tool_calls: Option<i64>,

    // --- Budgets ---
    /// Optional first-class budget envelope for pre-flight cost/token checks.
    pub budget: Option<crate::llm::cost::LlmBudgetEnvelope>,

    // --- Assistant prefill ---
    /// Optional prefill string. When set, providers append a final
    /// `role: "assistant"` message with this content so the model
    /// continues from there. Cleared by the agent loop after each turn.
    /// See `llm::providers::anthropic` and `llm::providers::openai_compat`
    /// for provider-specific plumbing.
    pub prefill: Option<String>,
    /// Optional prompt-structure transform applied immediately before
    /// each provider call.
    pub structural_experiment:
        Option<crate::llm::structural_experiments::StructuralExperimentConfig>,
    /// Metadata for the transform actually applied to this call.
    pub applied_structural_experiment:
        Option<crate::llm::structural_experiments::AppliedStructuralExperiment>,
}

/// Resolve effective request timeout: explicit value > `HARN_LLM_TIMEOUT` env > 120s default.
fn resolve_timeout(explicit: Option<u64>) -> u64 {
    explicit.unwrap_or_else(|| {
        std::env::var("HARN_LLM_TIMEOUT")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(120)
    })
}

impl LlmCallOptions {
    pub(crate) fn resolve_timeout(&self) -> u64 {
        resolve_timeout(self.timeout)
    }

    pub(crate) fn anthropic_beta_features_for_request(&self) -> Vec<String> {
        let caps = crate::llm::capabilities::lookup(&self.provider, &self.model);
        let mut features = caps.anthropic_beta_features;
        for feature in &self.anthropic_beta_features {
            push_unique_anthropic_beta_feature(&mut features, feature);
        }
        if matches!(
            self.thinking,
            ThinkingConfig::Enabled { .. } | ThinkingConfig::Adaptive
        ) && caps.interleaved_thinking_supported
        {
            push_unique_anthropic_beta_feature(
                &mut features,
                crate::llm::providers::anthropic::ANTHROPIC_INTERLEAVED_THINKING_BETA,
            );
        }
        features
    }
}

pub(crate) fn push_unique_anthropic_beta_feature(features: &mut Vec<String>, feature: &str) {
    if !features.iter().any(|existing| existing == feature) {
        features.push(feature.to_string());
    }
}

/// Send-safe subset of `LlmCallOptions` used for provider transport.
#[derive(Clone, Debug, serde::Serialize)]
pub(crate) struct LlmRequestPayload {
    pub provider: String,
    pub model: String,
    pub api_key: String,
    pub api_mode: LlmApiMode,
    pub fallback_chain: Vec<String>,
    pub route_fallbacks: Vec<LlmRouteFallback>,
    pub messages: Vec<serde_json::Value>,
    pub system: Option<String>,
    pub max_tokens: i64,
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub top_k: Option<i64>,
    pub logprobs: bool,
    pub top_logprobs: Option<i64>,
    pub stop: Option<Vec<String>>,
    pub seed: Option<i64>,
    pub frequency_penalty: Option<f64>,
    pub presence_penalty: Option<f64>,
    pub output_format: OutputFormat,
    pub response_format: Option<String>,
    pub json_schema: Option<serde_json::Value>,
    /// Normalized `output_schema` forwarded to the streaming transport
    /// so it can fail-fast when partial JSON can no longer satisfy the
    /// schema. Mirrors `LlmCallOptions::output_schema` after option
    /// resolution; transport layers only need this field, not the
    /// VM-local original.
    pub output_schema: Option<serde_json::Value>,
    /// See `LlmCallOptions::schema_stream_abort`. Forwarded so the
    /// off-thread transport task can decide without re-reading VM-local
    /// option state.
    pub schema_stream_abort: bool,
    pub thinking: ThinkingConfig,
    pub anthropic_beta_features: Vec<String>,
    pub vision: bool,
    pub native_tools: Option<Vec<serde_json::Value>>,
    pub provider_tools: Vec<serde_json::Value>,
    pub tool_choice: Option<serde_json::Value>,
    pub cache: bool,
    pub timeout: Option<u64>,
    pub stream: bool,
    pub provider_overrides: Option<serde_json::Value>,
    pub previous_response_id: Option<String>,
    pub store: Option<bool>,
    pub background: Option<bool>,
    pub truncation: Option<String>,
    pub compact: Option<bool>,
    pub include: Option<Vec<String>>,
    pub max_tool_calls: Option<i64>,
    pub prefill: Option<String>,
    /// Forwarded session id for streaming-tool-call event emission (#693).
    /// Cloned out of `LlmCallOptions::session_id` so the transport layer
    /// can fire `AgentEvent::ToolCall` / `AgentEvent::ToolCallUpdate`
    /// against the right session. `None` for non-agent-loop calls.
    pub session_id: Option<String>,
    /// See `LlmCallOptions::reminder_lifecycle`. Skipped in serialized
    /// transport snapshots because the prompt already contains the rendered
    /// reminder text and this is local audit routing metadata.
    #[serde(skip_serializing)]
    pub reminder_lifecycle: Vec<ReminderLifecycleEmission>,
}

impl LlmRequestPayload {
    pub(crate) fn resolve_timeout(&self) -> u64 {
        resolve_timeout(self.timeout)
    }

    pub(crate) fn emit_reminder_lifecycle(&self) {
        for reminder in &self.reminder_lifecycle {
            reminder.emit();
        }
    }
}

impl From<&LlmCallOptions> for LlmRequestPayload {
    fn from(opts: &LlmCallOptions) -> Self {
        let mut payload = Self {
            provider: opts.provider.clone(),
            model: opts.model.clone(),
            api_key: opts.api_key.clone(),
            api_mode: opts.api_mode,
            fallback_chain: opts.fallback_chain.clone(),
            route_fallbacks: opts.route_fallbacks.clone(),
            messages: opts.messages.clone(),
            system: opts.system.clone(),
            max_tokens: opts.max_tokens,
            temperature: opts.temperature,
            top_p: opts.top_p,
            top_k: opts.top_k,
            logprobs: opts.logprobs,
            top_logprobs: opts.top_logprobs,
            stop: opts.stop.clone(),
            seed: opts.seed,
            frequency_penalty: opts.frequency_penalty,
            presence_penalty: opts.presence_penalty,
            output_format: opts.output_format.clone(),
            response_format: opts.response_format.clone(),
            json_schema: opts.json_schema.clone(),
            output_schema: opts.output_schema.clone(),
            schema_stream_abort: opts.schema_stream_abort,
            thinking: opts.thinking.clone(),
            anthropic_beta_features: opts.anthropic_beta_features_for_request(),
            vision: opts.vision,
            native_tools: opts.native_tools.clone(),
            provider_tools: opts.provider_tools.clone(),
            tool_choice: opts.tool_choice.clone(),
            cache: opts.cache,
            timeout: opts.timeout,
            stream: opts.stream,
            provider_overrides: opts.provider_overrides.clone(),
            previous_response_id: opts.previous_response_id.clone(),
            store: opts.store,
            background: opts.background,
            truncation: opts.truncation.clone(),
            compact: opts.compact,
            include: opts.include.clone(),
            max_tool_calls: opts.max_tool_calls,
            prefill: opts.prefill.clone(),
            session_id: opts.session_id.clone(),
            reminder_lifecycle: opts.reminder_lifecycle.clone(),
        };
        apply_thinking_disable_directive(&mut payload);
        payload
    }
}

/// When the resolved capabilities for `(provider, model)` declare a
/// `thinking_disable_directive` (e.g. `/no_think` for Qwen3 chat
/// templates) and the requested `thinking` mode is `Disabled`, prepend
/// the directive to the system message. This lets script authors write
/// `thinking: false` once and have it work uniformly across providers
/// without learning per-template prompt directives.
///
/// Idempotent: if the directive (as the first non-blank token of the
/// system message) is already present, no change is made.
fn apply_thinking_disable_directive(payload: &mut LlmRequestPayload) {
    if !payload.thinking.is_disabled() {
        return;
    }
    let caps = crate::llm::capabilities::lookup(&payload.provider, &payload.model);
    let Some(directive) = caps.thinking_disable_directive.as_deref() else {
        return;
    };
    let directive = directive.trim();
    if directive.is_empty() {
        return;
    }

    let already_present = payload
        .system
        .as_deref()
        .map(|sys| sys.trim_start().starts_with(directive))
        .unwrap_or(false);
    if already_present {
        return;
    }

    let new_system = match payload.system.as_deref().filter(|s| !s.is_empty()) {
        Some(existing) => format!("{directive}\n{existing}"),
        None => directive.to_string(),
    };
    payload.system = Some(new_system);
}

#[cfg(test)]
pub(crate) fn base_opts(provider: &str) -> LlmCallOptions {
    use std::rc::Rc;
    LlmCallOptions {
        provider: provider.to_string(),
        model: "test-model".to_string(),
        api_key: String::new(),
        api_mode: LlmApiMode::ChatCompletions,
        route_policy: LlmRoutePolicy::Manual,
        fallback_chain: Vec::new(),
        route_fallbacks: Vec::new(),
        routing_decision: None,
        routing_policy: None,
        session_id: None,
        reminders: None,
        reminder_lifecycle: Vec::new(),
        messages: vec![serde_json::json!({"role": "user", "content": "hello"})],
        system: None,
        transcript_summary: Some("summary".to_string()),
        max_tokens: 64,
        temperature: Some(0.2),
        top_p: Some(0.8),
        top_k: Some(40),
        logprobs: false,
        top_logprobs: None,
        stop: Some(vec!["STOP".to_string()]),
        seed: Some(7),
        frequency_penalty: Some(0.1),
        presence_penalty: Some(0.2),
        output_format: OutputFormat::JsonSchema {
            schema: serde_json::json!({"type": "object"}),
            strict: true,
        },
        response_format: Some("json".to_string()),
        json_schema: Some(serde_json::json!({"type": "object"})),
        output_schema: Some(serde_json::json!({"type": "object"})),
        output_validation: Some("error".to_string()),
        // Default-off in the test fixture: most callers stream
        // non-JSON text and would unexpectedly abort if the watch ran.
        // Tests that exercise the mid-stream abort opt in explicitly.
        schema_stream_abort: false,
        thinking: ThinkingConfig::Disabled,
        anthropic_beta_features: Vec::new(),
        vision: false,
        tools: Some(VmValue::String(Rc::from("vm-local-tools"))),
        native_tools: Some(vec![
            serde_json::json!({"type": "function", "function": {"name": "tool"}}),
        ]),
        provider_tools: Vec::new(),
        tool_choice: Some(serde_json::json!({
            "type": "function",
            "function": {"name": "tool"}
        })),
        tool_search: None,
        cache: true,
        stream: true,
        timeout: Some(5),
        idle_timeout: None,
        provider_overrides: Some(serde_json::json!({"custom_flag": true})),
        previous_response_id: None,
        store: None,
        background: None,
        truncation: None,
        compact: None,
        include: None,
        max_tool_calls: None,
        budget: None,
        prefill: None,
        structural_experiment: None,
        applied_structural_experiment: None,
    }
}

#[cfg(test)]
mod tests {
    use super::{base_opts, LlmRequestPayload, ThinkingConfig};

    fn assert_send<T: Send>() {}

    #[test]
    fn request_payload_is_send_safe_and_drops_vm_local_fields() {
        let payload = LlmRequestPayload::from(&base_opts("openai"));
        assert_send::<LlmRequestPayload>();
        assert_eq!(payload.provider, "openai");
        assert_eq!(payload.model, "test-model");
        assert!(payload.native_tools.is_some());
        assert!(payload.tool_choice.is_some());
        assert_eq!(
            payload.provider_overrides,
            Some(serde_json::json!({"custom_flag": true}))
        );
    }

    #[test]
    fn thinking_disable_directive_prepended_for_qwen3_when_disabled() {
        let mut opts = base_opts("ollama");
        opts.model = "qwen3.5:30b".to_string();
        opts.system = Some("you are an agent.".to_string());
        opts.thinking = ThinkingConfig::Disabled;
        let payload = LlmRequestPayload::from(&opts);
        assert_eq!(
            payload.system.as_deref(),
            Some("/no_think\nyou are an agent."),
            "Qwen3-on-Ollama with thinking: false should auto-prepend /no_think to system",
        );
    }

    #[test]
    fn thinking_disable_directive_skipped_when_thinking_enabled() {
        let mut opts = base_opts("ollama");
        opts.model = "qwen3.5:30b".to_string();
        opts.system = Some("you are an agent.".to_string());
        opts.thinking = ThinkingConfig::Enabled {
            budget_tokens: None,
        };
        let payload = LlmRequestPayload::from(&opts);
        assert_eq!(payload.system.as_deref(), Some("you are an agent."));
    }

    #[test]
    fn thinking_disable_directive_idempotent_when_already_present() {
        let mut opts = base_opts("ollama");
        opts.model = "qwen3.5:30b".to_string();
        opts.system = Some("/no_think\nyou are an agent.".to_string());
        opts.thinking = ThinkingConfig::Disabled;
        let payload = LlmRequestPayload::from(&opts);
        assert_eq!(
            payload.system.as_deref(),
            Some("/no_think\nyou are an agent."),
            "Should not double-prepend /no_think when already at the head of system",
        );
    }

    #[test]
    fn thinking_disable_directive_creates_system_when_none() {
        let mut opts = base_opts("ollama");
        opts.model = "qwen3.5:30b".to_string();
        opts.system = None;
        opts.thinking = ThinkingConfig::Disabled;
        let payload = LlmRequestPayload::from(&opts);
        assert_eq!(payload.system.as_deref(), Some("/no_think"));
    }

    #[test]
    fn thinking_disable_directive_noop_for_provider_without_capability() {
        let mut opts = base_opts("anthropic");
        opts.model = "claude-haiku-4-7".to_string();
        opts.system = Some("you are an agent.".to_string());
        opts.thinking = ThinkingConfig::Disabled;
        let payload = LlmRequestPayload::from(&opts);
        assert_eq!(
            payload.system.as_deref(),
            Some("you are an agent."),
            "Anthropic has no thinking_disable_directive — system should be untouched",
        );
    }
}
