//! Typed caller intent and resolved provider request contracts.

pub use harn_provider_catalog::capabilities::ReasoningEffort;

/// Requested reasoning mode, before the provider's wire projection.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum ThinkingConfig {
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
    pub fn is_disabled(&self) -> bool {
        matches!(
            self,
            Self::Disabled
                | Self::Effort {
                    level: ReasoningEffort::None
                }
        )
    }

    pub fn is_enabled(&self) -> bool {
        !self.is_disabled()
    }
}

/// Provider-neutral structured-output request.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OutputFormat {
    #[default]
    Text,
    JsonObject,
    JsonSchema {
        schema: serde_json::Value,
        strict: bool,
    },
}

impl OutputFormat {
    pub fn is_structured(&self) -> bool {
        !matches!(self, Self::Text)
    }

    pub fn schema(&self) -> Option<&serde_json::Value> {
        match self {
            Self::JsonSchema { schema, .. } => Some(schema),
            _ => None,
        }
    }
}

/// Search vocabulary shared by caller intent and provider adapters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolSearchVariant {
    Bm25,
    Regex,
    Hybrid,
}

impl ToolSearchVariant {
    pub fn as_short(self) -> &'static str {
        match self {
            Self::Bm25 => "bm25",
            Self::Regex => "regex",
            Self::Hybrid => "hybrid",
        }
    }
}

/// Whether tool search selects the native or client implementation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolSearchMode {
    Auto,
    Native,
    Client,
}

/// Provider-side prompt-cache lifetime, preserving explicit caller intent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PromptCacheTtl {
    FiveMinutes,
    OneHour,
}

impl PromptCacheTtl {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "5m" => Some(Self::FiveMinutes),
            "1h" => Some(Self::OneHour),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::FiveMinutes => "5m",
            Self::OneHour => "1h",
        }
    }

    pub fn anthropic_ttl_field(self) -> Option<&'static str> {
        match self {
            Self::FiveMinutes => None,
            Self::OneHour => Some("1h"),
        }
    }
}

impl serde::Serialize for PromptCacheTtl {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub struct LogprobsConfig {
    pub top: Option<u8>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct TokenBias {
    pub token_id: u32,
    pub tokenizer: String,
    pub bias: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Verbosity {
    Low,
    Medium,
    High,
}

impl Verbosity {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize)]
pub struct MirostatConfig {
    pub version: u8,
    pub target_entropy: f64,
    pub learning_rate: f64,
}

/// The provider API surface selected for this call.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LlmApiMode {
    #[default]
    ChatCompletions,
    Responses,
}

impl LlmApiMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ChatCompletions => "chat_completions",
            Self::Responses => "responses",
        }
    }
}

#[derive(Clone, Debug)]
pub struct ToolSearchConfig {
    pub variant: ToolSearchVariant,
    pub mode: ToolSearchMode,
}

impl ToolSearchConfig {
    pub fn default_bm25_auto() -> Self {
        Self {
            variant: ToolSearchVariant::Bm25,
            mode: ToolSearchMode::Auto,
        }
    }
}

/// Cost/latency routing intent. Resolution remains with the host adapter.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum LlmRoutePolicy {
    #[default]
    Manual,
    Always(String),
    CheapestOverQuality(String),
    FastestOverQuality(String),
    PreferenceList {
        targets: Vec<String>,
        strategy: String,
    },
}

