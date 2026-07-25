use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, OnceLock, RwLock};

use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

use super::core::ProviderId;
use super::normalize::{
    a2a_push_payload, cron_payload, email_payload, github_payload, kafka_payload, linear_payload,
    nats_payload, notion_payload, postgres_cdc_payload, pulsar_payload, slack_payload,
    webhook_payload, websocket_payload,
};
use super::payloads::ProviderPayload;

impl ProviderPayload {
    pub fn normalize(
        provider: &ProviderId,
        kind: &str,
        headers: &BTreeMap<String, String>,
        raw: JsonValue,
    ) -> Result<Self, ProviderCatalogError> {
        provider_catalog()
            .read()
            .expect("provider catalog poisoned")
            .normalize(provider, kind, headers, raw)
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

pub trait ProviderSchema: Send + Sync {
    fn provider_id(&self) -> &str;
    fn harn_schema_name(&self) -> &str;
    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            provider: self.provider_id().to_string(),
            schema_name: self.harn_schema_name().to_string(),
            ..ProviderMetadata::default()
        }
    }
    fn normalize(
        &self,
        kind: &str,
        headers: &BTreeMap<String, String>,
        raw: JsonValue,
    ) -> Result<ProviderPayload, ProviderCatalogError>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProviderCatalogError {
    DuplicateProvider(String),
    UnknownProvider(String),
}

impl std::fmt::Display for ProviderCatalogError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DuplicateProvider(provider) => {
                write!(f, "provider `{provider}` is already registered")
            }
            Self::UnknownProvider(provider) => write!(f, "provider `{provider}` is not registered"),
        }
    }
}

impl std::error::Error for ProviderCatalogError {}

#[derive(Clone, Default)]
pub struct ProviderCatalog {
    providers: BTreeMap<String, Arc<dyn ProviderSchema>>,
}

impl ProviderCatalog {
    pub fn with_defaults() -> Self {
        let mut catalog = Self::default();
        for schema in default_provider_schemas() {
            catalog
                .register(schema)
                .expect("default providers must register cleanly");
        }
        catalog
    }

    pub fn with_defaults_and(
        schemas: Vec<Arc<dyn ProviderSchema>>,
    ) -> Result<Self, ProviderCatalogError> {
        let mut catalog = Self::with_defaults();
        catalog.merge(schemas)?;
        Ok(catalog)
    }

    /// Add `schemas` to this catalog without disturbing what is already in it.
    ///
    /// A contribution is skipped when its id belongs to a builtin schema — a
    /// package cannot redefine `github` — and when it re-registers a provider
    /// under the schema name already recorded for it, which is what reloading
    /// the same manifest looks like. Anything else is a real disagreement
    /// about what a provider id means, and is reported rather than settled by
    /// load order.
    pub fn merge(
        &mut self,
        schemas: Vec<Arc<dyn ProviderSchema>>,
    ) -> Result<(), ProviderCatalogError> {
        for schema in schemas {
            let provider = schema.provider_id().to_string();
            match self.providers.get(provider.as_str()) {
                Some(existing)
                    if default_provider_ids().contains(provider.as_str())
                        || existing.harn_schema_name() == schema.harn_schema_name() => {}
                Some(_) => return Err(ProviderCatalogError::DuplicateProvider(provider)),
                None => {
                    self.providers.insert(provider, schema);
                }
            }
        }
        Ok(())
    }

    pub fn register(
        &mut self,
        schema: Arc<dyn ProviderSchema>,
    ) -> Result<(), ProviderCatalogError> {
        let provider = schema.provider_id().to_string();
        if self.providers.contains_key(provider.as_str()) {
            return Err(ProviderCatalogError::DuplicateProvider(provider));
        }
        self.providers.insert(provider, schema);
        Ok(())
    }

    pub fn normalize(
        &self,
        provider: &ProviderId,
        kind: &str,
        headers: &BTreeMap<String, String>,
        raw: JsonValue,
    ) -> Result<ProviderPayload, ProviderCatalogError> {
        let schema = self
            .providers
            .get(provider.as_str())
            .ok_or_else(|| ProviderCatalogError::UnknownProvider(provider.0.clone()))?;
        schema.normalize(kind, headers, raw)
    }

    pub fn schema_names(&self) -> BTreeMap<String, String> {
        self.providers
            .iter()
            .map(|(provider, schema)| (provider.clone(), schema.harn_schema_name().to_string()))
            .collect()
    }

