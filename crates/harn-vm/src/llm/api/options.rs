//! Option and payload types for `llm_call`: `LlmCallOptions`,
//! `LlmRequestPayload`, plus the `tool_search` / `thinking` sub-configs.

use crate::value::VmValue;

/// Sender for streaming text deltas from an in-flight LLM call.
pub(crate) type DeltaSender = tokio::sync::mpsc::UnboundedSender<String>;

/// Extended thinking configuration.
#[derive(Clone, Debug, serde::Serialize)]
pub(crate) enum ThinkingConfig {
    /// Enable with provider defaults.
    Enabled,
    /// Enable with a specific token budget.
    WithBudget(i64),
}

/// Which tool-search variant to use. Two shapes today, matching the two
/// Anthropic variants (also reused as the mental model for the OpenAI path
/// landing in harn#71). Scripts write the lower-case short name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ToolSearchVariant {
    /// BM25 / natural-language queries. Default when the user wrote just
    /// `tool_search: true` or omitted the variant.
    Bm25,
    /// Python-regex queries (more precise, less ergonomic).
    Regex,
}

impl ToolSearchVariant {
    pub(crate) fn as_short(self) -> &'static str {
        match self {
            ToolSearchVariant::Bm25 => "bm25",
            ToolSearchVariant::Regex => "regex",
        }
    }
}

/// Implementation of the client-executed tool-search fallback (harn#70).
/// Only consulted when `ToolSearchMode::Client` resolves (either
/// explicit or via auto-fallback when the provider lacks native
/// support). Orthogonal to `ToolSearchVariant`: a user can ask for
/// `variant: bm25` (the model sees the BM25-style tool) and
/// `strategy: semantic` (the host runs embedding search under the
/// hood).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ToolSearchStrategy {
    /// In-tree BM25 over the deferred tool corpus. **Default.**
    Bm25,
    /// In-tree regex over the deferred tool corpus (case-insensitive).
    Regex,
    /// Delegated to the host via the `tool_search/query` bridge RPC so
    /// integrators can wire embeddings without Harn depending on ML
    /// crates.
    Semantic,
    /// Pure host-side implementation; the VM just round-trips the query
    /// and promotes whatever names the host returns.
    Host,
}

impl ToolSearchStrategy {
    pub(crate) fn as_short(self) -> &'static str {
        match self {
            ToolSearchStrategy::Bm25 => "bm25",
            ToolSearchStrategy::Regex => "regex",
            ToolSearchStrategy::Semantic => "semantic",
            ToolSearchStrategy::Host => "host",
        }
    }

    /// Whether this strategy runs entirely inside the VM (no bridge
    /// hop). Used by the dispatch path to decide between the sync
    /// in-tree index and the `tool_search/query` RPC.
    #[allow(dead_code)] // consumed by harness tests + future dispatch refactors
    pub(crate) fn is_in_tree(self) -> bool {
        matches!(self, ToolSearchStrategy::Bm25 | ToolSearchStrategy::Regex)
    }

    /// Map to the in-tree strategy enum used by
    /// [`crate::llm::tool_search::run_in_tree`]. Panics on non-in-tree
    /// strategies — callers must gate on `is_in_tree()`.
    pub(crate) fn as_in_tree(self) -> crate::llm::tool_search::InTreeStrategy {
        match self {
            ToolSearchStrategy::Bm25 => crate::llm::tool_search::InTreeStrategy::Bm25,
            ToolSearchStrategy::Regex => crate::llm::tool_search::InTreeStrategy::Regex,
            _ => unreachable!("as_in_tree called on {self:?}"),
        }
    }

    /// Default strategy for a given variant when the user did not
    /// specify one explicitly. Native-facing variant leaks into the
    /// client path as a sensible default: `variant: regex` users
    /// probably want regex semantics in the fallback too.
    pub(crate) fn default_for_variant(variant: ToolSearchVariant) -> Self {
        match variant {
            ToolSearchVariant::Bm25 => ToolSearchStrategy::Bm25,
            ToolSearchVariant::Regex => ToolSearchStrategy::Regex,
        }
    }
}

/// How to resolve `tool_search` against the active provider.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ToolSearchMode {
    /// Auto-select: native if the provider supports it, client-executed
    /// fallback otherwise (harn#70). Default.
    Auto,
    /// Force the provider's native mechanism; error if unsupported.
    Native,
    /// Force client-executed fallback even when native is available.
    /// Currently errors with a pointer to harn#70 until the fallback lands.
    Client,
}

