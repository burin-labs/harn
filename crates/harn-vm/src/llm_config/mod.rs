//! Provider and model catalog: the compiled-in default catalog, its overlay
//! layers, and the resolution/query surface Harn uses to turn a selector into
//! a concrete provider/model identity.
//!
//! Shared model records, configuration, and merge policy live in
//! [`harn_provider_catalog::config`]. This adapter owns [`loading`] of ambient overlays, model
//! [`taxonomy`] + provider inference, the [`catalog`] query surface, selector
//! [`resolution`], complementary-[`reviewer`] selection, and escalation-target
//! selection.

mod catalog;
mod endpoint_data_controls;
mod escalation;
mod loading;
mod provider_def;
mod resolution;
mod reviewer;
mod taxonomy;

#[cfg(test)]
mod escalation_tests;
#[cfg(test)]
mod logical_defaults_tests;
#[cfg(test)]
mod pricing_tests;
#[cfg(test)]
mod provider_endpoint_tests;
#[cfg(test)]
mod reviewer_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tool_format_fitness_tests;

pub use catalog::*;
pub(crate) use endpoint_data_controls::{
    trusted_data_controls, trusted_local_runtime, trusted_open_weight, EndpointDataControls,
};
pub use escalation::*;
pub use harn_provider_catalog::config::*;
pub use loading::*;
pub use provider_def::*;
pub use resolution::*;
pub use reviewer::*;
pub use taxonomy::*;