    pub fn entries(&self) -> Vec<ProviderMetadata> {
        self.providers
            .values()
            .map(|schema| schema.metadata())
            .collect()
    }

    pub fn metadata_for(&self, provider: &str) -> Option<ProviderMetadata> {
        self.providers.get(provider).map(|schema| schema.metadata())
    }
}

/// Contribute `schemas` to the process-wide catalog.
///
/// Loading a package's runtime extensions says what that package provides; it
/// does not describe the whole world. Components that load packages
/// independently — an orchestrator harness and a persona command sharing a
/// process — therefore compose rather than erase each other's providers.
pub fn register_provider_schemas(
    schemas: Vec<Arc<dyn ProviderSchema>>,
) -> Result<(), ProviderCatalogError> {
    provider_catalog()
        .write()
        .expect("provider catalog poisoned")
        .merge(schemas)
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

fn default_provider_ids() -> &'static BTreeSet<String> {
    static DEFAULT_PROVIDER_IDS: OnceLock<BTreeSet<String>> = OnceLock::new();
    DEFAULT_PROVIDER_IDS.get_or_init(|| {
        default_provider_schemas()
            .iter()
            .map(|schema| schema.provider_id().to_string())
            .collect()
    })
}

struct BuiltinProviderSchema {
    provider_id: &'static str,
    harn_schema_name: &'static str,
    metadata: ProviderMetadata,
    normalize: fn(&str, &BTreeMap<String, String>, JsonValue) -> ProviderPayload,
}

impl ProviderSchema for BuiltinProviderSchema {
    fn provider_id(&self) -> &str {
        self.provider_id
    }

    fn harn_schema_name(&self) -> &str {
        self.harn_schema_name
    }

    fn metadata(&self) -> ProviderMetadata {
        self.metadata.clone()
    }