/// User-facing tool_search configuration. Parsed from the `tool_search`
/// option on `llm_call` / `agent_loop`. Absent means no deferred-loading
/// machinery is engaged — tools ship eagerly as always.
#[derive(Clone, Debug)]
pub(crate) struct ToolSearchConfig {
    pub variant: ToolSearchVariant,
    pub mode: ToolSearchMode,
    /// Tool names that must remain eager even when `defer_loading: true`
    /// is otherwise set on them. Useful for a "safety net" a skill wants
    /// always available regardless of the tool-search index's decisions.
    /// Only consumed by the client-executed path — for the native
    /// Anthropic path, eagerness is already controlled per-tool via
    /// `defer_loading`.
    pub always_loaded: Vec<String>,
    /// Client-mode implementation strategy. When unset, defaults to
    /// `ToolSearchStrategy::default_for_variant(variant)`.
    pub strategy: Option<ToolSearchStrategy>,
    /// Soft cap on how many deferred tools the client-executed loop
    /// may promote into the eager set over the life of this call.
    /// Oldest-promoted tools are evicted when the cap is hit. `None`
    /// means no cap — rely on the `max_results` per search call.
    pub budget_tokens: Option<i64>,
    /// Override for the synthetic tool's name. Default
    /// `__harn_tool_search`. Lets skills with a brand-specific vocabulary
    /// name the tool something the model will understand out of the
    /// box (`find_tool`, `discover_tool`, etc.).
    pub name: Option<String>,
    /// When true, the client-mode loop includes a short stub line for
    /// each deferred tool (name + one-line summary) alongside the
    /// synthetic search tool so the model knows what's available
    /// without calling search first. Default: `false` — the Anthropic
    /// native path also ships no stubs.
    pub include_stub_listing: bool,
    /// Canonical native-shape JSON for every tool that had
    /// `defer_loading: true` at option-parse time, keyed by tool name.
    /// Populated by `apply_tool_search_client_injection` and later
    /// drained by `AgentLoopState::new` when it builds the per-loop
    /// client state. Never populated for native mode — the provider
    /// handles deferral server-side.
    pub deferred_bodies: std::collections::BTreeMap<String, serde_json::Value>,
}

impl ToolSearchConfig {
    /// Default when the user writes `tool_search: true` with no detail.
    pub(crate) fn default_bm25_auto() -> Self {
        Self {
            variant: ToolSearchVariant::Bm25,
            mode: ToolSearchMode::Auto,
            always_loaded: Vec::new(),
            strategy: None,
            budget_tokens: None,
            name: None,
            include_stub_listing: false,
            deferred_bodies: std::collections::BTreeMap::new(),
        }
    }

    /// Resolve the effective strategy, falling back to the variant
    /// default when the user left `strategy` unset.
    pub(crate) fn effective_strategy(&self) -> ToolSearchStrategy {
        self.strategy
            .unwrap_or_else(|| ToolSearchStrategy::default_for_variant(self.variant))
    }

    /// Resolve the synthetic tool's name. Default matches the spec's
    /// proposed `__harn_tool_search` sentinel.
    pub(crate) fn effective_name(&self) -> &str {
        self.name.as_deref().unwrap_or("__harn_tool_search")
    }
}

/// All options for an LLM API call, extracted once from user-facing args.
#[derive(Clone)]
pub(crate) struct LlmCallOptions {
    // --- Routing ---
    pub provider: String,
    pub model: String,
    pub api_key: String,

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
    pub stop: Option<Vec<String>>,
    pub seed: Option<i64>,
    pub frequency_penalty: Option<f64>,
    pub presence_penalty: Option<f64>,

    // --- Structured output ---
    pub response_format: Option<String>,
    pub json_schema: Option<serde_json::Value>,
    pub output_schema: Option<serde_json::Value>,
    pub output_validation: Option<String>,

    // --- Thinking ---
    pub thinking: Option<ThinkingConfig>,

