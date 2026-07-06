//! Provider-rule model and the capability resolution engine.
//!
//! Owns the [`ProviderRule`] matrix row plus the machinery that walks the
//! provider/family rule chain (`resolve_rule_chain`, `absorb_layer_matches`,
//! `first_matching_rule`) and materializes a matched rule (or provider
//! defaults) into a [`Capabilities`] value (`lookup_with`, `rule_to_caps`,
//! `defaults_to_caps`, and the `rule_*` field-derivation helpers).

use std::collections::{BTreeMap, HashSet};

use serde::Deserialize;

use super::model::{
    fill_opt, Capabilities, CapabilitiesFile, ComputerUseStyle, ProviderDefaults,
    ScreenshotScaling, WireDialect,
};
use crate::llm::providers::anthropic::claude_generation;
use crate::llm::providers::openai_compat::gpt_generation;

// Model-pattern matching for capability rules. Shared workspace semantics live
// in `harn-glob`; keep capability and provider matching on that helper instead
// of mirroring glob behavior locally.
use harn_glob::match_name as glob_match;

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
    /// Whether this provider/model route can be submitted through an
    /// asynchronous provider Batch API for offline, non-interactive work.
    #[serde(default)]
    pub batch_api: Option<bool>,
    /// Provider batch request/result family. Known values are `openai`,
    /// `anthropic_messages`, `gemini`, `mistral`, `fireworks`, and `xai`.
    #[serde(default)]
    pub batch_wire_format: Option<String>,
    /// How a batch accepts work: `jsonl_file`, `inline_requests`, or
    /// `jsonl_or_inline`.
    #[serde(default)]
    pub batch_input_mode: Option<String>,
    /// Published percent discount versus synchronous inference for equivalent
    /// model traffic, when known.
    #[serde(default)]
    pub batch_discount_percent: Option<u32>,
    /// Target or maximum turnaround window in hours, when the provider
    /// publishes one.
    #[serde(default)]
    pub batch_turnaround_hours: Option<u32>,
    /// Maximum requests/items per provider batch, when published.
    #[serde(default)]
    pub batch_max_requests: Option<u64>,
    /// Maximum submitted request-file/body bytes per provider batch, when
    /// published.
    #[serde(default)]
    pub batch_max_input_bytes: Option<u64>,
    /// Number of days provider-side result artifacts remain available, when
    /// published.
    #[serde(default)]
    pub batch_result_retention_days: Option<u32>,
    /// Result ordering contract. Known values: `custom_id_rejoin`,
    /// `provider_ordered`, `unknown`.
    #[serde(default)]
    pub batch_result_ordering: Option<String>,
    /// Partial-failure semantics. Known values: `per_request`, `whole_batch`,
    /// `unknown`.
    #[serde(default)]
    pub batch_partial_failure: Option<String>,
    /// Cancellation support. Known values: `supported`, `not_supported`,
    /// `unknown`.
    #[serde(default)]
    pub batch_cancellation: Option<String>,
    /// Provider-published storage/security notes safe to surface in catalogs
    /// and receipts. Never store secrets here.
    #[serde(default)]
    pub batch_security_notes: Option<Vec<String>>,
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
    /// Whether this route is served ONLY by the provider's Responses-style
    /// API and rejects `/v1/chat/completions` (OpenAI `*-codex` models
    /// return HTTP 404 "Use the v1/responses endpoint instead" on the chat
    /// endpoint). When set, Harn routes the call through the Responses
    /// provider even when the caller did not explicitly request
    /// `api_mode: "responses"`.
    #[serde(default)]
    pub chat_completions_unsupported: Option<bool>,
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
    /// How the neutral `computer` tool projects onto this route's native
    /// computer-use surface. `harn-vm` reads this to decide whether to inject
    /// a provider-native computer tool (and suppress the plain function copy)
    /// or leave the function-schema tool untouched. Known values are
    /// `native_anthropic` (Anthropic `computer_20251124`), `native_openai`
    /// (OpenAI Responses `computer`), `grounded` (element/mark addressing
    /// resolved locally), and `function` (generic function-schema fallback).
    /// Unset means the route has no computer-use surface.
    #[serde(default)]
    pub computer_use_style: Option<ComputerUseStyle>,
    /// Screenshot downscaling policy applied before the image reaches the
    /// model. `xga` fits within 1024x768 preserving aspect (Anthropic);
    /// `original` is identity (OpenAI). Unset means unset.
    #[serde(default)]
    pub screenshot_scaling: Option<ScreenshotScaling>,
    /// Whether this route requires echoing acknowledged safety checks on the
    /// computer-use follow-up turn (OpenAI Responses surfaces
    /// `pending_safety_checks` that must be echoed as
    /// `acknowledged_safety_checks`). Unset resolves to `false`.
    #[serde(default)]
    pub safety_ack_flow: Option<bool>,
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
            batch_api,
            batch_wire_format,
            batch_input_mode,
            batch_discount_percent,
            batch_turnaround_hours,
            batch_max_requests,
            batch_max_input_bytes,
            batch_result_retention_days,
            batch_result_ordering,
            batch_partial_failure,
            batch_cancellation,
            batch_security_notes,
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
            chat_completions_unsupported,
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
            computer_use_style,
            screenshot_scaling,
            safety_ack_flow,
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
        fill_opt(&mut self.batch_api, batch_api);
        fill_opt(&mut self.batch_wire_format, batch_wire_format);
        fill_opt(&mut self.batch_input_mode, batch_input_mode);
        fill_opt(&mut self.batch_discount_percent, batch_discount_percent);
        fill_opt(&mut self.batch_turnaround_hours, batch_turnaround_hours);
        fill_opt(&mut self.batch_max_requests, batch_max_requests);
        fill_opt(&mut self.batch_max_input_bytes, batch_max_input_bytes);
        fill_opt(
            &mut self.batch_result_retention_days,
            batch_result_retention_days,
        );
        fill_opt(&mut self.batch_result_ordering, batch_result_ordering);
        fill_opt(&mut self.batch_partial_failure, batch_partial_failure);
        fill_opt(&mut self.batch_cancellation, batch_cancellation);
        fill_opt(&mut self.batch_security_notes, batch_security_notes);
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
        fill_opt(
            &mut self.chat_completions_unsupported,
            chat_completions_unsupported,
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
        fill_opt(&mut self.computer_use_style, computer_use_style);
        fill_opt(&mut self.screenshot_scaling, screenshot_scaling);
        fill_opt(&mut self.safety_ack_flow, safety_ack_flow);
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

pub(super) struct MatchedCapabilityRule {
    /// Provider layer of the first (highest-precedence) matched rule.
    pub(super) provider: String,
    /// Effective rule: the first match, with fields it left unset filled from
    /// later matching rules while the chain opted into `extends` fall-through.
    pub(super) rule: ProviderRule,
    /// `model_match` patterns of every absorbed rule, in precedence order.
    /// A single entry unless the first match set `extends = true`.
    pub(super) matched_patterns: Vec<String>,
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

pub(super) fn first_matching_rule(
    user: Option<&CapabilitiesFile>,
    builtin: &CapabilitiesFile,
    provider: &str,
    model: &str,
) -> Option<MatchedCapabilityRule> {
    resolve_rule_chain(user, builtin, provider, model)
        .0
        .into_matched()
}

pub(super) fn rule_thinking_modes(rule: &ProviderRule) -> Vec<String> {
    rule.thinking_modes.clone().unwrap_or_else(|| {
        if rule.thinking.unwrap_or(false) {
            vec!["enabled".to_string()]
        } else {
            Vec::new()
        }
    })
}

pub(super) fn rule_vision(rule: &ProviderRule) -> bool {
    rule.vision.or(rule.vision_supported).unwrap_or(false)
}

pub(super) fn lookup_with(
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
        batch_api: None,
        batch_wire_format: None,
        batch_input_mode: None,
        batch_discount_percent: None,
        batch_turnaround_hours: None,
        batch_max_requests: None,
        batch_max_input_bytes: None,
        batch_result_retention_days: None,
        batch_result_ordering: None,
        batch_partial_failure: None,
        batch_cancellation: None,
        batch_security_notes: None,
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
        chat_completions_unsupported: None,
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
        computer_use_style: None,
        screenshot_scaling: None,
        safety_ack_flow: None,
    };
    let mut caps = rule_to_caps(&empty, defaults);
    caps.preferred_tool_format = None;
    caps.tool_mode_parity = None;
    caps
}

fn rule_to_caps(rule: &ProviderRule, defaults: &ProviderDefaults) -> Capabilities {
    let thinking_modes = rule_thinking_modes(rule);
    let thinking_block_style = rule_thinking_block_style(rule);
    // A route that represents reasoning as inline `<think>` blocks in prompt
    // context is exactly the one that emits inline `<think>` in its responses,
    // so derive the response-splitting quirk from the resolved style rather
    // than adding a second, drift-prone catalog field.
    let emits_inline_reasoning = thinking_block_style == "inline";
    Capabilities {
        native_tools: rule.native_tools.unwrap_or(false),
        message_wire_format: WireDialect::from_message_wire_format(
            &rule
                .message_wire_format
                .clone()
                .or_else(|| defaults.message_wire_format.clone())
                .unwrap_or_else(|| "openai".to_string()),
        ),
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
        batch_api: rule.batch_api.or(defaults.batch_api).unwrap_or(false),
        batch_wire_format: rule
            .batch_wire_format
            .clone()
            .or_else(|| defaults.batch_wire_format.clone()),
        batch_input_mode: rule
            .batch_input_mode
            .clone()
            .or_else(|| defaults.batch_input_mode.clone()),
        batch_discount_percent: rule
            .batch_discount_percent
            .or(defaults.batch_discount_percent),
        batch_turnaround_hours: rule
            .batch_turnaround_hours
            .or(defaults.batch_turnaround_hours),
        batch_max_requests: rule.batch_max_requests.or(defaults.batch_max_requests),
        batch_max_input_bytes: rule
            .batch_max_input_bytes
            .or(defaults.batch_max_input_bytes),
        batch_result_retention_days: rule
            .batch_result_retention_days
            .or(defaults.batch_result_retention_days),
        batch_result_ordering: rule
            .batch_result_ordering
            .clone()
            .or_else(|| defaults.batch_result_ordering.clone()),
        batch_partial_failure: rule
            .batch_partial_failure
            .clone()
            .or_else(|| defaults.batch_partial_failure.clone()),
        batch_cancellation: rule
            .batch_cancellation
            .clone()
            .or_else(|| defaults.batch_cancellation.clone()),
        batch_security_notes: rule
            .batch_security_notes
            .clone()
            .or_else(|| defaults.batch_security_notes.clone())
            .unwrap_or_default(),
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
        thinking_block_style,
        emits_inline_reasoning,
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
        chat_completions_unsupported: rule.chat_completions_unsupported.unwrap_or(false),
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
        computer_use_style: rule.computer_use_style,
        screenshot_scaling: rule.screenshot_scaling,
        safety_ack_flow: rule.safety_ack_flow.unwrap_or(false),
    }
}

pub(super) fn rule_preferred_tool_format(rule: &ProviderRule) -> String {
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

pub(super) fn rule_tool_mode_parity(rule: &ProviderRule) -> String {
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

pub(super) fn rule_structured_output(rule: &ProviderRule) -> Option<String> {
    rule.structured_output
        .clone()
        .or_else(|| rule.json_schema.clone())
        .filter(|value| value != "none")
}

pub(super) fn rule_structured_output_mode(rule: &ProviderRule) -> String {
    if let Some(mode) = &rule.structured_output_mode {
        return mode.clone();
    }
    match rule_structured_output(rule).as_deref() {
        Some("native") | Some("format_kw") => "native_json".to_string(),
        Some("tool_use") => "xml_tagged".to_string(),
        _ => "none".to_string(),
    }
}

pub(super) fn rule_thinking_block_style(rule: &ProviderRule) -> String {
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

#[cfg(test)]
mod tests {
    use super::super::lookup::parse_capabilities_toml;
    use super::*;

    #[test]
    fn glob_match_substring() {
        assert!(glob_match("*gpt*", "openai/gpt-5.4"));
        assert!(glob_match("*claude*", "anthropic/claude-opus-4-7"));
        assert!(!glob_match("*xyz*", "openai/gpt-5.4"));
    }

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
        assert_eq!(caps.message_wire_format, WireDialect::Anthropic);
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