    fn normalize(
        &self,
        kind: &str,
        headers: &BTreeMap<String, String>,
        raw: JsonValue,
    ) -> Result<ProviderPayload, ProviderCatalogError> {
        Ok((self.normalize)(kind, headers, raw))
    }
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

fn outbound_method(name: &str) -> ProviderOutboundMethod {
    ProviderOutboundMethod {
        name: name.to_string(),
    }
}

fn optional_secret(name: &str, namespace: &str) -> ProviderSecretRequirement {
    ProviderSecretRequirement {
        name: name.to_string(),
        required: false,
        namespace: namespace.to_string(),
    }
}

fn default_provider_schemas() -> Vec<Arc<dyn ProviderSchema>> {
    vec![
        Arc::new(BuiltinProviderSchema {
            provider_id: "github",
            harn_schema_name: "GitHubEventPayload",
            metadata: provider_metadata_entry(
                "github",
                &["webhook"],
                "GitHubEventPayload",
                &[
                    "github.pr.list",
                    "github.pr.view",
                    "github.pr.edit",
                    "github.pr.checks",
                    "github.pr.merge",
                    "github.pr.enable_auto_merge",
                    "github.pr.comment",
                    "github.actions.workflow_dispatch",
                    "github.actions.runs",
                    "github.actions.run",
                    "github.actions.logs",
                    "github.release.view",
                    "github.release.latest",
                    "github.release.assets",
                    "github.merge_queue.entries",
                    "github.merge_queue.enqueue",
                    "github.issue.create",
                    "github.issue.comment",
                    "github.branch.protection",
                    "api_call",
                    "issues.create_comment",
                    "issues.create",
                    "issues.create_with_template",
                    "issues.update",
                    "issues.add_labels",
                    "pulls.list",
                    "pulls.list_with_checks",
                    "pulls.get",
                    "pulls.update",
                    "pulls.merge",
                    "pulls.merge_safe",
                    "pulls.create_review_comment",
                    "pulls.get_diff",
                    "pulls.list_files",
                    "pulls.list_reviews",
                    "repos.get_content",
                    "repos.get_text",
                    "repos.get_latest_release",
                    "repos.list_release_assets",
                    "repos.get_branch_protection",
                    "git.delete_ref",
                    "actions.workflow_dispatch",
                    "actions.workflow_runs.list",
                    "actions.workflow_run.get",
                    "check_runs.create",
                    "check_runs.update",
                    "graphql",
                ],
                hmac_signature_metadata(
                    "github",
                    "X-Hub-Signature-256",
                    None,
                    Some("X-GitHub-Delivery"),
                    None,
                    "hex",
                ),
                vec![required_secret("signing_secret", "github")],
                ProviderRuntimeMetadata::Placeholder,
            ),
            normalize: github_payload,
        }),
        Arc::new(BuiltinProviderSchema {
            provider_id: "slack",
            harn_schema_name: "SlackEventPayload",
            metadata: provider_metadata_entry(
                "slack",
                &["webhook"],
                "SlackEventPayload",
                &[
                    "post_message",
                    "update_message",
                    "add_reaction",
                    "open_view",
                    "user_info",
                    "api_call",
                    "upload_file",
                ],
                hmac_signature_metadata(
                    "slack",
                    "X-Slack-Signature",
                    Some("X-Slack-Request-Timestamp"),
                    None,
                    Some(300),
                    "hex",
                ),
                vec![required_secret("signing_secret", "slack")],
                ProviderRuntimeMetadata::Placeholder,
            ),
            normalize: slack_payload,
        }),
        Arc::new(BuiltinProviderSchema {
            provider_id: "linear",
            harn_schema_name: "LinearEventPayload",
            metadata: {
                let mut metadata = provider_metadata_entry(
                    "linear",
                    &["webhook"],
                    "LinearEventPayload",
                    &[],
                    hmac_signature_metadata(
                        "linear",
                        "Linear-Signature",
                        None,
                        Some("Linear-Delivery"),
                        Some(75),
                        "hex",
                    ),
                    vec![
                        required_secret("signing_secret", "linear"),
                        optional_secret("access_token", "linear"),
                    ],
                    ProviderRuntimeMetadata::Placeholder,
                );
                metadata.outbound_methods = vec![
                    outbound_method("list_issues"),
                    outbound_method("update_issue"),
                    outbound_method("create_comment"),
                    outbound_method("search"),
                    outbound_method("graphql"),
                ];
                metadata
            },
            normalize: linear_payload,
        }),
        Arc::new(BuiltinProviderSchema {
            provider_id: "notion",
            harn_schema_name: "NotionEventPayload",
            metadata: {
                let mut metadata = provider_metadata_entry(
                    "notion",
                    &["webhook", "poll"],
                    "NotionEventPayload",
                    &[],
                    hmac_signature_metadata(
                        "notion",
                        "X-Notion-Signature",
                        None,
                        None,
                        None,
                        "hex",
                    ),
                    vec![required_secret("verification_token", "notion")],
                    ProviderRuntimeMetadata::Placeholder,
                );
                metadata.outbound_methods = vec![
                    outbound_method("get_page"),
                    outbound_method("update_page"),
                    outbound_method("append_blocks"),
                    outbound_method("query_database"),
                    outbound_method("search"),
                    outbound_method("create_comment"),
                    outbound_method("api_call"),
                ];
                metadata
            },
            normalize: notion_payload,
        }),
        Arc::new(BuiltinProviderSchema {
            provider_id: "cron",
            harn_schema_name: "CronEventPayload",
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
            normalize: cron_payload,
        }),
        Arc::new(BuiltinProviderSchema {
            provider_id: "webhook",
            harn_schema_name: "GenericWebhookPayload",
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
            normalize: webhook_payload,
        }),
        Arc::new(BuiltinProviderSchema {
            provider_id: "a2a-push",
            harn_schema_name: "A2aPushPayload",
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
            normalize: a2a_push_payload,
        }),
        Arc::new(stream_provider_schema("kafka", kafka_payload)),
        Arc::new(stream_provider_schema("nats", nats_payload)),
        Arc::new(stream_provider_schema("pulsar", pulsar_payload)),
        Arc::new(stream_provider_schema("postgres-cdc", postgres_cdc_payload)),
        Arc::new(stream_provider_schema("email", email_payload)),
        Arc::new(stream_provider_schema("websocket", websocket_payload)),
    ]
}

fn stream_provider_schema(
    provider_id: &'static str,
    normalize: fn(&str, &BTreeMap<String, String>, JsonValue) -> ProviderPayload,
) -> BuiltinProviderSchema {
    BuiltinProviderSchema {
        provider_id,
        harn_schema_name: "StreamEventPayload",
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
        normalize,
    }
}
