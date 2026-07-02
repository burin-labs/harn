//! Data-driven provider capabilities.
//!
//! The per-(provider, model) capability matrix (native tools, deferred
//! tool loading, tool-search variants, prompt caching, extended thinking,
//! max tool count) lives in `capability_sources/**/*.toml`, which generates
//! the shipped `capabilities.toml` snapshot, and is
//! overridable per-project via `[[capabilities.provider.<name>]]` blocks
//! in `harn.toml`. This module owns:
//!
//! - loading the built-in TOML (compiled in via `include_str!`);
//! - merging user overrides on top;
//! - matching a `(provider, model)` pair against the rule list with
//!   glob + semver semantics;
//! - exposing a stable `Capabilities` struct that the `LlmProvider`
//!   trait delegates to as the single source of truth.
//!
//! Provider adapters still supply generation parsers for `version_min`, but
//! feature gates live in this data table instead of adapter-specific boolean
//! branches.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashSet};
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use super::providers::anthropic::claude_generation;
use super::providers::openai_compat::gpt_generation;

/// Generated shipped default rules. Compiled into the binary at build time.
const BUILTIN_TOML: &str = include_str!("capabilities.toml");
/// Generated provider/model snapshot built from catalog_sources/**/*.toml.
const BUILTIN_PROVIDERS_TOML: &str = include_str!("providers.toml");

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
fn overlay_opt<T: Clone>(dst: &mut Option<T>, src: &Option<T>) {
    if src.is_some() {
        dst.clone_from(src);
    }
}

/// Copies `src` into `dst` only when `dst` is still unset (fill-the-gaps).
fn fill_opt<T: Clone>(dst: &mut Option<T>, src: &Option<T>) {
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
    fn overlay(&mut self, other: &ProviderDefaults) {
        merge_provider_defaults!(self, other, overlay_opt);
    }

    fn fill_missing_from(&mut self, other: &ProviderDefaults) {
        merge_provider_defaults!(self, other, fill_opt);
    }

    fn has_any_field(&self) -> bool {
        self.message_wire_format.is_some()
            || self.native_tool_wire_format.is_some()
            || self.image_url_input_supported.is_some()
            || self.file_upload_wire_format.is_some()
            || self.reasoning_wire_format.is_some()
            || self.files_api_supported.is_some()
            || self.seed_supported.is_some()
            || self.top_k_supported.is_some()
            || self.temperature_supported.is_some()
            || self.top_p_supported.is_some()
            || self.frequency_penalty_supported.is_some()
            || self.presence_penalty_supported.is_some()
    }
}

/// One row of the capability matrix.
#[derive(Debug, Clone, Deserialize)]
pub struct ProviderRule {
    /// Glob pattern (supports leading / trailing `*` and a single mid-`*`).
    /// Matched case-insensitively against the model ID.
    pub model_match: String,
    /// Optional `[major, minor]` lower bound. When set, the model ID
    /// must parse via the provider's version extractor AND compare ≥
    /// this tuple. Rules with an unparseable `version_min` for the
    /// given model are skipped, not merged.
    #[serde(default)]
    pub version_min: Option<Vec<u32>>,
    /// Per-rule fall-through. A matching rule with `extends = true`
    /// contributes ONLY the fields it explicitly sets; resolution then
    /// continues to later matching rules (user rules before built-in rules,
    /// then the `provider_family` chain) and ultimately to provider /
    /// built-in defaults to fill the rest. A matching rule without
    /// `extends` (or with `extends = false`) terminates resolution exactly
    /// as before this flag existed. This lets an overlay tweak one field of
    /// a shipped row without copying the whole row verbatim (which drifts).
    #[serde(default)]
    pub extends: bool,
    #[serde(default)]
    pub native_tools: Option<bool>,
    /// Message/request/response wire format used by shared helpers.
    /// Known values are `openai`, `anthropic`, `gemini`, and `ollama`.
    #[serde(default)]
    pub message_wire_format: Option<String>,
    /// Native tool definition wire shape. Known values are `openai`
    /// and `anthropic`.
    #[serde(default)]
    pub native_tool_wire_format: Option<String>,
    #[serde(default)]
    pub defer_loading: Option<bool>,
    #[serde(default)]
    pub tool_search: Option<Vec<String>>,
    /// Whether Harn supports this route through the provider's native
    /// Responses-style API instead of generic chat completions.
    #[serde(default)]
    pub responses_api: Option<bool>,
    /// Provider-hosted tools Harn can pass through without local execution.
    #[serde(default)]
    pub hosted_tools: Option<Vec<String>>,
    /// Whether provider-hosted remote MCP connectors can be mediated by the
    /// provider for this route.
    #[serde(default)]
    pub remote_mcp: Option<bool>,
    /// Whether provider-managed previous-response conversation state is
    /// available.
    #[serde(default)]
    pub conversation_state: Option<bool>,
    /// Whether provider-side truncation/compaction controls are available.
    #[serde(default)]
    pub compaction: Option<bool>,
    /// Whether provider-side background Responses jobs are available.
    #[serde(default)]
    pub background_mode: Option<bool>,
    /// Approval policy modes available when provider-hosted tools execute.
    #[serde(default)]
    pub tool_approval_policy: Option<String>,
    #[serde(default)]
    pub max_tools: Option<u32>,
    #[serde(default)]
    pub prompt_caching: Option<bool>,
    /// Request-side cache breakpoint strategy for routes that require
    /// `cache_control` to opt into provider prompt caching. Known values are
    /// `none`, `top_level`, and `last_block`.
    #[serde(default)]
    pub cache_breakpoint_style: Option<String>,
    /// Whether this provider/model route accepts image or other visual
    /// input blocks through Harn's LLM message path.
    #[serde(default)]
    pub vision: Option<bool>,
    /// Whether this provider/model route accepts audio input blocks
    /// through Harn's LLM message path.
    #[serde(default, alias = "audio_supported")]
    pub audio: Option<bool>,
    /// Whether this provider/model route accepts PDF/document input blocks
    /// through Harn's LLM message path.
    #[serde(default, alias = "pdf_supported")]
    pub pdf: Option<bool>,
    /// Whether this provider/model route accepts video input blocks
    /// through Harn's LLM message path.
    #[serde(default, alias = "video_supported")]
    pub video: Option<bool>,
    /// Whether uploaded file references can be reused in message content.
    #[serde(default)]
    pub files_api_supported: Option<bool>,
    /// File-upload transport used by `std/files.upload`. Known values
    /// are `anthropic` and `gemini`.
    #[serde(default)]
    pub file_upload_wire_format: Option<String>,
    /// Structured-output transport strategy. Known values are:
    /// `native`, `tool_use`, `format_kw`, and `none`.
    #[serde(default)]
    pub structured_output: Option<String>,
    /// Legacy name retained for project overrides written before
    /// `structured_output` became the canonical capability.
    #[serde(default)]
    pub json_schema: Option<String>,
    /// Whether prompt sections should prefer XML-style tags such as
    /// `<task>` / `<examples>` over Markdown headings.
    #[serde(default)]
    pub prefers_xml_scaffolding: Option<bool>,
    /// Whether this model's tokenizer reserves `<tool_call>` / `</tool_call>`
    /// as single special tokens (the native Hermes tool-call markers). When
    /// true, harn remaps those delimiters to a non-special bracket form on the
    /// wire to avoid degenerate opener repetition; see [`crate::llm::tool_delimiter`].
    #[serde(default)]
    pub reserved_tool_call_token: Option<bool>,
    /// Whether prompt sections should prefer Markdown headings such as
    /// `## Task` / `## Examples`.
    #[serde(default)]
    pub prefers_markdown_scaffolding: Option<bool>,
    /// Preferred logical structured-output prompt shape. This is separate
    /// from the transport-level `structured_output` strategy above.
    /// Known values are `native_json`, `delimited`, and `xml_tagged`.
    #[serde(default)]
    pub structured_output_mode: Option<String>,
    /// Whether the route accepts an assistant-role prefill message.
    #[serde(default)]
    pub supports_assistant_prefill: Option<bool>,
    /// Whether durable instructions should use OpenAI's `developer` role
    /// instead of `system`.
    #[serde(default)]
    pub prefers_role_developer: Option<bool>,
    /// Whether text-rendered tool specifications should use XML wrappers
    /// instead of JSON-schema prose.
    #[serde(default)]
    pub prefers_xml_tools: Option<bool>,
    /// Preferred representation for model thinking/reasoning blocks in
    /// transcript-like prompt context. Known values are `none`,
    /// `thinking_blocks`, `reasoning_summary`, and `inline`.
    #[serde(default)]
    pub thinking_block_style: Option<String>,
    /// Supported thinking/reasoning modes for this rule. Values are
    /// script-facing mode names: `enabled`, `adaptive`, and `effort`.
    #[serde(default)]
    pub thinking_modes: Option<Vec<String>>,
    /// Whether Anthropic interleaved thinking is supported for this
    /// provider/model route.
    #[serde(default)]
    pub interleaved_thinking_supported: Option<bool>,
    /// Anthropic beta features that should be requested for this route.
    #[serde(default)]
    pub anthropic_beta_features: Option<Vec<String>>,
    /// Legacy override compatibility. New built-in rules should use
    /// `thinking_modes` so the capability matrix preserves mode detail.
    #[serde(default)]
    pub thinking: Option<bool>,
    /// Whether the model accepts image inputs in chat content.
    #[serde(default)]
    pub vision_supported: Option<bool>,
    /// Whether image content blocks may reference remote URLs.
    #[serde(default)]
    pub image_url_input_supported: Option<bool>,
    /// Carry `<think>...</think>` blocks in assistant history across turns.
    /// Qwen3.6 exposes this as `chat_template_kwargs.preserve_thinking`;
    /// Alibaba recommends enabling it for long-horizon agent loops so the
    /// model doesn't re-derive context it already worked out in prior turns.
    /// Anthropic's adaptive-thinking signature contract is stricter but plays
    /// the same role there.
    #[serde(default)]
    pub preserve_thinking: Option<bool>,
    /// Name of any server-side response parser that can transform model
    /// bytes before Harn sees them. `none` means the provider returns the
    /// model text/tool channel without an implicit parser.
    #[serde(default)]
    pub server_parser: Option<String>,
    /// Whether provider-specific chat-template options are honored. Most
    /// OpenAI-compatible servers call this `chat_template_kwargs`; Baseten's
    /// Model APIs spell the same concept `chat_template_args`.
    #[serde(default)]
    pub honors_chat_template_kwargs: Option<bool>,
    /// Request body field for provider-specific chat-template options when it
    /// differs from the default `chat_template_kwargs`.
    #[serde(default)]
    pub chat_template_options_field: Option<String>,
    /// Whether this route requires OpenAI's `max_completion_tokens`
    /// request field instead of legacy `max_tokens`.
    #[serde(default)]
    pub requires_completion_tokens: Option<bool>,
    /// Whether this route rejects non-streaming chat-completion requests.
    /// Harn forces streaming for such routes so callers can keep provider-
    /// neutral `stream` preferences.
    #[serde(default)]
    pub requires_streaming: Option<bool>,
    /// Whether this route accepts Harn's provider-neutral reasoning effort
    /// control. Providers project this to their native field (for example
    /// OpenAI `reasoning_effort` or Anthropic `output_config.effort`).
    #[serde(default)]
    pub reasoning_effort_supported: Option<bool>,
    /// Accepted effort values for routes that expose a narrower subset than
    /// Harn's provider-neutral enum. Empty means "unknown/all".
    #[serde(default)]
    pub reasoning_effort_levels: Option<Vec<String>>,
    /// Whether this route accepts effort "none" as a true reasoning-off
    /// setting. Older GPT-5 variants support effort but only floor at
    /// `minimal`.
    #[serde(default)]
    pub reasoning_none_supported: Option<bool>,
    /// Maximum thinking-budget tokens this model accepts for its high/xhigh/max
    /// reasoning levels, when the provider takes an explicit token budget rather
    /// than an effort enum. The canonical case is the native Gemini API
    /// `generationConfig.thinkingConfig.thinkingBudget` field, whose ceiling
    /// differs by model (Gemini 2.5 Flash caps at 24576, Pro at 32768).
    /// Declared alongside the model's other wire capabilities instead of a
    /// hard-coded `model.contains("flash")` branch in the provider.
    #[serde(default)]
    pub max_thinking_budget: Option<i64>,
    /// Whether this route accepts an explicit disabled/off reasoning switch.
    /// Some routes require reasoning and reject the provider's disabled shape.
    #[serde(default)]
    pub reasoning_disable_supported: Option<bool>,
    /// Whether this model performs *tool calls inside its reasoning channel*,
    /// so disabling reasoning silently breaks tool calling. The canonical case
    /// is the OpenAI gpt-oss (Harmony) family: with reasoning disabled it emits
    /// 0 tool_calls and a tiny billed-noncommittal completion; with reasoning
    /// enabled (even `low`) it emits clean native tool calls. This is the
    /// *opposite* of the Qwen3 quirk (Qwen narrates tool intent in the
    /// reasoning trace and emits zero `tool_calls`, so Qwen needs reasoning
    /// OFF for tools). When set, `reasoning_policy` refuses to downgrade the
    /// auto reasoning level to `off` for tool-bearing tasks (agent/code/verify)
    /// — flooring instead to the lowest supported effort — so no future
    /// auto-policy default or session pin can re-introduce the
    /// billed-noncommittal failure at the data layer.
    #[serde(default)]
    pub reasoning_required_for_tools: Option<bool>,
    /// Whether reasoning-only clean stops may be promoted into visible text.
    /// Disable this for providers whose `reasoning` field is always private
    /// trace, even when `content` is empty.
    #[serde(default)]
    pub reasoning_text_promotable: Option<bool>,
    /// Provider-specific reasoning request shape for OpenAI-compatible
    /// transports. Known values are `openrouter`, `enabled`, and `minimax`.
    #[serde(default)]
    pub reasoning_wire_format: Option<String>,
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
    /// Accepted provider-native `tool_choice` modes. Empty means unrestricted
    /// or unknown. Use this for routes whose native tools work, but whose API
    /// rejects forced/specified tool choices.
    #[serde(default)]
    pub allowed_tool_choice_modes: Option<Vec<String>>,
    /// Whether an assistant `tool_calls` message must be followed immediately
    /// by `role=tool` messages for every emitted `tool_call_id`.
    #[serde(default)]
    pub requires_tool_result_adjacency: Option<bool>,
    /// Whether a single assistant message may contain multiple tool calls.
    /// Some OpenAI-compatible providers reject replayed history with more than
    /// one `tool_calls[]` entry even when the calls were parsed from Harn's text
    /// tool protocol, so the request builder must serialize history as
    /// one-call assistant turns for those routes.
    #[serde(default)]
    pub supports_parallel_tool_calls: Option<bool>,
    /// Whether the route rejects `response_format` when native `tools` are
    /// present. Strict OpenAI-compatible servers such as Cerebras accept each
    /// feature alone but reject the pair together.
    #[serde(default)]
    pub tools_exclude_response_format: Option<bool>,
    /// Preferred endpoint family for this provider/model route. Values
    /// are descriptive labels consumed by providers, e.g.
    /// `/api/generate-raw` for Ollama raw prompt bypass.
    #[serde(default)]
    pub recommended_endpoint: Option<String>,
    /// Whether Harn's text-tool protocol (`<tool_call>name({...})`) can
    /// survive the provider route and return in the visible response body.
    #[serde(default)]
    pub text_tool_wire_format_supported: Option<bool>,
    /// Preferred tool-calling mode for this provider/model route when
    /// callers do not explicitly choose `tool_format`. This lets the
    /// capability matrix route around known provider-native regressions
    /// without making presets branch on model names.
    #[serde(default)]
    pub preferred_tool_format: Option<String>,
    /// Empirical native/text interchangeability status for this route.
    /// Known values are descriptive, not gates: `interchangeable`,
    /// `native_unreliable`, `text_unreliable`, `native_only`,
    /// `text_only`, and `unknown`.
    #[serde(default)]
    pub tool_mode_parity: Option<String>,
    /// Short human-readable note explaining `tool_mode_parity`.
    #[serde(default)]
    pub tool_mode_parity_notes: Option<String>,
    /// In-prompt directive that disables this model's "thinking" mode when
    /// the API doesn't expose a first-class field (or exposes it
    /// inconsistently across templates / quantizations). For Qwen3 family
    /// chat templates this is `/no_think`. When `thinking: false` is
    /// requested and this is set, Harn auto-prepends the directive to the
    /// system message so script authors don't need to know it exists.
    #[serde(default)]
    pub thinking_disable_directive: Option<String>,
    /// Per-task auto-policy reasoning-level overrides for this route.
    /// Keys are task labels (`agent`, `verify`, `chat`, `summarize`,
    /// `code`); values are reasoning levels (`off`, `minimal`, `low`,
    /// `medium`, `high`, `xhigh`, `max`). Consulted by `reasoning_policy` only
    /// when policy resolves to `auto` — explicit policies always win.
    ///
    /// Use this to declare known per-model regressions that should
    /// flip the auto-policy default, instead of hard-coding the model/
    /// provider pattern in resolver code. The canonical example is the
    /// Qwen3 tool-call regression — `{ agent = "off" }` disables
    /// reasoning whenever a script registers tools with that route,
    /// matching Qwen's own published guidance.
    #[serde(default)]
    pub auto_reasoning_overrides: Option<BTreeMap<String, String>>,
    /// OpenRouter upstream provider names that must be excluded from routing
    /// for this `(provider, model)` row. Materialized into the request body's
    /// `provider.ignore` array (see
    /// [`crate::llm::providers::openai_compat::apply_openrouter_route_denylist`]).
    /// This is a data-driven route-around for upstreams that serve a route
    /// incorrectly while still advertising the model — the canonical case is
    /// OpenRouter's `Ambient` upstream billing reasoning tokens for
    /// `qwen/qwen3.6-35b-a3b` and then finishing with empty `tool_calls`,
    /// while Parasail / AtlasCloud / AkashML serve the identical request
    /// natively. Only consulted for the `openrouter` provider.
    #[serde(default)]
    pub provider_route_denylist: Option<Vec<String>>,
    /// OpenRouter upstream provider names this `(provider, model)` row is
    /// PINNED to, in preference order. Materialized into the request body's
    /// `provider.order` array with `allow_fallbacks = false` (see
    /// [`crate::llm::providers::openai_compat::apply_openrouter_provider_order`]),
    /// so OpenRouter only ever routes the model to these known-clean upstreams
    /// and never silently falls back to a sketchier one. This is the
    /// *allowlist* counterpart to [`Self::provider_route_denylist`]: prefer it
    /// when the bad upstreams are intermittent / hard to enumerate but the
    /// clean ones are few and stable. The canonical case is OpenRouter's
    /// `openai/gpt-oss-*` route, which fans out across ~17 upstreams in a
    /// sub-provider lottery; some mis-serialize the Harmony tool call even with
    /// reasoning ON (billed-noncommittal: 0 tool_calls), while Cerebras and
    /// Groq serve it cleanly. Only consulted for the `openrouter` provider. An
    /// empty / unset list means "no pin" (free OpenRouter routing). When both a
    /// pin and a denylist are present the pin wins (a closed allowlist already
    /// excludes everything not on it). Validated by the footgun gate in
    /// [`crate::llm::capability_audit`].
    #[serde(default)]
    pub openrouter_provider_order: Option<Vec<String>>,
    /// Serving-quality / precision trust verdict for this `(provider, model)`
    /// route. A provider can be live and fast yet still serve a model at
    /// DEGRADED quality (e.g. an undocumented quantization) or reject otherwise
    /// valid requests, silently contaminating any eval/meter that trusts its
    /// numbers. This is the data-driven sibling of [`Self::provider_route_denylist`]
    /// / [`Self::openrouter_provider_order`]: instead of routing *around* a bad
    /// upstream, it labels the route's measured precision so tooling (the
    /// meter precision canary) can refuse to trust a `degraded` route and flag a
    /// `throttled` one. Known values are `trusted` (full-precision verified
    /// against a reference), `degraded` (proven to serve at reduced quality),
    /// `throttled` (full-precision but rate-limited to unusable timing), and
    /// `unverified` (no verdict — treated the same as unset). Unset means
    /// `unverified`.
    #[serde(default)]
    pub serving_precision: Option<String>,
}

