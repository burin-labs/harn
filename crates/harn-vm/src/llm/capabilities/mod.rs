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
//!
//! The concrete data model and engine are split across submodules: `model`
//! (capability DTOs + [`WireDialect`]), `rule` ([`ProviderRule`] + the
//! resolution engine), `tool_format` (tool-call dialect validation), `audit`
//! (coverage audit + display matrix), and `lookup` (the override facade and
//! public [`lookup`] entry points). Every previously-public item is re-exported
//! here so `capabilities::X` paths resolve unchanged.

/// Generated provider/model snapshot built from catalog_sources/**/*.toml.
pub(super) const BUILTIN_PROVIDERS_TOML: &str = harn_provider_catalog::EMBEDDED_PROVIDERS_TOML;

mod admission;
mod audit;
mod diagnostics;
use harn_provider_catalog::capabilities as decision;
use harn_provider_catalog::capabilities as effort;
use harn_provider_catalog::capabilities as model;
use harn_provider_catalog::capabilities as projection;
mod lookup;
#[cfg(test)]
mod lookup_tests_parity;
#[cfg(test)]
mod lookup_tests_support;
#[cfg(test)]
mod lookup_tests_system_placement;
mod overrides;
mod rule;
mod runtime_probe;
mod tool_format;

pub(crate) use admission::admit_portable_option_for_thinking;
pub use effort::ReasoningEffort;
pub(crate) use lookup::should_use_responses_transport;
pub(crate) use overrides::current_user_overrides;
pub(crate) use overrides::swap_user_overrides;
pub(crate) use runtime_probe::cached_measurement;

pub use admission::{
    admit_portable_option, admit_portable_option_with_effort, admit_prompt_cache_ttl,
    CapabilityAdmissionError, PortableOption,
};
pub use audit::{
    audit_builtin_catalogued_chat_model_tool_capabilities,
    audit_catalogued_chat_model_tool_capabilities, matrix_rows, matrix_rows_for_base,
    ProviderCapabilityMatrixRow, ToolCapabilityAuditGap, ToolCapabilityAuditReport,
};
pub use decision::{DecisionLimits, DecisionProtocol, DecisionQuestionKind};
pub use lookup::{
    builtin_file, capability_rule_provenance, clear_user_overrides, lookup,
    lookup_for_mockable_route, lookup_with_base_file, lookup_with_source_config,
    lookup_with_user_overrides, parse_capabilities_toml, provider_limit_providers,
    provider_limits_for, set_user_overrides, set_user_overrides_from_manifest_toml,
    set_user_overrides_toml,
};
pub use model::{
    CacheBreakpointStyle, Capabilities, CapabilitiesFile, CapabilityProbeReceipt,
    CapabilityProbeStatus, ComputerUseStyle, GovernorBackoff, LiveEndpointFamily,
    PortableOptionQualification, ProviderDefaults, ProviderLimits, ReasoningHistoryWireField,
    ReasoningRoundTripPolicy, ScreenshotScaling, StructuredOutputStrategy, SystemMessagePlacement,
    ThinkingOffType, ToolFormatJustification, ToolFormatMirror, WireDialect,
};
pub(crate) use runtime_probe::ensure_runtime_probe;
#[cfg(test)]
pub(crate) use runtime_probe::forget_measurements;

/// Resolve the effective placement for an interleaved `system`/`developer`
/// message on this route. An explicit `system_message_placement` capability
/// wins; otherwise derive from the wire dialect — OpenAI Chat/Responses and
/// Ollama carry a system/developer message at any position ([`Inline`]), and
/// every other dialect exposes only a top-level system channel ([`Fold`]).
///
/// [`Inline`]: SystemMessagePlacement::Inline
/// [`Fold`]: SystemMessagePlacement::Fold
pub(crate) use harn_provider_catalog::capabilities::resolve_system_message_placement;
pub use rule::ProviderRule;
pub use tool_format::{
    direct_call_text_tools_refusal, no_viable_tool_channel, no_viable_tool_channel_with_caps,
    validate_tool_format, validate_tool_format_with_caps, ToolFormatDecision, ToolFormatWire,
};
