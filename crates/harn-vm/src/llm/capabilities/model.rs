//! Capability DTOs and the wire-dialect model.
//!
//! Pure data types: the on-disk [`CapabilitiesFile`] schema, per-provider
//! [`ProviderDefaults`], the resolved [`Capabilities`] struct callers consume,
//! and the [`WireDialect`] enum that types a route's message wire format. The
//! `ProviderRule` matrix row and the resolution engine that turns these DTOs
//! into a `Capabilities` live in `super::rule`.

use std::collections::BTreeMap;

use serde::Deserialize;

use super::rule::ProviderRule;

/// Parsed on-disk capabilities schema. Public so harn-cli can
/// construct one directly when wiring harn.toml overrides.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct CapabilitiesFile {
    /// Per-provider ordered rule lists. The first matching rule wins; a
    /// matching rule with `extends = true` contributes only the fields it
    /// sets and lets resolution continue to later matching rules (see
    /// [`ProviderRule::extends`]).
    #[serde(default)]
    pub provider: BTreeMap<String, Vec<ProviderRule>>,
    /// Per-provider defaults applied to every matching row and to
    /// provider/model pairs that have no model-specific row. This keeps
    /// transport-shape facts in data without repeating them on every
    /// generation-specific capability row.
    #[serde(default)]
    pub provider_defaults: BTreeMap<String, ProviderDefaults>,
    /// Sibling → canonical family mapping. Providers with no rule of
    /// their own fall through to the named family (recursively).
    #[serde(default)]
    pub provider_family: BTreeMap<String, String>,
    /// Per-provider adaptive rate/concurrency governor limits, keyed by
    /// provider id. Consumed by `crate::llm::rate_governor` when the
    /// `llm.rate_governor` flag is enabled, so provider limits stay catalog
    /// data instead of call-site branches.
    #[serde(default)]
    pub provider_limits: BTreeMap<String, ProviderLimits>,
}

/// Adaptive-governor limits for one provider. Every field is optional so a
/// catalog fragment can pin just the axes it knows; unset axes fall back to the
/// governor's conservative built-in defaults.
#[derive(Debug, Clone, Deserialize, Default, PartialEq)]
pub struct ProviderLimits {
    /// Ceiling the AIMD concurrency limiter additively climbs toward on
    /// sustained success.
    #[serde(default)]
    pub max_concurrency: Option<u32>,
    /// Floor the AIMD limiter multiplicatively decreases toward on a throttle
    /// signal.
    #[serde(default)]
    pub min_concurrency: Option<u32>,
    /// Requests-per-minute token bucket. `None` disables the RPM bucket.
    #[serde(default)]
    pub rpm: Option<u32>,
    /// Tokens-per-minute token bucket, charged by estimated input + output
    /// tokens. `None` disables the TPM bucket.
    #[serde(default)]
    pub tpm: Option<u64>,
    /// Whether the AIMD adaptive concurrency loop is active. When `false`, the
    /// concurrency limit is pinned at `max_concurrency`.
    #[serde(default)]
    pub adaptive: Option<bool>,
    /// Circuit-breaker / backoff parameters. Absent means built-in defaults.
    #[serde(default)]
    pub backoff: Option<GovernorBackoff>,
}

/// Exponential-backoff-with-jitter parameters for the governor circuit breaker.
/// Provider `Retry-After` values always take precedence over the computed
/// window.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct GovernorBackoff {
    /// First OPEN window, in milliseconds.
    #[serde(default)]
    pub base_ms: Option<u64>,
    /// Ceiling for the OPEN window, in milliseconds.
    #[serde(default)]
    pub max_ms: Option<u64>,
    /// Growth factor applied per consecutive OPEN cycle.
    #[serde(default)]
    pub multiplier: Option<f64>,
    /// Full-jitter toggle.
    #[serde(default)]
    pub jitter: Option<bool>,
}