impl ProviderRule {
    /// Fill every capability field that `self` (the accumulated `extends`
    /// fall-through chain so far) has NOT explicitly set from `other`, a
    /// later matching rule with lower precedence. "Explicitly set" is the
    /// serde `Option` raw-deserialization state — never inferred from a
    /// field's value equaling the default.
    ///
    /// The destructure of `other` is deliberately exhaustive (no `..`
    /// catch-all): adding a new capability field to [`ProviderRule`] fails
    /// to compile here until the merge handles it.
    fn fill_missing_from(&mut self, other: &ProviderRule) {
        let ProviderRule {
            // Rule-matching metadata, not capability payload: the merged
            // chain keeps the first (highest-precedence) rule's identity.
            model_match: _,
            version_min: _,
            extends: _,
            native_tools,
            message_wire_format,
            native_tool_wire_format,
            defer_loading,
            tool_search,
            responses_api,
            hosted_tools,
            remote_mcp,
            conversation_state,
            compaction,
            background_mode,
            tool_approval_policy,
            max_tools,
            prompt_caching,
            cache_breakpoint_style,
            vision,
            audio,
            pdf,
            video,
            files_api_supported,
            file_upload_wire_format,
            structured_output,
            json_schema,
            prefers_xml_scaffolding,
            reserved_tool_call_token,
            prefers_markdown_scaffolding,
            structured_output_mode,
            supports_assistant_prefill,
            prefers_role_developer,
            prefers_xml_tools,
            thinking_block_style,
            thinking_modes,
            interleaved_thinking_supported,
            anthropic_beta_features,
            thinking,
            vision_supported,
            image_url_input_supported,
            preserve_thinking,
            server_parser,
            honors_chat_template_kwargs,
            chat_template_options_field,
            requires_completion_tokens,
            requires_streaming,
            reasoning_effort_supported,
            reasoning_effort_levels,
            reasoning_none_supported,
            max_thinking_budget,
            reasoning_disable_supported,
            reasoning_required_for_tools,
            reasoning_text_promotable,
            reasoning_wire_format,
            seed_supported,
            top_k_supported,
            temperature_supported,
            top_p_supported,
            frequency_penalty_supported,
            presence_penalty_supported,
            allowed_tool_choice_modes,
            requires_tool_result_adjacency,
            supports_parallel_tool_calls,
            tools_exclude_response_format,
            recommended_endpoint,
            text_tool_wire_format_supported,
            preferred_tool_format,
            tool_mode_parity,
            tool_mode_parity_notes,
            thinking_disable_directive,
            auto_reasoning_overrides,
            provider_route_denylist,
            openrouter_provider_order,
            serving_precision,
        } = other;
        fill_opt(&mut self.native_tools, native_tools);
        fill_opt(&mut self.message_wire_format, message_wire_format);
        fill_opt(&mut self.native_tool_wire_format, native_tool_wire_format);
        fill_opt(&mut self.defer_loading, defer_loading);
        fill_opt(&mut self.tool_search, tool_search);
        fill_opt(&mut self.responses_api, responses_api);
        fill_opt(&mut self.hosted_tools, hosted_tools);
        fill_opt(&mut self.remote_mcp, remote_mcp);
        fill_opt(&mut self.conversation_state, conversation_state);
        fill_opt(&mut self.compaction, compaction);
        fill_opt(&mut self.background_mode, background_mode);
        fill_opt(&mut self.tool_approval_policy, tool_approval_policy);
        fill_opt(&mut self.max_tools, max_tools);
        fill_opt(&mut self.prompt_caching, prompt_caching);
        fill_opt(&mut self.cache_breakpoint_style, cache_breakpoint_style);
        fill_opt(&mut self.audio, audio);
        fill_opt(&mut self.pdf, pdf);
        fill_opt(&mut self.video, video);
        fill_opt(&mut self.files_api_supported, files_api_supported);
        fill_opt(&mut self.file_upload_wire_format, file_upload_wire_format);
        fill_opt(&mut self.prefers_xml_scaffolding, prefers_xml_scaffolding);
        fill_opt(&mut self.reserved_tool_call_token, reserved_tool_call_token);
        fill_opt(
            &mut self.prefers_markdown_scaffolding,
            prefers_markdown_scaffolding,
        );
        fill_opt(&mut self.structured_output_mode, structured_output_mode);
        fill_opt(
            &mut self.supports_assistant_prefill,
            supports_assistant_prefill,
        );
        fill_opt(&mut self.prefers_role_developer, prefers_role_developer);
        fill_opt(&mut self.prefers_xml_tools, prefers_xml_tools);
        fill_opt(&mut self.thinking_block_style, thinking_block_style);
        fill_opt(
            &mut self.interleaved_thinking_supported,
            interleaved_thinking_supported,
        );
        fill_opt(&mut self.anthropic_beta_features, anthropic_beta_features);
        fill_opt(
            &mut self.image_url_input_supported,
            image_url_input_supported,
        );
        fill_opt(&mut self.preserve_thinking, preserve_thinking);
        fill_opt(&mut self.server_parser, server_parser);
        fill_opt(
            &mut self.honors_chat_template_kwargs,
            honors_chat_template_kwargs,
        );
        fill_opt(
            &mut self.chat_template_options_field,
            chat_template_options_field,
        );
        fill_opt(
            &mut self.requires_completion_tokens,
            requires_completion_tokens,
        );
        fill_opt(&mut self.requires_streaming, requires_streaming);
        fill_opt(
            &mut self.reasoning_effort_supported,
            reasoning_effort_supported,
        );
        fill_opt(&mut self.reasoning_effort_levels, reasoning_effort_levels);
        fill_opt(&mut self.reasoning_none_supported, reasoning_none_supported);
        fill_opt(&mut self.max_thinking_budget, max_thinking_budget);
        fill_opt(
            &mut self.reasoning_disable_supported,
            reasoning_disable_supported,
        );
        fill_opt(
            &mut self.reasoning_required_for_tools,
            reasoning_required_for_tools,
        );
        fill_opt(
            &mut self.reasoning_text_promotable,
            reasoning_text_promotable,
        );
        fill_opt(&mut self.reasoning_wire_format, reasoning_wire_format);
        fill_opt(&mut self.seed_supported, seed_supported);
        fill_opt(&mut self.top_k_supported, top_k_supported);
        fill_opt(&mut self.temperature_supported, temperature_supported);
        fill_opt(&mut self.top_p_supported, top_p_supported);
        fill_opt(
            &mut self.frequency_penalty_supported,
            frequency_penalty_supported,
        );
        fill_opt(
            &mut self.presence_penalty_supported,
            presence_penalty_supported,
        );
        fill_opt(
            &mut self.allowed_tool_choice_modes,
            allowed_tool_choice_modes,
        );
        fill_opt(
            &mut self.requires_tool_result_adjacency,
            requires_tool_result_adjacency,
        );
        fill_opt(
            &mut self.supports_parallel_tool_calls,
            supports_parallel_tool_calls,
        );
        fill_opt(
            &mut self.tools_exclude_response_format,
            tools_exclude_response_format,
        );
        fill_opt(&mut self.recommended_endpoint, recommended_endpoint);
        fill_opt(
            &mut self.text_tool_wire_format_supported,
            text_tool_wire_format_supported,
        );
        fill_opt(&mut self.preferred_tool_format, preferred_tool_format);
        fill_opt(&mut self.tool_mode_parity, tool_mode_parity);
        fill_opt(&mut self.tool_mode_parity_notes, tool_mode_parity_notes);
        fill_opt(
            &mut self.thinking_disable_directive,
            thinking_disable_directive,
        );
        fill_opt(&mut self.auto_reasoning_overrides, auto_reasoning_overrides);
        fill_opt(&mut self.provider_route_denylist, provider_route_denylist);
        fill_opt(
            &mut self.openrouter_provider_order,
            openrouter_provider_order,
        );
        fill_opt(&mut self.serving_precision, serving_precision);
        // Legacy alias pairs resolve as ONE logical capability
        // (`rule_structured_output`, `rule_thinking_modes`, `rule_vision`),
        // so they fill as a unit: when the accumulated chain has explicitly
        // set either spelling, the later rule's pair must not leak through
        // the other spelling and override that explicit choice.
        if self.structured_output.is_none() && self.json_schema.is_none() {
            self.structured_output.clone_from(structured_output);
            self.json_schema.clone_from(json_schema);
        }
        if self.thinking_modes.is_none() && self.thinking.is_none() {
            self.thinking_modes.clone_from(thinking_modes);
            self.thinking.clone_from(thinking);
        }
        if self.vision.is_none() && self.vision_supported.is_none() {
            self.vision.clone_from(vision);
            self.vision_supported.clone_from(vision_supported);
        }
    }
}

/// Resolved capabilities for a `(provider, model)` pair. Unset rule
/// fields resolve to `false` / empty / `None` so callers never have to
/// unwrap an `Option<bool>` for what are really boolean gates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capabilities {
    pub native_tools: bool,
    pub message_wire_format: String,
    pub native_tool_wire_format: String,
    pub defer_loading: bool,
    pub tool_search: Vec<String>,
    pub responses_api: bool,
    pub hosted_tools: Vec<String>,
    pub remote_mcp: bool,
    pub conversation_state: bool,
    pub compaction: bool,
    pub background_mode: bool,
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
}

impl Default for Capabilities {
    fn default() -> Self {
        Self {
            native_tools: false,
            message_wire_format: "openai".to_string(),
            native_tool_wire_format: "openai".to_string(),
            defer_loading: false,
            tool_search: Vec::new(),
            responses_api: false,
            hosted_tools: Vec::new(),
            remote_mcp: false,
            conversation_state: false,
            compaction: false,
            background_mode: false,
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
        }
    }
}

/// Display-oriented row for `harn provider catalog matrix`, the legacy
/// `harn check --provider-matrix` surface, and the generated docs page. Rows
/// are intentionally rule-shaped: `model` is the rule's `model_match` pattern,
/// because the shipped capability source of truth is a first-match rule table
/// rather than an exhaustive remote model inventory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProviderCapabilityMatrixRow {
    pub provider: String,
    pub model: String,
    pub version_min: Option<Vec<u32>>,
    /// Whether this rule opts into field-wise fall-through
    /// ([`ProviderRule::extends`]). Rows in this matrix are rule-shaped, so
    /// an `extends` row honestly reports its OWN fields only — for a
    /// matching model, unset fields resolve from later matching rows and
    /// provider defaults rather than the printed per-rule values.
    pub extends: bool,
    pub thinking: Vec<String>,
    pub vision: bool,
    pub audio: bool,
    pub pdf: bool,
    pub video: bool,
    pub streaming: bool,
    pub files_api_supported: bool,
    pub json_schema: Option<String>,
    pub prefers_xml_scaffolding: bool,
    pub reserved_tool_call_token: bool,
    pub prefers_markdown_scaffolding: bool,
    pub structured_output_mode: String,
    pub supports_assistant_prefill: bool,
    pub prefers_role_developer: bool,
    pub prefers_xml_tools: bool,
    pub thinking_block_style: String,
    pub native_tools: bool,
    pub text_tools: bool,
    pub preferred_tool_format: String,
    pub tool_mode_parity: String,
    pub tools: bool,
    pub cache: bool,
    /// Serving-quality / precision trust verdict for this route. See
    /// [`ProviderRule::serving_precision`]. `"unverified"` when unset.
    pub serving_precision: String,
    pub source: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ToolCapabilityAuditReport {
    pub audited_models: usize,
    pub gaps: Vec<ToolCapabilityAuditGap>,
}

impl ToolCapabilityAuditReport {
    pub fn ok(&self) -> bool {
        self.gaps.is_empty()
    }

    pub fn render_human(&self) -> String {
        if self.gaps.is_empty() {
            return format!(
                "provider capability audit OK: {} priced chat models have explicit native_tools and preferred_tool_format rules",
                self.audited_models
            );
        }

        let mut out = format!(
            "provider capability audit found {} catalog gaps among {} priced chat models:",
            self.gaps.len(),
            self.audited_models
        );
        for gap in &self.gaps {
            let matched = match (&gap.rule_provider, &gap.rule_model_match) {
                (Some(provider), Some(model_match)) => {
                    format!("provider.{provider} model_match=\"{model_match}\"")
                }
                _ => "no matching rule".to_string(),
            };
            out.push_str(&format!(
                "\n- {}:{} ({matched}) missing {}; suggest native_tools = {}, preferred_tool_format = \"{}\"",
                gap.provider,
                gap.model,
                gap.missing_fields.join(", "),
                gap.suggested_native_tools,
                gap.suggested_preferred_tool_format,
            ));
        }
        out
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ToolCapabilityAuditGap {
    pub provider: String,
    pub model: String,
    pub rule_provider: Option<String>,
    pub rule_model_match: Option<String>,
    pub missing_fields: Vec<String>,
    pub suggested_native_tools: bool,
    pub suggested_preferred_tool_format: String,
}

thread_local! {
    /// Per-thread user overrides installed by the CLI at startup. Kept
    /// thread-local (not process-static) to match the rest of the VM
    /// state model — the VM is !Send and each VM thread owns its own
    /// configuration.
    static USER_OVERRIDES: RefCell<Option<CapabilitiesFile>> = const { RefCell::new(None) };
}

/// Lazily-parsed built-in rules. The `include_str!` content is a static
/// constant; parsing it once per process is safe and free of ordering
/// hazards.
static BUILTIN: OnceLock<CapabilitiesFile> = OnceLock::new();

fn builtin() -> &'static CapabilitiesFile {
    BUILTIN.get_or_init(|| {
        toml::from_str::<CapabilitiesFile>(BUILTIN_TOML)
            .expect("capabilities.toml must parse at build time")
    })
}

/// The shipped (built-in) capability matrix. Public so the footgun gate in
/// [`crate::llm::capability_audit`] can audit exactly what Harn ships.
pub fn builtin_file() -> &'static CapabilitiesFile {
    builtin()
}

/// Install project-level overrides for the current thread. Usually
/// called once at CLI bootstrap after reading `harn.toml`. Passing
/// `None` clears any prior override.
pub fn set_user_overrides(file: Option<CapabilitiesFile>) {
    USER_OVERRIDES.with(|cell| *cell.borrow_mut() = file);
}

/// Clear any thread-local user overrides. Used between test runs.
pub fn clear_user_overrides() {
    set_user_overrides(None);
}

/// Parse a TOML string containing the capabilities section's own shape
/// (i.e. top-level `[[provider.X]]` + optional `[provider_family]`, the
/// same layout used by the built-in `capabilities.toml`) and install as
/// the current thread's override.
pub fn set_user_overrides_toml(src: &str) -> Result<(), String> {
    set_user_overrides(Some(parse_capabilities_toml(src)?));
    Ok(())
}

/// Parse a capabilities TOML document (the same layout used by the built-in
/// `capabilities.toml`) without installing it anywhere, for callers that
/// thread an explicit capability overlay instead of mutating thread state
/// (e.g. `harn provider catalog export --capabilities-overlay`).
pub fn parse_capabilities_toml(src: &str) -> Result<CapabilitiesFile, String> {
    toml::from_str(src).map_err(|e| e.to_string())
}

/// Extract the `[capabilities]` section from a full `harn.toml` source
/// and install it as the current thread's override. The schema inside
/// that section mirrors `CapabilitiesFile` but with every key prefixed
/// by `capabilities.`:
///
/// ```toml
/// [[capabilities.provider.my-proxy]]
/// model_match = "*"
/// native_tools = true
/// tool_search = ["hosted"]
/// ```
pub fn set_user_overrides_from_manifest_toml(src: &str) -> Result<(), String> {
    #[derive(Deserialize)]
    struct Manifest {
        #[serde(default)]
        capabilities: Option<CapabilitiesFile>,
    }
    let parsed: Manifest = toml::from_str(src).map_err(|e| e.to_string())?;
    set_user_overrides(parsed.capabilities);
    Ok(())
}

/// Look up effective capabilities for a `(provider, model)` pair.
/// Walks the provider_family chain until it finds a rule list that
/// matches. Within any one provider's rule list, user overrides are
/// consulted before the built-in rules. The first matching rule wins —
/// later rules (and later layers in the family chain) are ignored —
/// unless it sets `extends = true`, in which case it contributes only the
/// fields it explicitly sets and resolution continues to later matching
/// rules (and ultimately provider / built-in defaults) to fill the rest.
pub fn lookup(provider: &str, model: &str) -> Capabilities {
    let user = USER_OVERRIDES.with(|cell| cell.borrow().clone());
    lookup_with_user_overrides(provider, model, user.as_ref())
}

