//! Compile-time footgun gate for the capability matrix.
//!
//! Harn is *opinionated* about provider/model/config combinations: a few
//! combos are known footguns that silently break tool calling at runtime, and
//! the only durable place to forbid them is the declarative matrix itself —
//! before a harness author can ship a misconfigured route.
//!
//! This audit walks the parsed [`CapabilitiesFile`] and flags
//! provider+model+config combinations that the matrix declares as invariants,
//! NOT hard-coded model-name patterns. It generalizes the
//! `reasoning_required_for_tools` precedent (a tool-using model that calls
//! tools inside its reasoning channel) into a small set of data-driven rules:
//!
//!   * **reasoning-off-for-tools contradiction** — a row that declares
//!     `reasoning_required_for_tools = true` must not also pin a tool task
//!     (`agent` / `code` / `verify`) to reasoning `"off"` via
//!     `auto_reasoning_overrides`. That is the self-inflicted
//!     billed-noncommittal failure #3305 fixed at its root; declaring both is a
//!     direct contradiction.
//!
//!   * **lottery-route without a clean pin** — an OpenRouter row that declares
//!     `reasoning_required_for_tools = true` is a Harmony-style tool route on a
//!     sub-provider-lottery provider. Some OpenRouter upstreams mis-serialize
//!     the Harmony tool call even with reasoning ON, so such a row MUST pin a
//!     closed allowlist of known-clean upstreams via `openrouter_provider_order`
//!     (materialized to `provider.order` + `allow_fallbacks:false`). Without a
//!     pin the route can silently land on a sketchy upstream.
//!
//!   * **native-tool declaration contradictions** — a row that prefers the
//!     native tool-call wire format, or declares native tool-choice modes, must
//!     also explicitly enable `native_tools`. Otherwise downstream request
//!     builders see mutually incompatible capability facts and harness authors
//!     get provider-specific surprises instead of one normalized toolchain.
//!
//!   * **native-unreliable family consistency** — for a model family whose
//!     provider-native tool channel is unreliable as a *weight-intrinsic*
//!     property (it leaks tool markup into content / bills empty native
//!     completions on every host that serves those weights), EVERY route must
//!     steer to a text channel. A single outlier host pinning
//!     `preferred_tool_format = "native"` while its siblings pin text is exactly
//!     how a value model silently thrashes on one provider. This is the only
//!     check keyed on a model-family substring (see
//!     [`NATIVE_UNRELIABLE_TOOL_FAMILIES`]) rather than pure capability fields,
//!     and the bar to add a family is deliberately high: weight-intrinsic
//!     unreliability reproduced across independent hosts, never one rehoster's
//!     flakiness (which belongs in that host's own row).
//!
//! The first three checks are driven entirely by capability-row fields and the
//! fourth by a tiny evidence-gated family list, so adding/closing a footgun
//! route is a data edit (set the flag / forget the pin / pin native for an
//! unreliable family) rather than a code change — and the mistake trips this
//! gate.
//!
//! The audit is wired into `harn provider catalog build-capabilities --check` (see
//! `harn-cli`), which runs under `make check-provider-capabilities` /
//! `make check-provider-matrix`, so the matrix cannot drift into a footgun
//! state without failing CI.

use crate::llm::capabilities::CapabilitiesFile;

/// Tool-bearing reasoning tasks. These are the tasks whose auto reasoning level
/// must never resolve to `"off"` on a route that calls tools in its reasoning
/// channel. Mirrors the guarded set in
/// [`crate::llm::reasoning_policy`].
const TOOL_TASKS: [&str; 3] = ["agent", "code", "verify"];

