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
//! `providers/anthropic.rs` (`claude_generation`, `claude_model_supports_tool_search`)
//! and `providers/openai_compat.rs` (`gpt_generation`, `gpt_model_supports_tool_search`).
//! Those parsers are still used here — they supply the version extractor —
//! but the boolean gates that used to live alongside them are now data.

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
    /// Sibling → canonical family mapping. Providers with no rule of
    /// their own fall through to the named family (recursively).
    #[serde(default)]
    pub provider_family: BTreeMap<String, String>,
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
    #[serde(default)]
    pub audio: Option<bool>,
    /// Structured-output transport strategy. Known values are:
    /// `native`, `tool_use`, and `format_kw`.
    #[serde(default)]
    pub json_schema: Option<String>,
    /// Supported thinking/reasoning modes for this rule. Values are
    /// script-facing mode names: `enabled`, `adaptive`, and `effort`.
    #[serde(default)]
    pub thinking_modes: Option<Vec<String>>,
    /// Legacy override compatibility. New built-in rules should use
    /// `thinking_modes` so the capability matrix preserves mode detail.
    #[serde(default)]
    pub thinking: Option<bool>,
    /// Whether the model accepts image inputs in chat content.
    #[serde(default)]
    pub vision_supported: Option<bool>,
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
    /// Preferred endpoint family for this provider/model route. Values
    /// are descriptive labels consumed by providers, e.g.
    /// `/api/generate-raw` for Ollama raw prompt bypass.
    #[serde(default)]
    pub recommended_endpoint: Option<String>,
    /// Whether Harn's text-tool protocol (`<tool_call>name({...})`) can
    /// survive the provider route and return in the visible response body.
    #[serde(default)]
    pub text_tool_wire_format_supported: Option<bool>,
}

/// Resolved capabilities for a `(provider, model)` pair. Unset rule
/// fields resolve to `false` / empty / `None` so callers never have to
/// unwrap an `Option<bool>` for what are really boolean gates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capabilities {
    pub native_tools: bool,
    pub defer_loading: bool,
    pub tool_search: Vec<String>,
    pub max_tools: Option<u32>,
    pub prompt_caching: bool,
    pub vision: bool,
    pub audio: bool,
    pub json_schema: Option<String>,
    pub thinking_modes: Vec<String>,
    pub vision_supported: bool,
    pub preserve_thinking: bool,
    pub server_parser: String,
    pub honors_chat_template_kwargs: bool,
    pub recommended_endpoint: Option<String>,
    pub text_tool_wire_format_supported: bool,
}

impl Default for Capabilities {
    fn default() -> Self {
        Self {
            native_tools: false,
            defer_loading: false,
            tool_search: Vec::new(),
            max_tools: None,
            prompt_caching: false,
            vision: false,
            audio: false,
            json_schema: None,
            thinking_modes: Vec::new(),
            vision_supported: false,
            preserve_thinking: false,
            server_parser: "none".to_string(),
            honors_chat_template_kwargs: false,
            recommended_endpoint: None,
            text_tool_wire_format_supported: true,
        }
    }
}