pub fn lookup_with_user_overrides(
    provider: &str,
    model: &str,
    user_overrides: Option<&CapabilitiesFile>,
) -> Capabilities {
    let mut caps = lookup_with(provider, model, builtin(), user_overrides);
    if provider != "openai" && provider != "mock" {
        caps.responses_api = false;
        caps.hosted_tools.clear();
        caps.remote_mcp = false;
        caps.conversation_state = false;
        caps.compaction = false;
        caps.background_mode = false;
        caps.tool_approval_policy = None;
    }
    caps
}

/// The wire channel a `tool_format` string flows through. `native` is the
/// provider's structured `tool_calls` JSON channel; `text` and `json` are
/// text-channel grammars carried in assistant content. Mirrors
/// `llm_config::ToolFormatChannel`, kept local so the capability registry
/// (the single source of truth for tool-call dialect validity) has no
/// dependency on the resolver.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolFormatWire {
    /// Provider-native JSON tool calling (`tool_format = "native"`).
    Native,
    /// A text-channel grammar (`tool_format = "text"` or `"json"`).
    Text,
}

impl ToolFormatWire {
    /// Classify a `tool_format` string. Returns `None` for unknown values so
    /// callers can reject typos loudly rather than guessing a channel.
    pub fn classify(tool_format: &str) -> Option<Self> {
        match tool_format {
            "native" => Some(Self::Native),
            "text" | "json" => Some(Self::Text),
            _ => None,
        }
    }
}

/// Outcome of validating a requested `(provider, model, tool_format)` combo
/// against the capability registry's tool-call dialect validity model.
///
/// This is the FOOTGUN-REMOVAL contract: a harness developer can ask for any
/// tool_format, and the registry guarantees the resolved format is one that
/// actually yields parseable tool calls for that route — auto-correcting a
/// known-broken combo (e.g. a `native` pin on a `native_unreliable` route that
/// silently drops to unparsed DSML text) and explaining why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolFormatDecision {
    /// The tool_format that should actually be used on the wire. Equal to the
    /// requested format when the combo was already valid; otherwise the
    /// registry's `preferred_tool_format` for the route.
    pub effective: String,
    /// Set when the requested format was overridden. Human-readable, names the
    /// bad combo and the working alternative — surface this to the harness
    /// developer so vanishing tool calls are never silent.
    pub correction: Option<String>,
}

impl ToolFormatDecision {
    fn accepted(format: String) -> Self {
        Self {
            effective: format,
            correction: None,
        }
    }
}

/// True when a route's `tool_mode_parity` says the native (provider JSON)
/// channel cannot be trusted to yield parseable tool calls. `unsupported`
/// (no working channel) is intentionally excluded: there is no better format
/// to steer to, so the gate leaves such a route alone rather than rewriting to
/// another broken channel under a misleading "Using X instead" message.
fn parity_forbids_native(parity: &str) -> bool {
    matches!(parity, "native_unreliable" | "text_only")
}

/// True when a route's `tool_mode_parity` says a text-channel grammar cannot be
/// trusted to yield parseable tool calls. See [`parity_forbids_native`] for why
/// `unsupported` is excluded.
fn parity_forbids_text(parity: &str) -> bool {
    matches!(parity, "text_unreliable" | "native_only")
}

/// True when the requested wire channel is known not to return parseable tool
/// calls for a route. The gate auto-corrects only on *positive* evidence of
/// breakage, never on a "we don't know" default:
///
/// - `tool_mode_parity` is an explicit verdict (`parity_forbids_*`).
/// - `text_tool_wire_format_supported = false` is an explicit declaration that
///   the text channel does not survive this route (e.g. native-only local
///   Ollama Qwen3 rows that omit a parity string). It defaults to `true`, so an
///   unknown route is never wrongly judged text-broken.
///
/// `native_tools` is deliberately NOT consulted here: it defaults to `false`
/// for unknown providers, so treating `!native_tools` as "native is broken"
/// would wrongly rewrite a custom proxy that does support native tools. The
/// hard `native` + `!native_tools` capability gate in `extract_llm_options`
/// already rejects a genuine native-on-non-native mismatch loudly.
fn channel_forbidden(wire: ToolFormatWire, caps: &Capabilities) -> bool {
    let parity = caps.tool_mode_parity.as_deref().unwrap_or("unknown");
    match wire {
        ToolFormatWire::Native => parity_forbids_native(parity),
        ToolFormatWire::Text => {
            parity_forbids_text(parity) || !caps.text_tool_wire_format_supported
        }
    }
}

/// Validate (and, where the registry knows better, auto-correct) a requested
/// `tool_format` for a `(provider, model)` route.
///
/// This is the single enforcement seam for tool-call dialect validity. The
/// capability registry already declares, per route, which channel actually
/// returns parseable tool calls (`tool_mode_parity`) and which format to use
/// (`preferred_tool_format`). Before this function those fields were advisory
/// metadata that any alias pin or explicit `--tool-format` flag could silently
/// override — the footgun behind the DeepSeek V3.2 DSML "vanishing tool calls"
/// dead-abstain. Now any combo whose requested channel is forbidden — by the
/// route's `tool_mode_parity` verdict OR by an explicit
/// `text_tool_wire_format_supported = false` declaration — is rewritten to a
/// working channel (preferring the route's `preferred_tool_format`), with a
/// `correction` message naming both. Unknown formats, routes with no adverse
/// signal (`unknown`/`interchangeable`), and routes with no working channel at
/// all pass through unchanged.
pub fn validate_tool_format(provider: &str, model: &str, requested: &str) -> ToolFormatDecision {
    let caps = lookup(provider, model);
    validate_tool_format_with_caps(provider, model, requested, &caps)
}

/// `validate_tool_format` against an already-resolved [`Capabilities`], so hot
/// callers that already hold one avoid a second matrix lookup.
pub fn validate_tool_format_with_caps(
    provider: &str,
    model: &str,
    requested: &str,
    caps: &Capabilities,
) -> ToolFormatDecision {
    // Unknown / unclassifiable formats are not ours to second-guess — the
    // exhaustive-match guard elsewhere already rejects typos loudly.
    let Some(wire) = ToolFormatWire::classify(requested) else {
        return ToolFormatDecision::accepted(requested.to_string());
    };

    if !channel_forbidden(wire, caps) {
        return ToolFormatDecision::accepted(requested.to_string());
    }

    // The requested channel is known-broken for this route. Pick the opposite
    // channel as the steer target, preferring the route's declared
    // `preferred_tool_format` when it lands on a channel that is itself not
    // forbidden. If BOTH channels are forbidden (a route with no working tool
    // surface), there is nothing better to offer — pass the request through
    // unchanged rather than rewrite to an equally-broken format under a
    // misleading correction message.
    let opposite = match wire {
        ToolFormatWire::Native => ToolFormatWire::Text,
        ToolFormatWire::Text => ToolFormatWire::Native,
    };
    if channel_forbidden(opposite, caps) {
        return ToolFormatDecision::accepted(requested.to_string());
    }
    let preferred = caps
        .preferred_tool_format
        .clone()
        .filter(|fmt| ToolFormatWire::classify(fmt) == Some(opposite))
        .unwrap_or_else(|| match opposite {
            ToolFormatWire::Native => "native".to_string(),
            ToolFormatWire::Text => "json".to_string(),
        });

    let parity = caps.tool_mode_parity.as_deref().unwrap_or("unknown");
    let mut correction = format!(
        "tool_format `{requested}` is not safe for {provider}/{model} \
         (tool_mode_parity = `{parity}`): this route does not return parseable \
         tool calls on the {} channel, so calls would silently vanish. \
         Using `{preferred}` instead.",
        match wire {
            ToolFormatWire::Native => "provider-native",
            ToolFormatWire::Text => "text",
        }
    );
    if let Some(note) = caps.tool_mode_parity_notes.as_deref() {
        if !note.is_empty() {
            correction.push_str(" (");
            correction.push_str(note);
            correction.push(')');
        }
    }

    ToolFormatDecision {
        effective: preferred,
        correction: Some(correction),
    }
}

/// FOOTGUN-REMOVAL — fail fast when a `(provider, model)` route has NO viable
/// tool channel at all: the registry forbids both the provider-native channel
/// AND every text-channel grammar. `validate_tool_format` deliberately passes
/// such a route through unchanged (it has no *better* format to steer to and
/// must not rewrite to an equally-broken one under a misleading "Using X
/// instead" message); but a tool-bearing call dispatched on a route with no
/// working channel can only produce a silent empty tool stream. This guard lets
/// the call seam reject that combo BEFORE dispatch with an actionable message —
/// naming the bad `(provider, model)` and a suggested alternative provider for
/// the same model family — instead of billing a noncommittal completion.
///
/// Returns `Some(message)` only when both channels are forbidden (e.g. a route
/// flagged `native_unreliable` whose text channel is also declared unsupported,
/// or one explicitly pinned `tool_mode_parity = "unsupported"`). Returns `None`
/// for every route that still has at least one working channel, so it never
/// fires on the auto-correctable DeepInfra/SambaNova gpt-oss rows (those keep a
/// working text channel) or on any healthy route. Modeled on the same
/// `channel_forbidden` machinery `validate_tool_format` uses, so the two stay in
/// lock-step: the gate auto-corrects when one channel works and fails fast when
/// neither does.
pub fn no_viable_tool_channel(provider: &str, model: &str) -> Option<String> {
    let caps = lookup(provider, model);
    no_viable_tool_channel_with_caps(provider, model, &caps)
}

/// `no_viable_tool_channel` against an already-resolved [`Capabilities`], so hot
/// callers that already hold one avoid a second matrix lookup.
pub fn no_viable_tool_channel_with_caps(
    provider: &str,
    model: &str,
    caps: &Capabilities,
) -> Option<String> {
    let native_forbidden = channel_forbidden(ToolFormatWire::Native, caps);
    let text_forbidden = channel_forbidden(ToolFormatWire::Text, caps);
    if !(native_forbidden && text_forbidden) {
        return None;
    }
    let parity = caps.tool_mode_parity.as_deref().unwrap_or("unknown");
    let mut message = format!(
        "no viable tool-calling channel for {provider}/{model} \
         (tool_mode_parity = `{parity}`): the registry trusts neither the \
         provider-native `tool_calls` channel nor a text-channel grammar to \
         return parseable tool calls on this route, so a tool-bearing call here \
         can only emit a silent empty tool stream. {}",
        suggested_alternative_provider_hint(model)
    );
    if let Some(note) = caps.tool_mode_parity_notes.as_deref() {
        if !note.is_empty() {
            message.push_str(" (");
            message.push_str(note);
            message.push(')');
        }
    }
    Some(message)
}

/// A short, actionable "try this provider instead" hint for a model whose
/// current route has no viable tool channel. gpt-oss (Harmony) is the canonical
/// case: its native channel is a footgun on several pay-per-token routes, so
/// steer callers to the channels Harn has proven clean (Fireworks/DeepInfra/
/// SambaNova on TEXT, or a native-clean route). Generic for everything else.
fn suggested_alternative_provider_hint(model: &str) -> String {
    if model.to_ascii_lowercase().contains("gpt-oss") {
        "For gpt-oss (Harmony), use a TEXT-channel route (e.g. \
         `fireworks`/`deepinfra`/`sambanova` gpt-oss, which Harn pins to \
         `tool_format = \"text\"`) or a native-clean route; the provider-native \
         Harmony channel drops tool calls into the reasoning channel."
            .to_string()
    } else {
        "Pick a provider whose route for this model has a working native or \
         text tool channel (see `harn provider catalog matrix`)."
            .to_string()
    }
}

/// Return the currently-effective provider capability rule matrix. User
/// override rows, when installed for the current thread, are emitted before
/// built-in rows so the display mirrors lookup precedence.
pub fn matrix_rows() -> Vec<ProviderCapabilityMatrixRow> {
    let user = USER_OVERRIDES.with(|cell| cell.borrow().clone());
    let mut rows = Vec::new();
    if let Some(user) = user.as_ref() {
        push_matrix_rows(&mut rows, user, "project");
    }
    push_matrix_rows(&mut rows, builtin(), "builtin");
    rows
}

/// Audit the currently effective provider/model catalog against the currently
/// effective capability rules. This is the user-facing path used by the CLI
/// when authors are adding provider catalog or capability override rows.
pub fn audit_catalogued_chat_model_tool_capabilities() -> ToolCapabilityAuditReport {
    let user = USER_OVERRIDES.with(|cell| cell.borrow().clone());
    audit_tool_capability_coverage(
        crate::llm_config::model_catalog_entries(),
        builtin(),
        user.as_ref(),
    )
}

/// Audit the built-in catalog only. The CI test uses this path so external
/// provider config cannot hide a gap in the shipped TOML assets.
pub fn audit_builtin_catalogued_chat_model_tool_capabilities() -> ToolCapabilityAuditReport {
    let catalog = crate::llm_config::parse_config_toml(BUILTIN_PROVIDERS_TOML)
        .expect("providers.toml must parse at build time");
    audit_tool_capability_coverage(catalog.models, builtin(), None)
}

fn audit_tool_capability_coverage<I>(
    models: I,
    builtin: &CapabilitiesFile,
    user: Option<&CapabilitiesFile>,
) -> ToolCapabilityAuditReport
where
    I: IntoIterator<Item = (String, crate::llm_config::ModelDef)>,
{
    let mut gaps = Vec::new();
    let mut audited_models = 0;

    for (model_id, model) in models {
        if model.pricing.is_none() {
            continue;
        }
        audited_models += 1;
        let matched = first_matching_rule(user, builtin, &model.provider, &model_id);
        let mut missing_fields = Vec::new();
        match matched.as_ref().map(|matched| &matched.rule) {
            Some(rule) => {
                if rule.native_tools.is_none() {
                    missing_fields.push("native_tools".to_string());
                }
                if rule.preferred_tool_format.is_none() {
                    missing_fields.push("preferred_tool_format".to_string());
                }
            }
            None => {
                missing_fields.push("native_tools".to_string());
                missing_fields.push("preferred_tool_format".to_string());
            }
        }
        if missing_fields.is_empty() {
            continue;
        }

        let (suggested_native_tools, suggested_preferred_tool_format) =
            suggested_tool_capability_defaults(
                &model.provider,
                &model_id,
                &model,
                matched.as_ref(),
            );
        gaps.push(ToolCapabilityAuditGap {
            provider: model.provider,
            model: model_id,
            rule_provider: matched.as_ref().map(|matched| matched.provider.clone()),
            // Honest per-rule provenance: an `extends` fall-through chain
            // reports every absorbed rule pattern in precedence order, not a
            // fake single source row.
            rule_model_match: matched.map(|matched| matched.matched_patterns.join(" -> ")),
            missing_fields,
            suggested_native_tools,
            suggested_preferred_tool_format,
        });
    }

    gaps.sort_by(|left, right| {
        left.provider
            .cmp(&right.provider)
            .then_with(|| left.model.cmp(&right.model))
    });
    ToolCapabilityAuditReport {
        audited_models,
        gaps,
    }
}

struct MatchedCapabilityRule {
    /// Provider layer of the first (highest-precedence) matched rule.
    provider: String,
    /// Effective rule: the first match, with fields it left unset filled from
    /// later matching rules while the chain opted into `extends` fall-through.
    rule: ProviderRule,
    /// `model_match` patterns of every absorbed rule, in precedence order.
    /// A single entry unless the first match set `extends = true`.
    matched_patterns: Vec<String>,
}

/// Accumulates matching rules along the resolution walk (user rules before
/// built-in rules within a layer, then the `provider_family` chain). The
/// first matched rule has the highest precedence; later matches only fill
/// fields the accumulated chain left unset, and only while every absorbed
/// rule so far opted into `extends` fall-through.
#[derive(Default)]
struct RuleResolution {
    /// Provider layer of the first matched rule.
    provider: Option<String>,
    merged: Option<ProviderRule>,
    /// `model_match` provenance of every absorbed rule, in precedence order.
    matched_patterns: Vec<String>,
}

impl RuleResolution {
    /// Merge `rule` into the accumulator. Returns `true` when the walk must
    /// terminate: the rule does not opt into `extends` fall-through, which is
    /// exactly the pre-`extends` first-match-wins behavior.
    fn absorb(&mut self, layer_provider: &str, rule: &ProviderRule) -> bool {
        if self.provider.is_none() {
            self.provider = Some(layer_provider.to_string());
        }
        self.matched_patterns.push(rule.model_match.clone());
        match &mut self.merged {
            None => self.merged = Some(rule.clone()),
            Some(merged) => merged.fill_missing_from(rule),
        }
        !rule.extends
    }

    fn into_matched(self) -> Option<MatchedCapabilityRule> {
        Some(MatchedCapabilityRule {
            provider: self.provider?,
            rule: self.merged.expect("merged is set whenever provider is set"),
            matched_patterns: self.matched_patterns,
        })
    }
}

/// Scan the ordered rule list for `layer_provider` (user rules first, then
/// built-in rules), absorbing every matching rule into `resolution` until a
/// terminating (non-`extends`) match. Returns `true` when resolution
/// terminated within this layer.
fn absorb_layer_matches(
    user: Option<&CapabilitiesFile>,
    builtin: &CapabilitiesFile,
    layer_provider: &str,
    model: &str,
    resolution: &mut RuleResolution,
) -> bool {
    for file in user.into_iter().chain(std::iter::once(builtin)) {
        if let Some(rules) = file.provider.get(layer_provider) {
            for rule in rules {
                if rule_matches(rule, model) && resolution.absorb(layer_provider, rule) {
                    return true;
                }
            }
        }
    }
    false
}

