//! Tool-format decision: validate and auto-correct a requested `tool_format`
//! against the route's declared tool-call dialect validity.
//!
//! The capability registry declares, per route, which channel actually returns
//! parseable tool calls. This module is the enforcement seam: it classifies a
//! requested format into a [`ToolFormatWire`] channel, decides whether that
//! channel is forbidden for the route, and either passes the request through or
//! steers it to a working channel with an explanatory [`ToolFormatDecision`].

use super::lookup::lookup;
use super::model::Capabilities;

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

#[cfg(test)]
mod tests {
    use super::super::lookup::{clear_user_overrides, lookup_with_user_overrides};
    use super::super::model::CapabilitiesFile;
    use super::super::BUILTIN_PROVIDERS_TOML;
    use super::*;

    fn reset() {
        clear_user_overrides();
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

        // llama.cpp (:8001) — the fresh #5162 family sweep used two replicates
        // across six coding-agent fixtures for each Qwen3.6 quant and found
        // native unreliable (2/12, 0/12, and 2/12 native passes for Q8, Q5,
        // and Q4 respectively, versus 8/12 text passes for each). A native
        // request therefore steers to the receipted JSON text contract.
        let llamacpp = validate_tool_format("llamacpp", "qwen3.6-35b-a3b-ud-q4-k-xl", "native");
        assert_eq!(
            llamacpp.effective, "json",
            "llama.cpp Qwen3.6 native route must steer to the measured text contract"
        );
        assert!(llamacpp.correction.is_some());

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
}