/// Model families whose **provider-native** tool channel is unreliable as a
/// *weight-intrinsic* property — the model itself emits tool-call markup as
/// assistant content (or bills empty native completions) on every host that
/// serves those weights, regardless of provider. For such a family, EVERY route
/// must steer to a text channel (`preferred_tool_format` = `text`/`json`) and
/// declare `tool_mode_parity = "native_unreliable"`; a route that pins
/// `preferred_tool_format = "native"` is a footgun (it re-opens the leak this
/// host can't fix server-side). Each entry is `(model_match-substring, evidence)`.
///
/// The bar for entry is HIGH on purpose: a quirk earns a row here only when it is
/// demonstrated to be intrinsic to the weights (reproduced across independent
/// hosts), NOT merely observed on one rehoster. Host-specific native flakiness
/// belongs in that host's own row, not this cross-host invariant — e.g. a
/// first-party authoritative endpoint may serve native cleanly while third-party
/// rehosters do not, and that difference must be measured per host, not assumed.
const NATIVE_UNRELIABLE_TOOL_FAMILIES: &[(&str, &str)] = &[(
    "glm-5",
    "GLM-5.x's native channel emits `<tool_call><arg_key>...` markup as assistant \
     content instead of OpenAI message.tool_calls — reproduced across every GLM-5 host \
     probed (zai/Baseten live, Together + OpenRouter agent-loop smoke, DeepInfra, Fireworks \
     glm-5p*). Pin a text channel + tool_mode_parity = \"native_unreliable\".",
)];

/// A single footgun finding: a capability row that violates an opinionated
/// provider/model/config invariant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityFootgun {
    /// Provider id whose rule list contains the offending row.
    pub provider: String,
    /// The row's `model_match` pattern.
    pub model_match: String,
    /// Human-readable explanation + the declarative fix.
    pub message: String,
}

/// Result of auditing a [`CapabilitiesFile`] for footgun combinations.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CapabilityAuditReport {
    pub footguns: Vec<CapabilityFootgun>,
}

impl CapabilityAuditReport {
    pub fn is_clean(&self) -> bool {
        self.footguns.is_empty()
    }