/// Walk provider → family(provider) → … with a visited-guard, absorbing
/// matching rules into a [`RuleResolution`] and accumulating per-layer
/// provider defaults (earlier layers win) exactly as far as the walk gets.
/// Stops at the first non-`extends` match, so a terminating match at layer N
/// never consults defaults from layers past N — the pre-`extends` behavior.
/// An unterminated `extends` chain keeps walking so later layers can fill
/// its gaps.
fn resolve_rule_chain(
    user: Option<&CapabilitiesFile>,
    builtin: &CapabilitiesFile,
    provider: &str,
    model: &str,
) -> (RuleResolution, ProviderDefaults) {
    let mut resolution = RuleResolution::default();
    let mut effective_defaults = ProviderDefaults::default();
    let mut current = provider.to_string();
    let mut visited = HashSet::new();
    while visited.insert(current.clone()) {
        let layer_defaults = merged_provider_defaults(user, builtin, &current);
        if effective_defaults.has_any_field() {
            effective_defaults.fill_missing_from(&layer_defaults);
        } else {
            effective_defaults.overlay(&layer_defaults);
        }
        if absorb_layer_matches(user, builtin, &current, model, &mut resolution) {
            break;
        }
        let next = user
            .and_then(|file| file.provider_family.get(&current))
            .or_else(|| builtin.provider_family.get(&current))
            .cloned();
        match next {
            Some(parent) => current = parent,
            None => break,
        }
    }
    (resolution, effective_defaults)
}

fn first_matching_rule(
    user: Option<&CapabilitiesFile>,
    builtin: &CapabilitiesFile,
    provider: &str,
    model: &str,
) -> Option<MatchedCapabilityRule> {
    resolve_rule_chain(user, builtin, provider, model)
        .0
        .into_matched()
}

fn suggested_tool_capability_defaults(
    provider: &str,
    model_id: &str,
    model: &crate::llm_config::ModelDef,
    matched: Option<&MatchedCapabilityRule>,
) -> (bool, String) {
    if let Some(rule) = matched.map(|matched| &matched.rule) {
        let native_tools = rule.native_tools.unwrap_or_else(|| {
            // Resolve native_tools from the pinned tool_format via its channel
            // so `json` (a TEXT-channel format) correctly implies
            // native_tools = false, identically to `text`. Falling through to
            // the provider heuristic for `json` would wrongly mark a gemini /
            // cerebras row native. Unknown formats keep the heuristic.
            match rule
                .preferred_tool_format
                .as_deref()
                .and_then(crate::llm_config::tool_format_channel)
            {
                Some(crate::llm_config::ToolFormatChannel::Native) => true,
                Some(crate::llm_config::ToolFormatChannel::Text) => false,
                None => suggested_native_tools(provider, model_id, model),
            }
        });
        let preferred_tool_format = rule
            .preferred_tool_format
            .clone()
            .unwrap_or_else(|| tool_format_for_native(native_tools));
        return (native_tools, preferred_tool_format);
    }

    let native_tools = suggested_native_tools(provider, model_id, model);
    (native_tools, tool_format_for_native(native_tools))
}

fn suggested_native_tools(
    provider: &str,
    model_id: &str,
    model: &crate::llm_config::ModelDef,
) -> bool {
    if provider == "anthropic" || model_id.contains("claude") {
        return true;
    }
    if matches!(
        provider,
        "openai" | "gemini" | "cerebras" | "bedrock" | "azure_openai" | "vertex"
    ) {
        return true;
    }
    model
        .capabilities
        .iter()
        .any(|capability| capability == "tools")
}

/// The derived `preferred_tool_format` for a capability row (or unmatched
/// model) that does not pin one. Native-capable models derive `native`;
/// text-channel models derive `json` (fenced-JSON), the GLOBAL text-channel
/// default. Heredoc (`text`) is never auto-derived — it is reachable only via
/// an explicit `preferred_tool_format = "text"` pin or an explicit request (the
/// reverse safety valve). This is the primary default site: it fires for every
/// model that matches a capability row without an explicit format pin.
fn tool_format_for_native(native_tools: bool) -> String {
    if native_tools {
        "native".to_string()
    } else {
        "json".to_string()
    }
}

fn push_matrix_rows(
    rows: &mut Vec<ProviderCapabilityMatrixRow>,
    file: &CapabilitiesFile,
    source: &str,
) {
    for (provider, rules) in &file.provider {
        for rule in rules {
            rows.push(rule_to_matrix_row(provider, rule, source));
        }
    }
}

fn rule_to_matrix_row(
    provider: &str,
    rule: &ProviderRule,
    source: &str,
) -> ProviderCapabilityMatrixRow {
    ProviderCapabilityMatrixRow {
        provider: provider.to_string(),
        model: rule.model_match.clone(),
        version_min: rule.version_min.clone(),
        extends: rule.extends,
        thinking: rule_thinking_modes(rule),
        vision: rule_vision(rule),
        audio: rule.audio.unwrap_or(false),
        pdf: rule.pdf.unwrap_or(false),
        video: rule.video.unwrap_or(false),
        streaming: true,
        files_api_supported: rule.files_api_supported.unwrap_or(false),
        json_schema: rule_structured_output(rule),
        prefers_xml_scaffolding: rule.prefers_xml_scaffolding.unwrap_or(false),
        reserved_tool_call_token: rule.reserved_tool_call_token.unwrap_or(false),
        prefers_markdown_scaffolding: rule.prefers_markdown_scaffolding.unwrap_or(false),
        structured_output_mode: rule_structured_output_mode(rule),
        supports_assistant_prefill: rule.supports_assistant_prefill.unwrap_or(false),
        prefers_role_developer: rule
            .prefers_role_developer
            .unwrap_or_else(|| rule.requires_completion_tokens.unwrap_or(false)),
        prefers_xml_tools: rule.prefers_xml_tools.unwrap_or(false),
        thinking_block_style: rule_thinking_block_style(rule),
        native_tools: rule.native_tools.unwrap_or(false),
        text_tools: rule.text_tool_wire_format_supported.unwrap_or(true),
        preferred_tool_format: rule_preferred_tool_format(rule),
        tool_mode_parity: rule_tool_mode_parity(rule),
        tools: rule.native_tools.unwrap_or(false)
            || rule.text_tool_wire_format_supported.unwrap_or(true),
        cache: rule.prompt_caching.unwrap_or(false),
        serving_precision: rule
            .serving_precision
            .clone()
            .unwrap_or_else(|| "unverified".to_string()),
        source: source.to_string(),
    }
}

fn rule_thinking_modes(rule: &ProviderRule) -> Vec<String> {
    rule.thinking_modes.clone().unwrap_or_else(|| {
        if rule.thinking.unwrap_or(false) {
            vec!["enabled".to_string()]
        } else {
            Vec::new()
        }
    })
}

fn rule_vision(rule: &ProviderRule) -> bool {
    rule.vision.or(rule.vision_supported).unwrap_or(false)
}

fn lookup_with(
    provider: &str,
    model: &str,
    builtin: &CapabilitiesFile,
    user: Option<&CapabilitiesFile>,
) -> Capabilities {
    // Special case: mock spoofs either shape. Try anthropic first
    // (Claude-shape model strings) so `mock` + `claude-opus-4-7`
    // resolves to the Anthropic capability row — the same behaviour
    // the hardcoded dispatch gave before this refactor. The native
    // tool-definition wire shape is pinned to OpenAI so existing
    // mock-based tests keep observing `t.function.name` regardless of
    // which family's capability row matched; per-message wire format
    // still tracks the matched family so Anthropic-specific request
    // plumbing (beta headers, file-id passthrough) is exercised when
    // a Claude model is mocked.
    if provider == "mock" {
        for family in ["anthropic", "openai", "gemini"] {
            let defaults = merged_provider_defaults(user, builtin, family);
            let mut resolution = RuleResolution::default();
            absorb_layer_matches(user, builtin, family, model, &mut resolution);
            if let Some(rule) = resolution.merged.as_ref() {
                let mut caps = rule_to_caps(rule, &defaults);
                if family == "anthropic" {
                    caps.native_tool_wire_format = "openai".to_string();
                }
                return caps;
            }
        }
        return Capabilities::default();
    }

    // Normal chain: walk provider → family(provider) → ... with a
    // visited-guard to avoid cycles in malformed user overrides.
    let (resolution, effective_defaults) = resolve_rule_chain(user, builtin, provider, model);
    if let Some(rule) = resolution.merged.as_ref() {
        return rule_to_caps(rule, &effective_defaults);
    }
    if effective_defaults.has_any_field() {
        return defaults_to_caps(&effective_defaults);
    }
    Capabilities::default()
}

fn merged_provider_defaults(
    user: Option<&CapabilitiesFile>,
    builtin: &CapabilitiesFile,
    provider: &str,
) -> ProviderDefaults {
    let mut defaults = builtin
        .provider_defaults
        .get(provider)
        .cloned()
        .unwrap_or_default();
    if let Some(user_defaults) = user.and_then(|file| file.provider_defaults.get(provider)) {
        defaults.overlay(user_defaults);
    }
    defaults
}

fn defaults_to_caps(defaults: &ProviderDefaults) -> Capabilities {
    let empty = ProviderRule {
        model_match: "*".to_string(),
        version_min: None,
        extends: false,
        native_tools: None,
        message_wire_format: None,
        native_tool_wire_format: None,
        defer_loading: None,
        tool_search: None,
        responses_api: None,
        hosted_tools: None,
        remote_mcp: None,
        conversation_state: None,
        compaction: None,
        background_mode: None,
        tool_approval_policy: None,
        max_tools: None,
        prompt_caching: None,
        cache_breakpoint_style: None,
        vision: None,
        audio: None,
        pdf: None,
        video: None,
        files_api_supported: None,
        file_upload_wire_format: None,
        structured_output: None,
        prefers_xml_scaffolding: None,
        reserved_tool_call_token: None,
        prefers_markdown_scaffolding: None,
        structured_output_mode: None,
        supports_assistant_prefill: None,
        prefers_role_developer: None,
        prefers_xml_tools: None,
        thinking_block_style: None,
        json_schema: None,
        thinking_modes: None,
        interleaved_thinking_supported: None,
        anthropic_beta_features: None,
        thinking: None,
        vision_supported: None,
        image_url_input_supported: None,
        preserve_thinking: None,
        server_parser: None,
        honors_chat_template_kwargs: None,
        chat_template_options_field: None,
        requires_completion_tokens: None,
        requires_streaming: None,
        reasoning_effort_supported: None,
        reasoning_effort_levels: None,
        reasoning_none_supported: None,
        max_thinking_budget: None,
        reasoning_disable_supported: None,
        reasoning_required_for_tools: None,
        reasoning_text_promotable: None,
        reasoning_wire_format: None,
        seed_supported: None,
        top_k_supported: None,
        temperature_supported: None,
        top_p_supported: None,
        frequency_penalty_supported: None,
        presence_penalty_supported: None,
        allowed_tool_choice_modes: None,
        requires_tool_result_adjacency: None,
        supports_parallel_tool_calls: None,
        tools_exclude_response_format: None,
        recommended_endpoint: None,
        text_tool_wire_format_supported: None,
        preferred_tool_format: None,
        tool_mode_parity: None,
        tool_mode_parity_notes: None,
        thinking_disable_directive: None,
        auto_reasoning_overrides: None,
        provider_route_denylist: None,
        openrouter_provider_order: None,
        serving_precision: None,
    };
    let mut caps = rule_to_caps(&empty, defaults);
    caps.preferred_tool_format = None;
    caps.tool_mode_parity = None;
    caps
}

fn rule_to_caps(rule: &ProviderRule, defaults: &ProviderDefaults) -> Capabilities {
    let thinking_modes = rule_thinking_modes(rule);
    Capabilities {
        native_tools: rule.native_tools.unwrap_or(false),
        message_wire_format: rule
            .message_wire_format
            .clone()
            .or_else(|| defaults.message_wire_format.clone())
            .unwrap_or_else(|| "openai".to_string()),
        native_tool_wire_format: rule
            .native_tool_wire_format
            .clone()
            .or_else(|| defaults.native_tool_wire_format.clone())
            .unwrap_or_else(|| "openai".to_string()),
        defer_loading: rule.defer_loading.unwrap_or(false),
        tool_search: rule.tool_search.clone().unwrap_or_default(),
        responses_api: rule.responses_api.unwrap_or(false),
        hosted_tools: rule.hosted_tools.clone().unwrap_or_default(),
        remote_mcp: rule.remote_mcp.unwrap_or(false),
        conversation_state: rule.conversation_state.unwrap_or(false),
        compaction: rule.compaction.unwrap_or(false),
        background_mode: rule.background_mode.unwrap_or(false),
        tool_approval_policy: rule.tool_approval_policy.clone(),
        max_tools: rule.max_tools,
        prompt_caching: rule.prompt_caching.unwrap_or(false),
        cache_breakpoint_style: rule
            .cache_breakpoint_style
            .clone()
            .unwrap_or_else(|| "none".to_string()),
        vision: rule_vision(rule),
        audio: rule.audio.unwrap_or(false),
        pdf: rule.pdf.unwrap_or(false),
        video: rule.video.unwrap_or(false),
        files_api_supported: rule
            .files_api_supported
            .or(defaults.files_api_supported)
            .unwrap_or(false),
        file_upload_wire_format: rule
            .file_upload_wire_format
            .clone()
            .or_else(|| defaults.file_upload_wire_format.clone()),
        structured_output: rule_structured_output(rule),
        json_schema: rule_structured_output(rule),
        prefers_xml_scaffolding: rule.prefers_xml_scaffolding.unwrap_or(false),
        reserved_tool_call_token: rule.reserved_tool_call_token.unwrap_or(false),
        prefers_markdown_scaffolding: rule.prefers_markdown_scaffolding.unwrap_or(false),
        structured_output_mode: rule_structured_output_mode(rule),
        supports_assistant_prefill: rule.supports_assistant_prefill.unwrap_or(false),
        prefers_role_developer: rule.prefers_role_developer.unwrap_or(false),
        prefers_xml_tools: rule.prefers_xml_tools.unwrap_or(false),
        thinking_block_style: rule_thinking_block_style(rule),
        thinking_modes,
        interleaved_thinking_supported: rule.interleaved_thinking_supported.unwrap_or(false),
        anthropic_beta_features: rule.anthropic_beta_features.clone().unwrap_or_default(),
        vision_supported: rule.vision_supported.unwrap_or(false),
        image_url_input_supported: rule
            .image_url_input_supported
            .or(defaults.image_url_input_supported)
            .unwrap_or(true),
        preserve_thinking: rule.preserve_thinking.unwrap_or(false),
        server_parser: rule
            .server_parser
            .clone()
            .unwrap_or_else(|| "none".to_string()),
        honors_chat_template_kwargs: rule.honors_chat_template_kwargs.unwrap_or(false),
        chat_template_options_field: rule.chat_template_options_field.clone(),
        requires_completion_tokens: rule.requires_completion_tokens.unwrap_or(false),
        requires_streaming: rule.requires_streaming.unwrap_or(false),
        reasoning_effort_supported: rule.reasoning_effort_supported.unwrap_or(false),
        reasoning_effort_levels: rule.reasoning_effort_levels.clone().unwrap_or_default(),
        reasoning_none_supported: rule.reasoning_none_supported.unwrap_or(false),
        max_thinking_budget: rule.max_thinking_budget,
        reasoning_disable_supported: rule.reasoning_disable_supported.unwrap_or(true),
        reasoning_required_for_tools: rule.reasoning_required_for_tools.unwrap_or(false),
        reasoning_text_promotable: rule.reasoning_text_promotable.unwrap_or(true),
        reasoning_wire_format: rule
            .reasoning_wire_format
            .clone()
            .or_else(|| defaults.reasoning_wire_format.clone()),
        seed_supported: rule
            .seed_supported
            .or(defaults.seed_supported)
            .unwrap_or(true),
        top_k_supported: rule
            .top_k_supported
            .or(defaults.top_k_supported)
            .unwrap_or(true),
        temperature_supported: rule
            .temperature_supported
            .or(defaults.temperature_supported)
            .unwrap_or(true),
        top_p_supported: rule
            .top_p_supported
            .or(defaults.top_p_supported)
            .unwrap_or(true),
        frequency_penalty_supported: rule
            .frequency_penalty_supported
            .or(defaults.frequency_penalty_supported)
            .unwrap_or(true),
        presence_penalty_supported: rule
            .presence_penalty_supported
            .or(defaults.presence_penalty_supported)
            .unwrap_or(true),
        allowed_tool_choice_modes: rule.allowed_tool_choice_modes.clone().unwrap_or_default(),
        requires_tool_result_adjacency: rule.requires_tool_result_adjacency.unwrap_or(false),
        supports_parallel_tool_calls: rule.supports_parallel_tool_calls.unwrap_or(true),
        tools_exclude_response_format: rule.tools_exclude_response_format.unwrap_or(false),
        recommended_endpoint: rule.recommended_endpoint.clone(),
        text_tool_wire_format_supported: rule.text_tool_wire_format_supported.unwrap_or(true),
        preferred_tool_format: Some(rule_preferred_tool_format(rule)),
        tool_mode_parity: Some(rule_tool_mode_parity(rule)),
        tool_mode_parity_notes: rule.tool_mode_parity_notes.clone(),
        thinking_disable_directive: rule.thinking_disable_directive.clone(),
        auto_reasoning_overrides: rule.auto_reasoning_overrides.clone().unwrap_or_default(),
        provider_route_denylist: rule.provider_route_denylist.clone().unwrap_or_default(),
        openrouter_provider_order: rule.openrouter_provider_order.clone().unwrap_or_default(),
        serving_precision: rule
            .serving_precision
            .clone()
            .unwrap_or_else(|| "unverified".to_string()),
    }
}

fn rule_preferred_tool_format(rule: &ProviderRule) -> String {
    // This is the `caps.preferred_tool_format` the runtime `lookup` returns for
    // a matched capability row. When the row pins a format, honor it (including
    // an explicit `text` — the reverse safety valve). Otherwise derive: native
    // models get `native`, text-channel models get `json` (fenced-JSON), the
    // GLOBAL text-channel default. Heredoc `text` is never auto-derived.
    rule.preferred_tool_format.clone().unwrap_or_else(|| {
        if rule.native_tools.unwrap_or(false) {
            "native".to_string()
        } else {
            "json".to_string()
        }
    })
}

fn rule_tool_mode_parity(rule: &ProviderRule) -> String {
    rule.tool_mode_parity.clone().unwrap_or_else(|| {
        match (
            rule.native_tools.unwrap_or(false),
            rule.text_tool_wire_format_supported.unwrap_or(true),
        ) {
            (true, true) => "unknown".to_string(),
            (true, false) => "native_only".to_string(),
            (false, true) => "text_only".to_string(),
            (false, false) => "unsupported".to_string(),
        }
    })
}

