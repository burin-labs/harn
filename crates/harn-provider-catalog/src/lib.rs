//! Provider catalog contracts shared by the runtime and host applications.
//! Configuration and capability policy have one owner here. Runtime environment
//! loading, credentials, provider requests, and live measurements remain in `harn-vm`.

// The catalog schema expands one complete JSON object through serde_json's macro.
#![recursion_limit = "256"]

pub mod artifact;
mod bindings;
pub mod capabilities;
mod catalog;
pub mod config;
pub mod data_controls;
pub mod decision_contract;
mod from_artifact;
pub mod model_def;
pub mod model_operation;
pub mod models_dev;
pub mod presentation;
pub mod pricing;
mod schema;
pub mod source;
pub mod taxonomy;
mod view;

pub use artifact::*;
pub use bindings::{swift_binding, swift_binding_embedded, typescript_declarations};
pub use catalog::{artifact_to_json, provider_classification, validate_artifact};
pub use data_controls::*;
pub use from_artifact::config_from_artifact;
pub use model_def::*;
pub use model_operation::*;
pub use presentation::*;
pub use pricing::*;
pub use schema::{schema_json, schema_value};
pub use view::{
    capability_model_id_for_config, configured_provider_selector, normalize_model_id_with_config,
    CatalogMeasurements, CatalogView,
};

/// Generated provider snapshot packaged with this crate.
pub const EMBEDDED_PROVIDERS_TOML: &str = include_str!("providers.toml");
/// Generated capability snapshot packaged with this crate.
pub const EMBEDDED_CAPABILITIES_TOML: &str = include_str!("capabilities.toml");

pub const PROVIDER_CATALOG_SCHEMA_VERSION: u32 = 13;
pub const PROVIDER_CATALOG_SCHEMA_ID: &str =
    "https://harnlang.com/schemas/provider-catalog.v13.json";
pub const PROVIDER_CATALOG_GENERATOR: &str = "harn provider catalog generate";
