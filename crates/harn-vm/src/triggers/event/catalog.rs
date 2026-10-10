use std::collections::BTreeMap;
use std::sync::{OnceLock, RwLock};

use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

use super::core::ProviderId;
use super::normalize::{a2a_push_payload, cron_payload, stream_payload, webhook_payload};
use super::payloads::{KnownProviderPayload, ProviderPayload, StreamEventPayload};

impl ProviderPayload {
    pub fn normalize(
        provider: &ProviderId,
        kind: &str,
        headers: &BTreeMap<String, String>,
        raw: JsonValue,
    ) -> Result<Self, ProviderCatalogError> {
        let registration = provider_catalog()
            .read()
            .expect("provider catalog poisoned")
            .registration(provider)?;
        registration.normalize(kind, headers, raw)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderSecretRequirement {
    pub name: String,
    pub required: bool,
    pub namespace: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderOutboundMethod {
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SignatureVerificationMetadata {
    #[default]
    None,
    Hmac {
        variant: String,
        raw_body: bool,
        signature_header: String,
        timestamp_header: Option<String>,
        id_header: Option<String>,
        default_tolerance_secs: Option<i64>,
        digest: String,
        encoding: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProviderRuntimeMetadata {
    Builtin {
        connector: String,
        default_signature_variant: Option<String>,
    },
    #[default]
    Placeholder,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ProviderMetadata {
    pub provider: String,
    #[serde(default)]
    pub kinds: Vec<String>,
    pub schema_name: String,
    #[serde(default)]
    pub outbound_methods: Vec<ProviderOutboundMethod>,
    #[serde(default)]
    pub secret_requirements: Vec<ProviderSecretRequirement>,
    #[serde(default)]
    pub signature_verification: SignatureVerificationMetadata,
    #[serde(default)]
    pub runtime: ProviderRuntimeMetadata,
}

impl ProviderMetadata {
    pub fn supports_kind(&self, kind: &str) -> bool {
        self.kinds.iter().any(|candidate| candidate == kind)
    }

    pub fn required_secret_names(&self) -> impl Iterator<Item = &str> {
        self.secret_requirements
            .iter()
            .filter(|requirement| requirement.required)
            .map(|requirement| requirement.name.as_str())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ProviderCatalogError {
    DuplicateProvider(String),
    UnknownProvider(String),
    InvalidMetadata(String),
    InvalidPayload(String),
}

impl std::fmt::Display for ProviderCatalogError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DuplicateProvider(provider) => {
                write!(f, "provider `{provider}` is already registered")
            }
            Self::UnknownProvider(provider) => write!(f, "provider `{provider}` is not registered"),
            Self::InvalidMetadata(message) | Self::InvalidPayload(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for ProviderCatalogError {}

/// A provider's description and the runtime-owned way to tag its payload.
/// Package connectors normalize their own inbound deliveries; no package code
/// is invoked through the catalog.
#[derive(Clone)]
struct ProviderRegistration {
    metadata: ProviderMetadata,
    normalizer: ProviderNormalizer,
}

#[derive(Clone, Copy)]
enum ProviderNormalizer {
    Builtin(fn(&str, &BTreeMap<String, String>, JsonValue) -> ProviderPayload),
    Stream(fn(StreamEventPayload) -> KnownProviderPayload),
    Extension,
}

impl ProviderRegistration {
    fn new(
        metadata: ProviderMetadata,
        normalizer: ProviderNormalizer,
    ) -> Result<Self, ProviderCatalogError> {
        if metadata.provider.is_empty() || metadata.provider.trim() != metadata.provider {
            return Err(ProviderCatalogError::InvalidMetadata(
                "provider registration requires a nonempty, unpadded provider id".into(),
            ));
        }
        if metadata.schema_name.trim().is_empty() {
            return Err(ProviderCatalogError::InvalidMetadata(format!(
                "provider `{}` requires a payload schema name",
                metadata.provider
            )));
        }
        let is_builtin = !matches!(normalizer, ProviderNormalizer::Extension);
        let declared_builtin = matches!(&metadata.runtime, ProviderRuntimeMetadata::Builtin { .. });
        if is_builtin != declared_builtin {
            return Err(ProviderCatalogError::InvalidMetadata(format!(
                "provider `{}` has a runtime classification that conflicts with its registration",
                metadata.provider
            )));
        }
        Ok(Self {
            metadata,
            normalizer,
        })
    }

    fn normalize(
        &self,
        kind: &str,
        headers: &BTreeMap<String, String>,
        raw: JsonValue,
    ) -> Result<ProviderPayload, ProviderCatalogError> {
        let payload = match self.normalizer {
            ProviderNormalizer::Builtin(normalize) => normalize(kind, headers, raw),
            ProviderNormalizer::Stream(variant) => {
                ProviderPayload::Known(variant(stream_payload(kind, headers, raw)))
            }
            ProviderNormalizer::Extension => {
                ProviderPayload::extension(&self.metadata.provider, &self.metadata.schema_name, raw)
            }
        };
        if payload.provider() != self.metadata.provider {
            return Err(ProviderCatalogError::InvalidPayload(format!(
                "provider `{}` normalized a payload for `{}`",
                self.metadata.provider,
                payload.provider()
            )));
        }
        Ok(payload)
    }
}

#[derive(Clone, Default)]
pub struct ProviderCatalog {
    providers: BTreeMap<String, ProviderRegistration>,
}

impl ProviderCatalog {
    pub fn with_defaults() -> Self {
        let mut catalog = Self::default();
        for builtin in default_providers() {
            catalog
                .register_with(builtin.metadata, builtin.normalizer)
                .expect("default providers must register cleanly");
        }
        catalog
    }

    pub fn with_defaults_and(
        providers: Vec<ProviderMetadata>,
    ) -> Result<Self, ProviderCatalogError> {
        let mut catalog = Self::with_defaults();
        catalog.merge(providers)?;
        Ok(catalog)
    }

    /// Add package providers to this catalog without disturbing existing ones.
    ///
    /// Repeating an identical registration is idempotent. A different
    /// description for the same provider is an error, including for a built-in
    /// provider. The entire batch is admitted or none of it is.
    pub fn merge(&mut self, providers: Vec<ProviderMetadata>) -> Result<(), ProviderCatalogError> {
        let mut next = self.providers.clone();
        for metadata in providers {
            let registration = ProviderRegistration::new(metadata, ProviderNormalizer::Extension)?;
            let provider = registration.metadata.provider.clone();
            match next.get(&provider) {
                Some(existing)
                    if matches!(existing.normalizer, ProviderNormalizer::Extension)
                        && existing.metadata == registration.metadata => {}
                Some(_) => return Err(ProviderCatalogError::DuplicateProvider(provider)),
                None => {
                    next.insert(provider, registration);
                }
            }
        }
        self.providers = next;
        Ok(())
    }

    pub fn register(&mut self, metadata: ProviderMetadata) -> Result<(), ProviderCatalogError> {
        self.register_with(metadata, ProviderNormalizer::Extension)
    }

    fn register_with(
        &mut self,
        metadata: ProviderMetadata,
        normalizer: ProviderNormalizer,
    ) -> Result<(), ProviderCatalogError> {
        let registration = ProviderRegistration::new(metadata, normalizer)?;
        let provider = registration.metadata.provider.clone();
        if self.providers.contains_key(provider.as_str()) {
            return Err(ProviderCatalogError::DuplicateProvider(provider));
        }
        self.providers.insert(provider, registration);
        Ok(())
    }

    pub fn normalize(
        &self,
        provider: &ProviderId,
        kind: &str,
        headers: &BTreeMap<String, String>,
        raw: JsonValue,
    ) -> Result<ProviderPayload, ProviderCatalogError> {
        self.registration(provider)?.normalize(kind, headers, raw)
    }

    fn registration(
        &self,
        provider: &ProviderId,
    ) -> Result<ProviderRegistration, ProviderCatalogError> {
        self.providers
            .get(provider.as_str())
            .cloned()
            .ok_or_else(|| ProviderCatalogError::UnknownProvider(provider.0.clone()))
    }

    pub fn schema_names(&self) -> BTreeMap<String, String> {
        self.providers
            .iter()
            .map(|(provider, registration)| {
                (provider.clone(), registration.metadata.schema_name.clone())
            })
            .collect()
    }

    pub fn entries(&self) -> Vec<ProviderMetadata> {
        self.providers
            .values()
            .map(|registration| registration.metadata.clone())
            .collect()
    }

    pub fn metadata_for(&self, provider: &str) -> Option<ProviderMetadata> {
        self.providers
            .get(provider)
            .map(|registration| registration.metadata.clone())
    }
}

/// Contribute package provider descriptions to the process-wide catalog.
///
/// Loading a package's runtime extensions says what that package provides; it
/// does not describe the whole world. Components that load packages
/// independently — an orchestrator harness and a persona command sharing a
/// process — therefore compose rather than erase each other's providers.
pub fn register_provider_metadata(
    providers: Vec<ProviderMetadata>,
) -> Result<(), ProviderCatalogError> {
    provider_catalog()
        .write()
        .expect("provider catalog poisoned")
        .merge(providers)
}

/// Drop every contributed provider, leaving the builtin schemas.
pub fn reset_provider_catalog() {
    *provider_catalog()
        .write()
        .expect("provider catalog poisoned") = ProviderCatalog::with_defaults();
}

pub fn registered_provider_schema_names() -> BTreeMap<String, String> {
    provider_catalog()
        .read()
        .expect("provider catalog poisoned")
        .schema_names()
}

pub fn registered_provider_metadata() -> Vec<ProviderMetadata> {
    provider_catalog()
        .read()
        .expect("provider catalog poisoned")
        .entries()
}

pub fn provider_metadata(provider: &str) -> Option<ProviderMetadata> {
    provider_catalog()
        .read()
        .expect("provider catalog poisoned")
        .metadata_for(provider)
}

fn provider_catalog() -> &'static RwLock<ProviderCatalog> {
    static PROVIDER_CATALOG: OnceLock<RwLock<ProviderCatalog>> = OnceLock::new();
    PROVIDER_CATALOG.get_or_init(|| RwLock::new(ProviderCatalog::with_defaults()))
}

struct BuiltinProvider {
    metadata: ProviderMetadata,
    normalizer: ProviderNormalizer,
}

fn provider_metadata_entry(
    provider: &str,
    kinds: &[&str],
    schema_name: &str,
    outbound_methods: &[&str],
    signature_verification: SignatureVerificationMetadata,
    secret_requirements: Vec<ProviderSecretRequirement>,
    runtime: ProviderRuntimeMetadata,
) -> ProviderMetadata {
    ProviderMetadata {
        provider: provider.to_string(),
        kinds: kinds.iter().map(|kind| kind.to_string()).collect(),
        schema_name: schema_name.to_string(),
        outbound_methods: outbound_methods
            .iter()
            .map(|name| ProviderOutboundMethod {
                name: (*name).to_string(),
            })
            .collect(),
        secret_requirements,
        signature_verification,
        runtime,
    }
}

fn hmac_signature_metadata(
    variant: &str,
    signature_header: &str,
    timestamp_header: Option<&str>,
    id_header: Option<&str>,
    default_tolerance_secs: Option<i64>,
    encoding: &str,
) -> SignatureVerificationMetadata {
    SignatureVerificationMetadata::Hmac {
        variant: variant.to_string(),
        raw_body: true,
        signature_header: signature_header.to_string(),
        timestamp_header: timestamp_header.map(ToString::to_string),
        id_header: id_header.map(ToString::to_string),
        default_tolerance_secs,
        digest: "sha256".to_string(),
        encoding: encoding.to_string(),
    }
}

fn required_secret(name: &str, namespace: &str) -> ProviderSecretRequirement {
    ProviderSecretRequirement {
        name: name.to_string(),
        required: true,
        namespace: namespace.to_string(),
    }
}

fn default_providers() -> Vec<BuiltinProvider> {
    vec![
        BuiltinProvider {
            metadata: provider_metadata_entry(
                "cron",
                &["cron"],
                "CronEventPayload",
                &[],
                SignatureVerificationMetadata::None,
                Vec::new(),
                ProviderRuntimeMetadata::Builtin {
                    connector: "cron".to_string(),
                    default_signature_variant: None,
                },
            ),
            normalizer: ProviderNormalizer::Builtin(cron_payload),
        },
        BuiltinProvider {
            metadata: provider_metadata_entry(
                "webhook",
                &["webhook"],
                "GenericWebhookPayload",
                &[],
                hmac_signature_metadata(
                    "standard",
                    "webhook-signature",
                    Some("webhook-timestamp"),
                    Some("webhook-id"),
                    Some(300),
                    "base64",
                ),
                vec![required_secret("signing_secret", "webhook")],
                ProviderRuntimeMetadata::Builtin {
                    connector: "webhook".to_string(),
                    default_signature_variant: Some("standard".to_string()),
                },
            ),
            normalizer: ProviderNormalizer::Builtin(webhook_payload),
        },
        BuiltinProvider {
            metadata: provider_metadata_entry(
                "a2a-push",
                &["a2a-push"],
                "A2aPushPayload",
                &[],
                SignatureVerificationMetadata::None,
                Vec::new(),
                ProviderRuntimeMetadata::Builtin {
                    connector: "a2a-push".to_string(),
                    default_signature_variant: None,
                },
            ),
            normalizer: ProviderNormalizer::Builtin(a2a_push_payload),
        },
        stream_provider("kafka", KnownProviderPayload::Kafka),
        stream_provider("nats", KnownProviderPayload::Nats),
        stream_provider("pulsar", KnownProviderPayload::Pulsar),
        stream_provider("postgres-cdc", KnownProviderPayload::PostgresCdc),
        stream_provider("email", KnownProviderPayload::Email),
        stream_provider("websocket", KnownProviderPayload::Websocket),
    ]
}

fn stream_provider(
    provider_id: &'static str,
    variant: fn(StreamEventPayload) -> KnownProviderPayload,
) -> BuiltinProvider {
    BuiltinProvider {
        metadata: provider_metadata_entry(
            provider_id,
            &["stream"],
            "StreamEventPayload",
            &[],
            SignatureVerificationMetadata::None,
            Vec::new(),
            ProviderRuntimeMetadata::Builtin {
                connector: "stream".to_string(),
                default_signature_variant: None,
            },
        ),
        normalizer: ProviderNormalizer::Stream(variant),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_payload_identity_check_rejects_a_miswired_normalizer() {
        let registration = ProviderRegistration::new(
            ProviderMetadata {
                provider: "wrong".into(),
                schema_name: "GenericWebhookPayload".into(),
                runtime: ProviderRuntimeMetadata::Builtin {
                    connector: "webhook".into(),
                    default_signature_variant: None,
                },
                ..ProviderMetadata::default()
            },
            ProviderNormalizer::Builtin(webhook_payload),
        )
        .unwrap();

        assert!(matches!(
            registration.normalize("webhook", &BTreeMap::new(), JsonValue::Null),
            Err(ProviderCatalogError::InvalidPayload(_))
        ));
    }
}