/// Provider-wide default fields merged into matching rules.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct ProviderDefaults {
    /// Message/request/response wire format used by shared helpers.
    /// Known values are `openai`, `anthropic`, `gemini`, and `ollama`.
    #[serde(default)]
    pub message_wire_format: Option<String>,
    /// Native tool definition wire shape. Known values are `openai`
    /// and `anthropic`.
    #[serde(default)]
    pub native_tool_wire_format: Option<String>,
    /// Whether image content blocks may reference remote URLs.
    #[serde(default)]
    pub image_url_input_supported: Option<bool>,
    /// File-upload transport used by `std/files.upload`. Known values
    /// are `anthropic` and `gemini`.
    #[serde(default)]
    pub file_upload_wire_format: Option<String>,
    /// Provider-specific reasoning request shape for OpenAI-compatible
    /// transports. Known values are `openrouter` and `enabled`.
    #[serde(default)]
    pub reasoning_wire_format: Option<String>,
    #[serde(default)]
    pub files_api_supported: Option<bool>,
    #[serde(default)]
    pub batch_api: Option<bool>,
    #[serde(default)]
    pub batch_wire_format: Option<String>,
    #[serde(default)]
    pub batch_input_mode: Option<String>,
    #[serde(default)]
    pub batch_discount_percent: Option<u32>,
    #[serde(default)]
    pub batch_turnaround_hours: Option<u32>,
    #[serde(default)]
    pub seed_supported: Option<bool>,
    #[serde(default)]
    pub top_k_supported: Option<bool>,
    #[serde(default)]
    pub temperature_supported: Option<bool>,
    #[serde(default)]
    pub top_p_supported: Option<bool>,
    #[serde(default)]
    pub frequency_penalty_supported: Option<bool>,
    #[serde(default)]
    pub presence_penalty_supported: Option<bool>,
}

/// Copies `src` into `dst` when `src` is set (last-writer-wins overlay).
pub(super) fn overlay_opt<T: Clone>(dst: &mut Option<T>, src: &Option<T>) {
    if src.is_some() {
        dst.clone_from(src);
    }
}

/// Copies `src` into `dst` only when `dst` is still unset (fill-the-gaps).
pub(super) fn fill_opt<T: Clone>(dst: &mut Option<T>, src: &Option<T>) {
    if dst.is_none() {
        dst.clone_from(src);
    }
}

/// Visits every `ProviderDefaults` field once, applying `$op` (`overlay_opt`
/// or `fill_opt`) to each `(dst, src)` pair. The field roster lives here only;
/// `overlay`/`fill_missing_from` differ solely in the merge rule they pass.
macro_rules! merge_provider_defaults {
    ($dst:expr, $src:expr, $op:path) => {{
        $op(&mut $dst.message_wire_format, &$src.message_wire_format);
        $op(
            &mut $dst.native_tool_wire_format,
            &$src.native_tool_wire_format,
        );
        $op(
            &mut $dst.image_url_input_supported,
            &$src.image_url_input_supported,
        );
        $op(
            &mut $dst.file_upload_wire_format,
            &$src.file_upload_wire_format,
        );
        $op(&mut $dst.reasoning_wire_format, &$src.reasoning_wire_format);
        $op(&mut $dst.files_api_supported, &$src.files_api_supported);
        $op(&mut $dst.batch_api, &$src.batch_api);
        $op(&mut $dst.batch_wire_format, &$src.batch_wire_format);
        $op(&mut $dst.batch_input_mode, &$src.batch_input_mode);
        $op(
            &mut $dst.batch_discount_percent,
            &$src.batch_discount_percent,
        );
        $op(
            &mut $dst.batch_turnaround_hours,
            &$src.batch_turnaround_hours,
        );
        $op(&mut $dst.seed_supported, &$src.seed_supported);
        $op(&mut $dst.top_k_supported, &$src.top_k_supported);
        $op(&mut $dst.temperature_supported, &$src.temperature_supported);
        $op(&mut $dst.top_p_supported, &$src.top_p_supported);
        $op(
            &mut $dst.frequency_penalty_supported,
            &$src.frequency_penalty_supported,
        );
        $op(
            &mut $dst.presence_penalty_supported,
            &$src.presence_penalty_supported,
        );
    }};
}

impl ProviderDefaults {
    pub(super) fn overlay(&mut self, other: &ProviderDefaults) {
        merge_provider_defaults!(self, other, overlay_opt);
    }

    pub(super) fn fill_missing_from(&mut self, other: &ProviderDefaults) {
        merge_provider_defaults!(self, other, fill_opt);
    }

