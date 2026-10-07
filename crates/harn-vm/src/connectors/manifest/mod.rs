//! Authoritative connector declarations shared by package resolution and hosts.
//! Credential trust direction is part of this contract, independent of the CLI.

use serde::Deserialize;

mod capabilities;
mod provider_setup;

pub use capabilities::{normalize_connector_capability, ConnectorCapabilities};
pub use provider_setup::*;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderConnectorManifest {
    #[serde(default)]
    pub harn: Option<String>,
    #[serde(default)]
    pub rust: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderOAuthManifest {
    #[serde(default, alias = "auth_url", alias = "authorization-endpoint")]
    pub authorization_endpoint: Option<String>,
    #[serde(default, alias = "token_url", alias = "token-endpoint")]
    pub token_endpoint: Option<String>,
    #[serde(default, alias = "registration_url", alias = "registration-endpoint")]
    pub registration_endpoint: Option<String>,
    #[serde(default)]
    pub resource: Option<String>,
    #[serde(default, alias = "scope")]
    pub scopes: Option<String>,
    #[serde(default, alias = "client-id")]
    pub client_id: Option<String>,
    #[serde(default, alias = "client-secret")]
    pub client_secret: Option<String>,
    #[serde(default, alias = "token_auth_method", alias = "token-auth-method")]
    pub token_endpoint_auth_method: Option<String>,
    /// Extra authorization query parameters, excluding fields owned by OAuth.
    #[serde(default, alias = "authorization-params")]
    pub authorization_params: std::collections::BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedProviderConnectorKind {
    Harn { module: String },
    RustBuiltin,
    Invalid(String),
}