fn rule_structured_output(rule: &ProviderRule) -> Option<String> {
    rule.structured_output
        .clone()
        .or_else(|| rule.json_schema.clone())
        .filter(|value| value != "none")
}

fn rule_structured_output_mode(rule: &ProviderRule) -> String {
    if let Some(mode) = &rule.structured_output_mode {
        return mode.clone();
    }
    match rule_structured_output(rule).as_deref() {
        Some("native") | Some("format_kw") => "native_json".to_string(),
        Some("tool_use") => "xml_tagged".to_string(),
        _ => "none".to_string(),
    }
}

fn rule_thinking_block_style(rule: &ProviderRule) -> String {
    rule.thinking_block_style.clone().unwrap_or_else(|| {
        if rule.reasoning_effort_supported.unwrap_or(false)
            || rule.requires_completion_tokens.unwrap_or(false)
        {
            "reasoning_summary".to_string()
        } else {
            "none".to_string()
        }
    })
}

pub(crate) fn rule_matches(rule: &ProviderRule, model: &str) -> bool {
    let lower = model.to_lowercase();
    if !glob_match(&rule.model_match.to_lowercase(), &lower) {
        return false;
    }
    if let Some(version_min) = &rule.version_min {
        if version_min.len() != 2 {
            return false;
        }
        let want = (version_min[0], version_min[1]);
        let have = match extract_version(model) {
            Some(v) => v,
            // `version_min` was set but the model ID can't be parsed.
            // Fail closed: skip this rule so more permissive catch-all
            // rules below can still match.
            None => return false,
        };
        if have < want {
            return false;
        }
    }
    true
}

/// Extract `(major, minor)` from a model ID by trying the Anthropic
/// parser first (for `claude-*` shapes) then the OpenAI parser (`gpt-*`).
/// Both parsers return `None` for shapes they don't recognise so this
/// never mis-parses across families.
fn extract_version(model: &str) -> Option<(u32, u32)> {
    claude_generation(model).or_else(|| gpt_generation(model))
}

// Model-pattern matching for capability rules. Shared workspace semantics live
// in `harn-glob`; keep capability and provider matching on that helper instead
// of mirroring glob behavior locally.
use harn_glob::match_name as glob_match;

#[cfg(test)]
mod tests {
    use super::*;

    fn reset() {
        clear_user_overrides();
    }

    fn assert_cerebras_effort_reasoning(model: &str, thinking_block_style: &str) {
        let caps = lookup("cerebras", model);
        assert_eq!(caps.thinking_modes, vec!["effort"]);
        assert!(caps.reasoning_effort_supported);
        // tool_format is NOT asserted here: cerebras gpt-oss and zai-glm have
        // different defaults (gpt-oss harmonized to `json`, glm stays
        // `native`), and this shared helper is about reasoning-effort
        // behavior. Tool-format resolution is asserted in the dedicated
        // harmonization tests.
        assert_eq!(caps.structured_output.as_deref(), Some("native"));
        assert_eq!(caps.structured_output_mode, "native_json");
        assert_eq!(caps.thinking_block_style, thinking_block_style);
    }

    fn assert_openrouter_anthropic_runtime_parity(model: &str) {
        let direct = lookup("anthropic", model);
        let routed = lookup("openrouter", model);

        assert_eq!(
            routed.native_tools, direct.native_tools,
            "{model}: native tool support should match direct Anthropic"
        );
        assert_eq!(
            routed.preferred_tool_format, direct.preferred_tool_format,
            "{model}: preferred tool format should match direct Anthropic"
        );
        assert_eq!(
            routed.structured_output, direct.structured_output,
            "{model}: structured output transport should match direct Anthropic"
        );
        assert_eq!(
            routed.structured_output_mode, direct.structured_output_mode,
            "{model}: structured output mode should match direct Anthropic"
        );
        assert_eq!(
            routed.thinking_modes,
            Vec::<String>::new(),
            "{model}: OpenRouter Claude routes must not advertise direct Anthropic thinking controls"
        );
        assert!(
            !routed.reasoning_effort_supported,
            "{model}: OpenRouter Claude routes must not advertise direct Anthropic effort controls"
        );
        assert!(
            !routed.interleaved_thinking_supported,
            "{model}: OpenRouter Claude routes must not advertise interleaved thinking"
        );
        assert_eq!(
            routed.supports_assistant_prefill, direct.supports_assistant_prefill,
            "{model}: assistant prefill support should match direct Anthropic"
        );
        assert_eq!(
            routed.prompt_caching, direct.prompt_caching,
            "{model}: prompt cache support should match direct Anthropic"
        );
        assert_eq!(
            routed.prefers_xml_scaffolding, direct.prefers_xml_scaffolding,
            "{model}: XML scaffolding preference should match direct Anthropic"
        );
        assert_eq!(
            routed.prefers_markdown_scaffolding, direct.prefers_markdown_scaffolding,
            "{model}: Markdown scaffolding preference should match direct Anthropic"
        );
        assert_eq!(
            routed.prefers_role_developer, direct.prefers_role_developer,
            "{model}: developer role preference should match direct Anthropic"
        );
        assert_eq!(
            routed.prefers_xml_tools, direct.prefers_xml_tools,
            "{model}: XML tool preference should match direct Anthropic"
        );
        assert_eq!(
            routed.thinking_block_style, direct.thinking_block_style,
            "{model}: thinking block style should match direct Anthropic"
        );
        assert_eq!(
            routed.text_tool_wire_format_supported, direct.text_tool_wire_format_supported,
            "{model}: text-tool fallback support should match direct Anthropic"
        );
    }

    #[test]
    fn every_catalogued_chat_model_has_explicit_tool_capabilities() {
        reset();
        let report = audit_builtin_catalogued_chat_model_tool_capabilities();
        assert!(report.ok(), "{}", report.render_human());
    }

    #[test]
    fn every_catalogued_alias_has_explicit_tool_capabilities() {
        // The model-level audit only covers priced catalog `models`, so a
        // `[[provider.local]]` / Ollama alias (e.g. the local gemma-4 route in
        // Fix A) could omit native_tools/preferred_tool_format and silently
        // degrade to text tools without tripping a test. Walk every alias's
        // (provider, id) through the same matcher and require explicit fields.
        reset();
        let catalog = crate::llm_config::parse_config_toml(BUILTIN_PROVIDERS_TOML)
            .expect("providers.toml must parse at build time");
        let builtin = builtin();
        let mut gaps = Vec::new();
        for (alias, def) in &catalog.aliases {
            let matched = first_matching_rule(None, builtin, &def.provider, &def.id);
            let explicit = matched
                .as_ref()
                .map(|matched| {
                    matched.rule.native_tools.is_some()
                        && matched.rule.preferred_tool_format.is_some()
                })
                .unwrap_or(false);
            if !explicit {
                gaps.push(format!(
                    "{alias} -> {}:{} (rule={})",
                    def.provider,
                    def.id,
                    matched
                        .as_ref()
                        .map(|matched| matched.rule.model_match.as_str())
                        .unwrap_or("<none>")
                ));
            }
        }
        assert!(
            gaps.is_empty(),
            "aliases missing explicit native_tools/preferred_tool_format:\n- {}",
            gaps.join("\n- ")
        );
    }

    #[test]
    fn every_catalogued_alias_tool_format_pin_is_safe_for_route() {
        // Alias pins are consumed directly by downstream catalogs and CLI
        // routing. They must not encode a known-broken channel that the
        // central runtime guard would have to correct later.
        reset();
        let catalog = crate::llm_config::parse_config_toml(BUILTIN_PROVIDERS_TOML)
            .expect("providers.toml must parse at build time");
        let mut unsafe_pins = Vec::new();
        for (alias, def) in &catalog.aliases {
            let Some(tool_format) = def.tool_format.as_deref() else {
                continue;
            };
            let decision = validate_tool_format(&def.provider, &def.id, tool_format);
            if let Some(correction) = decision.correction.as_deref() {
                unsafe_pins.push(format!(
                    "{alias} -> {}:{} pins {tool_format}, would be corrected to {} ({correction})",
                    def.provider, def.id, decision.effective
                ));
            }
        }
        assert!(
            unsafe_pins.is_empty(),
            "aliases pin unsafe tool_format values:\n- {}",
            unsafe_pins.join("\n- ")
        );
    }

    #[test]
    fn tool_capability_audit_reports_suggested_defaults() {
        reset();
        let capabilities: CapabilitiesFile = toml::from_str(
            r#"
[[provider.acme]]
model_match = "acme-good-*"
preferred_tool_format = "native"
"#,
        )
        .unwrap();
        let report = audit_tool_capability_coverage(
            vec![(
                "acme-good-1".to_string(),
                crate::llm_config::ModelDef {
                    name: "Acme Good".to_string(),
                    provider: "acme".to_string(),
                    context_window: 128_000,
                    logical_model: None,
                    equivalence_group: None,
                    served_variant: None,
                    wire_model: None,
                    api_dialect: None,
                    rate_limits: None,
                    performance: None,
                    architecture: None,
                    local_memory: None,
                    runtime_context_window: None,
                    stream_timeout: None,
                    capabilities: Vec::new(),
                    pricing: Some(crate::llm_config::ModelPricing {
                        input_per_mtok: 1.0,
                        output_per_mtok: 2.0,
                        cache_read_per_mtok: None,
                        cache_write_per_mtok: None,
                    }),
                    deprecated: false,
                    deprecation_note: None,
                    superseded_by: None,
                    fast_mode: None,
                    quality_tags: Vec::new(),
                    availability: crate::llm_config::ModelAvailability::Serverless,
                    tier: None,
                    open_weight: None,
                    strengths: Vec::new(),
                    benchmarks: std::collections::BTreeMap::new(),
                    family: None,
                    lineage: None,
                    complementary_with: Vec::new(),
                    avoid_as_reviewer_for: Vec::new(),
                },
            )],
            &capabilities,
            None,
        );

        assert!(!report.ok());
        assert_eq!(report.audited_models, 1);
        assert_eq!(report.gaps.len(), 1);
        assert_eq!(report.gaps[0].missing_fields, ["native_tools"]);
        assert!(report.gaps[0].suggested_native_tools);
        assert_eq!(report.gaps[0].suggested_preferred_tool_format, "native");
        assert!(report.render_human().contains(
            "acme:acme-good-1 (provider.acme model_match=\"acme-good-*\") missing native_tools; suggest native_tools = true, preferred_tool_format = \"native\""
        ));
    }

    #[test]
    fn openrouter_qwen36_keeps_native_and_denies_ambient_upstream() {
        reset();
        for model in [
            "qwen/qwen3.6-flash",
            "qwen/qwen3.6-plus",
            "qwen/qwen3.6-35b-a3b",
        ] {
            let caps = lookup("openrouter", model);
            // The route-around must NOT downgrade the tool format: native stays on.
            assert!(caps.native_tools, "{model}: native tools");
            assert_eq!(caps.preferred_tool_format.as_deref(), Some("native"));
            // The broken Ambient upstream is denied via the data-driven denylist.
            assert_eq!(
                caps.provider_route_denylist,
                vec!["Ambient".to_string()],
                "{model}: denylist",
            );
        }
    }

    #[test]
    fn provider_route_denylist_defaults_empty_for_unmarked_rows() {
        reset();
        let caps = lookup("anthropic", "claude-opus-4-7");
        assert!(caps.provider_route_denylist.is_empty());
    }

    #[test]
    fn strict_openai_compat_rows_require_tool_result_adjacency() {
        reset();
        assert!(lookup("moonshot", "moonshot/kimi-k2.6").requires_tool_result_adjacency);
        assert!(lookup("moonshot", "moonshot/kimi-k2.7-code").requires_tool_result_adjacency);
        assert!(lookup("minimax", "MiniMax-M2").requires_tool_result_adjacency);
        assert!(lookup("minimax", "MiniMax-M2.7").requires_tool_result_adjacency);
        assert!(!lookup("openai", "gpt-4o").requires_tool_result_adjacency);
    }

    #[test]
    fn fireworks_gpt_oss_disables_parallel_tool_call_history() {
        reset();
        assert!(
            !lookup("fireworks", "accounts/fireworks/models/gpt-oss-120b")
                .supports_parallel_tool_calls
        );
        assert!(lookup("openai", "gpt-4o").supports_parallel_tool_calls);
    }

    #[test]
    fn cerebras_tools_exclude_response_format() {
        reset();
        assert!(lookup("cerebras", "gpt-oss-120b").tools_exclude_response_format);
        assert!(lookup("cerebras", "zai-glm-4.7").tools_exclude_response_format);
        assert!(!lookup("openai", "gpt-4o").tools_exclude_response_format);
    }

    #[test]
    fn serving_precision_seeds_known_gpt_oss_verdicts() {
        reset();
        // Full-precision routes verified during the 2026-06 meter effort.
        assert_eq!(
            lookup("fireworks", "accounts/fireworks/models/gpt-oss-120b").serving_precision,
            "trusted"
        );
        assert_eq!(
            lookup("openrouter", "openai/gpt-oss-120b").serving_precision,
            "trusted"
        );
        // SambaNova serves gpt-oss quantized (proven 0/5 vs reference 3/3).
        assert_eq!(
            lookup("sambanova", "gpt-oss-120b").serving_precision,
            "degraded"
        );
        // Cerebras is full precision but rate-throttled to unusable timing.
        assert_eq!(
            lookup("cerebras", "gpt-oss-120b").serving_precision,
            "throttled"
        );
    }

    #[test]
    fn serving_precision_defaults_unverified_for_unmarked_rows() {
        reset();
        // A route with no serving_precision verdict resolves to "unverified",
        // never an empty string, so callers can branch on a stable enum.
        assert_eq!(
            lookup("anthropic", "claude-opus-4-7").serving_precision,
            "unverified"
        );
    }

    #[test]
    fn anthropic_opus_47_gets_full_capabilities() {
        reset();
        let caps = lookup("anthropic", "claude-opus-4-7");
        assert!(caps.native_tools);
        assert!(caps.defer_loading);
        assert_eq!(caps.tool_search, vec!["bm25", "regex"]);
        assert!(caps.prompt_caching);
        assert_eq!(caps.thinking_modes, vec!["adaptive", "effort"]);
        assert!(caps.reasoning_effort_supported);
        assert_eq!(
            caps.reasoning_effort_levels,
            vec!["low", "medium", "high", "xhigh", "max"]
        );
        assert!(caps.interleaved_thinking_supported);
        assert!(caps.vision_supported);
        assert!(caps.audio);
        assert!(caps.pdf);
        assert!(caps.files_api_supported);
        assert_eq!(caps.max_tools, Some(10000));
        assert!(caps.prefers_xml_scaffolding);
        assert!(!caps.prefers_markdown_scaffolding);
        assert_eq!(caps.structured_output_mode, "xml_tagged");
        assert!(!caps.supports_assistant_prefill);
        assert!(!caps.prefers_role_developer);
        assert!(caps.prefers_xml_tools);
        assert_eq!(caps.thinking_block_style, "thinking_blocks");
    }

    #[test]
    fn anthropic_sonnet_5_gets_adaptive_effort_capabilities() {
        reset();
        let caps = lookup("anthropic", "claude-sonnet-5");
        assert!(caps.native_tools);
        assert!(caps.defer_loading);
        assert_eq!(caps.tool_search, vec!["bm25", "regex"]);
        assert!(caps.prompt_caching);
        assert_eq!(caps.thinking_modes, vec!["adaptive", "effort"]);
        assert!(caps.reasoning_effort_supported);
        assert_eq!(
            caps.reasoning_effort_levels,
            vec!["low", "medium", "high", "xhigh", "max"]
        );
        assert!(caps.reasoning_disable_supported);
        assert!(!caps.reasoning_none_supported);
        assert!(caps.interleaved_thinking_supported);
        assert!(!caps.supports_assistant_prefill);
        assert_eq!(caps.thinking_block_style, "thinking_blocks");
    }

    #[test]
    fn anthropic_fable_effort_cannot_be_disabled() {
        reset();
        for model in ["claude-fable-5", "anthropic/claude-fable-5"] {
            let caps = lookup("anthropic", model);
            assert_eq!(caps.thinking_modes, vec!["adaptive", "effort"]);
            assert!(caps.reasoning_effort_supported);
            assert_eq!(
                caps.reasoning_effort_levels,
                vec!["low", "medium", "high", "xhigh", "max"]
            );
            assert!(!caps.reasoning_disable_supported);
            assert!(!caps.supports_assistant_prefill);
        }
    }

    #[test]
    fn anthropic_opus_46_uses_budgeted_thinking() {
        reset();
        let caps = lookup("anthropic", "claude-opus-4-6");
        assert_eq!(caps.thinking_modes, vec!["enabled"]);
        assert!(caps.interleaved_thinking_supported);
        assert!(!caps.supports_assistant_prefill);
    }

    #[test]
    fn anthropic_opus_45_does_not_support_interleaved_thinking() {
        reset();
        let caps = lookup("anthropic", "claude-opus-4-5");
        assert_eq!(caps.thinking_modes, vec!["enabled"]);
        assert!(!caps.interleaved_thinking_supported);
        assert!(caps.supports_assistant_prefill);
    }

    #[test]
    fn openrouter_claude_rows_track_direct_anthropic_runtime_quirks() {
        reset();
        for model in [
            "anthropic/claude-fable-5-0",
            "anthropic/claude-mythos-5-0",
            "anthropic/claude-haiku-4-5",
            "anthropic/claude-haiku-4-7",
            "anthropic/claude-sonnet-4-6",
            "anthropic/claude-sonnet-4-7",
            "anthropic/claude-sonnet-5",
            "anthropic/claude-opus-4-6",
            "anthropic/claude-opus-4-7",
        ] {
            assert_openrouter_anthropic_runtime_parity(model);
        }
    }

    #[test]
    fn override_can_supply_anthropic_beta_features() {
        reset();
        let toml_src = r#"
[[provider.anthropic]]
model_match = "claude-custom-*"
native_tools = true
anthropic_beta_features = ["fine-grained-tool-streaming-2025-05-14"]
"#;
        set_user_overrides_toml(toml_src).unwrap();
        let caps = lookup("anthropic", "claude-custom-1");
        assert_eq!(
            caps.anthropic_beta_features,
            vec!["fine-grained-tool-streaming-2025-05-14"]
        );
        reset();
    }

