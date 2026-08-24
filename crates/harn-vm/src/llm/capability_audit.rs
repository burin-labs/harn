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
//!   * **self-hosted tool-format justification** — a self-hosted row that
//!     sets `native_tools` or `preferred_tool_format` is a decision about how
//!     every request on that runtime is shaped. It must carry
//!     `tool_format_justification`, in one of three evidence tiers: a
//!     `measured` receipt of THIS runtime, an `assumed` pin that admits no
//!     probe has been run, or a structural `mirrors` link to another row.
//!     Prose that cites a sibling is not a receipt. A `mirrors` target must
//!     exist, and the dependant's resolved native/text decision must match
//!     the cited row so a later flip surfaces the dependants (#6829).
//!
//!     The tiers are load-bearing: the gate cannot tell a real probe from
//!     invented prose, so the one thing it can enforce is that a row with no
//!     probe says `assumed` instead of claiming `measured`. That keeps the
//!     table's unmeasured debt greppable rather than laundering it into
//!     receipts.
//!
//! The first three checks are driven entirely by capability-row fields and the
//! fourth by a tiny evidence-gated family list, so adding/closing a footgun
//! route is a data edit (set the flag / forget the pin / pin native for an
//! unreliable family) rather than a code change — and the mistake trips this
//! gate. The fifth is the same shape: a missing or stale justification is a
//! data edit, not a new code path.
//!
//! The audit is wired into `harn provider catalog generate --check` (see
//! `harn-cli`), which runs under `make check-provider-catalog` /
//! `make check-provider-matrix`, so the matrix cannot drift into a footgun
//! state without failing CI.

use crate::llm::capabilities::{CapabilitiesFile, ProviderRule, ToolFormatJustification};
use crate::llm_config::provider_is_self_hosted;

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
///
/// The list is currently EMPTY, and that is a finding rather than an oversight.
/// It previously carried a `glm-5` row asserting that GLM-5.x leaks
/// `<tool_call><arg_key>...` markup into assistant content on every host. A
/// 2026-08-15 sweep re-probed that claim directly and it did not survive: across
/// six independent hosts (zai-direct, OpenRouter, Fireworks, NVIDIA, Together,
/// DeepInfra) and both `tool_choice` values, in sync and streaming mode, GLM
/// returned exactly one well-formed `message.tool_calls` entry and zero markup
/// leaks in 19/19 probes. The per-host rows that fed the generalization each
/// described a *different* failure (markup leak / no dispatchable calls /
/// function name containing the whole payload), all recorded on 2026-06-20
/// immediately after a Harn parser fix — i.e. several distinct, since-resolved
/// parser bugs generalized into one weight-intrinsic verdict.
///
/// The one defect that reproduced is host-specific and now lives in its own row:
/// DeepInfra's `zai-org/GLM-5.2` deployment emits 38 duplicate tool calls under
/// `tool_choice = "required"` (deterministic, 4/4 runs), while GLM-5.1, GLM-4.7
/// and every DeepSeek route on that same host return a single call.
///
/// Keep the mechanism: it is the right shape for a genuine weight-intrinsic
/// family. Add a row only with fresh cross-host evidence, and re-verify an
/// existing row before relying on it.
const NATIVE_UNRELIABLE_TOOL_FAMILIES: &[(&str, &str)] = &[];

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
    audit_capabilities_with_families(file, NATIVE_UNRELIABLE_TOOL_FAMILIES)
}

