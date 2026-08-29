use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::{
    ConnectorCapabilities, ProviderConnectorManifest, ProviderOAuthManifest,
    ResolvedProviderConnectorKind,
};

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
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
    /// The arguments the operation accepts, for hosts that project it into an
    /// agent tool.
    ///
    /// Empty is a legitimate state, not an omission. A connector repository
    /// pins a Harn version and its manifest is `deny_unknown_fields`, so no
    /// connector can declare this key until a release carrying it reaches
    /// that repository. Hosts must therefore keep projecting an operation that
    /// declares nothing here, falling back to free-form arguments, rather than
    /// treating an empty list as "takes no arguments".
    #[serde(default)]
    pub parameters: Vec<ConnectorParameterManifest>,
}

/// One argument a connector operation accepts.
///
/// The service contract deliberately stops short of provider request and
/// response shapes, but a host projecting an operation into an agent tool has
/// to describe its arguments or the model is left guessing their names. This
/// is that minimum and no more.
///
/// It is a closed vocabulary rather than embedded JSON Schema on purpose:
/// every other field in this contract is a closed enum validated at the
/// manifest boundary, and an arbitrary schema blob would be unauthorable in
/// TOML, uncomparable, and unable to fail shut. Hosts widen this into whatever
/// schema dialect their tool surface speaks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectorParameterManifest {
    pub name: String,
    /// Plain-language description of the argument, shown to the model.
    pub description: String,
    #[serde(rename = "type")]
    pub value_type: ConnectorParameterType,
    #[serde(default)]
    pub required: bool,
    /// The closed set of accepted values, when the operation accepts only
    /// known ones. Empty means unconstrained.
    #[serde(default)]
    pub allowed_values: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectorParameterType {
    String,
    Integer,
    Number,
    Boolean,
    Object,
    Array,
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

        let mut parameter_names = std::collections::BTreeSet::new();
        for parameter in &operation.parameters {
            let parameter_label = if parameter.name.trim().is_empty() {
                "<missing>"
            } else {
                parameter.name.as_str()
            };
            if !portable_operation_id(&parameter.name) {
                issues.push(format!(
                    "service operation '{label}' parameter '{parameter_label}' name must use ASCII letters, digits, '.', '_', or '-'"
                ));
            } else if !parameter_names.insert(parameter.name.as_str()) {
                issues.push(format!(
                    "service operation '{label}' repeats parameter '{parameter_label}'"
                ));
            }
            // The description is the only thing that tells a model what to put
            // in the argument, so an undescribed parameter is worse than an
            // undeclared one: it advertises a name and explains nothing.
            if parameter.description.trim().is_empty() {
                issues.push(format!(
                    "service operation '{label}' parameter '{parameter_label}' description is required"
                ));
            }
            if !parameter.allowed_values.is_empty()
                && parameter.value_type != ConnectorParameterType::String
            {
                issues.push(format!(
                    "service operation '{label}' parameter '{parameter_label}' declares allowed values, which only apply to a string parameter"
                ));
            }
            let mut allowed = std::collections::BTreeSet::new();
            for value in &parameter.allowed_values {
                if !allowed.insert(value.as_str()) {
                    issues.push(format!(
                        "service operation '{label}' parameter '{parameter_label}' repeats allowed value '{value}'"
                    ));
                }
            }
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

/// How a host sets a connector up and recovers it when it breaks.
///
/// Like every other table in this contract, it fails closed: a key the runtime
/// does not parse is a key that does nothing at runtime, and an author has no
/// other signal that their field was discarded. The alternative — collecting
/// unrecognized keys into an ignored map — made a green `harn package verify
/// --strict` compatible with a misspelled or unsupported setup field.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderSetupManifest {
    #[serde(default, alias = "auth-type")]
    pub auth_type: Option<String>,
    #[serde(default)]
    pub flow: Option<String>,
    #[serde(default, alias = "required-scopes", alias = "scopes")]
    pub required_scopes: Vec<String>,
    #[serde(default, alias = "required-secrets")]
    pub required_secrets: Vec<ConnectorRequiredSecretManifest>,
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
}

impl ProviderSetupManifest {
    /// Every logical secret id required by the connector, independent of how
    /// trust flows across the provider seam.
    pub fn required_secret_ids(&self) -> impl Iterator<Item = &str> {
        self.required_secrets
            .iter()
            .map(|requirement| requirement.id.as_str())
    }

    /// Required secrets that the connector sends to the provider when it
    /// performs an authenticated operation.
    pub fn outbound_credentials(&self) -> impl Iterator<Item = &ConnectorRequiredSecretManifest> {
        self.required_secrets
            .iter()
            .filter(|requirement| requirement.direction == ConnectorSecretDirection::Outbound)
    }

    pub fn required_secret(&self, id: &str) -> Option<&ConnectorRequiredSecretManifest> {
        self.required_secrets
            .iter()
            .find(|requirement| requirement.id == id)
    }
}

/// The direction of trust for one connector secret.
///
/// Outbound credentials authenticate requests the connector sends. Inbound
/// verification secrets authenticate provider callbacks before their payloads
/// enter the connector. The manifest must declare this distinction; callers
/// never infer it from an id or list position.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectorSecretDirection {
    Outbound,
    Inbound,
}

