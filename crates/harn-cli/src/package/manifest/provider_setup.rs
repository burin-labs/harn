use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::{
    ConnectorCapabilities, ProviderConnectorManifest, ProviderOAuthManifest,
    ResolvedProviderConnectorKind,
};

#[derive(Debug, Clone, Deserialize)]
pub struct ProviderManifestEntry {
    pub id: harn_vm::ProviderId,
    pub connector: ProviderConnectorManifest,
    #[serde(default)]
    pub oauth: Option<ProviderOAuthManifest>,
    #[serde(default)]
    pub setup: Option<ProviderSetupManifest>,
    #[serde(default)]
    pub service: Option<ConnectorServiceManifest>,
    #[serde(default)]
    pub capabilities: ConnectorCapabilities,
}

#[derive(Debug, Clone)]
pub struct ResolvedProviderConnectorConfig {
    pub id: harn_vm::ProviderId,
    pub manifest_dir: PathBuf,
    pub connector: ResolvedProviderConnectorKind,
    pub oauth: Option<ProviderOAuthManifest>,
    pub setup: Option<ProviderSetupManifest>,
    pub service: Option<ConnectorServiceManifest>,
    pub connector_contract_version: u32,
}

/// Product-facing connector metadata shared by every host projection.
///
/// Provider-specific request and response shapes stay in the connector. This
/// contract describes only the portable service, action, disclosure, spend,
/// evidence, and reconciliation semantics that hosts must agree on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectorServiceManifest {
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub operations: Vec<ConnectorOperationManifest>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectorOperationManifest {
    pub id: String,
    pub capability: String,
    /// Plain-language, action-specific reason shown before disclosure.
    pub purpose: String,
    pub effect: ConnectorOperationEffect,
    #[serde(default)]
    pub environments: Vec<ConnectorEnvironment>,
    #[serde(default)]
    pub evidence: Vec<ConnectorEvidenceRequirement>,
    #[serde(default)]
    pub protected_profile: ConnectorProtectedProfileManifest,
    #[serde(default)]
    pub test_profile: ConnectorTestProfile,
    #[serde(default)]
    pub external_spend: ConnectorExternalSpend,
    #[serde(default)]
    pub reconciliation: ConnectorReconciliation,
    #[serde(default)]
    pub redaction: Vec<ConnectorRedactionTarget>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectorOperationEffect {
    Read,
    Consequential,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectorEnvironment {
    Mock,
    Test,
    Live,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectorEvidenceRequirement {
    Citation,
    CurrentProviderState,
    FreshQuote,
    UserConfirmation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProtectedProfileFieldClass {
    LegalIdentity,
    BirthDate,
    ContactDetails,
    AccessibilityNeeds,
    LoyaltyAccounts,
    TravelDocuments,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectorProtectedProfileManifest {
    #[serde(default)]
    pub required: Vec<ProtectedProfileFieldClass>,
    #[serde(default)]
    pub optional: Vec<ProtectedProfileFieldClass>,
    #[serde(default)]
    pub conditional: Vec<ConnectorConditionalProfileRequirement>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectorConditionalProfileRequirement {
    /// Stable connector-owned condition id evaluated by the adapter.
    pub condition: String,
    pub field_classes: Vec<ProtectedProfileFieldClass>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectorExternalSpend {
    #[default]
    None,
    Estimate,
    Commit,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectorReconciliation {
    #[default]
    None,
    Supported,
    Required,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectorRedactionTarget {
    RequestBody,
    ResponseBody,
    ErrorBody,
    Headers,
    Query,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectorTestProfile {
    #[default]
    None,
    FictionalRequired,
}

/// Validate the product-facing service contract at every manifest boundary.
pub fn connector_service_issues(service: &ConnectorServiceManifest) -> Vec<String> {
    let mut issues = Vec::new();
    if service.name.trim().is_empty() {
        issues.push("service.name is required".to_string());
    }
    if service.description.trim().is_empty() {
        issues.push("service.description is required".to_string());
    }
    if service.operations.is_empty() {
        issues.push("service.operations must include at least one operation".to_string());
    }

    let mut operation_ids = std::collections::BTreeSet::new();
    for operation in &service.operations {
        let label = if operation.id.trim().is_empty() {
            "<missing>"
        } else {
            operation.id.as_str()
        };
        if !portable_operation_id(&operation.id) {
            issues.push(format!(
                "service operation '{label}' id must use ASCII letters, digits, '.', '_', or '-'"
            ));
        } else if !operation_ids.insert(operation.id.as_str()) {
            issues.push(format!(
                "service operation id '{}' is repeated",
                operation.id
            ));
        }
        if !portable_id(&operation.capability) {
            issues.push(format!(
                "service operation '{label}' capability must use lowercase letters, digits, '.', '_', or '-'"
            ));
        }
        if operation.purpose.trim().is_empty() {
            issues.push(format!("service operation '{label}' purpose is required"));
        }
        if operation.environments.is_empty() {
            issues.push(format!(
                "service operation '{label}' environments must not be empty"
            ));
        }
        if operation.effect == ConnectorOperationEffect::Read
            && operation.external_spend != ConnectorExternalSpend::None
        {
            issues.push(format!(
                "read operation '{label}' cannot declare external spend"
            ));
        }
        if operation.effect == ConnectorOperationEffect::Read
            && operation.reconciliation == ConnectorReconciliation::Required
        {
            issues.push(format!(
                "read operation '{label}' cannot require reconciliation"
            ));
        }

        let profile = &operation.protected_profile;
        let mut declared_classes = std::collections::BTreeSet::new();
        for class in profile.required.iter().chain(profile.optional.iter()) {
            if !declared_classes.insert(*class as u8) {
                issues.push(format!(
                    "service operation '{label}' repeats protected profile class '{class:?}'"
                ));
            }
        }
        for requirement in &profile.conditional {
            if !portable_id(&requirement.condition) {
                issues.push(format!(
                    "service operation '{label}' conditional profile id '{}' is invalid",
                    requirement.condition
                ));
            }
            if requirement.field_classes.is_empty() {
                issues.push(format!(
                    "service operation '{label}' conditional profile '{}' has no field classes",
                    requirement.condition
                ));
            }
        }

        let has_profile = !profile.required.is_empty()
            || !profile.optional.is_empty()
            || profile
                .conditional
                .iter()
                .any(|requirement| !requirement.field_classes.is_empty());
        if has_profile
            && operation.environments.contains(&ConnectorEnvironment::Test)
            && operation.test_profile != ConnectorTestProfile::FictionalRequired
        {
            issues.push(format!(
                "service operation '{label}' uses protected profile fields in test mode but does not require a fictional fixture"
            ));
        }
        if has_profile {
            for required_target in [
                ConnectorRedactionTarget::RequestBody,
                ConnectorRedactionTarget::ResponseBody,
                ConnectorRedactionTarget::ErrorBody,
            ] {
                if !operation.redaction.contains(&required_target) {
                    issues.push(format!(
                        "service operation '{label}' with protected profile fields must redact {required_target:?}"
                    ));
                }
            }
        }
    }
    issues
}

fn portable_id(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
        })
}

fn portable_operation_id(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(|byte| {
            byte.is_ascii_alphabetic()
                || byte.is_ascii_digit()
                || matches!(byte, b'.' | b'_' | b'-')
        })
}

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
    #[serde(default, alias = "configuration-environment")]
    pub configuration_environment: Vec<ConnectorConfigurationEnvironmentManifest>,
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

/// Non-secret setup input that a connector may read from an explicit process
/// environment allowlist. Values are consumed only by the setup adapter and
/// are never projected into plans, status reports, or model context.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ConnectorSetupConfigurationField {
    #[serde(rename = "oauth_client_id")]
    OAuthClientId,
}

impl ConnectorSetupConfigurationField {
    pub const WIRE_VALUES: &'static [&'static str] = &["oauth_client_id"];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OAuthClientId => "oauth_client_id",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectorConfigurationEnvironmentManifest {
    pub field: ConnectorSetupConfigurationField,
    #[serde(default, alias = "environment-names")]
    pub environment_names: Vec<String>,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protected_profile_contract_is_typed_and_complete() {
        let service: ConnectorServiceManifest = toml::from_str(
            r#"
name = "Duffel"
description = "Searches flights and creates governed test orders."

[[operations]]
id = "orders.create"
capability = "travel.booking"
purpose = "Create the exact reviewed flight order."
effect = "consequential"
environments = ["test"]
evidence = ["fresh_quote", "user_confirmation"]
external_spend = "commit"
reconciliation = "required"
redaction = ["request_body", "response_body", "error_body"]
test_profile = "fictional_required"

[operations.protected_profile]
required = ["legal_identity", "birth_date"]
optional = ["contact_details"]

[[operations.protected_profile.conditional]]
condition = "international_itinerary"
field_classes = ["travel_documents"]
"#,
        )
        .expect("typed service manifest");

        assert!(connector_service_issues(&service).is_empty());
        assert_eq!(
            service.operations[0].protected_profile.required,
            [
                ProtectedProfileFieldClass::LegalIdentity,
                ProtectedProfileFieldClass::BirthDate,
            ]
        );
    }

    #[test]
    fn protected_profile_test_actions_require_fictional_fixture_and_redaction() {
        let service = ConnectorServiceManifest {
            name: "Duffel".to_string(),
            description: "Travel".to_string(),
            operations: vec![ConnectorOperationManifest {
                id: "orders.create".to_string(),
                capability: "travel.booking".to_string(),
                purpose: "Create an order".to_string(),
                effect: ConnectorOperationEffect::Consequential,
                environments: vec![ConnectorEnvironment::Test],
                evidence: Vec::new(),
                protected_profile: ConnectorProtectedProfileManifest {
                    required: vec![ProtectedProfileFieldClass::LegalIdentity],
                    ..ConnectorProtectedProfileManifest::default()
                },
                test_profile: ConnectorTestProfile::None,
                external_spend: ConnectorExternalSpend::Commit,
                reconciliation: ConnectorReconciliation::Required,
                redaction: vec![ConnectorRedactionTarget::ErrorBody],
            }],
        };

        let issues = connector_service_issues(&service).join("\n");
        assert!(issues.contains("does not require a fictional fixture"));
        assert!(issues.contains("RequestBody"));
        assert!(issues.contains("ResponseBody"));
    }

    #[test]
    fn service_manifest_rejects_unknown_policy_fields() {
        let error = toml::from_str::<ConnectorServiceManifest>(
            r#"
name = "Echo"
description = "Echoes messages."
automatic_approval = true
"#,
        )
        .expect_err("unknown policy keys must fail closed");
        assert!(error.to_string().contains("unknown field"));
    }
}
