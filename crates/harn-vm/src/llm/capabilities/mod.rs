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

/// Generated shipped default rules. Compiled into the binary at build time.
pub(super) const BUILTIN_TOML: &str = include_str!("../capabilities.toml");
/// Generated provider/model snapshot built from catalog_sources/**/*.toml.
pub(super) const BUILTIN_PROVIDERS_TOML: &str = include_str!("../providers.toml");

mod audit;
mod lookup;
mod model;
mod overrides;
mod rule;
mod tool_format;

pub(crate) use overrides::swap_user_overrides;

pub use audit::{
    audit_builtin_catalogued_chat_model_tool_capabilities,
    audit_catalogued_chat_model_tool_capabilities, matrix_rows, ProviderCapabilityMatrixRow,
    ToolCapabilityAuditGap, ToolCapabilityAuditReport,
};
pub use lookup::{
    builtin_file, clear_user_overrides, lookup, lookup_with_base_file, lookup_with_user_overrides,
    parse_capabilities_toml, provider_limit_providers, provider_limits_for, set_user_overrides,
    set_user_overrides_from_manifest_toml, set_user_overrides_toml,
};
pub use model::{
    Capabilities, CapabilitiesFile, ComputerUseStyle, GovernorBackoff, ProviderDefaults,
    ProviderLimits, ScreenshotScaling, WireDialect,
};
pub use rule::ProviderRule;
pub use tool_format::{
    no_viable_tool_channel, no_viable_tool_channel_with_caps, validate_tool_format,
    validate_tool_format_with_caps, ToolFormatDecision, ToolFormatWire,
};
