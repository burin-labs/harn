use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ProviderSetupManifest {
    #[serde(default, alias = "auth-type")]
    pub auth_type: Option<String>,
    #[serde(default)]
    pub flow: Option<String>,
    #[serde(default, alias = "required-scopes", alias = "scopes")]
    pub required_scopes: Vec<String>,
    #[serde(default, alias = "required-secrets")]
    pub required_secrets: Vec<String>,
    #[serde(default, alias = "credential-environment")]
    pub credential_environment: Vec<ConnectorCredentialEnvironmentManifest>,
    #[serde(default, alias = "setup-command")]
    pub setup_command: Vec<String>,
    #[serde(default, alias = "validation-command")]
    pub validation_command: Vec<String>,
    #[serde(default, alias = "health-checks")]
    pub health_checks: Vec<ConnectorHealthCheckManifest>,
    #[serde(default)]
    pub recovery: ConnectorRecoveryCopy,
    #[serde(flatten, default)]
    pub extra: BTreeMap<String, toml::Value>,
}

/// Bounded process-environment aliases for one logical connector secret.
///
/// The logical secret remains the stable interface. Environment names are
/// explicit recovery and automation sources. They never authorize scanning
/// arbitrary process variables.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectorCredentialEnvironmentManifest {
    pub secret: String,
    #[serde(default, alias = "environment-names")]
    pub environment_names: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectorHealthCheckManifest {
    pub id: String,
    pub kind: String,
    #[serde(default)]
    pub command: Vec<String>,
    #[serde(default)]
    pub secret: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectorRecoveryCopy {
    #[serde(default, alias = "missing-install")]
    pub missing_install: Option<String>,
    #[serde(default, alias = "missing-auth")]
    pub missing_auth: Option<String>,
    #[serde(default, alias = "expired-credentials")]
    pub expired_credentials: Option<String>,
    #[serde(default, alias = "revoked-credentials")]
    pub revoked_credentials: Option<String>,
    #[serde(default, alias = "missing-scopes")]
    pub missing_scopes: Option<String>,
    #[serde(default, alias = "inaccessible-resource")]
    pub inaccessible_resource: Option<String>,
    #[serde(default, alias = "transient-provider-outage")]
    pub transient_provider_outage: Option<String>,
}