impl ConnectorSecretDirection {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Outbound => "outbound",
            Self::Inbound => "inbound",
        }
    }
}

/// One logical secret a connector requires, plus the direction of trust.
///
/// Published connector packages predate the typed form and still write
/// `required_secrets = ["namespace/name"]`. Consumers pin those packages by
/// git rev, so refusing the bare string turns a Harn upgrade into a load
/// failure for an already-installed package cache that nothing in the
/// consumer can migrate. The bare string is therefore accepted here, at the
/// one seam that owns this shape, and every reader above this type sees the
/// typed struct. There is no second representation to keep in sync.
///
/// A legacy entry resolves to `outbound`, which is exactly the behavior the
/// package was published against: before the direction became typed, the
/// dispatch seam injected every declared secret into `args.secrets`, inbound
/// webhook secrets included. Some legacy lists do name a webhook secret, so
/// outbound is a faithful replay of the old semantics rather than a claim
/// about that secret's trust direction. Migrating the package to the typed
/// form is what earns the tightened guarantee; nothing here loosens a
/// manifest that already declares its directions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ConnectorRequiredSecretManifest {
    pub id: String,
    pub direction: ConnectorSecretDirection,
}

impl<'de> Deserialize<'de> for ConnectorRequiredSecretManifest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct RequiredSecretVisitor;

        impl<'de> serde::de::Visitor<'de> for RequiredSecretVisitor {
            type Value = ConnectorRequiredSecretManifest;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter
                    .write_str("a legacy secret id string, or a table with `id` and `direction`")
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(ConnectorRequiredSecretManifest::outbound(value))
            }

            fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(ConnectorRequiredSecretManifest::outbound(value))
            }

            fn visit_map<A>(self, map: A) -> Result<Self::Value, A::Error>
            where
                A: serde::de::MapAccess<'de>,
            {
                /// The typed spelling. Kept a separate private struct so the
                /// derive still enforces required keys and rejects unknown
                /// ones; the outer type only chooses between spellings.
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct TypedRequiredSecret {
                    id: String,
                    direction: ConnectorSecretDirection,
                }

                let typed = TypedRequiredSecret::deserialize(
                    serde::de::value::MapAccessDeserializer::new(map),
                )?;
                Ok(ConnectorRequiredSecretManifest {
                    id: typed.id,
                    direction: typed.direction,
                })
            }
        }

        deserializer.deserialize_any(RequiredSecretVisitor)
    }
}

impl ConnectorRequiredSecretManifest {
    pub fn outbound(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            direction: ConnectorSecretDirection::Outbound,
        }
    }

    pub fn inbound(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            direction: ConnectorSecretDirection::Inbound,
        }
    }
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
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
pub struct ConnectorCredentialEnvironmentManifest {
    pub secret: String,
    #[serde(default, alias = "environment-names")]
    pub environment_names: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
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

    /// The exact line shipped by every connector package published before the
    /// direction became typed. Refusing it turns a Harn upgrade into a load
    /// failure for every consumer that pins those packages by git rev.
    #[test]
    fn legacy_string_required_secret_parses_as_outbound() {
        let setup: ProviderSetupManifest = toml::from_str(
            r#"
required_secrets = ["duffel/test-access-token"]
"#,
        )
        .expect("legacy string form must load");

        assert_eq!(
            setup.required_secrets,
            vec![ConnectorRequiredSecretManifest::outbound(
                "duffel/test-access-token"
            )]
        );
        assert_eq!(
            setup.outbound_credentials().count(),
            1,
            "a legacy entry must reach outbound credential readers"
        );
    }