    #[test]
    fn anthropic_haiku_44_has_no_tool_search() {
        reset();
        let caps = lookup("anthropic", "claude-haiku-4-4");
        // Haiku 4.4 falls through to the `claude-*` catch-all row.
        assert!(caps.native_tools);
        assert!(caps.prompt_caching);
        assert!(!caps.defer_loading);
        assert!(caps.tool_search.is_empty());
    }

    #[test]
    fn anthropic_haiku_45_supports_tool_search() {
        reset();
        let caps = lookup("anthropic", "claude-haiku-4-5");
        assert!(caps.defer_loading);
        assert_eq!(caps.tool_search, vec!["bm25", "regex"]);
    }

    #[test]
    fn old_claude_gets_catchall() {
        reset();
        let caps = lookup("anthropic", "claude-opus-3-5");
        assert!(caps.native_tools);
        assert!(caps.prompt_caching);
        assert!(!caps.defer_loading);
        assert!(caps.tool_search.is_empty());
    }

    #[test]
    fn openai_gpt_54_supports_tool_search() {
        reset();
        let caps = lookup("openai", "gpt-5.4");
        assert!(caps.defer_loading);
        assert_eq!(caps.tool_search, vec!["hosted", "client"]);
        assert_eq!(caps.json_schema.as_deref(), Some("native"));
        assert_eq!(caps.thinking_modes, vec!["effort"]);
        assert!(caps.reasoning_effort_supported);
        assert!(caps.reasoning_none_supported);
        assert!(!caps.prefers_xml_scaffolding);
        assert!(caps.prefers_markdown_scaffolding);
        assert_eq!(caps.structured_output_mode, "native_json");
        assert!(!caps.supports_assistant_prefill);
        assert!(!caps.prefers_role_developer);
        assert!(!caps.prefers_xml_tools);
        assert_eq!(caps.thinking_block_style, "reasoning_summary");
    }

    #[test]
    fn openai_gpt_53_has_reasoning_none_without_tool_search() {
        reset();
        let caps = lookup("openai", "gpt-5.3");
        assert!(caps.native_tools);
        assert!(!caps.defer_loading);
        assert!(caps.vision_supported);
        assert!(caps.tool_search.is_empty());
        assert_eq!(caps.thinking_modes, vec!["effort"]);
        assert!(caps.reasoning_effort_supported);
        assert!(caps.reasoning_none_supported);
    }

    #[test]
    fn openai_original_gpt_5_has_reasoning_floor_without_none() {
        reset();
        let caps = lookup("openai", "gpt-5");
        assert!(caps.native_tools);
        assert!(!caps.defer_loading);
        assert_eq!(caps.thinking_modes, vec!["effort"]);
        assert!(caps.reasoning_effort_supported);
        assert!(!caps.reasoning_none_supported);
    }

    #[test]
    fn gemini_thinking_budget_quirks_are_declared_in_matrix() {
        reset();
        // Flash: 24576 ceiling, can disable thinking.
        let flash = lookup("gemini", "gemini-2.5-flash");
        assert_eq!(flash.max_thinking_budget, Some(24_576));
        assert!(flash.reasoning_disable_supported);
        assert!(flash.thinking_modes.iter().any(|m| m == "effort"));
        // Pro: 32768 ceiling, cannot disable thinking.
        let pro = lookup("gemini", "gemini-2.5-pro");
        assert_eq!(pro.max_thinking_budget, Some(32_768));
        assert!(!pro.reasoning_disable_supported);
        assert!(pro.thinking_modes.iter().any(|m| m == "effort"));
        // The `models/` REST resource name resolves the same.
        let flash_resource = lookup("gemini", "models/gemini-2.5-flash");
        assert_eq!(flash_resource.max_thinking_budget, Some(24_576));
        assert!(flash_resource.reasoning_disable_supported);
        // Non-2.5 gemini has no effort thinking support -> provider sends no
        // thinkingConfig (unchanged behavior).
        let legacy = lookup("gemini", "gemini-1.5-pro");
        assert!(!legacy.thinking_modes.iter().any(|m| m == "effort"));
    }

    #[test]
    fn openai_gpt_4o_matrix_fields_include_multimodal_support() {
        reset();
        let caps = lookup("openai", "gpt-4o");
        assert!(caps.native_tools);
        assert!(caps.vision);
        assert!(caps.audio);
        assert!(!caps.pdf);
        assert_eq!(caps.json_schema.as_deref(), Some("native"));
    }

    #[test]
    fn openai_reasoning_models_support_effort() {
        reset();
        let caps = lookup("openai", "o3");
        assert_eq!(caps.thinking_modes, vec!["effort"]);
        assert!(caps.requires_completion_tokens);
        assert!(caps.reasoning_effort_supported);
        assert!(caps.prefers_role_developer);
        assert_eq!(caps.thinking_block_style, "reasoning_summary");
        let prefixed = lookup("openrouter", "openai/o4-mini");
        assert!(prefixed.requires_completion_tokens);
        assert!(prefixed.reasoning_effort_supported);
    }

    #[test]
    fn vision_capability_gates_known_multimodal_models() {
        reset();
        let minimax_m3 = lookup("minimax", "MiniMax-M3");
        assert!(minimax_m3.vision_supported);
        assert!(minimax_m3.video);
        assert_eq!(minimax_m3.thinking_modes, vec!["adaptive"]);
        assert_eq!(minimax_m3.reasoning_wire_format.as_deref(), Some("minimax"));
        assert!(minimax_m3.requires_completion_tokens);
        let openrouter_m3 = lookup("openrouter", "minimax/minimax-m3");
        assert!(openrouter_m3.vision_supported);
        assert!(openrouter_m3.video);
        assert!(lookup("openai", "gpt-4o").vision_supported);
        assert!(lookup("openai", "gpt-5.4-preview").vision_supported);
        assert!(lookup("anthropic", "claude-sonnet-4-6").vision_supported);
        assert!(lookup("anthropic", "claude-sonnet-4-6").pdf);
        assert!(lookup("anthropic", "claude-sonnet-4-6").files_api_supported);
        assert!(lookup("openrouter", "google/gemini-2.5-flash").vision_supported);
        assert!(lookup("gemini", "gemini-2.5-flash").vision_supported);
        assert!(lookup("gemini", "gemini-2.5-flash").audio);
        assert!(lookup("gemini", "gemini-2.5-flash").pdf);
        assert_eq!(
            lookup("gemini", "gemini-2.5-flash").structured_output_mode,
            "native_json"
        );
        assert!(lookup("ollama", "llava:latest").vision_supported);
        assert!(lookup("ollama", "gemma4:26b").vision_supported);
        assert!(lookup("ollama", "gemma4-128k:latest").vision_supported);
        assert!(!lookup("openai", "gpt-3.5-turbo").vision_supported);
        assert!(!lookup("ollama", "qwen3.5:35b-a3b-coding-nvfp4").vision_supported);
    }

    #[test]
    fn openrouter_gemini_explicit_cache_uses_block_breakpoints() {
        reset();
        let caps = lookup("openrouter", "google/gemini-2.5-flash");
        assert!(caps.prompt_caching);
        assert_eq!(caps.cache_breakpoint_style, "last_block");
    }

    #[test]
    fn local_gemma4_exposes_native_tools_and_structured_output() {
        // Fix A: vLLM/SGLang serve Gemma 4 over the OpenAI-compatible surface,
        // so the local route must declare native tools + native structured
        // output like its hosted gemma-4 siblings — not silently fall back to
        // text tools.
        reset();
        let caps = lookup("local", "gemma-4-26b-a4b-it");
        assert!(caps.native_tools);
        assert_eq!(caps.preferred_tool_format.as_deref(), Some("native"));
        assert_eq!(caps.structured_output.as_deref(), Some("native"));
    }

    #[test]
    fn local_gemma4_exposes_vision_like_hosted_siblings() {
        // harn#3585: Gemma 4 is multimodal on every served surface. The local
        // OpenAI-compat route must declare vision so the derived structured
        // caps and emitted `capability_tags` agree with the gemini/openrouter/
        // together siblings.
        reset();
        for model in ["gemma-4-e4b-it", "gemma-4-e2b-it", "gemma-4-26b-a4b-it"] {
            let caps = lookup("local", model);
            assert!(
                caps.vision_supported,
                "local {model} should expose vision_supported"
            );
            let tags = crate::llm_config::capability_tags_from_capabilities(&caps);
            assert!(
                tags.iter().any(|t| t == "vision"),
                "local {model} emitted capability_tags should include `vision`, got {tags:?}"
            );
        }
    }

    #[test]
    fn ollama_vision_models_have_no_reasoning_scaffold() {
        // Fix B: bakllava / llama3.2-vision / gemma3 are caption/vision models
        // with no reasoning capability; they must resolve to the "none" thinking
        // block style (like the llava sibling) so the template does not emit a
        // spurious "## Reasoning" scaffold.
        reset();
        for model in ["bakllava:latest", "llama3.2-vision:11b", "gemma3:27b"] {
            assert_eq!(
                lookup("ollama", model).thinking_block_style,
                "none",
                "{model} should resolve to thinking_block_style=\"none\""
            );
        }
        // Sibling sanity check.
        assert_eq!(
            lookup("ollama", "llava:latest").thinking_block_style,
            "none"
        );
    }

    #[test]
    fn ollama_gemma4_supports_structured_output_and_text_tools() {
        // Fix C: Ollama honors the `format` kwarg, so both gemma4 rules must
        // declare structured_output="format_kw" (otherwise JSON/schema output
        // was blocked) plus explicit text tools for parity with the qwen rules.
        reset();
        for model in ["gemma4:12b-mlx", "gemma4:26b"] {
            let caps = lookup("ollama", model);
            assert_eq!(
                caps.structured_output.as_deref(),
                Some("format_kw"),
                "{model} should resolve structured_output=\"format_kw\""
            );
            assert!(!caps.native_tools, "{model} should use text tools");
            assert_eq!(
                caps.preferred_tool_format.as_deref(),
                Some("text"),
                "{model} should prefer text tool format"
            );
            assert_eq!(
                caps.thinking_block_style, "none",
                "{model} ships thinking-off"
            );
        }
    }

    #[test]
    fn openrouter_inherits_openai() {
        reset();
        let caps = lookup("openrouter", "gpt-5.4");
        assert!(caps.defer_loading);
        assert_eq!(caps.tool_search, vec!["hosted", "client"]);
        assert_eq!(caps.reasoning_wire_format.as_deref(), Some("openrouter"));
        assert!(!caps.top_k_supported);
    }

    #[test]
    fn openrouter_kimi27_code_records_tool_choice_and_sampling_limits() {
        reset();
        let caps = lookup("openrouter", "moonshotai/kimi-k2.7-code");
        assert!(caps.native_tools);
        assert!(caps.prompt_caching);
        assert!(caps.vision_supported);
        assert!(caps.video);
        // 2026-06-24 forced-format sweep flipped this route native -> text:
        // native double-escaped backslash bodies (1/5) and fenced-JSON produced
        // no parseable Harn call (0/5); heredoc text was 5/5 byte-clean.
        assert_eq!(caps.preferred_tool_format.as_deref(), Some("text"));
        assert_eq!(caps.tool_mode_parity.as_deref(), Some("native_unreliable"));
        assert_eq!(caps.thinking_modes, vec!["enabled"]);
        assert_eq!(caps.allowed_tool_choice_modes, vec!["auto", "none"]);
        assert!(!caps.temperature_supported);
        assert!(!caps.top_p_supported);
        assert!(!caps.frequency_penalty_supported);
        assert!(!caps.presence_penalty_supported);

        let prior = lookup("openrouter", "moonshotai/kimi-k2.6");
        assert!(prior.prompt_caching);
        assert!(prior.vision_supported);
        assert!(!prior.video);
        assert!(prior.allowed_tool_choice_modes.is_empty());
        assert!(prior.temperature_supported);
    }

    #[test]
    fn qwen37_routes_record_prompt_cache_vision_and_streaming_quirks() {
        reset();
        let plus = lookup("openrouter", "qwen/qwen3.7-plus");
        assert!(plus.native_tools);
        assert!(plus.prompt_caching);
        assert!(plus.vision_supported);
        assert_eq!(plus.preferred_tool_format.as_deref(), Some("native"));
        assert_eq!(plus.thinking_modes, vec!["enabled"]);
        assert_eq!(
            plus.auto_reasoning_overrides
                .get("agent")
                .map(String::as_str),
            Some("off"),
            "Qwen tool-bearing agent turns should disable reasoning automatically",
        );

        let max = lookup("openrouter", "qwen/qwen3.7-max");
        assert!(max.native_tools);
        assert!(max.prompt_caching);
        assert!(!max.vision_supported);
        assert_eq!(max.thinking_modes, vec!["enabled"]);

        let together = lookup("together", "Qwen/Qwen3.7-Max");
        assert!(together.native_tools);
        assert!(together.prompt_caching);
        assert!(together.requires_streaming);
        assert!(!together.honors_chat_template_kwargs);

        let glm = lookup("together", "zai-org/GLM-5.1");
        assert!(glm.native_tools);
        assert!(glm.prompt_caching);
        assert_eq!(glm.preferred_tool_format.as_deref(), Some("text"));
        assert_eq!(glm.tool_mode_parity.as_deref(), Some("native_unreliable"));
        assert_eq!(
            glm.auto_reasoning_overrides
                .get("agent")
                .map(String::as_str),
            Some("off"),
        );

        let openrouter_glm = lookup("openrouter", "z-ai/glm-5.2");
        assert!(openrouter_glm.reasoning_effort_supported);
        assert_eq!(
            openrouter_glm.reasoning_effort_levels,
            vec!["high", "xhigh", "max"]
        );
        assert_eq!(
            openrouter_glm.preferred_tool_format.as_deref(),
            Some("text")
        );

        let minimax = lookup("together", "MiniMaxAI/MiniMax-M2.7");
        assert!(minimax.native_tools);
        assert!(minimax.prompt_caching);
        // 2026-06-24 forced-format sweep flipped this route json -> text: heredoc
        // beat fenced-JSON on both dispatch and backslash-body fidelity at N=5.
        assert_eq!(minimax.preferred_tool_format.as_deref(), Some("text"));
        assert_eq!(
            minimax.tool_mode_parity.as_deref(),
            Some("native_unreliable")
        );
        assert!(!minimax.reasoning_text_promotable);

        let step = lookup("openrouter", "stepfun/step-3.7-flash");
        assert!(step.native_tools);
        assert!(step.prompt_caching);
        assert!(!step.reasoning_disable_supported);
        assert_eq!(step.thinking_modes, vec!["enabled"]);
    }

    #[test]
    fn openrouter_structured_routes_cover_current_open_models() {
        reset();
        for model in [
            "deepseek/deepseek-v4-flash",
            "mistralai/devstral-small",
            "meta-llama/llama-4-scout",
            "kwaipilot/kat-coder-pro-v2",
        ] {
            let caps = lookup("openrouter", model);
            assert!(caps.native_tools, "{model} should expose native tools");
            assert_eq!(caps.structured_output.as_deref(), Some("native"));
            assert_eq!(caps.structured_output_mode, "native_json");
        }
        assert!(lookup("openrouter", "deepseek/deepseek-v4-flash").top_k_supported);
        assert!(lookup("openrouter", "meta-llama/llama-4-scout").top_k_supported);
        assert!(!lookup("openrouter", "mistralai/devstral-small").top_k_supported);
        assert!(lookup("openrouter", "google/gemma-4-26b-a4b-it").top_k_supported);
    }

    #[test]
    fn openrouter_anthropic_claude_models_support_native_tools() {
        // Regression for #2319: OpenRouter Anthropic slugs must match the
        // Anthropic capability rules before the OpenRouter -> OpenAI family
        // chain, otherwise native-tool requests get rejected as unsupported.
        reset();
        for model in [
            "anthropic/claude-haiku-4-5",
            "anthropic/claude-haiku-4-5-20251001",
            "anthropic/claude-sonnet-4-6",
            "anthropic/claude-sonnet-4-7",
            "anthropic/claude-opus-4-7",
        ] {
            let caps = lookup("openrouter", model);
            assert!(
                caps.native_tools,
                "{model} via openrouter should report native_tools=true",
            );
            assert!(
                caps.prompt_caching,
                "{model} via openrouter should report prompt_caching=true",
            );
            assert_eq!(
                caps.cache_breakpoint_style, "top_level",
                "{model} via openrouter should use top-level cache_control",
            );
            assert_eq!(
                caps.structured_output.as_deref(),
                Some("tool_use"),
                "{model} via openrouter should structured_output=tool_use (matches direct anthropic)",
            );
        }
    }

    #[test]
    fn openrouter_deepseek_v32_defaults_to_text_tools() {
        reset();
        let caps = lookup("openrouter", "deepseek/deepseek-v3.2");
        assert!(caps.native_tools);
        assert!(caps.text_tool_wire_format_supported);
        assert_eq!(caps.preferred_tool_format.as_deref(), Some("text"));
        assert_eq!(caps.tool_mode_parity.as_deref(), Some("native_unreliable"));
        assert_eq!(caps.structured_output.as_deref(), Some("native"));
        assert!(caps.prompt_caching);
        assert_eq!(caps.cache_breakpoint_style, "last_block");

        let automated = lookup("openrouter", "deepseek/deepseek-v3");
        assert!(automated.prompt_caching);
        assert_eq!(automated.cache_breakpoint_style, "none");
    }

    #[test]
    fn openrouter_explicit_cache_routes_get_block_breakpoints() {
        reset();
        for model in [
            "qwen/qwen3.6-plus",
            "qwen/qwen3-coder-plus",
            "qwen/qwen3-coder-flash",
            "qwen/qwen3-max",
            "qwen/qwen-plus",
        ] {
            let caps = lookup("openrouter", model);
            assert!(caps.prompt_caching, "{model} should support prompt cache");
            assert_eq!(
                caps.cache_breakpoint_style, "last_block",
                "{model} should request explicit content-block cache breakpoints",
            );
        }

        let open_weight = lookup("openrouter", "qwen/qwen3.6-35b-a3b");
        assert!(!open_weight.prompt_caching);
        assert_eq!(open_weight.cache_breakpoint_style, "none");
    }