/// Same audit, with the native-unreliable family list injected.
///
/// [`NATIVE_UNRELIABLE_TOOL_FAMILIES`] is legitimately empty right now, so tests
/// supply their own list to keep the family-consistency gate covered. Without
/// this seam, emptying the shipped list would silently retire the gate's tests
/// along with its data.
fn audit_capabilities_with_families(
    file: &CapabilitiesFile,
    native_unreliable_families: &[(&str, &str)],
) -> CapabilityAuditReport {
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
                            model_match: rule.match_label(),
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
                        model_match: rule.match_label(),
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
                    model_match: rule.match_label(),
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
                    model_match: rule.match_label(),
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
                for (family, evidence) in native_unreliable_families {
                    if rule
                        .match_patterns()
                        .any(|pattern| pattern.to_ascii_lowercase().contains(family))
                    {
                        report.footguns.push(CapabilityFootgun {
                            provider: provider.clone(),
                            model_match: rule.match_label(),
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

            // Footgun 5: a self-hosted native/text decision without a receipt
            // of THIS runtime, or a stale structural sibling link (#6829).
            if provider_is_self_hosted(provider) && decides_tool_format(rule) {
                match &rule.tool_format_justification {
                    None => {
                        report.footguns.push(CapabilityFootgun {
                            provider: provider.clone(),
                            model_match: rule.match_label(),
                            message: "is a self-hosted native/text decision without \
                                tool_format_justification. Record a measurement of THIS \
                                runtime (`tool_format_justification = { measured = \"...\" }`) \
                                or a structural sibling link (`{ mirrors = { provider, \
                                model_match } }`). A comment that cites another row is not a \
                                receipt."
                                .to_string(),
                        });
                    }
                    Some(ToolFormatJustification::Measured(receipt))
                        if receipt.trim().is_empty() =>
                    {
                        report.footguns.push(CapabilityFootgun {
                            provider: provider.clone(),
                            model_match: rule.match_label(),
                            message: "declares tool_format_justification.measured but the \
                                receipt is empty. Write the measurement of THIS runtime, or \
                                use mirrors to name the cited row."
                                .to_string(),
                        });
                    }
                    Some(ToolFormatJustification::Assumed(rationale))
                        if rationale.trim().is_empty() =>
                    {
                        report.footguns.push(CapabilityFootgun {
                            provider: provider.clone(),
                            model_match: rule.match_label(),
                            message: "declares tool_format_justification.assumed but the \
                                rationale is empty. State what the pin rests on and how to \
                                roll it back, so the next reader knows this row was never \
                                probed."
                                .to_string(),
                        });
                    }
                    Some(ToolFormatJustification::Mirrors(target)) => {
                        match find_rule(file, &target.provider, &target.model_match) {
                            None => {
                                report.footguns.push(CapabilityFootgun {
                                    provider: provider.clone(),
                                    model_match: rule.match_label(),
                                    message: format!(
                                        "mirrors provider.{} model_match=\"{}\", but that row \
                                         does not exist. Point mirrors at a real row, or \
                                         replace it with a measurement of THIS runtime.",
                                        target.provider, target.model_match
                                    ),
                                });
                            }
                            Some(cited) => {
                                let ours = resolved_tool_decision(rule);
                                let theirs = resolved_tool_decision(cited);
                                if ours != theirs {
                                    report.footguns.push(CapabilityFootgun {
                                        provider: provider.clone(),
                                        model_match: rule.match_label(),
                                        message: format!(
                                            "mirrors provider.{} model_match=\"{}\" \
                                             (native_tools={}, preferred_tool_format={}), but \
                                             this row resolved to native_tools={}, \
                                             preferred_tool_format={}. The cited row changed \
                                             or this runtime diverged; re-measure THIS \
                                             runtime or update the dependant so the link \
                                             stays honest.",
                                            target.provider,
                                            target.model_match,
                                            theirs.0,
                                            theirs.1,
                                            ours.0,
                                            ours.1
                                        ),
                                    });
                                }
                            }
                        }
                    }
                    Some(ToolFormatJustification::Measured(_))
                    | Some(ToolFormatJustification::Assumed(_)) => {}
                }
            }
        }
    }
    report
}

fn decides_tool_format(rule: &ProviderRule) -> bool {
    rule.native_tools.is_some() || rule.preferred_tool_format.is_some()
}

fn resolved_tool_decision(rule: &ProviderRule) -> (bool, String) {
    let native = rule.native_tools.unwrap_or(false);
    let format = rule.preferred_tool_format.clone().unwrap_or_else(|| {
        if native {
            "native".to_string()
        } else {
            "json".to_string()
        }
    });
    (native, format)
}

fn find_rule<'a>(
    file: &'a CapabilitiesFile,
    provider: &str,
    model_match: &str,
) -> Option<&'a ProviderRule> {
    file.provider.get(provider)?.iter().find(|rule| {
        rule.match_label() == model_match
            || rule.match_patterns().any(|pattern| pattern == model_match)
    })
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

    /// A synthetic native-unreliable family, so the family-consistency gate is
    /// exercised independently of whatever the shipped list happens to contain.
    const TEST_FAMILIES: &[(&str, &str)] =
        &[("flaky-fam", "Synthetic family used to exercise the gate.")];

    fn audit_toml_with_families(src: &str) -> CapabilityAuditReport {
        audit_capabilities_with_families(
            &parse_capabilities_toml(src).expect("parses"),
            TEST_FAMILIES,
        )
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
    fn shipped_self_hosted_qwen36_rows_are_independently_justified_and_not_unanimous() {
        let file = crate::llm::capabilities::builtin_file();
        let mut native_votes = Vec::new();
        let mut measured = Vec::new();
        for provider in ["ollama", "llamacpp", "local", "mlx"] {
            let rule = file
                .provider
                .get(provider)
                .into_iter()
                .flatten()
                .find(|rule| {
                    rule.match_patterns()
                        .any(|pattern| pattern.contains("qwen3.6"))
                })
                .unwrap_or_else(|| panic!("{provider} is missing a qwen3.6 capability row"));
            match &rule.tool_format_justification {
                Some(ToolFormatJustification::Measured(receipt)) => {
                    assert!(
                        !receipt.trim().is_empty(),
                        "{provider} qwen3.6 measured receipt is empty"
                    );
                    measured.push(provider);
                }
                Some(ToolFormatJustification::Assumed(rationale)) => {
                    assert!(
                        !rationale.trim().is_empty(),
                        "{provider} qwen3.6 assumed rationale is empty"
                    );
                }
                Some(ToolFormatJustification::Mirrors(_)) => {
                    panic!("{provider} qwen3.6 must carry its own receipt, not a sibling link");
                }
                None => panic!("{provider} qwen3.6 is missing tool_format_justification"),
            }
            native_votes.push((
                provider,
                rule.native_tools
                    .unwrap_or_else(|| panic!("{provider} qwen3.6 must set native_tools")),
            ));
        }
        assert!(
            native_votes.iter().any(|(_, native)| *native)
                && native_votes.iter().any(|(_, native)| !*native),
            "the four self-hosted qwen3.6 rows must not all agree; runtimes differ: {native_votes:?}"
        );
        // The divergence is only meaningful if at least one side rests on a
        // real probe. Four mutually disagreeing guesses would satisfy every
        // assertion above while leaving the table exactly as unfalsifiable as
        // #6829 found it.
        assert!(
            !measured.is_empty(),
            "no self-hosted qwen3.6 row carries a measured receipt; the split is \
             then four assumptions, not four findings"
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
        // A route that pins the native channel for a listed family (the outlier
        // shape): native_tools = true keeps Footgun 3 quiet, so the ONLY footgun
        // is the family-consistency gate.
        let report = audit_toml_with_families(
            r#"
[[provider.someprov]]
model_match = "*flaky-fam*"
native_tools = true
preferred_tool_format = "native"
"#,
        );
        assert_eq!(report.footguns.len(), 1, "{}", report.render());
        assert!(report.footguns[0]
            .message
            .contains("native-unreliable `flaky-fam` family"));
    }

    #[test]
    fn native_unreliable_family_on_text_channel_is_clean() {
        // The family verdict satisfied: text channel + native_unreliable.
        let report = audit_toml_with_families(
            r#"
[[provider.someprov]]
model_match = "*flaky-fam*"
native_tools = true
preferred_tool_format = "text"
tool_mode_parity = "native_unreliable"
"#,
        );
        assert!(report.is_clean(), "{}", report.render());
    }

    #[test]
    fn glm_native_pin_is_no_longer_a_family_footgun() {
        // Regression guard for the 2026-08-15 re-probe: GLM's native channel
        // returned clean `message.tool_calls` on all six hosts probed, so a GLM
        // route pinning native must audit clean against the SHIPPED family list.
        // If someone re-adds a `glm-5` row without fresh cross-host evidence,
        // this fails and points them back at the probe record.
        let report = audit_toml(
            r#"
[[provider.zai]]
model_match = "glm-5*"
native_tools = true
preferred_tool_format = "native"
"#,
        );
        assert!(
            report.is_clean(),
            "GLM native pin should not trip the family gate: {}",
            report.render()
        );
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

    #[test]
    fn flags_self_hosted_tool_format_decision_without_justification() {
        let report = audit_toml(
            r#"
[[provider.mlx]]
model_match = "*qwen3.6*"
native_tools = true
preferred_tool_format = "native"
"#,
        );
        assert_eq!(report.footguns.len(), 1, "{}", report.render());
        assert!(
            report.footguns[0]
                .message
                .contains("tool_format_justification"),
            "{}",
            report.render()
        );
    }

    #[test]
    fn measured_self_hosted_tool_format_decision_is_clean() {
        let report = audit_toml(
            r#"
[[provider.llamacpp]]
model_match = "*qwen3.6*"
native_tools = true
preferred_tool_format = "native"
tool_format_justification = { measured = "2026-08-19 CUDA receipt" }
"#,
        );
        assert!(report.is_clean(), "{}", report.render());
    }

    #[test]
    fn flags_empty_measured_receipt() {
        let report = audit_toml(
            r#"
[[provider.local]]
model_match = "*qwen3.6*"
native_tools = true
tool_format_justification = { measured = "   " }
"#,
        );
        assert_eq!(report.footguns.len(), 1, "{}", report.render());
        assert!(
            report.footguns[0].message.contains("receipt is empty"),
            "{}",
            report.render()
        );
    }

    #[test]
    fn assumed_self_hosted_tool_format_decision_is_clean() {
        let report = audit_toml(
            r#"
[[provider.mlx]]
model_match = "*qwen3*"
native_tools = true
preferred_tool_format = "native"
tool_format_justification = { assumed = "OpenAI-compat tools wire; no probe on this runtime. Roll back to json if tool_calls come back empty." }
"#,
        );
        assert!(report.is_clean(), "{}", report.render());
    }

    #[test]
    fn flags_empty_assumed_rationale() {
        let report = audit_toml(
            r#"
[[provider.local]]
model_match = "*qwen3*"
native_tools = true
tool_format_justification = { assumed = "  " }
"#,
        );
        assert_eq!(report.footguns.len(), 1, "{}", report.render());
        assert!(
            report.footguns[0].message.contains("rationale is empty"),
            "{}",
            report.render()
        );
    }

    #[test]
    fn flags_mirror_whose_cited_row_diverged() {
        // The original #6829 shape: mlx cites llama.cpp in prose (now a
        // structural link) after llama.cpp flipped to text and mlx stayed native.
        let report = audit_toml(
            r#"
[[provider.llamacpp]]
model_match = "*qwen3.6*"
native_tools = false
preferred_tool_format = "json"
tool_format_justification = { measured = "forced-format sweep, text channel" }

[[provider.mlx]]
model_match = "*qwen3.6*"
native_tools = true
preferred_tool_format = "native"
tool_format_justification = { mirrors = { provider = "llamacpp", model_match = "*qwen3.6*" } }
"#,
        );
        assert_eq!(report.footguns.len(), 1, "{}", report.render());
        assert_eq!(report.footguns[0].provider, "mlx");
        assert!(
            report.footguns[0].message.contains("cited row changed"),
            "{}",
            report.render()
        );
    }

    #[test]
    fn matching_mirror_is_clean() {
        let report = audit_toml(
            r#"
[[provider.llamacpp]]
model_match = "*qwen3.6*"
native_tools = true
preferred_tool_format = "native"
tool_format_justification = { measured = "CUDA receipt" }

[[provider.mlx]]
model_match = "*qwen3.6*"
native_tools = true
preferred_tool_format = "native"
tool_format_justification = { mirrors = { provider = "llamacpp", model_match = "*qwen3.6*" } }
"#,
        );
        assert!(report.is_clean(), "{}", report.render());
    }

    #[test]
    fn flags_mirror_to_missing_row() {
        let report = audit_toml(
            r#"
[[provider.mlx]]
model_match = "*qwen3.6*"
native_tools = true
preferred_tool_format = "native"
tool_format_justification = { mirrors = { provider = "llamacpp", model_match = "*qwen3.6*" } }
"#,
        );
        assert_eq!(report.footguns.len(), 1, "{}", report.render());
        assert!(
            report.footguns[0].message.contains("does not exist"),
            "{}",
            report.render()
        );
    }

    #[test]
    fn hosted_tool_format_decision_does_not_need_justification() {
        let report = audit_toml(
            r#"
[[provider.openrouter]]
model_match = "qwen/qwen3.6*"
native_tools = true
preferred_tool_format = "native"
"#,
        );
        assert!(report.is_clean(), "{}", report.render());
    }

    #[test]
    fn self_hosted_row_without_a_tool_format_decision_is_clean() {
        let report = audit_toml(
            r#"
[[provider.ollama]]
model_match = "llava*"
vision_supported = true
"#,
        );
        assert!(report.is_clean(), "{}", report.render());
    }
}