impl LlmRoutePolicy {
    pub fn as_label(&self) -> String {
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

#[derive(Clone, Debug, serde::Serialize, PartialEq)]
pub struct LlmRouteAlternative {
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

/// The resolved route decision retained for transcript/replay fidelity.
#[derive(Clone, Debug, serde::Serialize, PartialEq)]
pub struct LlmRoutingDecision {
    pub policy: String,
    pub requested_quality: Option<String>,
    pub selected_provider: String,
    pub selected_model: String,
    pub alternatives: Vec<LlmRouteAlternative>,
}

#[derive(Clone, Debug, serde::Serialize, PartialEq, Eq)]
pub struct LlmRouteFallback {
    pub provider: String,
    pub model: String,
}

/// Reminder routing metadata. Hosts emit it only after transport admission.
#[derive(Clone, Debug)]
pub struct ReminderLifecycleEmission {
    pub session_id: Option<String>,
    pub turn_number: i64,
    pub reminder_id: String,
    pub tags: Vec<String>,
    pub body: String,
    pub dedupe_key: Option<String>,
    pub source: String,
    pub role_hint: String,
    pub authority: String,
    pub rendered_role: String,
    pub body_bytes: usize,
    pub rendered_bytes: usize,
    pub ttl_turns: Option<i64>,
    pub propagate: String,
    pub originating_agent_id: Option<String>,
}

/// Send-safe provider request and per-call routing metadata.
///
/// Native adapters construct this after option extraction and retain ownership
/// of credentials, catalog capture, admission, and event publication.
///
/// `Default` exists for fixtures and synthetic requests; the production
/// conversion from call options stays exhaustive so a new field is considered
/// there.
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct LlmRequestPayload {
    #[serde(skip_serializing)]
    pub reasoning_effort_source: crate::effective_effort::ReasoningEffortSource,
    pub provider: String,
    pub model: String,
    /// A per-call region override; absent preserves the adapter's resolution.
    pub region: Option<String>,
    pub api_key: String,
    pub api_mode: LlmApiMode,
    pub messages: Vec<serde_json::Value>,
    pub system: Option<String>,
    pub max_tokens: i64,
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub top_k: Option<i64>,
    pub logprobs: Option<LogprobsConfig>,
    pub logit_bias: Vec<TokenBias>,
    pub min_p: Option<f64>,
    pub repetition_penalty: Option<f64>,
    pub prediction: Option<String>,
    pub verbosity: Option<Verbosity>,
    pub mirostat: Option<MirostatConfig>,
    pub stop: Option<Vec<String>>,
    pub seed: Option<i64>,
    pub frequency_penalty: Option<f64>,
    pub presence_penalty: Option<f64>,
    pub parallel_tool_calls: Option<bool>,
    #[serde(skip_serializing)]
    pub provider_contract_probe: Option<harn_provider_catalog::capabilities::PortableOption>,
    /// Caller intent, distinct from catalog defaults.
    #[serde(skip_serializing)]
    pub portable_option_intent:
        std::collections::BTreeSet<harn_provider_catalog::capabilities::PortableOption>,
    pub fast: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_mode: Option<String>,
    pub output_format: OutputFormat,
    /// The provider-compatible schema used for the request and streaming
    /// validation. Completed-response validation uses the caller's contract.
    pub output_schema: Option<serde_json::Value>,
    pub schema_stream_abort: bool,
    pub thinking: ThinkingConfig,
    pub anthropic_beta_features: Vec<String>,
    pub vision: bool,
    pub native_tools: Option<Vec<serde_json::Value>>,
    pub provider_tools: Vec<serde_json::Value>,
    pub tool_choice: Option<serde_json::Value>,
    pub cache: bool,
    pub prompt_cache_ttl: Option<PromptCacheTtl>,
    pub timeout: Option<u64>,
    pub idle_timeout: Option<u64>,
    pub stream: bool,
    pub provider_overrides: Option<serde_json::Value>,
    pub previous_response_id: Option<String>,
    pub store: Option<bool>,
    /// Effective posture, already merged with the catalog policy default.
    pub data_controls: harn_provider_catalog::config::DataPosture,
    #[serde(skip_serializing)]
    pub inference_boundary: Option<crate::inference::InferenceBoundary>,
    pub background: Option<bool>,
    pub truncation: Option<String>,
    pub compact: Option<bool>,
    pub include: Option<Vec<String>>,
    pub max_tool_calls: Option<i64>,
    pub prefill: Option<String>,
    /// Target for streaming tool-call events, absent outside an agent loop.
    pub session_id: Option<String>,
    #[serde(skip_serializing)]
    pub reminder_lifecycle: Vec<ReminderLifecycleEmission>,
    /// Captured before crossing onto the transport scheduler.
    #[serde(skip_serializing)]
    pub cli_llm_mock_scope: Option<u64>,
    /// Fixture purpose, separating auxiliary calls from the main mock queue.
    #[serde(skip_serializing)]
    pub mock_scope: Option<String>,
    /// Simulator completion data, ignored by real providers.
    #[serde(skip_serializing)]
    pub done_sentinel: Option<String>,
    #[serde(skip_serializing)]
    pub done_sentinel_form: Option<String>,
    /// The tools this call offers, read once from the provider tool list and
    /// the tool registry. Derived from fields the digest already covers.
    #[serde(skip_serializing)]
    pub offered_tools: Vec<OfferedTool>,
}

/// One tool a model call offered, in a provider-neutral shape.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct OfferedTool {
    pub name: String,
    /// The tool's plain-English title, such as "Read a file": its
    /// `annotations.title`, or an MCP tool's own `title`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// The provider holds this tool's schema back until a tool search
    /// surfaces it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub deferred: bool,
}