    /// One line per finding, suitable for CLI/CI output.
    pub fn render(&self) -> String {
        self.footguns
            .iter()
            .map(|footgun| {
                format!(
                    "provider.{} model_match=\"{}\": {}",
                    footgun.provider, footgun.model_match, footgun.message
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// Audit the in-memory capability matrix for footgun provider/model/config
/// combinations. Pure over the parsed file — no I/O, no model-name patterns.
pub fn audit_capabilities(file: &CapabilitiesFile) -> CapabilityAuditReport {
    let mut report = CapabilityAuditReport::default();
    for (provider, rules) in &file.provider {
        for rule in rules {
            let reasoning_required_for_tools = rule.reasoning_required_for_tools.unwrap_or(false);

            // Footgun 1: reasoning-off-for-tools contradiction. A route that
            // calls tools inside its reasoning channel must not also force a
            // tool task to reasoning-off.
            if reasoning_required_for_tools {
                if let Some(overrides) = &rule.auto_reasoning_overrides {
                    let offending: Vec<&str> = TOOL_TASKS
                        .iter()
                        .copied()
                        .filter(|task| {
                            overrides
                                .get(*task)
                                .map(|level| level.eq_ignore_ascii_case("off"))
                                .unwrap_or(false)
                        })
                        .collect();
                    if !offending.is_empty() {
                        report.footguns.push(CapabilityFootgun {
                            provider: provider.clone(),
                            model_match: rule.model_match.clone(),
                            message: format!(
                                "declares reasoning_required_for_tools = true but also pins \
                                 auto_reasoning_overrides {{ {} = \"off\" }}; this route calls \
                                 tools inside its reasoning channel, so forcing reasoning off \
                                 for a tool task is the billed-noncommittal failure (0 \
                                 tool_calls). Remove the \"off\" override(s) for tool tasks.",
                                offending.join("/")
                            ),
                        });
                    }
                }
            }

            // Footgun 2: lottery-route without a clean sub-provider pin. An
            // OpenRouter Harmony-style tool route must allowlist known-clean
            // upstreams or it can silently land on a mis-serializing one.
            if provider == "openrouter" && reasoning_required_for_tools {
                let pinned = rule
                    .openrouter_provider_order
                    .as_ref()
                    .map(|order| !order.is_empty())
                    .unwrap_or(false);
                if !pinned {
                    report.footguns.push(CapabilityFootgun {
                        provider: provider.clone(),
                        model_match: rule.model_match.clone(),
                        message: "is an OpenRouter route with \
                            reasoning_required_for_tools = true (a Harmony-style tool route on \
                            the OpenRouter sub-provider lottery) but declares no \
                            openrouter_provider_order pin. Some OpenRouter upstreams \
                            mis-serialize the tool call even with reasoning ON. Pin a closed \
                            allowlist of known-clean upstreams, e.g. \
                            openrouter_provider_order = [\"Cerebras\", \"Groq\"]."
                            .to_string(),
                    });
                }
            }

            // Footgun 3: native tool declaration contradictions. These fields
            // describe native tool-call request shape and must not be set on a
            // text-tool-only row.
            if rule
                .preferred_tool_format
                .as_deref()
                .map(|format| format.eq_ignore_ascii_case("native"))
                .unwrap_or(false)
                && !rule.native_tools.unwrap_or(false)
            {
                report.footguns.push(CapabilityFootgun {
                    provider: provider.clone(),
                    model_match: rule.model_match.clone(),
                    message: "declares preferred_tool_format = \"native\" without \
                        native_tools = true. Native tool format is only coherent \
                        for rows that enable native tool calls; either set \
                        native_tools = true or choose a text-channel tool format."
                        .to_string(),
                });
            }

            if rule
                .allowed_tool_choice_modes
                .as_ref()
                .map(|modes| !modes.is_empty())
                .unwrap_or(false)
                && !rule.native_tools.unwrap_or(false)
            {
                report.footguns.push(CapabilityFootgun {
                    provider: provider.clone(),
                    model_match: rule.model_match.clone(),
                    message: "declares allowed_tool_choice_modes while native_tools is \
                        not true. Tool-choice modes are native request-shape \
                        capabilities; enable native_tools or remove the native \
                        tool-choice declaration."
                        .to_string(),
                });
            }

            // Footgun 4: a route pins the provider-native tool channel for a model
            // family whose native channel is unreliable as a weight-intrinsic
            // property (see NATIVE_UNRELIABLE_TOOL_FAMILIES). One outlier host
            // pinning `native` while every sibling host pins text is exactly how a
            // value model silently thrashes (the model leaks tool markup into
            // content / bills empty native completions, and this host can't fix it
            // server-side). The family verdict must hold on every route.
            let pins_native = rule
                .preferred_tool_format
                .as_deref()
                .map(|format| format.eq_ignore_ascii_case("native"))
                .unwrap_or(false);
            if pins_native {
                let model_match_lower = rule.model_match.to_ascii_lowercase();
                for (family, evidence) in NATIVE_UNRELIABLE_TOOL_FAMILIES {
                    if model_match_lower.contains(family) {
                        report.footguns.push(CapabilityFootgun {
                            provider: provider.clone(),
                            model_match: rule.model_match.clone(),
                            message: format!(
                                "pins preferred_tool_format = \"native\" for the \
                                 native-unreliable `{family}` family. {evidence} Steer this \
                                 route to a text channel (preferred_tool_format = \"text\" or \
                                 \"json\") and set tool_mode_parity = \"native_unreliable\" so \
                                 the family verdict is consistent across hosts."
                            ),
                        });
                    }
                }
            }
        }
    }
    report
}

/// Audit the built-in (shipped) capability matrix. Convenience entry point for
/// the CLI gate.
pub fn audit_builtin() -> CapabilityAuditReport {
    audit_capabilities(crate::llm::capabilities::builtin_file())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::capabilities::parse_capabilities_toml;

    fn audit_toml(src: &str) -> CapabilityAuditReport {
        audit_capabilities(&parse_capabilities_toml(src).expect("parses"))
    }

    #[test]
    fn shipped_matrix_has_no_footguns() {
        let report = audit_builtin();
        assert!(
            report.is_clean(),
            "shipped capability matrix has footguns:\n{}",
            report.render()
        );
    }

    #[test]
    fn flags_reasoning_off_for_tools_contradiction() {
        let report = audit_toml(
            r#"
[[provider.someprov]]
model_match = "harmony-*"
reasoning_required_for_tools = true
auto_reasoning_overrides = { agent = "off" }
"#,
        );
        assert_eq!(report.footguns.len(), 1, "{}", report.render());
        assert_eq!(report.footguns[0].provider, "someprov");
        assert!(report.footguns[0].message.contains("billed-noncommittal"));
    }

    #[test]
    fn flags_lottery_route_without_pin() {
        let report = audit_toml(
            r#"
[[provider.openrouter]]
model_match = "vendor/harmony-*"
reasoning_required_for_tools = true
reasoning_effort_levels = ["low", "medium", "high"]
"#,
        );
        assert_eq!(report.footguns.len(), 1, "{}", report.render());
        assert!(report.footguns[0]
            .message
            .contains("openrouter_provider_order"));
    }

    #[test]
    fn pinned_lottery_route_is_clean() {
        let report = audit_toml(
            r#"
[[provider.openrouter]]
model_match = "vendor/harmony-*"
reasoning_required_for_tools = true
openrouter_provider_order = ["Cerebras", "Groq"]
"#,
        );
        assert!(report.is_clean(), "{}", report.render());
    }

    #[test]
    fn empty_pin_is_treated_as_no_pin() {
        let report = audit_toml(
            r#"
[[provider.openrouter]]
model_match = "vendor/harmony-*"
reasoning_required_for_tools = true
openrouter_provider_order = []
"#,
        );
        assert_eq!(report.footguns.len(), 1, "{}", report.render());
    }

    #[test]
    fn non_openrouter_required_route_does_not_need_a_pin() {
        // Groq/Cerebras/Together gpt-oss rows require reasoning for tools but
        // are NOT on the OpenRouter lottery, so they must not be flagged for a
        // missing pin.
        let report = audit_toml(
            r#"
[[provider.groq]]
model_match = "*gpt-oss-*"
reasoning_required_for_tools = true
reasoning_effort_levels = ["low", "medium", "high"]
"#,
        );
        assert!(report.is_clean(), "{}", report.render());
    }

    #[test]
    fn qwen_style_off_override_without_required_flag_is_clean() {
        // The Qwen quirk (reasoning-OFF-for-tools, no required-for-tools flag)
        // is a legitimate config and must NOT be flagged.
        let report = audit_toml(
            r#"
[[provider.ollama]]
model_match = "qwen3.6*"
auto_reasoning_overrides = { agent = "off" }
"#,
        );
        assert!(report.is_clean(), "{}", report.render());
    }

    #[test]
    fn ordinary_models_are_clean() {
        let report = audit_toml(
            r#"
[[provider.openrouter]]
model_match = "anthropic/claude-*"
native_tools = true

[[provider.openai]]
model_match = "gpt-*"
native_tools = true
"#,
        );
        assert!(report.is_clean(), "{}", report.render());
    }

    #[test]
    fn flags_native_tool_format_without_native_tools() {
        let report = audit_toml(
            r#"
[[provider.someprov]]
model_match = "some-model"
native_tools = false
preferred_tool_format = "native"
"#,
        );
        assert_eq!(report.footguns.len(), 1, "{}", report.render());
        assert!(report.footguns[0]
            .message
            .contains("preferred_tool_format = \"native\""));
    }

    #[test]
    fn flags_native_unreliable_family_pinning_native() {
        // A GLM-5 route that pins the native channel (the nvidia outlier shape):
        // native_tools = true keeps Footgun 3 quiet, so the ONLY footgun is the
        // family-consistency gate.
        let report = audit_toml(
            r#"
[[provider.nvidia]]
model_match = "*glm-5*"
native_tools = true
preferred_tool_format = "native"
"#,
        );
        assert_eq!(report.footguns.len(), 1, "{}", report.render());
        assert!(report.footguns[0]
            .message
            .contains("native-unreliable `glm-5` family"));
    }

    #[test]
    fn native_unreliable_family_on_text_channel_is_clean() {
        // The family verdict satisfied: text channel + native_unreliable.
        let report = audit_toml(
            r#"
[[provider.nvidia]]
model_match = "*glm-5*"
native_tools = true
preferred_tool_format = "text"
tool_mode_parity = "native_unreliable"
"#,
        );
        assert!(report.is_clean(), "{}", report.render());
    }

    #[test]
    fn native_pin_for_non_family_model_is_clean() {
        // A native pin is fine for a model NOT in the native-unreliable family
        // list — the gate is scoped to families with weight-intrinsic evidence.
        let report = audit_toml(
            r#"
[[provider.someprov]]
model_match = "some-reliable-native-model-*"
native_tools = true
preferred_tool_format = "native"
"#,
        );
        assert!(report.is_clean(), "{}", report.render());
    }

    #[test]
    fn flags_tool_choice_modes_without_native_tools() {
        let report = audit_toml(
            r#"
[[provider.someprov]]
model_match = "some-model"
native_tools = false
preferred_tool_format = "text"
allowed_tool_choice_modes = ["auto", "none"]
"#,
        );
        assert_eq!(report.footguns.len(), 1, "{}", report.render());
        assert!(report.footguns[0]
            .message
            .contains("allowed_tool_choice_modes"));
    }
}