    pub(super) fn has_any_field(&self) -> bool {
        self.message_wire_format.is_some()
            || self.native_tool_wire_format.is_some()
            || self.image_url_input_supported.is_some()
            || self.file_upload_wire_format.is_some()
            || self.reasoning_wire_format.is_some()
            || self.files_api_supported.is_some()
            || self.batch_api.is_some()
            || self.batch_wire_format.is_some()
            || self.batch_input_mode.is_some()
            || self.batch_discount_percent.is_some()
            || self.batch_turnaround_hours.is_some()
            || self.seed_supported.is_some()
            || self.top_k_supported.is_some()
            || self.temperature_supported.is_some()
            || self.top_p_supported.is_some()
            || self.frequency_penalty_supported.is_some()
            || self.presence_penalty_supported.is_some()
    }
}

/// The message/request/response wire dialect a route speaks.
///
/// This is the single typed representation of what used to be encoded two
/// different, drift-prone ways: the stringly `Capabilities.message_wire_format`
/// field (compared against `"anthropic"`/`"gemini"`/`"ollama"` literals at a
/// dozen call sites) and the `(is_anthropic_style, is_ollama)` boolean pair
/// threaded independently through the transport/response layers. A closed enum
/// makes an unhandled or mistyped dialect a compile error and removes the
/// boolean-blindness where two `bool`s could silently disagree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireDialect {
    /// Anthropic native Messages API (`/v1/messages`). The only dialect that
    /// surfaces Claude's extended-thinking stream. `message_wire_format =
    /// "anthropic"`.
    Anthropic,
    /// OpenAI-compatible Chat Completions (`/v1/chat/completions`). The default
    /// for hosted/openai-shape routes. `message_wire_format = "openai"`.
    OpenAiCompat,
    /// Ollama native `/api/chat`. `message_wire_format = "ollama"`.
    Ollama,
    /// Google Gemini `generateContent`. `message_wire_format = "gemini"`.
    Gemini,
}

impl WireDialect {
    /// Parse the catalog's `message_wire_format` string. Unrecognized values
    /// (including the explicit `"openai"`) resolve to [`WireDialect::OpenAiCompat`],
    /// exactly matching the pre-cutover behavior where every
    /// `== "anthropic"/"gemini"/"ollama"` check fell through to the
    /// OpenAI-compatible path.
    pub fn from_message_wire_format(value: &str) -> WireDialect {
        match value {
            "anthropic" => WireDialect::Anthropic,
            "ollama" => WireDialect::Ollama,
            "gemini" => WireDialect::Gemini,
            _ => WireDialect::OpenAiCompat,
        }
    }

    /// The canonical `message_wire_format` string for display and round-trip.
    pub fn as_str(self) -> &'static str {
        match self {
            WireDialect::Anthropic => "anthropic",
            WireDialect::OpenAiCompat => "openai",
            WireDialect::Ollama => "ollama",
            WireDialect::Gemini => "gemini",
        }
    }

    /// Whether this route speaks Anthropic's native Messages shape.
    pub fn is_anthropic(self) -> bool {
        matches!(self, WireDialect::Anthropic)
    }

    /// Whether this route speaks Ollama's native `/api/chat` shape.
    pub fn is_ollama(self) -> bool {
        matches!(self, WireDialect::Ollama)
    }

    /// Whether this route speaks Google Gemini's `generateContent` shape.
    pub fn is_gemini(self) -> bool {
        matches!(self, WireDialect::Gemini)
    }
}

/// How the neutral `computer` tool projects onto a route's native computer-use
/// surface (the `computer_use_style` capability). A typed enum rather than a raw
/// string so an unknown value in a capability source is a load-time
/// deserialize error instead of a silently-disabled computer tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComputerUseStyle {
    /// Anthropic `computer_20251124` native tool.
    NativeAnthropic,
    /// OpenAI Responses `computer` native tool.
    NativeOpenai,
    /// Accessibility / set-of-marks grounding over the universal function tool.
    Grounded,
    /// The plain function-schema `computer` tool (the universal default).
    Function,
}

/// Screenshot downscaling policy applied before an image reaches the model (the
/// `screenshot_scaling` capability). Typed for the same reason as
/// [`ComputerUseStyle`] — an unknown value fails the capability load loudly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScreenshotScaling {
    /// Fit within Anthropic's XGA (1024x768), preserving aspect ratio.
    Xga,
    /// Send the capture at its native resolution (OpenAI et al.).
    Original,
}