/// Display-oriented row for `harn check --provider-matrix` and the generated
/// docs page. Rows are intentionally rule-shaped: `model` is the rule's
/// `model_match` pattern, because the shipped capability source of truth is a
/// first-match rule table rather than an exhaustive remote model inventory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProviderCapabilityMatrixRow {
    pub provider: String,
    pub model: String,
    pub thinking: Vec<String>,
    pub vision: bool,
    pub audio: bool,
    pub json_schema: Option<String>,
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
        thinking: rule_thinking_modes(rule),
        vision: rule_vision(rule),
        audio: rule.audio.unwrap_or(false),
        json_schema: rule.json_schema.clone(),
        tools: rule.native_tools.unwrap_or(false),
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
    // the hardcoded dispatch gave before this refactor.
    if provider == "mock" {
        if let Some(caps) = try_match_layer(user, builtin, "anthropic", model, provider) {
            return caps;
        }
        if let Some(caps) = try_match_layer(user, builtin, "openai", model, provider) {
            return caps;
        }
        return Capabilities::default();
    }

    // Normal chain: walk provider → family(provider) → ... with a
    // visited-guard to avoid cycles in malformed user overrides.
    let mut current = provider.to_string();
    let mut visited: std::collections::HashSet<String> = std::collections::HashSet::new();
    while visited.insert(current.clone()) {
        if let Some(caps) = try_match_layer(user, builtin, &current, model, provider) {
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
    _original_provider: &str,
) -> Option<Capabilities> {
    if let Some(user) = user {
        if let Some(rules) = user.provider.get(layer_provider) {
            for rule in rules {
                if rule_matches(rule, model) {
                    return Some(rule_to_caps(rule));
                }
            }
        }
    }
    if let Some(rules) = builtin.provider.get(layer_provider) {
        for rule in rules {
            if rule_matches(rule, model) {
                return Some(rule_to_caps(rule));
            }
        }
    }
    None
}

fn rule_to_caps(rule: &ProviderRule) -> Capabilities {
    let thinking_modes = rule_thinking_modes(rule);
    Capabilities {
        native_tools: rule.native_tools.unwrap_or(false),
        defer_loading: rule.defer_loading.unwrap_or(false),
        tool_search: rule.tool_search.clone().unwrap_or_default(),
        max_tools: rule.max_tools,
        prompt_caching: rule.prompt_caching.unwrap_or(false),
        vision: rule_vision(rule),
        audio: rule.audio.unwrap_or(false),
        json_schema: rule.json_schema.clone(),
        thinking_modes,
        vision_supported: rule.vision_supported.unwrap_or(false),
        preserve_thinking: rule.preserve_thinking.unwrap_or(false),
        server_parser: rule
            .server_parser
            .clone()
            .unwrap_or_else(|| "none".to_string()),
        honors_chat_template_kwargs: rule.honors_chat_template_kwargs.unwrap_or(false),
        recommended_endpoint: rule.recommended_endpoint.clone(),
        text_tool_wire_format_supported: rule.text_tool_wire_format_supported.unwrap_or(true),
    }
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
        assert_eq!(caps.max_tools, Some(10000));
    }

    #[test]
    fn anthropic_opus_46_uses_budgeted_thinking() {
        reset();
        let caps = lookup("anthropic", "claude-opus-4-6");
        assert_eq!(caps.thinking_modes, vec!["enabled"]);
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
    }

    #[test]
    fn openai_gpt_53_has_native_tools_only() {
        reset();
        let caps = lookup("openai", "gpt-5.3");
        assert!(caps.native_tools);
        assert!(!caps.defer_loading);
        assert!(!caps.vision_supported);
        assert!(caps.tool_search.is_empty());
    }

    #[test]
    fn openai_gpt_4o_matrix_fields_include_multimodal_support() {
        reset();
        let caps = lookup("openai", "gpt-4o");
        assert!(caps.native_tools);
        assert!(caps.vision);
        assert!(caps.audio);
        assert_eq!(caps.json_schema.as_deref(), Some("native"));
    }

    #[test]
    fn openai_reasoning_models_support_effort() {
        reset();
        let caps = lookup("openai", "o3");
        assert_eq!(caps.thinking_modes, vec!["effort"]);
    }

    #[test]
    fn vision_capability_gates_known_multimodal_models() {
        reset();
        assert!(lookup("openai", "gpt-4o").vision_supported);
        assert!(lookup("openai", "gpt-5.4-preview").vision_supported);
        assert!(lookup("anthropic", "claude-sonnet-4-6").vision_supported);
        assert!(lookup("openrouter", "google/gemini-2.5-flash").vision_supported);
        assert!(lookup("gemini", "gemini-2.5-flash").vision_supported);
        assert!(lookup("ollama", "llava:latest").vision_supported);
        assert!(!lookup("openai", "gpt-3.5-turbo").vision_supported);
        assert!(!lookup("ollama", "qwen3.5:35b-a3b-coding-nvfp4").vision_supported);
    }

    #[test]
    fn openrouter_inherits_openai() {
        reset();
        let caps = lookup("openrouter", "gpt-5.4");
        assert!(caps.defer_loading);
        assert_eq!(caps.tool_search, vec!["hosted", "client"]);
    }

    #[test]
    fn groq_inherits_openai_family_only() {
        reset();
        let caps = lookup("groq", "gpt-5.5-preview");
        assert!(caps.defer_loading);
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
    fn qwen36_ollama_preserves_thinking() {
        reset();
        let caps = lookup("ollama", "qwen3.6:35b-a3b-coding-nvfp4");
        assert!(caps.native_tools);
        assert_eq!(caps.json_schema.as_deref(), Some("format_kw"));
        assert!(!caps.thinking_modes.is_empty());
        assert!(
            caps.preserve_thinking,
            "Qwen3.6 should enable preserve_thinking by default for long-horizon loops"
        );
        assert_eq!(caps.server_parser, "ollama_qwen3coder");
        assert!(!caps.honors_chat_template_kwargs);
        assert_eq!(
            caps.recommended_endpoint.as_deref(),
            Some("/api/generate-raw")
        );
        assert!(!caps.text_tool_wire_format_supported);
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
            ("together", "Qwen/Qwen3.6-35B-A3B"),
            ("huggingface", "Qwen/Qwen3.6-35B-A3B"),
            ("fireworks", "accounts/fireworks/models/qwen3p6-plus"),
            ("dashscope", "qwen3.6-plus"),
            ("llamacpp", "unsloth/Qwen3.6-35B-A3B-GGUF"),
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
    }

    #[test]
    fn llamacpp_qwen_keeps_text_tool_wire_format() {
        reset();
        let caps = lookup("llamacpp", "unsloth/Qwen3.5-Coder-GGUF");
        assert_eq!(caps.server_parser, "none");
        assert!(caps.honors_chat_template_kwargs);
        assert!(caps.text_tool_wire_format_supported);
        assert_eq!(
            caps.recommended_endpoint.as_deref(),
            Some("/v1/chat/completions")
        );
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
    fn user_override_adds_new_provider() {
        reset();
        let toml_src = r#"
[[provider.my-proxy]]
model_match = "*"
native_tools = true
tool_search = ["hosted"]
"#;
        set_user_overrides_toml(toml_src).unwrap();
        let caps = lookup("my-proxy", "anything");
        assert!(caps.native_tools);
        assert_eq!(caps.tool_search, vec!["hosted"]);
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
        let manifest = r#"
[package]
name = "demo"

[[capabilities.provider.my-proxy]]
model_match = "*"
native_tools = true
tool_search = ["hosted"]
"#;
        set_user_overrides_from_manifest_toml(manifest).unwrap();
        let caps = lookup("my-proxy", "foo");
        assert!(caps.native_tools);
        assert_eq!(caps.tool_search, vec!["hosted"]);
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
