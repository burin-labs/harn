//! Data-driven provider capabilities.
//!
//! The per-(provider, model) capability matrix (native tools, deferred
//! tool loading, tool-search variants, prompt caching, extended thinking,
//! max tool count) lives in the shipped `capabilities.toml` and is
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
//! Before this module the Anthropic / OpenAI gates were spread across
//! `providers/anthropic.rs` and `providers/openai_compat.rs`. Their
//! generation parsers are still used here for `version_min`, but the
//! boolean gates that used to live alongside them are now data.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use super::providers::anthropic::claude_generation;
use super::providers::openai_compat::gpt_generation;

/// Shipped default rules. Compiled into the binary at build time.
const BUILTIN_TOML: &str = include_str!("capabilities.toml");

/// Parsed on-disk capabilities schema. Public so harn-cli can
/// construct one directly when wiring harn.toml overrides.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct CapabilitiesFile {
    /// Per-provider ordered rule lists. First matching rule wins.
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
    pub frequency_penalty_supported: Option<bool>,
    #[serde(default)]
    pub presence_penalty_supported: Option<bool>,
}

impl ProviderDefaults {
    fn overlay(&mut self, other: &ProviderDefaults) {
        if other.message_wire_format.is_some() {
            self.message_wire_format = other.message_wire_format.clone();
        }
        if other.native_tool_wire_format.is_some() {
            self.native_tool_wire_format = other.native_tool_wire_format.clone();
        }
        if other.image_url_input_supported.is_some() {
            self.image_url_input_supported = other.image_url_input_supported;
        }
        if other.file_upload_wire_format.is_some() {
            self.file_upload_wire_format = other.file_upload_wire_format.clone();
        }
        if other.reasoning_wire_format.is_some() {
            self.reasoning_wire_format = other.reasoning_wire_format.clone();
        }
        if other.files_api_supported.is_some() {
            self.files_api_supported = other.files_api_supported;
        }
        if other.seed_supported.is_some() {
            self.seed_supported = other.seed_supported;
        }
        if other.top_k_supported.is_some() {
            self.top_k_supported = other.top_k_supported;
        }
        if other.frequency_penalty_supported.is_some() {
            self.frequency_penalty_supported = other.frequency_penalty_supported;
        }
        if other.presence_penalty_supported.is_some() {
            self.presence_penalty_supported = other.presence_penalty_supported;
        }
    }