    /// The exact input that broke the fleet bump. `harn-bump-fleet` pins
    /// `harn-github-connector` v0.8.6 at commit 3649fd06aae1b0669771ecad5ed1b68
    /// 31fd2e76b, whose `harn.toml` line 23 is reproduced verbatim below. The
    /// released v0.10.121 CLI could not install its own locked dependencies
    /// against it (run 33261340737, exit 1 then 125).
    ///
    /// `github/webhook-secret` resolving to outbound is deliberate, not an
    /// oversight, and is asserted here so the choice stays visible. Harn's own
    /// typed fixtures classify that name inbound, but they govern manifests
    /// that declare a direction. This one does not, and pre-#7570
    /// `commands/connect/status.rs:155` cloned `required_secrets` whole with no
    /// direction filter, so the package required both secrets to report
    /// healthy. Dropping the webhook secret here would let an unconfigured
    /// connector report healthy, contradicting the package's own
    /// `missing_auth` text.
    #[test]
    fn fleet_github_connector_legacy_secrets_both_resolve_outbound() {
        let setup: ProviderSetupManifest = toml::from_str(
            r#"
required_secrets = ["github/app-private-key", "github/webhook-secret"]
"#,
        )
        .expect("the pinned fleet manifest's secret list must load");

        assert_eq!(
            setup.required_secrets,
            vec![
                ConnectorRequiredSecretManifest::outbound("github/app-private-key"),
                ConnectorRequiredSecretManifest::outbound("github/webhook-secret"),
            ]
        );
        assert_eq!(
            setup.outbound_credentials().count(),
            2,
            "both legacy entries must reach the outbound credential seam, as they did before the direction became typed"
        );
    }

    #[test]
    fn typed_table_required_secret_keeps_its_declared_direction() {
        let setup: ProviderSetupManifest = toml::from_str(
            r#"
required_secrets = [
  { id = "github/app-token", direction = "outbound" },
  { id = "github/webhook-signing-secret", direction = "inbound" },
]
"#,
        )
        .expect("typed form must load");

        assert_eq!(
            setup.required_secrets,
            vec![
                ConnectorRequiredSecretManifest::outbound("github/app-token"),
                ConnectorRequiredSecretManifest::inbound("github/webhook-signing-secret"),
            ]
        );
        assert_eq!(setup.outbound_credentials().count(), 1);
    }

    /// A package mid-migration writes both spellings in one list. Neither
    /// entry may be dropped or reclassified.
    #[test]
    fn mixed_required_secret_spellings_load_together() {
        let setup: ProviderSetupManifest = toml::from_str(
            r#"
required_secrets = [
  "gitlab/api-token",
  { id = "gitlab/webhook-token", direction = "inbound" },
  "gitlab/registry-token",
]
"#,
        )
        .expect("mixed list must load");

        assert_eq!(
            setup.required_secrets,
            vec![
                ConnectorRequiredSecretManifest::outbound("gitlab/api-token"),
                ConnectorRequiredSecretManifest::inbound("gitlab/webhook-token"),
                ConnectorRequiredSecretManifest::outbound("gitlab/registry-token"),
            ]
        );
        assert_eq!(
            setup.required_secret_ids().collect::<Vec<_>>(),
            vec![
                "gitlab/api-token",
                "gitlab/webhook-token",
                "gitlab/registry-token"
            ]
        );
    }

    /// Negative control. Accepting the bare string must not turn the table
    /// spelling permissive: a table is still checked key by key, so a missing
    /// `id`, a missing `direction`, an unknown key, and an unknown direction
    /// each still fail the load.
    #[test]
    fn malformed_required_secret_tables_still_fail_the_load() {
        let cases = [
            (
                "missing id",
                r#"required_secrets = [{ direction = "outbound" }]"#,
                "missing field",
            ),
            (
                "missing direction",
                r#"required_secrets = [{ id = "duffel/test-access-token" }]"#,
                "missing field",
            ),
            (
                "unknown key",
                r#"required_secrets = [{ id = "a/b", direction = "outbound", scope = "x" }]"#,
                "unknown field",
            ),
            (
                "unknown direction",
                r#"required_secrets = [{ id = "a/b", direction = "sideways" }]"#,
                "unknown variant",
            ),
        ];

        for (label, document, expected) in cases {
            let Err(error) = toml::from_str::<ProviderSetupManifest>(document) else {
                panic!("{label} was silently accepted");
            };
            assert!(
                error.to_string().contains(expected),
                "{label}: expected `{expected}` in: {error}"
            );
        }
    }