    #[test]
    fn openrouter_deepseek_alias_slugs_support_native_tools() {
        reset();
        for model in ["deepseek/deepseek-chat", "deepseek/deepseek-chat-v3-0324"] {
            let caps = lookup("openrouter", model);
            assert!(caps.native_tools, "{model} should expose native tools");
            assert_eq!(caps.preferred_tool_format.as_deref(), Some("native"));
            assert_eq!(caps.structured_output.as_deref(), Some("native"));
            assert!(
                caps.thinking_modes.is_empty(),
                "{model} is not a reasoning route"
            );
            assert_eq!(caps.thinking_block_style, "none");
            assert!(
                caps.top_k_supported,
                "{model} should accept top_k through OpenRouter"
            );
        }

        for model in [
            "deepseek/deepseek-chat-v3.1",
            "deepseek/deepseek-r1",
            "deepseek/deepseek-r1-0528",
        ] {
            let caps = lookup("openrouter", model);
            assert!(caps.native_tools, "{model} should expose native tools");
            assert_eq!(caps.preferred_tool_format.as_deref(), Some("native"));
            assert_eq!(caps.structured_output.as_deref(), Some("native"));
            assert_eq!(caps.thinking_modes, vec!["enabled", "effort"]);
            assert_eq!(caps.thinking_block_style, "reasoning_summary");
            assert!(
                caps.top_k_supported,
                "{model} should accept top_k through OpenRouter"
            );
        }

        assert!(!lookup("openrouter", "deepseek/deepseek-r1-distill-qwen-32b").native_tools);
    }

    #[test]
    fn openrouter_qwen_coder_defaults_to_text_tools() {
        reset();
        let caps = lookup("openrouter", "qwen/qwen3-coder-flash");
        assert!(caps.native_tools);
        assert!(caps.text_tool_wire_format_supported);
        assert_eq!(caps.preferred_tool_format.as_deref(), Some("text"));
        assert_eq!(caps.tool_mode_parity.as_deref(), Some("native_unreliable"));
    }

    #[test]
    fn bedrock_claude_uses_anthropic_wire_capabilities() {
        reset();
        let caps = lookup("bedrock", "anthropic.claude-3-5-sonnet-20240620-v1:0");
        assert!(caps.native_tools);
        assert_eq!(caps.message_wire_format, "anthropic");
        assert_eq!(caps.native_tool_wire_format, "anthropic");
    }

    #[test]
    fn groq_inherits_openai_family_only() {
        reset();
        let caps = lookup("groq", "gpt-5.5-preview");
        assert!(caps.defer_loading);
    }

    #[test]
    fn cerebras_inherits_openai_family() {
        reset();
        let caps = lookup("cerebras", "gpt-oss-120b");
        assert_eq!(caps.message_wire_format, "openai");
        assert_eq!(caps.native_tool_wire_format, "openai");
        // gpt-oss uses NATIVE tool calls across cerebras/groq/together. Under
        // json/text it emits a bare {"tool","arguments"} dialect the
        // fenced-JSON parser rejects (zero parsed calls), so native is the only
        // working channel.
        assert!(caps.native_tools);
        assert_eq!(caps.preferred_tool_format.as_deref(), Some("native"));
    }

    #[test]
    fn cerebras_gpt_oss_declares_supported_reasoning_efforts() {
        // Cerebras GPT-OSS accepts low/medium/high only. The policy resolver
        // uses this list to floor `reasoning_policy: "off"` to `low` instead
        // of sending unsupported `none` or `minimal` values.
        reset();
        let caps = lookup("cerebras", "gpt-oss-120b");
        assert_cerebras_effort_reasoning("gpt-oss-120b", "reasoning_summary");
        assert!(!caps.reasoning_none_supported);
        assert_eq!(caps.reasoning_effort_levels, vec!["low", "medium", "high"]);
    }

    #[test]
    fn gpt_oss_requires_reasoning_for_tools_with_provider_specific_tool_wire() {
        // gpt-oss (Harmony) calls tools INSIDE the chain-of-thought channel, so
        // reasoning-off breaks tool calling. Provider catch-all rules carry no
        // reasoning fields, so without a dedicated `*gpt-oss*` row gpt-oss
        // would fall through to reasoning-OFF and the eval loop would bill a
        // noncommittal. Tool wire support is provider-specific: the pay-per-token
        // routes (OpenRouter, Fireworks, DeepInfra, SambaNova) ride Harn's TEXT
        // channel — their provider-native Harmony path drops tool calls into the
        // reasoning/commentary channel (empty `tool_calls` / billed-noncommittal,
        // see the DeepInfra/SambaNova rows + vLLM #22578/#44216, SGLang
        // #8976/#10738, openai/harmony #68). Within the text channel they use the
        // escape-free heredoc (`text`) grammar rather than fenced-JSON, because
        // gpt-oss double-escapes the backslashes a JSON string arg requires and
        // corrupts `\\`-heavy code bodies (empirical A/B 2026-06-21: text beats
        // json on both dispatch and byte-fidelity). Only the native-clean direct
        // routes (Cerebras, Groq) still use provider-native tools.
        reset();
        for (provider, model, native_tools, preferred_tool_format) in [
            ("openrouter", "openai/gpt-oss-120b", false, "text"),
            (
                "fireworks",
                "accounts/fireworks/models/gpt-oss-120b",
                false,
                "text",
            ),
            ("deepinfra", "openai/gpt-oss-120b", false, "text"),
            ("sambanova", "sambanova/gpt-oss-120b", false, "text"),
            ("cerebras", "gpt-oss-120b", true, "native"),
            ("groq", "openai/gpt-oss-120b", true, "native"),
        ] {
            let caps = lookup(provider, model);
            assert!(
                caps.reasoning_required_for_tools,
                "{provider}/{model}: reasoning_required_for_tools must be true"
            );
            assert!(
                caps.reasoning_effort_supported,
                "{provider}/{model}: reasoning_effort_supported must be true"
            );
            assert_eq!(
                caps.reasoning_effort_levels,
                vec!["low", "medium", "high"],
                "{provider}/{model}: effort levels"
            );
            assert_eq!(caps.thinking_modes, vec!["effort"], "{provider}/{model}");
            assert_eq!(
                caps.native_tools, native_tools,
                "{provider}/{model}: native_tools"
            );
            assert_eq!(
                caps.preferred_tool_format.as_deref(),
                Some(preferred_tool_format),
                "{provider}/{model}: preferred tool format"
            );
            assert_eq!(
                caps.thinking_block_style, "reasoning_summary",
                "{provider}/{model}"
            );
        }
    }

    #[test]
    fn cerebras_glm_47_supports_reasoning_none() {
        // Cerebras documents GLM 4.7's no-reasoning value as
        // reasoning_effort="none"; the older disable_reasoning knob is
        // deprecated. Keep the route on the same policy path as GPT-OSS.
        reset();
        let caps = lookup("cerebras", "zai-glm-4.7");
        assert_cerebras_effort_reasoning("zai-glm-4.7", "inline");
        assert!(caps.reasoning_none_supported);
    }

    #[test]
    fn mock_with_claude_model_routes_to_anthropic() {
        reset();
        let caps = lookup("mock", "claude-sonnet-4-7");
        assert!(caps.defer_loading);
        assert_eq!(caps.tool_search, vec!["bm25", "regex"]);
    }

    #[test]
    fn mock_with_gpt_model_routes_to_openai() {
        reset();
        let caps = lookup("mock", "gpt-5.4-preview");
        assert!(caps.defer_loading);
        assert_eq!(caps.tool_search, vec!["hosted", "client"]);
    }

    #[test]
    fn mock_with_gemini_model_routes_to_gemini() {
        reset();
        let caps = lookup("mock", "gemini-2.5-flash");
        assert_eq!(caps.message_wire_format, "gemini");
        assert_eq!(caps.native_tool_wire_format, "openai");
        assert!(caps.prefers_xml_scaffolding);
    }

    #[test]
    fn qwen36_ollama_preserves_thinking() {
        reset();
        let caps = lookup("ollama", "qwen3.6:35b-a3b-coding-nvfp4");
        assert!(!caps.native_tools);
        assert_eq!(caps.json_schema.as_deref(), Some("format_kw"));
        assert!(!caps.thinking_modes.is_empty());
        assert!(
            caps.preserve_thinking,
            "Qwen3.6 should enable preserve_thinking by default for long-horizon loops"
        );
        assert_eq!(caps.server_parser, "none");
        assert!(!caps.honors_chat_template_kwargs);
        assert_eq!(caps.recommended_endpoint.as_deref(), Some("/api/chat"));
        assert!(caps.text_tool_wire_format_supported);
        assert!(caps.prefers_markdown_scaffolding);
        assert_eq!(caps.structured_output_mode, "delimited");
        assert!(!caps.prefers_xml_tools);
        assert_eq!(caps.thinking_block_style, "inline");
    }

    #[test]
    fn qwen35_ollama_does_not_preserve_thinking() {
        reset();
        let caps = lookup("ollama", "qwen3.5:35b-a3b-coding-nvfp4");
        assert!(caps.native_tools);
        assert!(!caps.thinking_modes.is_empty());
        assert!(
            !caps.preserve_thinking,
            "Qwen3.5 lacks the preserve_thinking kwarg — rely on the chat template's rolling checkpoint instead"
        );
        assert_eq!(caps.server_parser, "ollama_qwen3coder");
        assert!(!caps.text_tool_wire_format_supported);
    }

    #[test]
    fn qwen36_routed_providers_all_preserve_thinking() {
        reset();
        for (provider, model) in [
            ("openrouter", "qwen/qwen3.6-plus"),
            ("together", "Qwen/Qwen3.6-Plus"),
            ("huggingface", "Qwen/Qwen3.6-35B-A3B"),
            ("fireworks", "accounts/fireworks/models/qwen3p6-plus"),
            ("dashscope", "qwen3.6-plus"),
            ("local", "Qwen3.6-35B-A3B"),
            ("mlx", "unsloth/Qwen3.6-35B-A3B-UD-MLX-4bit"),
            ("mlx", "Qwen/Qwen3.6-35B-A3B"),
        ] {
            let caps = lookup(provider, model);
            assert!(
                !caps.thinking_modes.is_empty(),
                "{provider}/{model}: thinking"
            );
            assert!(
                caps.preserve_thinking,
                "{provider}/{model}: preserve_thinking must be on for Qwen3.6"
            );
            assert!(caps.native_tools, "{provider}/{model}: native_tools");
            assert_ne!(
                caps.server_parser, "ollama_qwen3coder",
                "{provider}/{model}: only Ollama routes through the qwen3coder response parser"
            );
        }

        let caps = lookup("llamacpp", "unsloth/Qwen3.6-35B-A3B-GGUF");
        assert!(!caps.thinking_modes.is_empty());
        assert!(caps.preserve_thinking);
        assert!(!caps.native_tools);
        assert!(caps.text_tool_wire_format_supported);
        assert_eq!(caps.server_parser, "none");
    }

    #[test]
    fn qwen_coder_models_do_not_claim_thinking_modes() {
        reset();
        for (provider, model) in [
            ("together", "Qwen/Qwen3-Coder-Next-FP8"),
            ("together", "Qwen/Qwen3-Coder-480B-A35B-Instruct-FP8"),
            ("openrouter", "qwen/qwen3-coder-next"),
            ("huggingface", "Qwen/Qwen3-Coder-Next"),
        ] {
            let caps = lookup(provider, model);
            assert!(caps.native_tools, "{provider}/{model}: native_tools");
            assert!(
                caps.thinking_modes.is_empty(),
                "{provider}/{model}: coder models are non-thinking routes"
            );
            assert!(
                !caps.preserve_thinking,
                "{provider}/{model}: preserve_thinking must stay off"
            );
            assert!(
                caps.thinking_disable_directive.is_none(),
                "{provider}/{model}: no /no_think shim should be needed"
            );
        }
    }

    #[test]
    fn llamacpp_qwen_keeps_text_tool_wire_format() {
        reset();
        let caps = lookup("llamacpp", "unsloth/Qwen3.5-Coder-GGUF");
        assert_eq!(caps.server_parser, "none");
        assert!(caps.honors_chat_template_kwargs);
        assert!(!caps.native_tools);
        assert!(caps.text_tool_wire_format_supported);
        assert_eq!(
            caps.recommended_endpoint.as_deref(),
            Some("/v1/chat/completions")
        );
    }

    #[test]
    fn devstral_local_routes_default_to_json_tools() {
        reset();
        for provider in ["ollama", "llamacpp"] {
            let caps = lookup(provider, "devstral-small-2:24b");
            assert!(!caps.native_tools, "{provider}: native tools stay opt-in");
            assert!(
                caps.text_tool_wire_format_supported,
                "{provider}: text tools should remain available"
            );
            // devstral has no reserved-token constraint, so it uses the global
            // `json` (fenced-JSON) text-channel default. Heredoc stays
            // reachable via an explicit `preferred_tool_format = "text"` pin.
            assert_eq!(
                caps.preferred_tool_format.as_deref(),
                Some("json"),
                "{provider}: devstral inherits the global json default"
            );
        }
    }

    #[test]
    fn openrouter_mistral_routes_use_native_tools() {
        reset();
        let caps = lookup("openrouter", "mistralai/mistral-small-2603");
        assert!(caps.native_tools);
        assert!(caps.text_tool_wire_format_supported);
        assert_eq!(caps.structured_output.as_deref(), Some("native"));
        assert_eq!(caps.structured_output_mode, "native_json");
    }

    #[test]
    fn dashscope_and_llamacpp_resolve_capabilities() {
        reset();
        // New sibling providers should fall through to `openai` for
        // gpt-*  models even without dedicated rules.
        let caps = lookup("dashscope", "gpt-5.4-preview");
        assert!(caps.defer_loading);
        let caps = lookup("llamacpp", "gpt-5.4-preview");
        assert!(caps.defer_loading);
    }

    #[test]
    fn unknown_provider_has_no_capabilities() {
        reset();
        let caps = lookup("my-custom-proxy", "foo-bar-1");
        assert!(!caps.native_tools);
        assert!(!caps.defer_loading);
        assert!(caps.tool_search.is_empty());
    }

    #[test]
    fn openrouter_specific_rules_win_and_family_inheritance_is_preserved() {
        // Capability resolution is first-match-wins over fragment order
        // (`first_matching_rule_in_file` -> `Iterator::find`), and when no
        // `provider.openrouter` rule matches it walks the `[provider_family]`
        // chain (openrouter -> openai). Both contracts must hold so that:
        //   1. a specific OpenRouter carve-out beats a broader OpenRouter rule,
        //   2. gpt-/o-family slugs routed through OpenRouter still inherit the
        //      rich openai-family capability set (a blanket `*` openrouter row
        //      would shadow this — see the catalog-or-defaults report).
        reset();

        // 1. Specific carve-out wins: deepseek/deepseek-v3.2 is pinned to the
        // Harn text-tool channel even though the broader deepseek/deepseek-v3*
        // rule below it would otherwise resolve `native`.
        let deepseek = lookup("openrouter", "deepseek/deepseek-v3.2");
        assert_eq!(
            deepseek.preferred_tool_format.as_deref(),
            Some("text"),
            "deepseek-v3.2 text carve-out must win over the broader deepseek-v3* rule"
        );
        assert_eq!(
            deepseek.tool_mode_parity.as_deref(),
            Some("native_unreliable")
        );
        // The broader sibling still resolves native for non-3.2 v3 slugs.
        assert_eq!(
            lookup("openrouter", "deepseek/deepseek-v3-base")
                .preferred_tool_format
                .as_deref(),
            Some("native")
        );

        // 2. Family inheritance preserved: an openai-prefixed slug routed via
        // OpenRouter still picks up openai-family reasoning fields.
        let prefixed = lookup("openrouter", "openai/o4-mini");
        assert!(prefixed.requires_completion_tokens);
        assert!(prefixed.reasoning_effort_supported);

        // The newly added MiniMax M2.5 OR mirror resolves native via the
        // existing `minimax/minimax-m2*` rule.
        let m25 = lookup("openrouter", "minimax/minimax-m2.5");
        assert!(m25.native_tools);
        assert_eq!(m25.preferred_tool_format.as_deref(), Some("native"));
    }

    #[test]
    fn enterprise_routes_expose_format_preferences() {
        reset();
        let bedrock_claude = lookup("bedrock", "anthropic.claude-opus-4-7-v1:0");
        assert!(bedrock_claude.prefers_xml_scaffolding);
        assert_eq!(bedrock_claude.structured_output_mode, "xml_tagged");
        assert!(!bedrock_claude.supports_assistant_prefill);
        assert!(bedrock_claude.prefers_xml_tools);

        let azure_o = lookup("azure_openai", "o3-prod");
        assert!(azure_o.prefers_markdown_scaffolding);
        assert_eq!(azure_o.structured_output_mode, "native_json");
        assert!(azure_o.prefers_role_developer);
        assert_eq!(azure_o.thinking_block_style, "reasoning_summary");
    }

    #[test]
    fn user_override_adds_new_provider() {
        reset();
        let toml_src = concat!(
            "[[provider.my-proxy]]\n",
            "model_match = \"*\"\n",
            "native_tools = true\n",
            "tool_search = [\"hosted\"]\n",
            "prefers_xml_scaffolding = true\n",
            "structured_output_mode = \"xml_tagged\"\n",
            "supports_assistant_prefill = true\n",
            "prefers_xml_tools = true\n",
            "thinking_block_style = \"thinking_blocks\"\n",
        );
        set_user_overrides_toml(toml_src).unwrap();
        let caps = lookup("my-proxy", "anything");
        assert!(caps.native_tools);
        assert_eq!(caps.tool_search, vec!["hosted"]);
        assert!(caps.prefers_xml_scaffolding);
        assert_eq!(caps.structured_output_mode, "xml_tagged");
        assert!(caps.supports_assistant_prefill);
        assert!(caps.prefers_xml_tools);
        assert_eq!(caps.thinking_block_style, "thinking_blocks");
        clear_user_overrides();
    }

