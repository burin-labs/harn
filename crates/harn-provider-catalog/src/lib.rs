//! Provider catalog contracts shared by the runtime and host applications.
//! Configuration and capability policy have one owner here. Runtime environment
//! loading, credentials, provider requests, and live measurements remain in `harn-vm`.

pub mod artifact;
pub mod capabilities;
pub mod config;
pub mod data_controls;
pub mod model_def;
pub mod model_operation;
pub mod presentation;
pub mod pricing;

pub use artifact::*;
pub use data_controls::*;
pub use model_def::*;
pub use model_operation::*;
pub use presentation::*;
pub use pricing::*;

/// Generated provider snapshot packaged with this crate.
pub const EMBEDDED_PROVIDERS_TOML: &str = include_str!("providers.toml");
/// Generated capability snapshot packaged with this crate.
pub const EMBEDDED_CAPABILITIES_TOML: &str = include_str!("capabilities.toml");