    fn fill_missing_from(&mut self, other: &ProviderDefaults) {
        if self.message_wire_format.is_none() {
            self.message_wire_format = other.message_wire_format.clone();
        }
        if self.native_tool_wire_format.is_none() {
            self.native_tool_wire_format = other.native_tool_wire_format.clone();
        }
        if self.image_url_input_supported.is_none() {
            self.image_url_input_supported = other.image_url_input_supported;
        }
        if self.file_upload_wire_format.is_none() {
            self.file_upload_wire_format = other.file_upload_wire_format.clone();
        }
        if self.reasoning_wire_format.is_none() {
            self.reasoning_wire_format = other.reasoning_wire_format.clone();
        }
        if self.files_api_supported.is_none() {
            self.files_api_supported = other.files_api_supported;
        }
        if self.seed_supported.is_none() {
            self.seed_supported = other.seed_supported;
        }
        if self.top_k_supported.is_none() {
            self.top_k_supported = other.top_k_supported;
        }
        if self.frequency_penalty_supported.is_none() {
            self.frequency_penalty_supported = other.frequency_penalty_supported;
        }
        if self.presence_penalty_supported.is_none() {
            self.presence_penalty_supported = other.presence_penalty_supported;
        }
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
    #[serde(default)]
    pub max_tools: Option<u32>,
    #[serde(default)]
    pub prompt_caching: Option<bool>,
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
    /// Whether provider-specific `chat_template_kwargs` are honored.
    /// Some OpenAI-compatible servers silently drop unknown kwargs.
    #[serde(default)]
    pub honors_chat_template_kwargs: Option<bool>,
    /// Whether this route requires OpenAI's `max_completion_tokens`
    /// request field instead of legacy `max_tokens`.
    #[serde(default)]
    pub requires_completion_tokens: Option<bool>,
    /// Whether this route accepts OpenAI's `reasoning_effort` request field.
    #[serde(default)]
    pub reasoning_effort_supported: Option<bool>,
    /// Whether this route accepts `reasoning_effort: "none"` as a true
    /// reasoning-off setting. Older GPT-5 variants support effort but only
    /// floor at `minimal`.
    #[serde(default)]
    pub reasoning_none_supported: Option<bool>,
    /// Provider-specific reasoning request shape for OpenAI-compatible
    /// transports. Known values are `openrouter` and `enabled`.
    #[serde(default)]
    pub reasoning_wire_format: Option<String>,
    #[serde(default)]
    pub seed_supported: Option<bool>,
    #[serde(default)]
    pub top_k_supported: Option<bool>,
    #[serde(default)]
    pub frequency_penalty_supported: Option<bool>,
    #[serde(default)]
    pub presence_penalty_supported: Option<bool>,
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
    /// `medium`, `high`, `xhigh`). Consulted by `reasoning_policy` only
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
    pub max_tools: Option<u32>,
    pub prompt_caching: bool,
    pub vision: bool,
    pub audio: bool,
    pub pdf: bool,
    pub files_api_supported: bool,
    pub file_upload_wire_format: Option<String>,
    pub structured_output: Option<String>,
    /// Legacy mirror for CLI display and older callers.
    pub json_schema: Option<String>,
    pub prefers_xml_scaffolding: bool,
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
    pub requires_completion_tokens: bool,
    pub reasoning_effort_supported: bool,
    pub reasoning_none_supported: bool,
    pub reasoning_wire_format: Option<String>,
    pub seed_supported: bool,
    pub top_k_supported: bool,
    pub frequency_penalty_supported: bool,
    pub presence_penalty_supported: bool,
    pub recommended_endpoint: Option<String>,
    pub text_tool_wire_format_supported: bool,
    pub preferred_tool_format: Option<String>,
    pub tool_mode_parity: Option<String>,
    pub tool_mode_parity_notes: Option<String>,
    pub thinking_disable_directive: Option<String>,
    /// Per-task auto-policy reasoning-level overrides for this route.
    /// See [`ProviderRule::auto_reasoning_overrides`].
    pub auto_reasoning_overrides: BTreeMap<String, String>,
}

impl Default for Capabilities {
    fn default() -> Self {
        Self {
            native_tools: false,
            message_wire_format: "openai".to_string(),
            native_tool_wire_format: "openai".to_string(),
            defer_loading: false,
            tool_search: Vec::new(),
            max_tools: None,
            prompt_caching: false,
            vision: false,
            audio: false,
            pdf: false,
            files_api_supported: false,
            file_upload_wire_format: None,
            structured_output: None,
            json_schema: None,
            prefers_xml_scaffolding: false,
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
            requires_completion_tokens: false,
            reasoning_effort_supported: false,
            reasoning_none_supported: false,
            reasoning_wire_format: None,
            seed_supported: true,
            top_k_supported: true,
            frequency_penalty_supported: true,
            presence_penalty_supported: true,
            recommended_endpoint: None,
            text_tool_wire_format_supported: true,
            preferred_tool_format: None,
            tool_mode_parity: None,
            tool_mode_parity_notes: None,
            thinking_disable_directive: None,
            auto_reasoning_overrides: BTreeMap::new(),
        }
    }
}

/// Display-oriented row for `harn providers matrix`, the legacy
/// `harn check --provider-matrix` surface, and the generated docs page. Rows
/// are intentionally rule-shaped: `model` is the rule's `model_match` pattern,
/// because the shipped capability source of truth is a first-match rule table
/// rather than an exhaustive remote model inventory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProviderCapabilityMatrixRow {
    pub provider: String,
    pub model: String,
    pub version_min: Option<Vec<u32>>,
    pub thinking: Vec<String>,
    pub vision: bool,
    pub audio: bool,
    pub pdf: bool,
    pub streaming: bool,
    pub files_api_supported: bool,
    pub json_schema: Option<String>,
    pub prefers_xml_scaffolding: bool,
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
    pub source: String,
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
    let parsed: CapabilitiesFile = toml::from_str(src).map_err(|e| e.to_string())?;
    set_user_overrides(Some(parsed));
    Ok(())
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
/// later rules (and later layers in the family chain) are ignored.
pub fn lookup(provider: &str, model: &str) -> Capabilities {
    let user = USER_OVERRIDES.with(|cell| cell.borrow().clone());
    lookup_with(provider, model, builtin(), user.as_ref())
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
        thinking: rule_thinking_modes(rule),
        vision: rule_vision(rule),
        audio: rule.audio.unwrap_or(false),
        pdf: rule.pdf.unwrap_or(false),
        streaming: true,
        files_api_supported: rule.files_api_supported.unwrap_or(false),
        json_schema: rule_structured_output(rule),
        prefers_xml_scaffolding: rule.prefers_xml_scaffolding.unwrap_or(false),
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
        let anthropic_defaults = merged_provider_defaults(user, builtin, "anthropic");
        if let Some(mut caps) =
            try_match_layer(user, builtin, "anthropic", model, &anthropic_defaults)
        {
            caps.native_tool_wire_format = "openai".to_string();
            return caps;
        }
        let openai_defaults = merged_provider_defaults(user, builtin, "openai");
        if let Some(caps) = try_match_layer(user, builtin, "openai", model, &openai_defaults) {
            return caps;
        }
        let gemini_defaults = merged_provider_defaults(user, builtin, "gemini");
        if let Some(caps) = try_match_layer(user, builtin, "gemini", model, &gemini_defaults) {
            return caps;
        }
        return Capabilities::default();
    }