/// Resolved capabilities for a `(provider, model)` pair. Unset rule
/// fields resolve to `false` / empty / `None` so callers never have to
/// unwrap an `Option<bool>` for what are really boolean gates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capabilities {
    pub native_tools: bool,
    pub message_wire_format: WireDialect,
    pub native_tool_wire_format: String,
    pub defer_loading: bool,
    pub tool_search: Vec<String>,
    pub responses_api: bool,
    pub hosted_tools: Vec<String>,
    pub remote_mcp: bool,
    pub conversation_state: bool,
    pub compaction: bool,
    pub background_mode: bool,
    pub batch_api: bool,
    pub batch_wire_format: Option<String>,
    pub batch_input_mode: Option<String>,
    pub batch_discount_percent: Option<u32>,
    pub batch_turnaround_hours: Option<u32>,
    pub tool_approval_policy: Option<String>,
    pub max_tools: Option<u32>,
    pub prompt_caching: bool,
    pub cache_breakpoint_style: String,
    pub vision: bool,
    pub audio: bool,
    pub pdf: bool,
    pub video: bool,
    pub files_api_supported: bool,
    pub file_upload_wire_format: Option<String>,
    pub structured_output: Option<String>,
    /// Legacy mirror for CLI display and older callers.
    pub json_schema: Option<String>,
    pub prefers_xml_scaffolding: bool,
    /// See [`ProviderRule::reserved_tool_call_token`].
    pub reserved_tool_call_token: bool,
    pub prefers_markdown_scaffolding: bool,
    pub structured_output_mode: String,
    pub supports_assistant_prefill: bool,
    pub prefers_role_developer: bool,
    pub prefers_xml_tools: bool,
    pub thinking_block_style: String,
    /// Whether this route emits its reasoning INLINE in the text channel as
    /// `<think>...</think>` blocks (local Ollama/llama.cpp reasoning models,
    /// Qwen3 via vLLM, Kimi) rather than in a separate provider reasoning
    /// field. When true, the `llm_call` envelope builder splits those blocks
    /// out of `text`/`prose`/`visible_text` and folds them into the reasoning
    /// channel, mirroring how hosted providers surface a dedicated thinking
    /// field. Derived from `thinking_block_style == "inline"` — the same
    /// population that represents reasoning as inline `<think>` in prompt
    /// context is the one that emits it that way in responses.
    pub emits_inline_reasoning: bool,
    pub thinking_modes: Vec<String>,
    pub interleaved_thinking_supported: bool,
    pub anthropic_beta_features: Vec<String>,
    pub vision_supported: bool,
    pub image_url_input_supported: bool,
    pub preserve_thinking: bool,
    pub server_parser: String,
    pub honors_chat_template_kwargs: bool,
    pub chat_template_options_field: Option<String>,
    pub requires_completion_tokens: bool,
    /// True when the route is served ONLY by the provider Responses API and
    /// rejects `/v1/chat/completions` (OpenAI `*-codex` models). Harn routes
    /// such calls through the Responses provider automatically.
    pub chat_completions_unsupported: bool,
    pub requires_streaming: bool,
    pub reasoning_effort_supported: bool,
    pub reasoning_effort_levels: Vec<String>,
    pub reasoning_none_supported: bool,
    /// See [`ProviderRule::max_thinking_budget`]. `None` means the model uses
    /// the provider's own default ceiling.
    pub max_thinking_budget: Option<i64>,
    pub reasoning_disable_supported: bool,
    /// See [`ProviderRule::reasoning_required_for_tools`].
    pub reasoning_required_for_tools: bool,
    pub reasoning_text_promotable: bool,
    pub reasoning_wire_format: Option<String>,
    pub seed_supported: bool,
    pub top_k_supported: bool,
    pub temperature_supported: bool,
    pub top_p_supported: bool,
    pub frequency_penalty_supported: bool,
    pub presence_penalty_supported: bool,
    pub allowed_tool_choice_modes: Vec<String>,
    pub requires_tool_result_adjacency: bool,
    pub supports_parallel_tool_calls: bool,
    pub tools_exclude_response_format: bool,
    pub recommended_endpoint: Option<String>,
    pub text_tool_wire_format_supported: bool,
    pub preferred_tool_format: Option<String>,
    pub tool_mode_parity: Option<String>,
    pub tool_mode_parity_notes: Option<String>,
    pub thinking_disable_directive: Option<String>,
    /// Per-task auto-policy reasoning-level overrides for this route.
    /// See [`ProviderRule::auto_reasoning_overrides`].
    pub auto_reasoning_overrides: BTreeMap<String, String>,
    /// OpenRouter upstream provider names to exclude from routing for this
    /// row. See [`ProviderRule::provider_route_denylist`]. Empty means "no
    /// route restriction".
    pub provider_route_denylist: Vec<String>,
    /// OpenRouter upstream provider names this row is PINNED to (allowlist), in
    /// preference order. See [`ProviderRule::openrouter_provider_order`]. Empty
    /// means "no pin" (free OpenRouter routing).
    pub openrouter_provider_order: Vec<String>,
    /// Serving-quality / precision trust verdict for this route. See
    /// [`ProviderRule::serving_precision`]. `"unverified"` when unset.
    pub serving_precision: String,
    /// How the neutral `computer` tool projects onto this route's native
    /// computer-use surface. `None` means the route exposes no computer-use
    /// surface. See [`ComputerUseStyle`].
    pub computer_use_style: Option<ComputerUseStyle>,
    /// Screenshot downscaling policy applied before the image reaches the
    /// model. `None` means unset. See [`ScreenshotScaling`].
    pub screenshot_scaling: Option<ScreenshotScaling>,
    /// Whether this route requires echoing acknowledged safety checks on the
    /// computer-use follow-up turn (OpenAI Responses `pending_safety_checks`
    /// → `acknowledged_safety_checks`). See [`ProviderRule::safety_ack_flow`].
    pub safety_ack_flow: bool,
}

