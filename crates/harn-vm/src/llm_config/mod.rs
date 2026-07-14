//! Provider and model catalog: the compiled-in default catalog, its overlay
//! layers, and the resolution/query surface Harn uses to turn a selector into
//! a concrete provider/model identity.
//!
//! Split into bounded contexts: [`model_def`]/[`provider_def`] DTOs, the
//! [`config`] aggregate + merge, [`loading`] of ambient overlays, model
//! [`taxonomy`] + provider inference, the [`catalog`] query surface, selector
//! [`resolution`], and complementary-[`reviewer`] selection.

mod catalog;
mod config;
mod loading;
mod model_def;
mod presentation;
mod provider_def;
mod resolution;
mod reviewer;
mod taxonomy;

#[cfg(test)]
mod logical_defaults_tests;
#[cfg(test)]
mod tests;

pub use catalog::*;
pub use config::*;
pub use loading::*;
pub use model_def::*;
pub use presentation::*;
pub use provider_def::*;
pub use resolution::*;
pub use reviewer::*;
pub use taxonomy::*;
