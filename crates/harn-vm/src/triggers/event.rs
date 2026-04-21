use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, OnceLock, RwLock};

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value as JsonValue;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::triggers::test_util::clock;

const REDACTED_HEADER_VALUE: &str = "[redacted]";

fn serialize_optional_bytes_b64<S>(
    value: &Option<Vec<u8>>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    use base64::Engine;

    match value {
        Some(bytes) => {
            serializer.serialize_some(&base64::engine::general_purpose::STANDARD.encode(bytes))
        }
        None => serializer.serialize_none(),
    }
}

fn deserialize_optional_bytes_b64<'de, D>(deserializer: D) -> Result<Option<Vec<u8>>, D::Error>
where
    D: Deserializer<'de>,
{
    use base64::Engine;

    let encoded = Option::<String>::deserialize(deserializer)?;
    encoded
        .map(|text| {
            base64::engine::general_purpose::STANDARD
                .decode(text.as_bytes())
                .map_err(serde::de::Error::custom)
        })
        .transpose()
}

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
pub struct GitHubEventCommon {
    pub event: String,
    pub action: Option<String>,
    pub delivery_id: Option<String>,
    pub installation_id: Option<i64>,
    pub raw: JsonValue,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GitHubIssuesEventPayload {
    #[serde(flatten)]
    pub common: GitHubEventCommon,
    pub issue: JsonValue,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GitHubPullRequestEventPayload {
    #[serde(flatten)]
    pub common: GitHubEventCommon,
    pub pull_request: JsonValue,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GitHubIssueCommentEventPayload {
    #[serde(flatten)]
    pub common: GitHubEventCommon,
    pub issue: JsonValue,
    pub comment: JsonValue,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GitHubPullRequestReviewEventPayload {
    #[serde(flatten)]
    pub common: GitHubEventCommon,
    pub pull_request: JsonValue,
    pub review: JsonValue,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GitHubPushEventPayload {
    #[serde(flatten)]
    pub common: GitHubEventCommon,
    #[serde(default)]
    pub commits: Vec<JsonValue>,
    pub distinct_size: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GitHubWorkflowRunEventPayload {
    #[serde(flatten)]
    pub common: GitHubEventCommon,
    pub workflow_run: JsonValue,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GitHubDeploymentStatusEventPayload {
    #[serde(flatten)]
    pub common: GitHubEventCommon,
    pub deployment_status: JsonValue,
    pub deployment: JsonValue,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GitHubCheckRunEventPayload {
    #[serde(flatten)]
    pub common: GitHubEventCommon,
    pub check_run: JsonValue,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum GitHubEventPayload {
    Issues(GitHubIssuesEventPayload),
    PullRequest(GitHubPullRequestEventPayload),
    IssueComment(GitHubIssueCommentEventPayload),
    PullRequestReview(GitHubPullRequestReviewEventPayload),
    Push(GitHubPushEventPayload),
    WorkflowRun(GitHubWorkflowRunEventPayload),
    DeploymentStatus(GitHubDeploymentStatusEventPayload),
    CheckRun(GitHubCheckRunEventPayload),
    Other(GitHubEventCommon),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SlackEventCommon {
    pub event: String,
    pub event_id: Option<String>,
    pub api_app_id: Option<String>,
    pub team_id: Option<String>,
    pub channel_id: Option<String>,
    pub user_id: Option<String>,
    pub event_ts: Option<String>,
    pub raw: JsonValue,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SlackMessageEventPayload {
    #[serde(flatten)]
    pub common: SlackEventCommon,
    pub subtype: Option<String>,
    pub channel_type: Option<String>,
    pub channel: Option<String>,
    pub user: Option<String>,
    pub text: Option<String>,
    pub ts: Option<String>,
    pub thread_ts: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SlackAppMentionEventPayload {
    #[serde(flatten)]
    pub common: SlackEventCommon,
    pub channel: Option<String>,
    pub user: Option<String>,
    pub text: Option<String>,
    pub ts: Option<String>,
    pub thread_ts: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SlackReactionAddedEventPayload {
    #[serde(flatten)]
    pub common: SlackEventCommon,
    pub reaction: Option<String>,
    pub item_user: Option<String>,
    pub item: JsonValue,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SlackAppHomeOpenedEventPayload {
    #[serde(flatten)]
    pub common: SlackEventCommon,
    pub user: Option<String>,
    pub channel: Option<String>,
    pub tab: Option<String>,
    pub view: JsonValue,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SlackAssistantThreadStartedEventPayload {
    #[serde(flatten)]
    pub common: SlackEventCommon,
    pub assistant_thread: JsonValue,
    pub thread_ts: Option<String>,
    pub context: JsonValue,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SlackEventPayload {
    Message(SlackMessageEventPayload),
    AppMention(SlackAppMentionEventPayload),
    ReactionAdded(SlackReactionAddedEventPayload),
    AppHomeOpened(SlackAppHomeOpenedEventPayload),
    AssistantThreadStarted(SlackAssistantThreadStartedEventPayload),
    Other(SlackEventCommon),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LinearEventCommon {
    pub event: String,
    pub action: Option<String>,
    pub delivery_id: Option<String>,
    pub organization_id: Option<String>,
    pub webhook_timestamp: Option<i64>,
    pub webhook_id: Option<String>,
    pub url: Option<String>,
    pub created_at: Option<String>,
    pub actor: JsonValue,
    pub raw: JsonValue,
}

#[derive(Clone, Debug, PartialEq)]
pub enum LinearIssueChange {
    Title { previous: Option<String> },
    Description { previous: Option<String> },
    Priority { previous: Option<i64> },
    Estimate { previous: Option<i64> },
    StateId { previous: Option<String> },
    TeamId { previous: Option<String> },
    AssigneeId { previous: Option<String> },
    ProjectId { previous: Option<String> },
    CycleId { previous: Option<String> },
    DueDate { previous: Option<String> },
    ParentId { previous: Option<String> },
    SortOrder { previous: Option<f64> },
    LabelIds { previous: Vec<String> },
    CompletedAt { previous: Option<String> },
    Other { field: String, previous: JsonValue },
}

impl Serialize for LinearIssueChange {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let value = match self {
            Self::Title { previous } => {
                serde_json::json!({ "field_name": "title", "previous": previous })
            }
            Self::Description { previous } => {
                serde_json::json!({ "field_name": "description", "previous": previous })
            }
            Self::Priority { previous } => {
                serde_json::json!({ "field_name": "priority", "previous": previous })
            }
            Self::Estimate { previous } => {
                serde_json::json!({ "field_name": "estimate", "previous": previous })
            }
            Self::StateId { previous } => {
                serde_json::json!({ "field_name": "state_id", "previous": previous })
            }
            Self::TeamId { previous } => {
                serde_json::json!({ "field_name": "team_id", "previous": previous })
            }
            Self::AssigneeId { previous } => {
                serde_json::json!({ "field_name": "assignee_id", "previous": previous })
            }
            Self::ProjectId { previous } => {
                serde_json::json!({ "field_name": "project_id", "previous": previous })
            }
            Self::CycleId { previous } => {
                serde_json::json!({ "field_name": "cycle_id", "previous": previous })
            }
            Self::DueDate { previous } => {
                serde_json::json!({ "field_name": "due_date", "previous": previous })
            }
            Self::ParentId { previous } => {
                serde_json::json!({ "field_name": "parent_id", "previous": previous })
            }
            Self::SortOrder { previous } => {
                serde_json::json!({ "field_name": "sort_order", "previous": previous })
            }
            Self::LabelIds { previous } => {
                serde_json::json!({ "field_name": "label_ids", "previous": previous })
            }
            Self::CompletedAt { previous } => {
                serde_json::json!({ "field_name": "completed_at", "previous": previous })
            }
            Self::Other { field, previous } => {
                serde_json::json!({ "field_name": "other", "field": field, "previous": previous })
            }
        };
        value.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for LinearIssueChange {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = JsonValue::deserialize(deserializer)?;
        let field_name = value
            .get("field_name")
            .and_then(JsonValue::as_str)
            .ok_or_else(|| serde::de::Error::custom("linear issue change missing field_name"))?;
        let previous = value.get("previous").cloned().unwrap_or(JsonValue::Null);
        Ok(match field_name {
            "title" => Self::Title {
                previous: previous.as_str().map(ToString::to_string),
            },
            "description" => Self::Description {
                previous: previous.as_str().map(ToString::to_string),
            },
            "priority" => Self::Priority {
                previous: parse_json_i64ish(&previous),
            },
            "estimate" => Self::Estimate {
                previous: parse_json_i64ish(&previous),
            },
            "state_id" => Self::StateId {
                previous: previous.as_str().map(ToString::to_string),
            },
            "team_id" => Self::TeamId {
                previous: previous.as_str().map(ToString::to_string),
            },
            "assignee_id" => Self::AssigneeId {
                previous: previous.as_str().map(ToString::to_string),
            },
            "project_id" => Self::ProjectId {
                previous: previous.as_str().map(ToString::to_string),
            },
            "cycle_id" => Self::CycleId {
                previous: previous.as_str().map(ToString::to_string),
            },
            "due_date" => Self::DueDate {
                previous: previous.as_str().map(ToString::to_string),
            },
            "parent_id" => Self::ParentId {
                previous: previous.as_str().map(ToString::to_string),
            },
            "sort_order" => Self::SortOrder {
                previous: previous.as_f64(),
            },
            "label_ids" => Self::LabelIds {
                previous: parse_string_array(&previous),
            },
            "completed_at" => Self::CompletedAt {
                previous: previous.as_str().map(ToString::to_string),
            },
            "other" => Self::Other {
                field: value
                    .get("field")
                    .and_then(JsonValue::as_str)
                    .map(ToString::to_string)
                    .unwrap_or_else(|| "unknown".to_string()),
                previous,
            },
            other => Self::Other {
                field: other.to_string(),
                previous,
            },
        })
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LinearIssueEventPayload {
    #[serde(flatten)]
    pub common: LinearEventCommon,
    pub issue: JsonValue,
    #[serde(default)]
    pub changes: Vec<LinearIssueChange>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LinearIssueCommentEventPayload {
    #[serde(flatten)]
    pub common: LinearEventCommon,
    pub comment: JsonValue,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LinearIssueLabelEventPayload {
    #[serde(flatten)]
    pub common: LinearEventCommon,
    pub label: JsonValue,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LinearProjectEventPayload {
    #[serde(flatten)]
    pub common: LinearEventCommon,
    pub project: JsonValue,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LinearCycleEventPayload {
    #[serde(flatten)]
    pub common: LinearEventCommon,
    pub cycle: JsonValue,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LinearCustomerEventPayload {
    #[serde(flatten)]
    pub common: LinearEventCommon,
    pub customer: JsonValue,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LinearCustomerRequestEventPayload {
    #[serde(flatten)]
    pub common: LinearEventCommon,
    pub customer_request: JsonValue,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum LinearEventPayload {
    Issue(LinearIssueEventPayload),
    IssueComment(LinearIssueCommentEventPayload),
    IssueLabel(LinearIssueLabelEventPayload),
    Project(LinearProjectEventPayload),
    Cycle(LinearCycleEventPayload),
    Customer(LinearCustomerEventPayload),
    CustomerRequest(LinearCustomerRequestEventPayload),
    Other(LinearEventCommon),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NotionPolledChangeEvent {
    pub resource: String,
    pub source_id: String,
    pub entity_id: String,
    pub high_water_mark: String,
    pub before: Option<JsonValue>,
    pub after: JsonValue,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NotionEventPayload {
    pub event: String,
    pub workspace_id: Option<String>,
    pub request_id: Option<String>,
    pub subscription_id: Option<String>,
    pub integration_id: Option<String>,
    pub attempt_number: Option<u32>,
    pub entity_id: Option<String>,
    pub entity_type: Option<String>,
    pub api_version: Option<String>,
    pub verification_token: Option<String>,
    pub polled: Option<NotionPolledChangeEvent>,
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

#[allow(clippy::large_enum_variant)]
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
    Slack(Box<SlackEventPayload>),
    #[serde(rename = "linear")]
    Linear(LinearEventPayload),
    #[serde(rename = "notion")]
    Notion(Box<NotionEventPayload>),
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub batch: Option<Vec<JsonValue>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        serialize_with = "serialize_optional_bytes_b64",
        deserialize_with = "deserialize_optional_bytes_b64"
    )]
    pub raw_body: Option<Vec<u8>>,
    pub provider_payload: ProviderPayload,
    pub signature_status: SignatureStatus,
    #[serde(skip)]
    pub dedupe_claimed: bool,
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
            received_at: clock::now_utc(),
            occurred_at,
            dedupe_key: dedupe_key.into(),
            trace_id: TraceId::new(),
            tenant_id,
            headers,
            batch: None,
            raw_body: None,
            provider_payload,
            signature_status,
            dedupe_claimed: false,
        }
    }

    pub fn dedupe_claimed(&self) -> bool {
        self.dedupe_claimed
    }

    pub fn mark_dedupe_claimed(&mut self) {
        self.dedupe_claimed = true;
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
                &[],
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
                ProviderRuntimeMetadata::Builtin {
                    connector: "slack".to_string(),
                    default_signature_variant: Some("slack".to_string()),
                },
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
                    ProviderRuntimeMetadata::Builtin {
                        connector: "linear".to_string(),
                        default_signature_variant: Some("linear".to_string()),
                    },
                );
                metadata.outbound_methods = vec![
                    ProviderOutboundMethod {
                        name: "list_issues".to_string(),
                    },
                    ProviderOutboundMethod {
                        name: "update_issue".to_string(),
                    },
                    ProviderOutboundMethod {
                        name: "create_comment".to_string(),
                    },
                    ProviderOutboundMethod {
                        name: "search".to_string(),
                    },
                    ProviderOutboundMethod {
                        name: "graphql".to_string(),
                    },
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
                    ProviderRuntimeMetadata::Builtin {
                        connector: "notion".to_string(),
                        default_signature_variant: Some("notion".to_string()),
                    },
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
    let common = GitHubEventCommon {
        event: kind.to_string(),
        action: raw
            .get("action")
            .and_then(JsonValue::as_str)
            .map(ToString::to_string),
        delivery_id: headers.get("X-GitHub-Delivery").cloned(),
        installation_id: raw
            .get("installation")
            .and_then(|value| value.get("id"))
            .and_then(JsonValue::as_i64),
        raw: raw.clone(),
    };
    let payload = match kind {
        "issues" => GitHubEventPayload::Issues(GitHubIssuesEventPayload {
            common,
            issue: raw.get("issue").cloned().unwrap_or(JsonValue::Null),
        }),
        "pull_request" => GitHubEventPayload::PullRequest(GitHubPullRequestEventPayload {
            common,
            pull_request: raw.get("pull_request").cloned().unwrap_or(JsonValue::Null),
        }),
        "issue_comment" => GitHubEventPayload::IssueComment(GitHubIssueCommentEventPayload {
            common,
            issue: raw.get("issue").cloned().unwrap_or(JsonValue::Null),
            comment: raw.get("comment").cloned().unwrap_or(JsonValue::Null),
        }),
        "pull_request_review" => {
            GitHubEventPayload::PullRequestReview(GitHubPullRequestReviewEventPayload {
                common,
                pull_request: raw.get("pull_request").cloned().unwrap_or(JsonValue::Null),
                review: raw.get("review").cloned().unwrap_or(JsonValue::Null),
            })
        }
        "push" => GitHubEventPayload::Push(GitHubPushEventPayload {
            common,
            commits: raw
                .get("commits")
                .and_then(JsonValue::as_array)
                .cloned()
                .unwrap_or_default(),
            distinct_size: raw.get("distinct_size").and_then(JsonValue::as_i64),
        }),
        "workflow_run" => GitHubEventPayload::WorkflowRun(GitHubWorkflowRunEventPayload {
            common,
            workflow_run: raw.get("workflow_run").cloned().unwrap_or(JsonValue::Null),
        }),
        "deployment_status" => {
            GitHubEventPayload::DeploymentStatus(GitHubDeploymentStatusEventPayload {
                common,
                deployment_status: raw
                    .get("deployment_status")
                    .cloned()
                    .unwrap_or(JsonValue::Null),
                deployment: raw.get("deployment").cloned().unwrap_or(JsonValue::Null),
            })
        }
        "check_run" => GitHubEventPayload::CheckRun(GitHubCheckRunEventPayload {
            common,
            check_run: raw.get("check_run").cloned().unwrap_or(JsonValue::Null),
        }),
        _ => GitHubEventPayload::Other(common),
    };
    ProviderPayload::Known(KnownProviderPayload::GitHub(payload))
}

fn slack_payload(
    kind: &str,
    _headers: &BTreeMap<String, String>,
    raw: JsonValue,
) -> ProviderPayload {
    let event = raw.get("event");
    let common = SlackEventCommon {
        event: kind.to_string(),
        event_id: raw
            .get("event_id")
            .and_then(JsonValue::as_str)
            .map(ToString::to_string),
        api_app_id: raw
            .get("api_app_id")
            .and_then(JsonValue::as_str)
            .map(ToString::to_string),
        team_id: raw
            .get("team_id")
            .and_then(JsonValue::as_str)
            .map(ToString::to_string),
        channel_id: slack_channel_id(event),
        user_id: slack_user_id(event),
        event_ts: event
            .and_then(|value| value.get("event_ts"))
            .and_then(JsonValue::as_str)
            .map(ToString::to_string),
        raw: raw.clone(),
    };
    let payload = match kind {
        kind if kind == "message" || kind.starts_with("message.") => {
            SlackEventPayload::Message(SlackMessageEventPayload {
                subtype: event
                    .and_then(|value| value.get("subtype"))
                    .and_then(JsonValue::as_str)
                    .map(ToString::to_string),
                channel_type: event
                    .and_then(|value| value.get("channel_type"))
                    .and_then(JsonValue::as_str)
                    .map(ToString::to_string),
                channel: event
                    .and_then(|value| value.get("channel"))
                    .and_then(JsonValue::as_str)
                    .map(ToString::to_string),
                user: event
                    .and_then(|value| value.get("user"))
                    .and_then(JsonValue::as_str)
                    .map(ToString::to_string),
                text: event
                    .and_then(|value| value.get("text"))
                    .and_then(JsonValue::as_str)
                    .map(ToString::to_string),
                ts: event
                    .and_then(|value| value.get("ts"))
                    .and_then(JsonValue::as_str)
                    .map(ToString::to_string),
                thread_ts: event
                    .and_then(|value| value.get("thread_ts"))
                    .and_then(JsonValue::as_str)
                    .map(ToString::to_string),
                common,
            })
        }
        "app_mention" => SlackEventPayload::AppMention(SlackAppMentionEventPayload {
            channel: event
                .and_then(|value| value.get("channel"))
                .and_then(JsonValue::as_str)
                .map(ToString::to_string),
            user: event
                .and_then(|value| value.get("user"))
                .and_then(JsonValue::as_str)
                .map(ToString::to_string),
            text: event
                .and_then(|value| value.get("text"))
                .and_then(JsonValue::as_str)
                .map(ToString::to_string),
            ts: event
                .and_then(|value| value.get("ts"))
                .and_then(JsonValue::as_str)
                .map(ToString::to_string),
            thread_ts: event
                .and_then(|value| value.get("thread_ts"))
                .and_then(JsonValue::as_str)
                .map(ToString::to_string),
            common,
        }),
        "reaction_added" => SlackEventPayload::ReactionAdded(SlackReactionAddedEventPayload {
            reaction: event
                .and_then(|value| value.get("reaction"))
                .and_then(JsonValue::as_str)
                .map(ToString::to_string),
            item_user: event
                .and_then(|value| value.get("item_user"))
                .and_then(JsonValue::as_str)
                .map(ToString::to_string),
            item: event
                .and_then(|value| value.get("item"))
                .cloned()
                .unwrap_or(JsonValue::Null),
            common,
        }),
        "app_home_opened" => SlackEventPayload::AppHomeOpened(SlackAppHomeOpenedEventPayload {
            user: event
                .and_then(|value| value.get("user"))
                .and_then(JsonValue::as_str)
                .map(ToString::to_string),
            channel: event
                .and_then(|value| value.get("channel"))
                .and_then(JsonValue::as_str)
                .map(ToString::to_string),
            tab: event
                .and_then(|value| value.get("tab"))
                .and_then(JsonValue::as_str)
                .map(ToString::to_string),
            view: event
                .and_then(|value| value.get("view"))
                .cloned()
                .unwrap_or(JsonValue::Null),
            common,
        }),
        "assistant_thread_started" => {
            let assistant_thread = event
                .and_then(|value| value.get("assistant_thread"))
                .cloned()
                .unwrap_or(JsonValue::Null);
            SlackEventPayload::AssistantThreadStarted(SlackAssistantThreadStartedEventPayload {
                thread_ts: assistant_thread
                    .get("thread_ts")
                    .and_then(JsonValue::as_str)
                    .map(ToString::to_string),
                context: assistant_thread
                    .get("context")
                    .cloned()
                    .unwrap_or(JsonValue::Null),
                assistant_thread,
                common,
            })
        }
        _ => SlackEventPayload::Other(common),
    };
    ProviderPayload::Known(KnownProviderPayload::Slack(Box::new(payload)))
}

fn slack_channel_id(event: Option<&JsonValue>) -> Option<String> {
    event
        .and_then(|value| value.get("channel"))
        .and_then(JsonValue::as_str)
        .map(ToString::to_string)
        .or_else(|| {
            event
                .and_then(|value| value.get("item"))
                .and_then(|value| value.get("channel"))
                .and_then(JsonValue::as_str)
                .map(ToString::to_string)
        })
        .or_else(|| {
            event
                .and_then(|value| value.get("channel"))
                .and_then(|value| value.get("id"))
                .and_then(JsonValue::as_str)
                .map(ToString::to_string)
        })
        .or_else(|| {
            event
                .and_then(|value| value.get("assistant_thread"))
                .and_then(|value| value.get("channel_id"))
                .and_then(JsonValue::as_str)
                .map(ToString::to_string)
        })
}

fn slack_user_id(event: Option<&JsonValue>) -> Option<String> {
    event
        .and_then(|value| value.get("user"))
        .and_then(JsonValue::as_str)
        .map(ToString::to_string)
        .or_else(|| {
            event
                .and_then(|value| value.get("user"))
                .and_then(|value| value.get("id"))
                .and_then(JsonValue::as_str)
                .map(ToString::to_string)
        })
        .or_else(|| {
            event
                .and_then(|value| value.get("item_user"))
                .and_then(JsonValue::as_str)
                .map(ToString::to_string)
        })
        .or_else(|| {
            event
                .and_then(|value| value.get("assistant_thread"))
                .and_then(|value| value.get("user_id"))
                .and_then(JsonValue::as_str)
                .map(ToString::to_string)
        })
}

fn linear_payload(
    _kind: &str,
    headers: &BTreeMap<String, String>,
    raw: JsonValue,
) -> ProviderPayload {
    let common = linear_event_common(headers, &raw);
    let event = common.event.clone();
    let payload = match event.as_str() {
        "issue" => LinearEventPayload::Issue(LinearIssueEventPayload {
            common,
            issue: raw.get("data").cloned().unwrap_or(JsonValue::Null),
            changes: parse_linear_issue_changes(raw.get("updatedFrom")),
        }),
        "comment" => LinearEventPayload::IssueComment(LinearIssueCommentEventPayload {
            common,
            comment: raw.get("data").cloned().unwrap_or(JsonValue::Null),
        }),
        "issue_label" => LinearEventPayload::IssueLabel(LinearIssueLabelEventPayload {
            common,
            label: raw.get("data").cloned().unwrap_or(JsonValue::Null),
        }),
        "project" => LinearEventPayload::Project(LinearProjectEventPayload {
            common,
            project: raw.get("data").cloned().unwrap_or(JsonValue::Null),
        }),
        "cycle" => LinearEventPayload::Cycle(LinearCycleEventPayload {
            common,
            cycle: raw.get("data").cloned().unwrap_or(JsonValue::Null),
        }),
        "customer" => LinearEventPayload::Customer(LinearCustomerEventPayload {
            common,
            customer: raw.get("data").cloned().unwrap_or(JsonValue::Null),
        }),
        "customer_request" => {
            LinearEventPayload::CustomerRequest(LinearCustomerRequestEventPayload {
                common,
                customer_request: raw.get("data").cloned().unwrap_or(JsonValue::Null),
            })
        }
        _ => LinearEventPayload::Other(common),
    };
    ProviderPayload::Known(KnownProviderPayload::Linear(payload))
}

fn linear_event_common(headers: &BTreeMap<String, String>, raw: &JsonValue) -> LinearEventCommon {
    LinearEventCommon {
        event: linear_event_name(
            raw.get("type")
                .and_then(JsonValue::as_str)
                .or_else(|| headers.get("Linear-Event").map(String::as_str)),
        ),
        action: raw
            .get("action")
            .and_then(JsonValue::as_str)
            .map(ToString::to_string),
        delivery_id: header_value(headers, "Linear-Delivery").map(ToString::to_string),
        organization_id: raw
            .get("organizationId")
            .and_then(JsonValue::as_str)
            .map(ToString::to_string),
        webhook_timestamp: raw.get("webhookTimestamp").and_then(parse_json_i64ish),
        webhook_id: raw
            .get("webhookId")
            .and_then(JsonValue::as_str)
            .map(ToString::to_string),
        url: raw
            .get("url")
            .and_then(JsonValue::as_str)
            .map(ToString::to_string),
        created_at: raw
            .get("createdAt")
            .and_then(JsonValue::as_str)
            .map(ToString::to_string),
        actor: raw.get("actor").cloned().unwrap_or(JsonValue::Null),
        raw: raw.clone(),
    }
}

fn linear_event_name(raw_type: Option<&str>) -> String {
    match raw_type.unwrap_or_default().to_ascii_lowercase().as_str() {
        "issue" => "issue".to_string(),
        "comment" | "issuecomment" | "issue_comment" => "comment".to_string(),
        "issuelabel" | "issue_label" => "issue_label".to_string(),
        "project" | "projectupdate" | "project_update" => "project".to_string(),
        "cycle" => "cycle".to_string(),
        "customer" => "customer".to_string(),
        "customerrequest" | "customer_request" => "customer_request".to_string(),
        other if !other.is_empty() => other.to_string(),
        _ => "other".to_string(),
    }
}

fn parse_linear_issue_changes(updated_from: Option<&JsonValue>) -> Vec<LinearIssueChange> {
    let Some(JsonValue::Object(fields)) = updated_from else {
        return Vec::new();
    };
    let mut changes = Vec::new();
    for (field, previous) in fields {
        let change = match field.as_str() {
            "title" => LinearIssueChange::Title {
                previous: previous.as_str().map(ToString::to_string),
            },
            "description" => LinearIssueChange::Description {
                previous: previous.as_str().map(ToString::to_string),
            },
            "priority" => LinearIssueChange::Priority {
                previous: parse_json_i64ish(previous),
            },
            "estimate" => LinearIssueChange::Estimate {
                previous: parse_json_i64ish(previous),
            },
            "stateId" => LinearIssueChange::StateId {
                previous: previous.as_str().map(ToString::to_string),
            },
            "teamId" => LinearIssueChange::TeamId {
                previous: previous.as_str().map(ToString::to_string),
            },
            "assigneeId" => LinearIssueChange::AssigneeId {
                previous: previous.as_str().map(ToString::to_string),
            },
            "projectId" => LinearIssueChange::ProjectId {
                previous: previous.as_str().map(ToString::to_string),
            },
            "cycleId" => LinearIssueChange::CycleId {
                previous: previous.as_str().map(ToString::to_string),
            },
            "dueDate" => LinearIssueChange::DueDate {
                previous: previous.as_str().map(ToString::to_string),
            },
            "parentId" => LinearIssueChange::ParentId {
                previous: previous.as_str().map(ToString::to_string),
            },
            "sortOrder" => LinearIssueChange::SortOrder {
                previous: previous.as_f64(),
            },
            "labelIds" => LinearIssueChange::LabelIds {
                previous: parse_string_array(previous),
            },
            "completedAt" => LinearIssueChange::CompletedAt {
                previous: previous.as_str().map(ToString::to_string),
            },
            _ => LinearIssueChange::Other {
                field: field.clone(),
                previous: previous.clone(),
            },
        };
        changes.push(change);
    }
    changes
}

fn parse_json_i64ish(value: &JsonValue) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_u64().and_then(|raw| i64::try_from(raw).ok()))
        .or_else(|| value.as_str().and_then(|raw| raw.parse::<i64>().ok()))
}

fn parse_string_array(value: &JsonValue) -> Vec<String> {
    let Some(array) = value.as_array() else {
        return Vec::new();
    };
    array
        .iter()
        .filter_map(|entry| {
            entry.as_str().map(ToString::to_string).or_else(|| {
                entry
                    .get("id")
                    .and_then(JsonValue::as_str)
                    .map(ToString::to_string)
            })
        })
        .collect()
}

fn header_value<'a>(headers: &'a BTreeMap<String, String>, name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
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
    ProviderPayload::Known(KnownProviderPayload::Notion(Box::new(NotionEventPayload {
        event: kind.to_string(),
        workspace_id,
        request_id: headers
            .get("request-id")
            .cloned()
            .or_else(|| headers.get("x-request-id").cloned()),
        subscription_id: raw
            .get("subscription_id")
            .and_then(JsonValue::as_str)
            .map(ToString::to_string),
        integration_id: raw
            .get("integration_id")
            .and_then(JsonValue::as_str)
            .map(ToString::to_string),
        attempt_number: raw
            .get("attempt_number")
            .and_then(JsonValue::as_u64)
            .and_then(|value| u32::try_from(value).ok()),
        entity_id: raw
            .get("entity")
            .and_then(|value| value.get("id"))
            .and_then(JsonValue::as_str)
            .map(ToString::to_string),
        entity_type: raw
            .get("entity")
            .and_then(|value| value.get("type"))
            .and_then(JsonValue::as_str)
            .map(ToString::to_string),
        api_version: raw
            .get("api_version")
            .and_then(JsonValue::as_str)
            .map(ToString::to_string),
        verification_token: raw
            .get("verification_token")
            .and_then(JsonValue::as_str)
            .map(ToString::to_string),
        polled: None,
        raw,
    })))
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
                    &[],
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
                    &[],
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

        assert_eq!(builtin.len(), 6);
        assert!(builtin.iter().any(|entry| entry.provider == "cron"));
        assert!(builtin.iter().any(|entry| entry.provider == "github"));
        assert!(builtin.iter().any(|entry| entry.provider == "linear"));
        assert!(builtin.iter().any(|entry| entry.provider == "notion"));
        assert!(builtin.iter().any(|entry| entry.provider == "slack"));
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
            dedupe_claimed: false,
            batch: None,
            raw_body: Some(vec![0, 159, 255, 10]),
        };

        let once = serde_json::to_value(&event).unwrap();
        assert_eq!(once["raw_body"], serde_json::json!("AJ//Cg=="));
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