    #[test]
    fn user_override_takes_precedence_over_builtin() {
        reset();
        let toml_src = r#"
[[provider.anthropic]]
model_match = "claude-opus-*"
native_tools = true
defer_loading = false
tool_search = []
"#;
        set_user_overrides_toml(toml_src).unwrap();
        let caps = lookup("anthropic", "claude-opus-4-7");
        assert!(caps.native_tools);
        assert!(!caps.defer_loading);
        assert!(caps.tool_search.is_empty());
        clear_user_overrides();
    }

    #[test]
    fn user_override_from_manifest_toml() {
        reset();
        let manifest = concat!(
            "[package]\n",
            "name = \"demo\"\n\n",
            "[[capabilities.provider.my-proxy]]\n",
            "model_match = \"*\"\n",
            "native_tools = true\n",
            "tool_search = [\"hosted\"]\n",
            "prefers_markdown_scaffolding = true\n",
            "structured_output_mode = \"native_json\"\n",
            "prefers_role_developer = true\n",
            "thinking_block_style = \"reasoning_summary\"\n",
        );
        set_user_overrides_from_manifest_toml(manifest).unwrap();
        let caps = lookup("my-proxy", "foo");
        assert!(caps.native_tools);
        assert_eq!(caps.tool_search, vec!["hosted"]);
        assert!(caps.prefers_markdown_scaffolding);
        assert_eq!(caps.structured_output_mode, "native_json");
        assert!(caps.prefers_role_developer);
        assert_eq!(caps.thinking_block_style, "reasoning_summary");
        clear_user_overrides();
    }

    #[test]
    fn version_min_requires_parseable_model() {
        reset();
        let toml_src = r#"
[[provider.custom]]
model_match = "*"
version_min = [5, 4]
native_tools = true
"#;
        set_user_overrides_toml(toml_src).unwrap();
        // Unparseable model ID + version_min → rule doesn't match.
        let caps = lookup("custom", "mystery-model");
        assert!(!caps.native_tools);
        clear_user_overrides();
    }

    #[test]
    fn glob_match_substring() {
        assert!(glob_match("*gpt*", "openai/gpt-5.4"));
        assert!(glob_match("*claude*", "anthropic/claude-opus-4-7"));
        assert!(!glob_match("*xyz*", "openai/gpt-5.4"));
    }

    #[test]
    fn openrouter_namespaced_anthropic_model() {
        reset();
        let caps = lookup("anthropic", "anthropic/claude-opus-4-7");
        assert!(caps.defer_loading);
    }

    #[test]
    fn matrix_rows_include_provider_patterns_and_sources() {
        reset();
        let rows = matrix_rows();
        assert!(rows.iter().any(|row| {
            row.provider == "openai"
                && row.model == "gpt-4o*"
                && row.vision
                && row.audio
                && row.json_schema.as_deref() == Some("native")
                && row.source == "builtin"
        }));
    }

    #[test]
    fn validate_tool_format_autocorrects_native_pin_on_native_unreliable_route() {
        reset();
        // DeepSeek V3.2 on OpenRouter: tool_mode_parity = native_unreliable,
        // preferred_tool_format = text. A `native` request is the footgun — it
        // drops to unparsed DSML text and gets rejected. The gate must steer it
        // to the route's preferred text-channel format and explain why.
        let decision = validate_tool_format("openrouter", "deepseek/deepseek-v3.2", "native");
        assert_eq!(
            decision.effective, "text",
            "native must be auto-corrected to the route's preferred text format"
        );
        let reason = decision.correction.expect("a correction must be reported");
        assert!(reason.contains("native"), "names the rejected format");
        assert!(reason.contains("native_unreliable"), "names the parity");
        assert!(reason.contains("text"), "names the working alternative");
    }

    #[test]
    fn validate_tool_format_passes_through_safe_combos() {
        reset();
        // A native-capable route with no adverse parity keeps the requested
        // native format untouched (no spurious correction).
        let decision = validate_tool_format("openrouter", "deepseek/deepseek-v3-base", "native");
        assert_eq!(decision.effective, "native");
        assert!(decision.correction.is_none());

        // The same native_unreliable route is fine when text is requested.
        let decision = validate_tool_format("openrouter", "deepseek/deepseek-v3.2", "text");
        assert_eq!(decision.effective, "text");
        assert!(decision.correction.is_none());

        // json is also a text-channel grammar and is accepted on a text route.
        let decision = validate_tool_format("openrouter", "deepseek/deepseek-v3.2", "json");
        assert_eq!(decision.effective, "json");
        assert!(decision.correction.is_none());
    }

    #[test]
    fn validate_tool_format_leaves_unknown_routes_and_formats_alone() {
        reset();
        // Unknown provider/model has parity = unknown -> no opinion, pass through.
        let decision = validate_tool_format("my-proxy", "mystery-1", "native");
        assert_eq!(decision.effective, "native");
        assert!(decision.correction.is_none());

        // An unclassifiable tool_format string is not ours to rewrite.
        let decision = validate_tool_format("openrouter", "deepseek/deepseek-v3.2", "frobnicate");
        assert_eq!(decision.effective, "frobnicate");
        assert!(decision.correction.is_none());
    }

    #[test]
    fn validate_tool_format_steers_off_text_on_native_only_route() {
        reset();
        // Synthesize a native_only route via a project override and confirm a
        // text request is steered to native (the symmetric direction).
        let overrides: CapabilitiesFile = toml::from_str(
            "[[provider.acme]]\n\
             model_match = \"native-only-*\"\n\
             native_tools = true\n\
             text_tool_wire_format_supported = false\n\
             tool_mode_parity = \"native_only\"\n\
             preferred_tool_format = \"native\"\n",
        )
        .expect("override parses");
        let caps = lookup_with_user_overrides("acme", "native-only-1", Some(&overrides));
        let decision = validate_tool_format_with_caps("acme", "native-only-1", "text", &caps);
        assert_eq!(decision.effective, "native");
        let reason = decision
            .correction
            .expect("text on native_only is corrected");
        assert!(reason.contains("native_only"));
    }

    #[test]
    fn validate_tool_format_honors_structural_text_unsupported_bit() {
        reset();
        // Real shipping route: ollama/qwen3* declares native_tools = true and
        // text_tool_wire_format_supported = false with NO tool_mode_parity
        // string. The gate's contract ("always yields parseable tool calls")
        // must hold from the structural bit alone — a text/json request is
        // steered to native, not passed through onto an unsupported channel.
        let caps = lookup("ollama", "qwen3-coder:30b");
        assert!(!caps.text_tool_wire_format_supported);
        for requested in ["text", "json"] {
            let decision =
                validate_tool_format_with_caps("ollama", "qwen3-coder:30b", requested, &caps);
            assert_eq!(
                decision.effective, "native",
                "{requested} must be steered to native on a text-unsupported route"
            );
            assert!(decision.correction.is_some());
        }
        // native is the route's working channel — untouched.
        let native = validate_tool_format_with_caps("ollama", "qwen3-coder:30b", "native", &caps);
        assert_eq!(native.effective, "native");
        assert!(native.correction.is_none());
    }

    #[test]
    fn tool_format_resolution_is_serving_stack_aware_for_same_weights() {
        // The (model x serving-stack) insight: the SAME Qwen3.6 weights resolve
        // to DIFFERENT working tool-call channels depending on who serves them.
        // This divergence lives in the capability matrix as data (provider rows),
        // NOT in alias pins — so an alias refactor must not be able to regress
        // it. Locking the three live serving stacks here makes that explicit.
        reset();

        // llama.cpp (:8001) — native is probe-validated and trusted.
        let llamacpp = validate_tool_format("llamacpp", "qwen3.6-35b-a3b-ud-q4-k-xl", "native");
        assert_eq!(
            llamacpp.effective, "native",
            "llama.cpp serves qwen3.6 native"
        );
        assert!(llamacpp.correction.is_none());

        // Ollama (/v1) — the embedded qwen tool-call parser 500s on text-mode
        // output, so this route is served on the text/json channel: a native
        // request must be auto-corrected to json (never silently dropped).
        let ollama = validate_tool_format("ollama", "qwen3.6-35b-a3b", "native");
        assert_eq!(
            ollama.effective, "json",
            "ollama qwen3.6 must steer native -> json (server-side parser 500 leak)"
        );
        assert!(
            ollama.correction.is_some(),
            "the native->json steer must be explained, not silent"
        );

        // A native_unreliable cloud route (deepinfra GLM-5) carries the same
        // serving-stack verdict via tool_mode_parity + empirical notes, and is
        // likewise steered off native.
        let glm = validate_tool_format("deepinfra", "deepinfra/glm-5.2", "native");
        assert_eq!(glm.effective, "json");
        assert!(glm.correction.is_some());
    }

    #[test]
    fn validate_tool_format_passes_through_when_no_channel_works() {
        reset();
        // A route with no working tool surface — text_only parity forbids the
        // native channel, and text_tool_wire_format_supported = false forbids
        // the text channel — so BOTH channels are forbidden. The gate has
        // nothing better to steer to; it must NOT rewrite to an equally broken
        // format under a misleading correction. Pass through unchanged.
        let overrides: CapabilitiesFile = toml::from_str(
            "[[provider.acme]]\n\
             model_match = \"no-tools-*\"\n\
             native_tools = false\n\
             tool_mode_parity = \"text_only\"\n\
             text_tool_wire_format_supported = false\n",
        )
        .expect("override parses");
        let caps = lookup_with_user_overrides("acme", "no-tools-1", Some(&overrides));
        for requested in ["native", "text", "json"] {
            let decision = validate_tool_format_with_caps("acme", "no-tools-1", requested, &caps);
            assert_eq!(
                decision.effective, requested,
                "{requested} passes through unchanged"
            );
            assert!(decision.correction.is_none());
        }
    }

    /// FOOTGUN-REMOVAL — gpt-oss (Harmony) on the pay-per-token DeepInfra and
    /// SambaNova routes drops tool calls into the reasoning channel on native, so
    /// a `native` pin must auto-correct to the route's `text` channel with an
    /// explanatory correction. The known-good native routes (cerebras gpt-oss,
    /// sambanova minimax) must stay untouched.
    #[test]
    fn validate_tool_format_autocorrects_gpt_oss_native_pin_to_text() {
        reset();
        for (provider, model) in [
            ("deepinfra", "deepinfra/openai/gpt-oss-120b"),
            ("sambanova", "sambanova/gpt-oss-120b"),
        ] {
            let decision = validate_tool_format(provider, model, "native");
            assert_eq!(
                decision.effective, "text",
                "{provider}/{model}: native must auto-correct to text"
            );
            let reason = decision
                .correction
                .unwrap_or_else(|| panic!("{provider}/{model}: a correction must be reported"));
            assert!(
                reason.contains("native_unreliable"),
                "{provider}/{model}: names the parity"
            );
            assert!(
                reason.contains("text"),
                "{provider}/{model}: names the working alternative"
            );
            // text is already safe and passes through unchanged.
            let text = validate_tool_format(provider, model, "text");
            assert_eq!(text.effective, "text");
            assert!(text.correction.is_none());
        }
    }

    /// FOOTGUN-REMOVAL — the GLM-5.x native channel emits `<tool_call>` markup
    /// instead of provider-native `tool_calls`, so the zai-direct GLM rows pin
    /// text and a `native` pin must auto-correct, matching the Fireworks/
    /// DeepInfra/Baseten precedents.
    #[test]
    fn validate_tool_format_autocorrects_zai_glm_native_pin_to_text() {
        reset();
        for model in ["glm-5.2", "glm-5.1", "glm-5"] {
            let decision = validate_tool_format("zai", model, "native");
            assert_eq!(
                decision.effective, "text",
                "zai/{model}: native must auto-correct to text"
            );
            let reason = decision
                .correction
                .unwrap_or_else(|| panic!("zai/{model}: a correction must be reported"));
            assert!(
                reason.contains("native_unreliable"),
                "zai/{model}: names the parity"
            );
        }
    }

    /// The known-good native routes must NOT be touched by the gpt-oss/GLM
    /// pins above — a native pin stays native with no spurious correction.
    #[test]
    fn validate_tool_format_leaves_known_good_native_routes_unchanged() {
        reset();
        for (provider, model) in [
            // cerebras gpt-oss is native-clean (only throttled).
            ("cerebras", "gpt-oss-120b"),
            // sambanova deepseek-v3.2 is native and interchangeable; minimax is
            // native_unreliable upstream and is not a known-good native
            // exemplar.
            ("sambanova", "DeepSeek-V3.2"),
        ] {
            let decision = validate_tool_format(provider, model, "native");
            assert_eq!(
                decision.effective, "native",
                "{provider}/{model}: known-good native route must stay native"
            );
            assert!(
                decision.correction.is_none(),
                "{provider}/{model}: no spurious correction"
            );
        }
    }

    /// FOOTGUN-REMOVAL — the first-class no-viable-channel guard fires when BOTH
    /// channels are forbidden (a route the registry trusts on neither native nor
    /// text), naming the bad combo and a suggested alternative — never a silent
    /// empty tool stream.
    #[test]
    fn no_viable_tool_channel_guard_fires_only_when_both_channels_forbidden() {
        reset();
        // Construct a gpt-oss route with NO working channel: native_unreliable
        // forbids native, and text_tool_wire_format_supported = false forbids the
        // text channel too.
        let overrides: CapabilitiesFile = toml::from_str(
            "[[provider.acme]]\n\
             model_match = \"acme/gpt-oss-stub\"\n\
             native_tools = false\n\
             tool_mode_parity = \"native_unreliable\"\n\
             text_tool_wire_format_supported = false\n",
        )
        .expect("override parses");
        let caps = lookup_with_user_overrides("acme", "acme/gpt-oss-stub", Some(&overrides));
        let message = no_viable_tool_channel_with_caps("acme", "acme/gpt-oss-stub", &caps)
            .expect("the guard must fire when neither channel works");
        assert!(
            message.contains("no viable tool-calling channel"),
            "names the failure: {message}"
        );
        assert!(
            message.contains("acme/gpt-oss-stub"),
            "names the bad combo: {message}"
        );
        // gpt-oss models get the Harmony-specific text-channel hint.
        assert!(
            message.contains("gpt-oss") && message.contains("text"),
            "suggests an alternative: {message}"
        );

        // The DeepInfra/SambaNova gpt-oss rows keep a working text channel, so
        // the guard must NOT fire on them (they auto-correct instead).
        assert!(
            no_viable_tool_channel("deepinfra", "deepinfra/openai/gpt-oss-120b").is_none(),
            "auto-correctable route must not trip the fail-fast guard"
        );
        assert!(
            no_viable_tool_channel("sambanova", "sambanova/gpt-oss-120b").is_none(),
            "auto-correctable route must not trip the fail-fast guard"
        );
        // A healthy native-clean route never trips it.
        assert!(
            no_viable_tool_channel("cerebras", "gpt-oss-120b").is_none(),
            "healthy native route must not trip the guard"
        );
        // The generic (non-gpt-oss) no-channel case still fires with a generic
        // hint.
        let generic: CapabilitiesFile = toml::from_str(
            "[[provider.acme]]\n\
             model_match = \"mystery-1\"\n\
             native_tools = false\n\
             tool_mode_parity = \"text_only\"\n\
             text_tool_wire_format_supported = false\n",
        )
        .expect("override parses");
        let caps = lookup_with_user_overrides("acme", "mystery-1", Some(&generic));
        let message = no_viable_tool_channel_with_caps("acme", "mystery-1", &caps)
            .expect("guard fires on the generic no-channel route too");
        assert!(
            message.contains("harn provider catalog matrix"),
            "{message}"
        );
    }

    // --- `extends = true` field-wise fall-through ---

    /// Resolve capabilities for a synthetic provider whose rules come entirely
    /// from `src`: the parsed file is passed as the builtin base with no user
    /// layer, so no shipped rule interferes with the `extends` assertions.
    fn extends_caps(src: &str) -> Capabilities {
        let file = parse_capabilities_toml(src).expect("test capabilities toml parses");
        lookup_with("testprov", "test-model", &file, None)
    }

    #[test]
    fn extends_rule_fills_unset_fields_from_later_matching_rule() {
        // Rule 1 opts into `extends` and sets only native_tools; rule 2 (lower
        // precedence, same match) supplies the fields the chain left unset.
        let caps = extends_caps(
            r#"
[[provider.testprov]]
model_match = "test-*"
extends = true
native_tools = true

[[provider.testprov]]
model_match = "test-*"
vision = true
message_wire_format = "anthropic"
"#,
        );
        assert!(caps.native_tools, "field from the extends rule applies");
        assert!(
            caps.vision,
            "unset field filled from the later matching rule"
        );
        assert_eq!(caps.message_wire_format, "anthropic");
    }

    #[test]
    fn non_extends_rule_terminates_resolution_unchanged() {
        // Without `extends`, the first match wins outright and the later
        // rule's vision never applies — the pre-`extends` first-match-wins
        // behavior is preserved.
        let caps = extends_caps(
            r#"
[[provider.testprov]]
model_match = "test-*"
native_tools = true

[[provider.testprov]]
model_match = "test-*"
vision = true
"#,
        );
        assert!(caps.native_tools);
        assert!(
            !caps.vision,
            "a non-extends first match must not absorb later rules"
        );
    }

    #[test]
    fn extends_rule_does_not_override_explicitly_set_field() {
        // The higher-precedence extends rule's explicit native_tools = true
        // wins; the later rule only fills fields the chain left unset, so its
        // native_tools = false is ignored while its vision still applies.
        let caps = extends_caps(
            r#"
[[provider.testprov]]
model_match = "test-*"
extends = true
native_tools = true

[[provider.testprov]]
model_match = "test-*"
native_tools = false
vision = true
"#,
        );
        assert!(
            caps.native_tools,
            "the extends rule's explicit value is not overridden by a lower rule"
        );
        assert!(caps.vision, "still fills the field the chain left unset");
    }

    #[test]
    fn extends_chain_falls_through_to_provider_defaults() {
        // An unterminated extends chain (no later matching rule) fills its
        // remaining gaps from provider defaults.
        let caps = extends_caps(
            r#"
[provider_defaults.testprov]
seed_supported = true

[[provider.testprov]]
model_match = "test-*"
extends = true
native_tools = true
"#,
        );
        assert!(caps.native_tools, "field from the extends rule applies");
        assert!(
            caps.seed_supported,
            "unset field filled from provider defaults"
        );
    }
}