    // --- Tools ---
    pub tools: Option<VmValue>,
    pub native_tools: Option<Vec<serde_json::Value>>,
    pub tool_choice: Option<serde_json::Value>,
    /// Progressive-disclosure configuration. When set, the options
    /// extractor resolves this against the active provider's capability
    /// matrix and, for native-supporting providers, prepends a
    /// `tool_search_tool_*_20251119` meta-tool to `native_tools`. For
    /// client-executed mode (harn#70) this carries the config forward
    /// into the agent-loop fallback. See [`ToolSearchConfig`].
    #[allow(dead_code)] // consumed by the options extractor; persisted for transcript /
    // replay fidelity and harn#70's client-executed loop
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

    // --- Assistant prefill ---
    /// Optional prefill string. When set, providers append a final
    /// `role: "assistant"` message with this content so the model
    /// continues from there. Cleared by the agent loop after each turn.
    /// See `llm::providers::anthropic` and `llm::providers::openai_compat`
    /// for provider-specific plumbing.
    pub prefill: Option<String>,
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
}

/// Send-safe subset of `LlmCallOptions` used for provider transport.
#[derive(Clone, Debug, serde::Serialize)]
pub(crate) struct LlmRequestPayload {
    pub provider: String,
    pub model: String,
    pub api_key: String,
    pub messages: Vec<serde_json::Value>,
    pub system: Option<String>,
    pub max_tokens: i64,
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub top_k: Option<i64>,
    pub stop: Option<Vec<String>>,
    pub seed: Option<i64>,
    pub frequency_penalty: Option<f64>,
    pub presence_penalty: Option<f64>,
    pub response_format: Option<String>,
    pub json_schema: Option<serde_json::Value>,
    pub thinking: Option<ThinkingConfig>,
    pub native_tools: Option<Vec<serde_json::Value>>,
    pub tool_choice: Option<serde_json::Value>,
    pub cache: bool,
    pub timeout: Option<u64>,
    pub stream: bool,
    pub provider_overrides: Option<serde_json::Value>,
    pub prefill: Option<String>,
}

impl LlmRequestPayload {
    pub(crate) fn resolve_timeout(&self) -> u64 {
        resolve_timeout(self.timeout)
    }
}

impl From<&LlmCallOptions> for LlmRequestPayload {
    fn from(opts: &LlmCallOptions) -> Self {
        Self {
            provider: opts.provider.clone(),
            model: opts.model.clone(),
            api_key: opts.api_key.clone(),
            messages: opts.messages.clone(),
            system: opts.system.clone(),
            max_tokens: opts.max_tokens,
            temperature: opts.temperature,
            top_p: opts.top_p,
            top_k: opts.top_k,
            stop: opts.stop.clone(),
            seed: opts.seed,
            frequency_penalty: opts.frequency_penalty,
            presence_penalty: opts.presence_penalty,
            response_format: opts.response_format.clone(),
            json_schema: opts.json_schema.clone(),
            thinking: opts.thinking.clone(),
            native_tools: opts.native_tools.clone(),
            tool_choice: opts.tool_choice.clone(),
            cache: opts.cache,
            timeout: opts.timeout,
            stream: opts.stream,
            provider_overrides: opts.provider_overrides.clone(),
            prefill: opts.prefill.clone(),
        }
    }
}

#[cfg(test)]
pub(super) fn base_opts(provider: &str) -> LlmCallOptions {
    use std::rc::Rc;
    LlmCallOptions {
        provider: provider.to_string(),
        model: "test-model".to_string(),
        api_key: String::new(),
        messages: vec![serde_json::json!({"role": "user", "content": "hello"})],
        system: None,
        transcript_summary: Some("summary".to_string()),
        max_tokens: 64,
        temperature: Some(0.2),
        top_p: Some(0.8),
        top_k: Some(40),
        stop: Some(vec!["STOP".to_string()]),
        seed: Some(7),
        frequency_penalty: Some(0.1),
        presence_penalty: Some(0.2),
        response_format: Some("json".to_string()),
        json_schema: Some(serde_json::json!({"type": "object"})),
        output_schema: Some(serde_json::json!({"type": "object"})),
        output_validation: Some("error".to_string()),
        thinking: None,
        tools: Some(VmValue::String(Rc::from("vm-local-tools"))),
        native_tools: Some(vec![
            serde_json::json!({"type": "function", "function": {"name": "tool"}}),
        ]),
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
        prefill: None,
    }
}

#[cfg(test)]
mod tests {
    use super::{base_opts, LlmRequestPayload};

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
}
