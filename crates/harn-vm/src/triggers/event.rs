use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, OnceLock, RwLock};

use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use time::OffsetDateTime;
use uuid::Uuid;

const REDACTED_HEADER_VALUE: &str = "[redacted]";

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TriggerEventId(pub String);

impl TriggerEventId {
    pub fn new() -> Self {
        Self(format!("trigger_evt_{}", Uuid::now_v7()))
    }
}

impl Default for TriggerEventId {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProviderId(pub String);

impl ProviderId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl From<&str> for ProviderId {
    fn from(value: &str) -> Self {
        Self::new(value)
    }
}

impl From<String> for ProviderId {
    fn from(value: String) -> Self {
        Self::new(value)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TraceId(pub String);

impl TraceId {
    pub fn new() -> Self {
        Self(format!("trace_{}", Uuid::now_v7()))
    }
}

impl Default for TraceId {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TenantId(pub String);

impl TenantId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum SignatureStatus {
    Verified,
    Unsigned,
    Failed { reason: String },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GitHubEventPayload {
    pub event: String,
    pub action: Option<String>,
    pub delivery_id: Option<String>,
    pub installation_id: Option<i64>,
    pub raw: JsonValue,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SlackEventPayload {
    pub event: String,
    pub subtype: Option<String>,
    pub team_id: Option<String>,
    pub channel_id: Option<String>,
    pub raw: JsonValue,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LinearEventPayload {
    pub action: Option<String>,
    pub organization_id: Option<String>,
    pub webhook_timestamp: Option<String>,
    pub raw: JsonValue,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NotionEventPayload {
    pub event: String,
    pub workspace_id: Option<String>,
    pub request_id: Option<String>,
    pub raw: JsonValue,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CronEventPayload {
    pub cron_id: Option<String>,
    pub schedule: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub tick_at: OffsetDateTime,
    pub raw: JsonValue,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GenericWebhookPayload {
    pub source: Option<String>,
    pub content_type: Option<String>,
    pub raw: JsonValue,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct A2aPushPayload {
    pub task_id: Option<String>,
    pub sender: Option<String>,
    pub raw: JsonValue,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExtensionProviderPayload {
    pub provider: String,
    pub schema_name: String,
    pub raw: JsonValue,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ProviderPayload {
    Known(KnownProviderPayload),
    Extension(ExtensionProviderPayload),
}

impl ProviderPayload {
    pub fn provider(&self) -> &str {
        match self {
            Self::Known(known) => known.provider(),
            Self::Extension(payload) => payload.provider.as_str(),
        }
    }

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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "provider")]
pub enum KnownProviderPayload {
    #[serde(rename = "github")]
    GitHub(GitHubEventPayload),
    #[serde(rename = "slack")]
    Slack(SlackEventPayload),
    #[serde(rename = "linear")]
    Linear(LinearEventPayload),
    #[serde(rename = "notion")]
    Notion(NotionEventPayload),
    #[serde(rename = "cron")]
    Cron(CronEventPayload),
    #[serde(rename = "webhook")]
    Webhook(GenericWebhookPayload),
    #[serde(rename = "a2a-push")]
    A2aPush(A2aPushPayload),
}

impl KnownProviderPayload {
    pub fn provider(&self) -> &str {
        match self {
            Self::GitHub(_) => "github",
            Self::Slack(_) => "slack",
            Self::Linear(_) => "linear",
            Self::Notion(_) => "notion",
            Self::Cron(_) => "cron",
            Self::Webhook(_) => "webhook",
            Self::A2aPush(_) => "a2a-push",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TriggerEvent {
    pub id: TriggerEventId,
    pub provider: ProviderId,
    pub kind: String,
    #[serde(with = "time::serde::rfc3339")]
    pub received_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    pub occurred_at: Option<OffsetDateTime>,
    pub dedupe_key: String,
    pub trace_id: TraceId,
    pub tenant_id: Option<TenantId>,
    pub headers: BTreeMap<String, String>,
    pub provider_payload: ProviderPayload,
    pub signature_status: SignatureStatus,
}

impl TriggerEvent {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        provider: ProviderId,
        kind: impl Into<String>,
        occurred_at: Option<OffsetDateTime>,
        dedupe_key: impl Into<String>,
        tenant_id: Option<TenantId>,
        headers: BTreeMap<String, String>,
        provider_payload: ProviderPayload,
        signature_status: SignatureStatus,
    ) -> Self {
        Self {
            id: TriggerEventId::new(),
            provider,
            kind: kind.into(),
            received_at: OffsetDateTime::now_utc(),
            occurred_at,
            dedupe_key: dedupe_key.into(),
            trace_id: TraceId::new(),
            tenant_id,
            headers,
            provider_payload,
            signature_status,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeaderRedactionPolicy {
    safe_exact_names: BTreeSet<String>,
}

impl HeaderRedactionPolicy {
    pub fn with_safe_header(mut self, name: impl Into<String>) -> Self {
        self.safe_exact_names
            .insert(name.into().to_ascii_lowercase());
        self
    }

    fn should_keep(&self, name: &str) -> bool {
        let lower = name.to_ascii_lowercase();
        if self.safe_exact_names.contains(lower.as_str()) {
            return true;
        }
        matches!(
            lower.as_str(),
            "user-agent"
                | "request-id"
                | "x-request-id"
                | "x-correlation-id"
                | "content-type"
                | "content-length"
                | "x-github-event"
                | "x-github-delivery"
                | "x-github-hook-id"
                | "x-hub-signature-256"
                | "x-slack-request-timestamp"
                | "x-slack-signature"
                | "x-linear-signature"
                | "x-notion-signature"
                | "x-a2a-signature"
                | "x-a2a-delivery"
        ) || lower.ends_with("-event")
            || lower.ends_with("-delivery")
            || lower.contains("timestamp")
            || lower.contains("request-id")
    }

    fn should_redact(&self, name: &str) -> bool {
        let lower = name.to_ascii_lowercase();
        if self.should_keep(lower.as_str()) {
            return false;
        }
        lower.contains("authorization")
            || lower.contains("cookie")
            || lower.contains("secret")
            || lower.contains("token")
            || lower.contains("key")
    }
}

impl Default for HeaderRedactionPolicy {
    fn default() -> Self {
        Self {
            safe_exact_names: BTreeSet::from([
                "content-length".to_string(),
                "content-type".to_string(),
                "request-id".to_string(),
                "user-agent".to_string(),
                "x-a2a-delivery".to_string(),
                "x-a2a-signature".to_string(),
                "x-correlation-id".to_string(),
                "x-github-delivery".to_string(),
                "x-github-event".to_string(),
                "x-github-hook-id".to_string(),
                "x-hub-signature-256".to_string(),
                "x-linear-signature".to_string(),
                "x-notion-signature".to_string(),
                "x-request-id".to_string(),
                "x-slack-request-timestamp".to_string(),
                "x-slack-signature".to_string(),
            ]),
        }
    }
}

pub fn redact_headers(
    headers: &BTreeMap<String, String>,
    policy: &HeaderRedactionPolicy,
) -> BTreeMap<String, String> {
    headers
        .iter()
        .map(|(name, value)| {
            if policy.should_redact(name) {
                (name.clone(), REDACTED_HEADER_VALUE.to_string())
            } else {
                (name.clone(), value.clone())
            }
        })
        .collect()
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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
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
    fn provider_id(&self) -> &'static str;
    fn harn_schema_name(&self) -> &'static str;
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

pub fn register_provider_schema(
    schema: Arc<dyn ProviderSchema>,
) -> Result<(), ProviderCatalogError> {
    provider_catalog()
        .write()
        .expect("provider catalog poisoned")
        .register(schema)
}

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

struct BuiltinProviderSchema {
    provider_id: &'static str,
    harn_schema_name: &'static str,
    metadata: ProviderMetadata,
    normalize: fn(&str, &BTreeMap<String, String>, JsonValue) -> ProviderPayload,
}

impl ProviderSchema for BuiltinProviderSchema {
    fn provider_id(&self) -> &'static str {
        self.provider_id
    }

    fn harn_schema_name(&self) -> &'static str {
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
    signature_verification: SignatureVerificationMetadata,
    secret_requirements: Vec<ProviderSecretRequirement>,
    runtime: ProviderRuntimeMetadata,
) -> ProviderMetadata {
    ProviderMetadata {
        provider: provider.to_string(),
        kinds: kinds.iter().map(|kind| kind.to_string()).collect(),
        schema_name: schema_name.to_string(),
        outbound_methods: Vec::new(),
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

fn default_provider_schemas() -> Vec<Arc<dyn ProviderSchema>> {
    vec![
        Arc::new(BuiltinProviderSchema {
            provider_id: "github",
            harn_schema_name: "GitHubEventPayload",
            metadata: provider_metadata_entry(
                "github",
                &["webhook"],
                "GitHubEventPayload",
                hmac_signature_metadata(
                    "github",
                    "X-Hub-Signature-256",
                    None,
                    Some("X-GitHub-Delivery"),
                    None,
                    "hex",
                ),
                vec![required_secret("signing_secret", "github")],
                ProviderRuntimeMetadata::Builtin {
                    connector: "webhook".to_string(),
                    default_signature_variant: Some("github".to_string()),
                },
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
                SignatureVerificationMetadata::None,
                Vec::new(),
                ProviderRuntimeMetadata::Placeholder,
            ),
            normalize: slack_payload,
        }),
        Arc::new(BuiltinProviderSchema {
            provider_id: "linear",
            harn_schema_name: "LinearEventPayload",
            metadata: provider_metadata_entry(
                "linear",
                &["webhook"],
                "LinearEventPayload",
                SignatureVerificationMetadata::None,
                Vec::new(),
                ProviderRuntimeMetadata::Placeholder,
            ),
            normalize: linear_payload,
        }),
        Arc::new(BuiltinProviderSchema {
            provider_id: "notion",
            harn_schema_name: "NotionEventPayload",
            metadata: provider_metadata_entry(
                "notion",
                &["webhook"],
                "NotionEventPayload",
                SignatureVerificationMetadata::None,
                Vec::new(),
                ProviderRuntimeMetadata::Placeholder,
            ),
            normalize: notion_payload,
        }),
        Arc::new(BuiltinProviderSchema {
            provider_id: "cron",
            harn_schema_name: "CronEventPayload",
            metadata: provider_metadata_entry(
                "cron",
                &["cron"],
                "CronEventPayload",
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
                SignatureVerificationMetadata::None,
                Vec::new(),
                ProviderRuntimeMetadata::Placeholder,
            ),
            normalize: a2a_push_payload,
        }),
    ]
}

fn github_payload(
    kind: &str,
    headers: &BTreeMap<String, String>,
    raw: JsonValue,
) -> ProviderPayload {
    let action = raw
        .get("action")
        .and_then(JsonValue::as_str)
        .map(ToString::to_string);
    let installation_id = raw
        .get("installation")
        .and_then(|value| value.get("id"))
        .and_then(JsonValue::as_i64);
    ProviderPayload::Known(KnownProviderPayload::GitHub(GitHubEventPayload {
        event: kind.to_string(),
        action,
        delivery_id: headers.get("X-GitHub-Delivery").cloned(),
        installation_id,
        raw,
    }))
}

fn slack_payload(
    kind: &str,
    _headers: &BTreeMap<String, String>,
    raw: JsonValue,
) -> ProviderPayload {
    let subtype = raw
        .get("event")
        .and_then(|value| value.get("subtype"))
        .and_then(JsonValue::as_str)
        .map(ToString::to_string);
    let team_id = raw
        .get("team_id")
        .and_then(JsonValue::as_str)
        .map(ToString::to_string);
    let channel_id = raw
        .get("event")
        .and_then(|value| value.get("channel"))
        .and_then(JsonValue::as_str)
        .map(ToString::to_string);
    ProviderPayload::Known(KnownProviderPayload::Slack(SlackEventPayload {
        event: kind.to_string(),
        subtype,
        team_id,
        channel_id,
        raw,
    }))
}

fn linear_payload(
    _kind: &str,
    headers: &BTreeMap<String, String>,
    raw: JsonValue,
) -> ProviderPayload {
    let action = raw
        .get("action")
        .and_then(JsonValue::as_str)
        .map(ToString::to_string);
    let organization_id = raw
        .get("organizationId")
        .and_then(JsonValue::as_str)
        .map(ToString::to_string);
    ProviderPayload::Known(KnownProviderPayload::Linear(LinearEventPayload {
        action,
        organization_id,
        webhook_timestamp: headers.get("Linear-Request-Timestamp").cloned(),
        raw,
    }))
}

fn notion_payload(
    kind: &str,
    headers: &BTreeMap<String, String>,
    raw: JsonValue,
) -> ProviderPayload {
    let workspace_id = raw
        .get("workspace_id")
        .and_then(JsonValue::as_str)
        .map(ToString::to_string);
    ProviderPayload::Known(KnownProviderPayload::Notion(NotionEventPayload {
        event: kind.to_string(),
        workspace_id,
        request_id: headers
            .get("request-id")
            .cloned()
            .or_else(|| headers.get("x-request-id").cloned()),
        raw,
    }))
}

fn cron_payload(
    _kind: &str,
    _headers: &BTreeMap<String, String>,
    raw: JsonValue,
) -> ProviderPayload {
    let cron_id = raw
        .get("cron_id")
        .and_then(JsonValue::as_str)
        .map(ToString::to_string);
    let schedule = raw
        .get("schedule")
        .and_then(JsonValue::as_str)
        .map(ToString::to_string);
    let tick_at = raw
        .get("tick_at")
        .and_then(JsonValue::as_str)
        .and_then(parse_rfc3339)
        .unwrap_or_else(OffsetDateTime::now_utc);
    ProviderPayload::Known(KnownProviderPayload::Cron(CronEventPayload {
        cron_id,
        schedule,
        tick_at,
        raw,
    }))
}

fn webhook_payload(
    _kind: &str,
    headers: &BTreeMap<String, String>,
    raw: JsonValue,
) -> ProviderPayload {
    ProviderPayload::Known(KnownProviderPayload::Webhook(GenericWebhookPayload {
        source: headers.get("X-Webhook-Source").cloned(),
        content_type: headers.get("Content-Type").cloned(),
        raw,
    }))
}

fn a2a_push_payload(
    _kind: &str,
    _headers: &BTreeMap<String, String>,
    raw: JsonValue,
) -> ProviderPayload {
    let task_id = raw
        .get("task_id")
        .and_then(JsonValue::as_str)
        .map(ToString::to_string);
    let sender = raw
        .get("sender")
        .and_then(JsonValue::as_str)
        .map(ToString::to_string);
    ProviderPayload::Known(KnownProviderPayload::A2aPush(A2aPushPayload {
        task_id,
        sender,
        raw,
    }))
}

fn parse_rfc3339(text: &str) -> Option<OffsetDateTime> {
    OffsetDateTime::parse(text, &time::format_description::well_known::Rfc3339).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_headers() -> BTreeMap<String, String> {
        BTreeMap::from([
            ("Authorization".to_string(), "Bearer secret".to_string()),
            ("Cookie".to_string(), "session=abc".to_string()),
            ("User-Agent".to_string(), "GitHub-Hookshot/123".to_string()),
            ("X-GitHub-Delivery".to_string(), "delivery-123".to_string()),
            ("X-GitHub-Event".to_string(), "issues".to_string()),
            ("X-Webhook-Token".to_string(), "token".to_string()),
        ])
    }

    #[test]
    fn default_redaction_policy_keeps_safe_headers() {
        let redacted = redact_headers(&sample_headers(), &HeaderRedactionPolicy::default());
        assert_eq!(redacted.get("User-Agent").unwrap(), "GitHub-Hookshot/123");
        assert_eq!(redacted.get("X-GitHub-Delivery").unwrap(), "delivery-123");
        assert_eq!(
            redacted.get("Authorization").unwrap(),
            REDACTED_HEADER_VALUE
        );
        assert_eq!(redacted.get("Cookie").unwrap(), REDACTED_HEADER_VALUE);
        assert_eq!(
            redacted.get("X-Webhook-Token").unwrap(),
            REDACTED_HEADER_VALUE
        );
    }

    #[test]
    fn provider_catalog_rejects_duplicates() {
        let mut catalog = ProviderCatalog::default();
        catalog
            .register(Arc::new(BuiltinProviderSchema {
                provider_id: "github",
                harn_schema_name: "GitHubEventPayload",
                metadata: provider_metadata_entry(
                    "github",
                    &["webhook"],
                    "GitHubEventPayload",
                    SignatureVerificationMetadata::None,
                    Vec::new(),
                    ProviderRuntimeMetadata::Placeholder,
                ),
                normalize: github_payload,
            }))
            .unwrap();
        let error = catalog
            .register(Arc::new(BuiltinProviderSchema {
                provider_id: "github",
                harn_schema_name: "GitHubEventPayload",
                metadata: provider_metadata_entry(
                    "github",
                    &["webhook"],
                    "GitHubEventPayload",
                    SignatureVerificationMetadata::None,
                    Vec::new(),
                    ProviderRuntimeMetadata::Placeholder,
                ),
                normalize: github_payload,
            }))
            .unwrap_err();
        assert_eq!(
            error,
            ProviderCatalogError::DuplicateProvider("github".to_string())
        );
    }

    #[test]
    fn registered_provider_metadata_marks_builtin_connectors() {
        let entries = registered_provider_metadata();
        let builtin: Vec<&ProviderMetadata> = entries
            .iter()
            .filter(|entry| matches!(entry.runtime, ProviderRuntimeMetadata::Builtin { .. }))
            .collect();

        assert_eq!(builtin.len(), 3);
        assert!(builtin.iter().any(|entry| entry.provider == "cron"));
        assert!(builtin.iter().any(|entry| entry.provider == "github"));
        assert!(builtin.iter().any(|entry| entry.provider == "webhook"));
    }

    #[test]
    fn trigger_event_round_trip_is_stable() {
        let provider = ProviderId::from("github");
        let headers = redact_headers(&sample_headers(), &HeaderRedactionPolicy::default());
        let payload = ProviderPayload::normalize(
            &provider,
            "issues",
            &sample_headers(),
            serde_json::json!({
                "action": "opened",
                "installation": {"id": 42},
                "issue": {"number": 99}
            }),
        )
        .unwrap();
        let event = TriggerEvent {
            id: TriggerEventId("trigger_evt_fixed".to_string()),
            provider,
            kind: "issues".to_string(),
            received_at: parse_rfc3339("2026-04-19T07:00:00Z").unwrap(),
            occurred_at: Some(parse_rfc3339("2026-04-19T06:59:59Z").unwrap()),
            dedupe_key: "delivery-123".to_string(),
            trace_id: TraceId("trace_fixed".to_string()),
            tenant_id: Some(TenantId("tenant_1".to_string())),
            headers,
            provider_payload: payload,
            signature_status: SignatureStatus::Verified,
        };

        let once = serde_json::to_value(&event).unwrap();
        let decoded: TriggerEvent = serde_json::from_value(once.clone()).unwrap();
        let twice = serde_json::to_value(&decoded).unwrap();
        assert_eq!(decoded, event);
        assert_eq!(once, twice);
    }

    #[test]
    fn unknown_provider_errors() {
        let error = ProviderPayload::normalize(
            &ProviderId::from("custom-provider"),
            "thing.happened",
            &BTreeMap::new(),
            serde_json::json!({"ok": true}),
        )
        .unwrap_err();
        assert_eq!(
            error,
            ProviderCatalogError::UnknownProvider("custom-provider".to_string())
        );
    }
}