impl Default for Capabilities {
    fn default() -> Self {
        Self {
            native_tools: false,
            message_wire_format: WireDialect::OpenAiCompat,
            native_tool_wire_format: "openai".to_string(),
            defer_loading: false,
            tool_search: Vec::new(),
            responses_api: false,
            hosted_tools: Vec::new(),
            remote_mcp: false,
            conversation_state: false,
            compaction: false,
            background_mode: false,
            batch_api: false,
            batch_wire_format: None,
            batch_input_mode: None,
            batch_discount_percent: None,
            batch_turnaround_hours: None,
            tool_approval_policy: None,
            max_tools: None,
            prompt_caching: false,
            cache_breakpoint_style: "none".to_string(),
            vision: false,
            audio: false,
            pdf: false,
            video: false,
            files_api_supported: false,
            file_upload_wire_format: None,
            structured_output: None,
            json_schema: None,
            prefers_xml_scaffolding: false,
            reserved_tool_call_token: false,
            prefers_markdown_scaffolding: false,
            structured_output_mode: "none".to_string(),
            supports_assistant_prefill: false,
            prefers_role_developer: false,
            prefers_xml_tools: false,
            thinking_block_style: "none".to_string(),
            emits_inline_reasoning: false,
            thinking_modes: Vec::new(),
            interleaved_thinking_supported: false,
            anthropic_beta_features: Vec::new(),
            vision_supported: false,
            image_url_input_supported: true,
            preserve_thinking: false,
            server_parser: "none".to_string(),
            honors_chat_template_kwargs: false,
            chat_template_options_field: None,
            requires_completion_tokens: false,
            chat_completions_unsupported: false,
            requires_streaming: false,
            reasoning_effort_supported: false,
            reasoning_effort_levels: Vec::new(),
            reasoning_none_supported: false,
            max_thinking_budget: None,
            reasoning_disable_supported: true,
            reasoning_required_for_tools: false,
            reasoning_text_promotable: true,
            reasoning_wire_format: None,
            seed_supported: true,
            top_k_supported: true,
            temperature_supported: true,
            top_p_supported: true,
            frequency_penalty_supported: true,
            presence_penalty_supported: true,
            allowed_tool_choice_modes: Vec::new(),
            requires_tool_result_adjacency: false,
            supports_parallel_tool_calls: true,
            tools_exclude_response_format: false,
            recommended_endpoint: None,
            text_tool_wire_format_supported: true,
            preferred_tool_format: None,
            tool_mode_parity: None,
            tool_mode_parity_notes: None,
            thinking_disable_directive: None,
            auto_reasoning_overrides: BTreeMap::new(),
            provider_route_denylist: Vec::new(),
            openrouter_provider_order: Vec::new(),
            serving_precision: "unverified".to_string(),
            computer_use_style: None,
            screenshot_scaling: None,
            safety_ack_flow: false,
        }
    }
}
