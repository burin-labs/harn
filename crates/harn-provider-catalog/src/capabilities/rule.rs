//! Provider-rule model and the capability resolution engine.
//!
//! Owns the [`ProviderRule`] matrix row plus the machinery that walks the
//! provider/family rule chain (`resolve_rule_chain`, `absorb_layer_matches`,
//! `first_matching_rule`) and materializes a matched rule (or provider
//! defaults) into capability facts (the `rule_*`
//! field-derivation helpers). The field-by-field projection itself lives in
//! the sibling `projection` module.

use serde::Deserialize;
use std::collections::{BTreeMap, HashSet};

use super::decision::{DecisionLimits, DecisionProtocol, DecisionQuestionKind};
use super::effort::rule_reasoning_effort_supported;
use super::model::{
    fill_opt, CacheBreakpointStyle, CapabilitiesFile, ComputerUseStyle, LiveEndpointFamily,
    ProviderDefaults, ReasoningHistoryWireField, ReasoningRoundTripPolicy, ScreenshotScaling,
    SystemMessagePlacement, ThinkingOffType, ToolFormatJustification, ToolModeParitySource,
};
use super::pattern::{rule_matches, ModelPatterns};

/// The declaration owns merging and the empty projection. Identity fields
/// are retained; alias groups fill together so a lower-priority spelling can
/// never override an explicitly set alias.
macro_rules! provider_rule {
    (
        identity { $($(#[$ia:meta])* pub $identity:ident: $it:ty,)* }
        fields { $($(#[$fa:meta])* pub $field:ident: $ft:ty,)* }
        aliases { $( { $($(#[$aa:meta])* pub $alias:ident: $at:ty,)* } )* }
    ) => {
        #[derive(Debug, Clone, Deserialize)]
        #[non_exhaustive]
        pub struct ProviderRule {
            $($(#[$ia])* pub $identity: $it,)*
            $($(#[$fa])* pub $field: $ft,)*
            $($($(#[$aa])* pub $alias: $at,)*)*
        }

        impl ProviderRule {
            pub(super) fn for_defaults() -> Self {
                Self {
                    model_match: ModelPatterns::One("*".to_string()),
                    version_min: None,
                    extends: false,
                    $($field: None,)*
                    $($($alias: None,)*)*
                }
            }

            fn fill_missing_from(&mut self, other: &Self) {
                $(fill_opt(&mut self.$field, &other.$field);)*
                $(if $(self.$alias.is_none())&&* {
                    $(self.$alias.clone_from(&other.$alias);)*
                })*
            }
        }
    };
}

provider_rule! {
    identity {
        /// One glob pattern, or a list of aliases that share one contract.
        pub model_match: ModelPatterns,
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
    }
    fields {
        #[serde(default)]
        pub native_tools: Option<bool>,
        /// Message/request/response wire format used by shared helpers.
        /// Known values are `openai`, `anthropic`, `gemini`, and `ollama`.
        #[serde(default)]
        pub message_wire_format: Option<String>,
        /// Which synchronous endpoint family this route dispatches to when its
        /// dialect serves more than one. Unset on a Gemini-dialect row resolves to
        /// `gemini_generate_content`; unset elsewhere stays `None`. Independent of
        /// `batch_wire_format` — Gemini Batch is `generateContent`-only regardless.
        /// See [`LiveEndpointFamily`].
        #[serde(default)]
        pub live_endpoint_family: Option<LiveEndpointFamily>,
        /// Wire protocol this route serves the `decision` operation over. Typed
        /// and closed: an unknown value fails the capability load rather than
        /// falling back to the structured-output chat backend. The catalog
        /// `operations` set stays the admission owner; this says how an admitted
        /// route is dialled. See [`DecisionProtocol`].
        #[serde(default)]
        pub decision_protocol: Option<DecisionProtocol>,
        /// Question kinds this decision route accepts.
        #[serde(default)]
        pub decision_question_kinds: Option<Vec<DecisionQuestionKind>>,
        /// Declared request ceilings for this decision route. See
        /// [`DecisionLimits`].
        #[serde(default)]
        pub decision_limits: Option<DecisionLimits>,
        /// Exact upstream provider pin for a gateway decision route. Model
        /// authors and serving providers can differ, so do not infer it.
        #[serde(default)]
        pub decision_upstream_provider: Option<String>,
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
        /// `anthropic_messages`, `gemini`, `mistral`, `fireworks`, `xai`, and
        /// `bedrock`.
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
        /// Provider-published operational constraints for submit/retry/rejoin
        /// behavior. Keep these non-secret and capability-level, not caller
        /// branches.
        #[serde(default)]
        pub batch_operational_notes: Option<Vec<String>>,
        /// Explicit provider regions where this model's batch API is available.
        #[serde(default)]
        pub batch_regions: Option<Vec<String>>,
        /// Approval policy modes available when provider-hosted tools execute.
        #[serde(default)]
        pub tool_approval_policy: Option<String>,
        #[serde(default)]
        pub max_tools: Option<u32>,
        #[serde(default)]
        pub prompt_caching: Option<bool>,
        /// Explicit prompt-cache TTL values this rule can honor on request.
        /// Empty means the route may cache, but Harn has no explicit TTL knob for
        /// it. Known values today: `5m`, `1h`.
        #[serde(default)]
        pub prompt_cache_ttls: Option<Vec<String>>,
        /// Shortest prefix this route will actually cache, in tokens. Below this
        /// length a `cache_control` breakpoint is accepted and then silently
        /// ignored — no error, no `cache_creation_input_tokens`, just a full-price
        /// prompt every request. The floor is a *per-model* fact, not a wire-dialect
        /// one: on the Anthropic dialect it is 512 (Opus 5, Fable 5, Mythos 5),
        /// 1024 (Opus 4.8, Sonnet 5/4.6/4.5), 2048 (Opus 4.7, Mythos Preview), or
        /// 4096 (Opus 4.6/4.5, Haiku 4.5), and it is not monotonic across
        /// generations. `None` falls back to the dialect default in
        /// the runtime's cache-control profile.
        #[serde(default)]
        pub prompt_cache_min_prefix_tokens: Option<u32>,
        /// Request-side cache breakpoint strategy for routes that require
        /// `cache_control` to opt into provider prompt caching. Known values are
        /// `none`, `top_level`, and `last_block`.
        #[serde(default)]
        pub cache_breakpoint_style: Option<CacheBreakpointStyle>,
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
        /// Whether prompt sections should prefer XML-style tags such as
        /// `<task>` / `<examples>` over Markdown headings.
        #[serde(default)]
        pub prefers_xml_scaffolding: Option<bool>,
        /// Whether this model's tokenizer reserves `<tool_call>` / `</tool_call>`
        /// as single special tokens (the native Hermes tool-call markers). When
        /// true, harn remaps those delimiters to a non-special bracket form on the
        /// wire to avoid degenerate opener repetition.
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
        /// Whether Anthropic interleaved thinking is supported for this
        /// provider/model route.
        #[serde(default)]
        pub interleaved_thinking_supported: Option<bool>,
        /// Anthropic beta features that should be requested for this route.
        #[serde(default)]
        pub anthropic_beta_features: Option<Vec<String>>,
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
        /// Whether this transport honors `preserve_thinking` in chat-template options.
        #[serde(default)]
        pub honors_preserve_thinking_kwarg: Option<bool>,
        /// Provider-visible replay policy for prior assistant reasoning. Typed so
        /// unknown or misspelled policies fail capability loading.
        #[serde(default)]
        pub reasoning_round_trip: Option<ReasoningRoundTripPolicy>,
        /// Provider-specific field that must receive Harn's private reasoning on
        /// replayed assistant history. Typed so catalog typos fail at load time.
        #[serde(default)]
        pub reasoning_history_wire_field: Option<ReasoningHistoryWireField>,
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
        /// Whether function tools combined with enabled reasoning must use the
        /// Responses API even though reasoning-off calls remain compatible with
        /// Chat Completions.
        #[serde(default)]
        pub reasoning_tools_require_responses: Option<bool>,
        /// Whether this route rejects non-streaming chat-completion requests.
        /// Harn forces streaming for such routes so callers can keep provider-
        /// neutral `stream` preferences.
        #[serde(default)]
        pub requires_streaming: Option<bool>,
        /// Legacy spelling of "this route accepts Harn's provider-neutral reasoning
        /// effort control". Prefer `thinking_modes = [..., "effort"]`, which owns
        /// the answer; `true` here is absorbed into the modes by
        /// `rule_thinking_modes`, and `false` is not a way to remove `effort` that
        /// the modes declare. Providers project the capability to their native
        /// field (for example OpenAI `reasoning_effort` or Anthropic
        /// `output_config.effort`).
        #[serde(default)]
        pub reasoning_effort_supported: Option<bool>,
        /// Accepted effort values for routes that expose a narrower subset than
        /// Harn's provider-neutral enum. Empty means "unknown/all". A non-empty
        /// ladder also asserts that the route takes effort at all, so it is
        /// absorbed into `thinking_modes` the same way the legacy flag is --
        /// declaring rungs a route is not allowed to be asked for was the original
        /// contradiction this absorption removes.
        ///
        /// Verify a ladder with `harn provider effort-probe` rather than asserting
        /// it; the rungs are a claim about a live route, not a schema choice.
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
        /// The Anthropic `thinking.type` that turns thinking off where
        /// `reasoning_disable_supported` is true. Unset means `disabled`.
        #[serde(default)]
        pub thinking_off_type: Option<ThinkingOffType>,
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
        /// transports. `openai` selects scalar `reasoning_effort`, overriding
        /// an inherited dialect. Other values include `openrouter`, `enabled`,
        /// `minimax`, `groq_qwen`, and `thinking_type`.
        #[serde(default)]
        pub reasoning_wire_format: Option<String>,
        /// Caller-selected portable generation options this route permits normally
        /// but rejects while reasoning is enabled; `*_supported` remains route-wide.
        #[serde(default)]
        pub reasoning_excluded_portable_options: Option<Vec<super::PortableOption>>,
        /// Options admitted only with an explicitly selected, supported effort.
        /// This overrides route-wide support for the listed options; an unset
        /// request never satisfies the condition, even when its default is off.
        #[serde(default)]
        pub portable_option_effort_support:
            Option<std::collections::BTreeMap<super::PortableOption, Vec<super::ReasoningEffort>>>,
        /// A changing pool admits options by routing to currently compatible
        /// backends; one controlled refusal does not establish stable non-support.
        #[serde(default)]
        pub portable_option_qualification: Option<super::PortableOptionQualification>,
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
        /// Whether the route accepts OpenAI `stop` sequences. `false` strips the
        /// field before dispatch (xAI's Grok models reject it with HTTP 400).
        #[serde(default)]
        pub stop_supported: Option<bool>,
        #[serde(default)]
        pub advanced_generation_options: Option<Vec<super::PortableOption>>,
        /// Accepted provider-native `tool_choice` modes. Empty means unrestricted
        /// or unknown. Use this for routes whose native tools work, but whose API
        /// rejects forced/specified tool choices.
        #[serde(default)]
        pub allowed_tool_choice_modes: Option<Vec<String>>,
        /// Whether a single assistant message may contain multiple tool calls.
        /// Some OpenAI-compatible providers reject replayed history with more than
        /// one `tool_calls[]` entry even when the calls were parsed from Harn's text
        /// tool protocol, so the request builder must serialize history as
        /// one-call assistant turns for those routes.
        #[serde(default)]
        pub supports_parallel_tool_calls: Option<bool>,
        /// Whether this route requires `parallel_tool_calls: false` on the wire.
        #[serde(default)]
        pub requires_parallel_tool_calls_false: Option<bool>,
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
        /// Receipt or structural sibling link for this row's native/text
        /// decision. Required on self-hosted rows that set `native_tools` or
        /// `preferred_tool_format` (#6829).
        #[serde(default)]
        pub tool_format_justification: Option<ToolFormatJustification>,
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
        /// the runtime's OpenRouter route-denylist adapter).
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
        /// the runtime's OpenRouter provider-order adapter),
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
        /// the runtime's capability audit.
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
        /// How this route carries an interleaved `system`/`developer` message:
        /// `inline`, `native_directive`, or `fold`. Unset derives from the wire
        /// dialect (OpenAI/Ollama → inline, else fold). See
        /// [`SystemMessagePlacement`].
        #[serde(default)]
        pub system_message_placement: Option<SystemMessagePlacement>,
    }
    aliases {
        {
            /// Structured-output transport strategy. Known values are:
            /// `native`, `tool_use`, `format_kw`, and `none`.
            #[serde(default)]
            pub structured_output: Option<String>,
            /// Legacy name retained for project overrides written before
            /// `structured_output` became the canonical capability.
            #[serde(default)]
            pub json_schema: Option<String>,
        }
        {
            /// Supported thinking/reasoning modes for this rule. Values are
            /// script-facing mode names: `enabled`, `adaptive`, and `effort`.
            #[serde(default)]
            pub thinking_modes: Option<Vec<String>>,
            /// Legacy override compatibility. New built-in rules should use
            /// `thinking_modes` so the capability matrix preserves mode detail.
            #[serde(default)]
            pub thinking: Option<bool>,
        }
        {
            /// Whether this provider/model route accepts image or other visual
            /// input blocks through Harn's LLM message path.
            #[serde(default)]
            pub vision: Option<bool>,
            /// Whether the model accepts image inputs in chat content.
            #[serde(default)]
            pub vision_supported: Option<bool>,
        }
    }
}

impl ProviderRule {
    pub fn match_patterns(&self) -> impl Iterator<Item = &str> {
        self.model_match.iter()
    }

    pub fn match_label(&self) -> String {
        self.match_patterns().collect::<Vec<_>>().join(" | ")
    }
}

pub struct MatchedCapabilityRule {
    /// Provider layer of the first (highest-precedence) matched rule.
    pub provider: String,
    /// Effective rule: the first match, with fields it left unset filled from
    /// later matching rules while the chain opted into `extends` fall-through.
    pub rule: ProviderRule,
    /// `model_match` patterns of every absorbed rule, in precedence order.
    /// A single entry unless the first match set `extends = true`.
    pub matched_patterns: Vec<String>,
}

/// Accumulates matching rules along the resolution walk (user rules before
/// built-in rules within a layer, then the `provider_family` chain). The
/// first matched rule has the highest precedence; later matches only fill
/// fields the accumulated chain left unset, and only while every absorbed
/// rule so far opted into `extends` fall-through.
#[derive(Default)]
pub(super) struct RuleResolution {
    /// Provider layer of the first matched rule.
    pub(super) provider: Option<String>,
    pub(super) merged: Option<ProviderRule>,
    /// `model_match` provenance of every absorbed rule, in precedence order.
    pub(super) matched_patterns: Vec<String>,
    /// Each absorbed rule as an author would find it: its table header,
    /// `model_match`, and whether it came from a user overlay or Harn's
    /// built-in capability sources. Named in refusals and steer notes so a
    /// quirk is attributed to the row that caused it.
    pub(super) matched_rules: Vec<String>,
}

impl RuleResolution {
    /// Merge `rule` into the accumulator. Returns `true` when the walk must
    /// terminate: the rule does not opt into `extends` fall-through, which is
    /// exactly the pre-`extends` first-match-wins behavior.
    fn absorb(&mut self, layer_provider: &str, rule: &ProviderRule, origin: &str) -> bool {
        if self.provider.is_none() {
            self.provider = Some(layer_provider.to_string());
        }
        self.matched_patterns.push(rule.match_label());
        self.matched_rules.push(format!(
            "[[provider.{layer_provider}]] model_match = \"{}\" ({origin})",
            rule.match_label()
        ));
        match &mut self.merged {
            None => self.merged = Some(rule.clone()),
            Some(merged) => merged.fill_missing_from(rule),
        }
        !rule.extends
    }

    pub(super) fn into_matched(self) -> Option<MatchedCapabilityRule> {
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
pub(super) fn absorb_layer_matches(
    user: Option<&CapabilitiesFile>,
    builtin: &CapabilitiesFile,
    layer_provider: &str,
    model: &str,
    resolution: &mut RuleResolution,
) -> bool {
    let files = user
        .map(|file| (file, "user capability overlay"))
        .into_iter()
        .chain(std::iter::once((
            builtin,
            "Harn built-in, crates/harn-vm/src/llm/capability_sources",
        )));
    for (file, origin) in files {
        if let Some(rules) = file.provider.get(layer_provider) {
            for rule in rules {
                if rule_matches(rule, model) && resolution.absorb(layer_provider, rule, origin) {
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
pub(super) fn resolve_rule_chain(
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

pub fn rule_vision(rule: &ProviderRule) -> bool {
    rule.vision.or(rule.vision_supported).unwrap_or(false)
}

pub(super) fn merged_provider_defaults(
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

/// A route's tool-mode parity verdict together with where it came from.
///
/// The two travel as one value so a caller cannot read the verdict without
/// also being handed its provenance. Keeping them in one slot is what let a
/// derived fallback pass as an authored finding (#5885).
pub fn rule_tool_mode_parity(rule: &ProviderRule) -> (String, ToolModeParitySource) {
    if let Some(declared) = rule.tool_mode_parity.clone() {
        return (declared, ToolModeParitySource::Declared);
    }
    let derived = match (
        rule.native_tools.unwrap_or(false),
        rule.text_tool_wire_format_supported.unwrap_or(true),
    ) {
        (true, true) => "unknown",
        (true, false) => "native_only",
        (false, true) => "text_only",
        (false, false) => "unsupported",
    };
    (derived.to_string(), ToolModeParitySource::Derived)
}

pub fn rule_structured_output(rule: &ProviderRule) -> Option<String> {
    rule.structured_output
        .clone()
        .or_else(|| rule.json_schema.clone())
        .filter(|value| value != "none")
}

pub fn rule_structured_output_mode(rule: &ProviderRule) -> String {
    if let Some(mode) = &rule.structured_output_mode {
        return mode.clone();
    }
    match rule_structured_output(rule).as_deref() {
        Some("native") | Some("format_kw") => "native_json".to_string(),
        Some("tool_use") => "xml_tagged".to_string(),
        _ => "none".to_string(),
    }
}

pub fn rule_thinking_block_style(rule: &ProviderRule) -> String {
    rule.thinking_block_style.clone().unwrap_or_else(|| {
        if rule_reasoning_effort_supported(rule) || rule.requires_completion_tokens.unwrap_or(false)
        {
            "reasoning_summary".to_string()
        } else {
            "none".to_string()
        }
    })
}