    // Normal chain: walk provider → family(provider) → ... with a
    // visited-guard to avoid cycles in malformed user overrides.
    let mut current = provider.to_string();
    let mut effective_defaults = ProviderDefaults::default();
    let mut visited: std::collections::HashSet<String> = std::collections::HashSet::new();
    while visited.insert(current.clone()) {
        let layer_defaults = merged_provider_defaults(user, builtin, &current);
        if effective_defaults.has_any_field() {
            effective_defaults.fill_missing_from(&layer_defaults);
        } else {
            effective_defaults.overlay(&layer_defaults);
        }
        if let Some(caps) = try_match_layer(user, builtin, &current, model, &effective_defaults) {
            return caps;
        }
        let next = user
            .and_then(|f| f.provider_family.get(&current))
            .or_else(|| builtin.provider_family.get(&current))
            .cloned();
        match next {
            Some(parent) => current = parent,
            None => break,
        }
    }
    if effective_defaults.has_any_field() {
        return defaults_to_caps(&effective_defaults);
    }
    Capabilities::default()
}

/// Try the ordered rule list for `layer_provider` (user rules first,
/// then built-in rules). Returns `Some(caps)` on the first match, else
/// `None`. `original_provider` is threaded through only for diagnostics.
fn try_match_layer(
    user: Option<&CapabilitiesFile>,
    builtin: &CapabilitiesFile,
    layer_provider: &str,
    model: &str,
    defaults: &ProviderDefaults,
) -> Option<Capabilities> {
    if let Some(user) = user {
        if let Some(rules) = user.provider.get(layer_provider) {
            for rule in rules {
                if rule_matches(rule, model) {
                    return Some(rule_to_caps(rule, defaults));
                }
            }
        }
    }
    if let Some(rules) = builtin.provider.get(layer_provider) {
        for rule in rules {
            if rule_matches(rule, model) {
                return Some(rule_to_caps(rule, defaults));
            }
        }
    }
    None
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
        native_tools: None,
        message_wire_format: None,
        native_tool_wire_format: None,
        defer_loading: None,
        tool_search: None,
        max_tools: None,
        prompt_caching: None,
        vision: None,
        audio: None,
        pdf: None,
        files_api_supported: None,
        file_upload_wire_format: None,
        structured_output: None,
        prefers_xml_scaffolding: None,
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
        requires_completion_tokens: None,
        reasoning_effort_supported: None,
        reasoning_none_supported: None,
        reasoning_wire_format: None,
        seed_supported: None,
        top_k_supported: None,
        frequency_penalty_supported: None,
        presence_penalty_supported: None,
        recommended_endpoint: None,
        text_tool_wire_format_supported: None,
        preferred_tool_format: None,
        tool_mode_parity: None,
        tool_mode_parity_notes: None,
        thinking_disable_directive: None,
        auto_reasoning_overrides: None,
    };
    rule_to_caps(&empty, defaults)
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
        max_tools: rule.max_tools,
        prompt_caching: rule.prompt_caching.unwrap_or(false),
        vision: rule_vision(rule),
        audio: rule.audio.unwrap_or(false),
        pdf: rule.pdf.unwrap_or(false),
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
        requires_completion_tokens: rule.requires_completion_tokens.unwrap_or(false),
        reasoning_effort_supported: rule.reasoning_effort_supported.unwrap_or(false),
        reasoning_none_supported: rule.reasoning_none_supported.unwrap_or(false),
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
        frequency_penalty_supported: rule
            .frequency_penalty_supported
            .or(defaults.frequency_penalty_supported)
            .unwrap_or(true),
        presence_penalty_supported: rule
            .presence_penalty_supported
            .or(defaults.presence_penalty_supported)
            .unwrap_or(true),
        recommended_endpoint: rule.recommended_endpoint.clone(),
        text_tool_wire_format_supported: rule.text_tool_wire_format_supported.unwrap_or(true),
        preferred_tool_format: Some(rule_preferred_tool_format(rule)),
        tool_mode_parity: Some(rule_tool_mode_parity(rule)),
        tool_mode_parity_notes: rule.tool_mode_parity_notes.clone(),
        thinking_disable_directive: rule.thinking_disable_directive.clone(),
        auto_reasoning_overrides: rule.auto_reasoning_overrides.clone().unwrap_or_default(),
    }
}