    /// The legacy spelling may not leak past this seam. Anything that
    /// re-encodes the parsed contract writes the typed table.
    #[test]
    fn legacy_entries_reserialize_in_the_typed_form() {
        let setup: ProviderSetupManifest =
            toml::from_str(r#"required_secrets = ["duffel/test-access-token"]"#)
                .expect("legacy string form must load");
        let encoded = toml::to_string(&setup).expect("setup manifest must encode");

        assert!(
            encoded.contains(r#"id = "duffel/test-access-token""#)
                && encoded.contains(r#"direction = "outbound""#),
            "{encoded}"
        );
    }

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
                parameters: Vec::new(),
            }],
        };

        let issues = connector_service_issues(&service).join("\n");
        assert!(issues.contains("does not require a fictional fixture"));
        assert!(issues.contains("RequestBody"));
        assert!(issues.contains("ResponseBody"));
    }

    #[test]
    fn operation_parameters_are_typed_and_optional() {
        let service: ConnectorServiceManifest = toml::from_str(
            r#"
name = "Duffel"
description = "Searches flights."

[[operations]]
id = "offers.list"
capability = "flights.research"
purpose = "List offers for a completed offer request."
effect = "read"
environments = ["test"]

[[operations.parameters]]
name = "offer_request_id"
description = "The offer request to list offers for."
type = "string"
required = true

[[operations.parameters]]
name = "limit"
description = "How many offers to return."
type = "integer"

[[operations.parameters]]
name = "sort"
description = "Ordering applied to the returned offers."
type = "string"
allowed_values = ["total_amount", "total_duration"]

[[operations]]
id = "places.list"
capability = "flights.research"
purpose = "Search airports and cities."
effect = "read"
environments = ["test"]
"#,
        )
        .expect("typed parameters");

        assert!(connector_service_issues(&service).is_empty());

        let listed = &service.operations[0].parameters;
        assert_eq!(listed.len(), 3);
        assert_eq!(listed[0].name, "offer_request_id");
        assert_eq!(listed[0].value_type, ConnectorParameterType::String);
        assert!(listed[0].required);
        // Absent `required` means optional, so a host never has to guess.
        assert!(!listed[1].required);
        assert_eq!(listed[1].value_type, ConnectorParameterType::Integer);
        assert_eq!(listed[2].allowed_values, ["total_amount", "total_duration"]);

        // An operation that declares no parameters stays valid. Connector
        // repositories cannot add the key until a release carrying it reaches
        // them, so this is the state every existing manifest is in.
        assert!(service.operations[1].parameters.is_empty());
    }

    #[test]
    fn operation_parameters_must_be_named_described_and_distinct() {
        let service: ConnectorServiceManifest = toml::from_str(
            r#"
name = "Duffel"
description = "Searches flights."

[[operations]]
id = "offers.list"
capability = "flights.research"
purpose = "List offers."
effect = "read"
environments = ["test"]

[[operations.parameters]]
name = "limit"
description = "How many offers to return."
type = "integer"

[[operations.parameters]]
name = "limit"
description = "A repeat of the same argument."
type = "integer"

[[operations.parameters]]
name = "sort by"
description = "Name is not portable."
type = "string"

[[operations.parameters]]
name = "cursor"
description = "   "
type = "string"

[[operations.parameters]]
name = "page"
description = "Allowed values only apply to a string."
type = "integer"
allowed_values = ["1", "2"]
"#,
        )
        .expect("parses; the issues are semantic");

        let issues = connector_service_issues(&service).join("\n");
        assert!(issues.contains("repeats parameter 'limit'"), "{issues}");
        assert!(
            issues.contains("parameter 'sort by' name must use"),
            "{issues}"
        );
        assert!(
            issues.contains("parameter 'cursor' description is required"),
            "{issues}"
        );
        assert!(
            issues.contains("parameter 'page' declares allowed values"),
            "{issues}"
        );
    }

    /// The parameter block is `deny_unknown_fields` like the rest of the
    /// contract, so a misspelled key fails closed instead of silently
    /// projecting an argument nobody described.
    #[test]
    fn operation_parameters_reject_unknown_fields() {
        let error = toml::from_str::<ConnectorServiceManifest>(
            r#"
name = "Duffel"
description = "Searches flights."

[[operations]]
id = "offers.list"
capability = "flights.research"
purpose = "List offers."
effect = "read"
environments = ["test"]

[[operations.parameters]]
name = "limit"
description = "How many offers to return."
type = "integer"
defualt = 10
"#,
        )
        .expect_err("unknown parameter keys must fail closed");

        assert!(error.to_string().contains("defualt"), "{error}");
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

    /// The reported defect: `[providers.setup]` accepted any key it did not
    /// recognize, so a misspelled or unsupported setup field passed a strict
    /// verify and was then discarded at runtime. Two differently shaped keys,
    /// matching the two shapes reported on the issue, so this cannot pass by
    /// happening to reject one shape.
    #[test]
    fn setup_manifest_rejects_unknown_fields() {
        for unknown in [
            "parameters = [\"bogus\"]",
            "totally_unknown_field = { nested = \"bogus\" }",
            "credential_enviroment = []",
        ] {
            let document = format!(
                r#"
auth_type = "api-key"
required_secrets = [{{ id = "demo/api-token", direction = "outbound" }}]
{unknown}
"#
            );
            let Err(error) = toml::from_str::<ProviderSetupManifest>(&document) else {
                panic!("unknown setup key was silently accepted: {unknown}");
            };
            assert!(
                error.to_string().contains("unknown field"),
                "{unknown}: {error}"
            );
        }
    }

    /// Every nested table under `[providers.setup]` fails closed too. A key
    /// discarded one table down is exactly as invisible to its author as one
    /// discarded at the top.
    #[test]
    fn setup_manifest_nested_tables_reject_unknown_fields() {
        let cases = [
            (
                "recovery",
                r#"
[recovery]
missing_auth = "Run the setup command."
missing_credentials = "Not a field."
"#,
            ),
            (
                "health_checks",
                r#"
[[health_checks]]
id = "ping"
kind = "http"
timeout_ms = 5000
"#,
            ),
            (
                "credential_environment",
                r#"
[[credential_environment]]
secret = "demo/api-token"
environment_names = ["DEMO_TOKEN"]
fallback = "DEMO_TOKEN_OLD"
"#,
            ),
            (
                "configuration_environment",
                r#"
[[configuration_environment]]
field = "oauth_client_id"
environment_names = ["DEMO_CLIENT_ID"]
required = true
"#,
            ),
        ];

        for (table, document) in cases {
            let Err(error) = toml::from_str::<ProviderSetupManifest>(document) else {
                panic!("unknown key under [{table}] was silently accepted");
            };
            assert!(
                error.to_string().contains("unknown field"),
                "{table}: {error}"
            );
        }
    }

    /// The class-killing guard. `deny_unknown_fields` was applied to this
    /// contract one struct at a time as each was written, which is how
    /// `[providers.setup]` ended up permissive while the operations table one
    /// subtree over failed closed. Adding a new deserialized table here without
    /// the attribute reopens the same hole, so require it structurally rather
    /// than trusting the next author to remember.
    #[test]
    fn every_deserialized_connector_manifest_table_denies_unknown_fields() {
        let source = include_str!("provider_setup.rs");
        let mut attributes = Vec::new();
        let mut permissive = Vec::new();

        for line in source.lines() {
            let line = line.trim();
            if line.starts_with("#[") {
                attributes.push(line.to_string());
                continue;
            }
            let Some(rest) = line
                .strip_prefix("pub struct ")
                .or_else(|| line.strip_prefix("struct "))
            else {
                if !line.starts_with("///") && !line.starts_with("//") {
                    attributes.clear();
                }
                continue;
            };
            let name = rest
                .split(|character: char| !character.is_alphanumeric() && character != '_')
                .next()
                .unwrap_or_default()
                .to_string();
            let derives_deserialize = attributes
                .iter()
                .any(|attribute| attribute.contains("Deserialize"));
            let denies = attributes
                .iter()
                .any(|attribute| attribute.contains("deny_unknown_fields"));
            if derives_deserialize && !denies {
                permissive.push(name);
            }
            attributes.clear();
        }

        assert!(
            permissive.is_empty(),
            "connector manifest tables must reject unknown keys, but these accept them: {}",
            permissive.join(", ")
        );
    }
}
