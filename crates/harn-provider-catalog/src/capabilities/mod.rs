//! Capability policy over explicit rules and an already normalized route.
//!
//! Runtime configuration, credentials, mock activity and live measurements
//! belong to the calling adapter. Source generators use this same policy
//! without borrowing the runtime's ambient configuration.

mod decision;
mod effort;
mod generation;
mod model;
mod option;
mod pattern;
mod projection;
mod route;
mod rule;

#[cfg(test)]
mod resolution_tests;
#[cfg(test)]
mod rule_tests;

pub use decision::{DecisionLimits, DecisionProtocol, DecisionQuestionKind};
pub use effort::{rule_reasoning_effort_supported, rule_thinking_modes, ReasoningEffort};
pub use generation::{claude_generation, gpt_generation, is_claude_model_id};
pub use model::*;
pub use option::PortableOption;
pub use pattern::ModelPatterns;
pub use projection::rule_preferred_tool_format;
pub use rule::{
    rule_structured_output, rule_structured_output_mode, rule_thinking_block_style,
    rule_tool_mode_parity, rule_vision, MatchedCapabilityRule, ProviderRule,
};

/// Identity and adapter features for one capability lookup. `model` is the
/// provider's capability identity, including any configured wire-model alias.
pub struct CapabilityRoute<'a> {
    pub provider: &'a str,
    pub model: &'a str,
    pub responses_api: bool,
}

/// One immutable base and its optional explicit overlay.
pub struct CapabilityRules<'a> {
    builtin: &'a CapabilitiesFile,
    overlay: Option<&'a CapabilitiesFile>,
}

/// Resolved facts retain authored support and provenance independently from
/// conservative defaults. An unmatched route must never look like an authored
/// rule that deliberately supports nothing.
pub struct CapabilityResolution {
    pub matched_rule: Option<MatchedCapabilityRule>,
    pub defaults: ProviderDefaults,
    pub provenance: Vec<String>,
    route_kind: CapabilityRouteKind,
}

#[derive(Clone, Copy)]
enum CapabilityRouteKind {
    OpenAi,
    Mock,
    Other { responses_api: bool },
}

impl CapabilityResolution {
    pub fn matched(&self) -> bool {
        self.matched_rule.is_some()
    }

    pub fn has_defaults(&self) -> bool {
        self.defaults.has_any_field()
    }

    /// Materialize effective capabilities only when requested. Admission and
    /// provenance callers can inspect authored rules without allocating the
    /// full capability projection.
    pub fn capabilities(&self) -> Capabilities {
        let mut capabilities = if let Some(matched) = self.matched_rule.as_ref() {
            projection::rule_to_caps(&matched.rule, &self.defaults)
        } else if matches!(self.route_kind, CapabilityRouteKind::Mock) || !self.has_defaults() {
            Capabilities::default()
        } else {
            projection::defaults_to_caps(&self.defaults)
        };
        if matches!(self.route_kind, CapabilityRouteKind::Mock)
            && self
                .matched_rule
                .as_ref()
                .is_some_and(|matched| matched.provider == "anthropic")
        {
            capabilities.native_tool_wire_format = "openai".to_string();
        }
        if let CapabilityRouteKind::Other { responses_api } = self.route_kind {
            if !responses_api {
                capabilities.responses_api = false;
                capabilities.chat_completions_unsupported = false;
            }
            capabilities.hosted_tools.clear();
            capabilities.remote_mcp = false;
            capabilities.conversation_state = false;
            capabilities.compaction = false;
            capabilities.background_mode = false;
            capabilities.tool_approval_policy = None;
        }
        capabilities
    }
}

impl<'a> CapabilityRules<'a> {
    pub fn new(builtin: &'a CapabilitiesFile, overlay: Option<&'a CapabilitiesFile>) -> Self {
        Self { builtin, overlay }
    }

    pub fn resolve(&self, route: CapabilityRoute<'_>) -> CapabilityResolution {
        let (mut resolution, defaults, _) =
            route::resolve_route(self.overlay, self.builtin, route.provider, route.model);
        let provenance = std::mem::take(&mut resolution.matched_rules);
        let route_kind = match route.provider {
            "openai" => CapabilityRouteKind::OpenAi,
            "mock" => CapabilityRouteKind::Mock,
            _ => CapabilityRouteKind::Other {
                responses_api: route.responses_api,
            },
        };
        CapabilityResolution {
            matched_rule: resolution.into_matched(),
            defaults,
            provenance,
            route_kind,
        }
    }
}

pub fn resolve_system_message_placement(caps: &Capabilities) -> SystemMessagePlacement {
    caps.system_message_placement
        .unwrap_or(match caps.message_wire_format {
            WireDialect::OpenAiCompat | WireDialect::Ollama => SystemMessagePlacement::Inline,
            WireDialect::Anthropic | WireDialect::Gemini => SystemMessagePlacement::Fold,
        })
}