fn rule_preferred_tool_format(rule: &ProviderRule) -> String {
    rule.preferred_tool_format.clone().unwrap_or_else(|| {
        if rule.native_tools.unwrap_or(false) {
            "native".to_string()
        } else {
            "text".to_string()
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

fn rule_matches(rule: &ProviderRule, model: &str) -> bool {
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

/// Simple glob matching with `*` wildcards. Mirrors the helper in
/// `llm_config.rs` — keep them in sync if either ever grows regex or
/// character-class support.
fn glob_match(pattern: &str, input: &str) -> bool {
    if let Some(prefix) = pattern.strip_suffix('*') {
        if let Some(rest) = prefix.strip_prefix('*') {
            // `*foo*` — substring match.
            return input.contains(rest);
        }
        return input.starts_with(prefix);
    }
    if let Some(suffix) = pattern.strip_prefix('*') {
        return input.ends_with(suffix);
    }
    if pattern.contains('*') {
        let parts: Vec<&str> = pattern.split('*').collect();
        if parts.len() == 2 {
            return input.starts_with(parts[0]) && input.ends_with(parts[1]);
        }
        return input == pattern;
    }
    input == pattern
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reset() {
        clear_user_overrides();
    }

    #[test]
    fn anthropic_opus_47_gets_full_capabilities() {
        reset();
        let caps = lookup("anthropic", "claude-opus-4-7");
        assert!(caps.native_tools);
        assert!(caps.defer_loading);
        assert_eq!(caps.tool_search, vec!["bm25", "regex"]);
        assert!(caps.prompt_caching);
        assert_eq!(caps.thinking_modes, vec!["adaptive"]);
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
    fn openrouter_inherits_openai() {
        reset();
        let caps = lookup("openrouter", "gpt-5.4");
        assert!(caps.defer_loading);
        assert_eq!(caps.tool_search, vec!["hosted", "client"]);
        assert_eq!(caps.reasoning_wire_format.as_deref(), Some("openrouter"));
        assert!(!caps.top_k_supported);
    }

    #[test]
    fn openrouter_structured_routes_cover_current_open_models() {
        reset();
        for model in [
            "deepseek/deepseek-v4-flash",
            "mistralai/devstral-small",
            "meta-llama/llama-4-scout",
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
    fn openrouter_deepseek_v32_defaults_to_text_tools() {
        reset();
        let caps = lookup("openrouter", "deepseek/deepseek-v3.2");
        assert!(caps.native_tools);
        assert!(caps.text_tool_wire_format_supported);
        assert_eq!(caps.preferred_tool_format.as_deref(), Some("text"));
        assert_eq!(caps.tool_mode_parity.as_deref(), Some("native_unreliable"));
        assert_eq!(caps.structured_output.as_deref(), Some("native"));
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
        assert!(caps.native_tools);
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
            ("mlx", "unsloth/Qwen3.6-27B-UD-MLX-4bit"),
            ("mlx", "Qwen/Qwen3.6-27B"),
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
    fn devstral_local_routes_default_to_text_tools() {
        reset();
        for provider in ["ollama", "llamacpp"] {
            let caps = lookup(provider, "devstral-small-2:24b");
            assert!(!caps.native_tools, "{provider}: native tools stay opt-in");
            assert!(
                caps.text_tool_wire_format_supported,
                "{provider}: text tools should remain available"
            );
        }
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
}
