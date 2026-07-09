use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value as JsonValue;
use time::OffsetDateTime;

use super::util::{parse_json_i64ish, parse_string_array};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitHubEventCommon {
    pub event: String,
    pub action: Option<String>,
    pub delivery_id: Option<String>,
    pub installation_id: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub topic: Option<String>,
    #[serde(default)]
    pub reaction_topics: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository: Option<JsonValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<JsonValue>,
    pub raw: JsonValue,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitHubIssuesEventPayload {
    #[serde(flatten)]
    pub common: GitHubEventCommon,
    pub issue: JsonValue,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitHubPullRequestEventPayload {
    #[serde(flatten)]
    pub common: GitHubEventCommon,
    pub pull_request: JsonValue,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitHubIssueCommentEventPayload {
    #[serde(flatten)]
    pub common: GitHubEventCommon,
    pub issue: JsonValue,
    pub comment: JsonValue,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitHubPullRequestReviewEventPayload {
    #[serde(flatten)]
    pub common: GitHubEventCommon,
    pub pull_request: JsonValue,
    pub review: JsonValue,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitHubPushEventPayload {
    #[serde(flatten)]
    pub common: GitHubEventCommon,
    #[serde(default)]
    pub commits: Vec<JsonValue>,
    pub distinct_size: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitHubWorkflowRunEventPayload {
    #[serde(flatten)]
    pub common: GitHubEventCommon,
    pub workflow_run: JsonValue,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitHubDeploymentStatusEventPayload {
    #[serde(flatten)]
    pub common: GitHubEventCommon,
    pub deployment_status: JsonValue,
    pub deployment: JsonValue,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitHubCheckRunEventPayload {
    #[serde(flatten)]
    pub common: GitHubEventCommon,
    pub check_run: JsonValue,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitHubCheckSuiteEventPayload {
    #[serde(flatten)]
    pub common: GitHubEventCommon,
    pub check_suite: JsonValue,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check_suite_id: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pull_request_number: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_sha: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conclusion: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitHubStatusEventPayload {
    #[serde(flatten)]
    pub common: GitHubEventCommon,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit_status: Option<JsonValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_id: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_sha: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_url: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitHubMergeGroupEventPayload {
    #[serde(flatten)]
    pub common: GitHubEventCommon,
    pub merge_group: JsonValue,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merge_group_id: Option<JsonValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_sha: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_sha: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_ref: Option<String>,
    #[serde(default)]
    pub pull_requests: Vec<JsonValue>,
    #[serde(default)]
    pub pull_request_numbers: Vec<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitHubInstallationEventPayload {
    #[serde(flatten)]
    pub common: GitHubEventCommon,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installation: Option<JsonValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<JsonValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installation_state: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suspended: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoked: Option<bool>,
    #[serde(default)]
    pub repositories: Vec<JsonValue>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitHubInstallationRepositoriesEventPayload {
    #[serde(flatten)]
    pub common: GitHubEventCommon,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installation: Option<JsonValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<JsonValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installation_state: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suspended: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoked: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository_selection: Option<String>,
    #[serde(default)]
    pub repositories_added: Vec<JsonValue>,
    #[serde(default)]
    pub repositories_removed: Vec<JsonValue>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
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
    CheckSuite(GitHubCheckSuiteEventPayload),
    Status(GitHubStatusEventPayload),
    MergeGroup(GitHubMergeGroupEventPayload),
    Installation(GitHubInstallationEventPayload),
    InstallationRepositories(GitHubInstallationRepositoriesEventPayload),
    Other(GitHubEventCommon),
}

// Manual `Deserialize` that dispatches on the `event` field. An untagged
// enum cannot reliably round-trip these variants because `Push` and the
// all-optional installation/status variants will accept payloads that
// belong to a different event kind.
impl<'de> Deserialize<'de> for GitHubEventPayload {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = JsonValue::deserialize(deserializer)?;
        let kind = value
            .get("event")
            .and_then(JsonValue::as_str)
            .unwrap_or("")
            .to_string();
        let from_value = |v: JsonValue| -> Result<GitHubEventPayload, D::Error> {
            let payload = match kind.as_str() {
                "issues" => GitHubEventPayload::Issues(
                    serde_json::from_value(v).map_err(serde::de::Error::custom)?,
                ),
                "pull_request" => GitHubEventPayload::PullRequest(
                    serde_json::from_value(v).map_err(serde::de::Error::custom)?,
                ),
                "issue_comment" => GitHubEventPayload::IssueComment(
                    serde_json::from_value(v).map_err(serde::de::Error::custom)?,
                ),
                "pull_request_review" => GitHubEventPayload::PullRequestReview(
                    serde_json::from_value(v).map_err(serde::de::Error::custom)?,
                ),
                "push" => GitHubEventPayload::Push(
                    serde_json::from_value(v).map_err(serde::de::Error::custom)?,
                ),
                "workflow_run" => GitHubEventPayload::WorkflowRun(
                    serde_json::from_value(v).map_err(serde::de::Error::custom)?,
                ),
                "deployment_status" => GitHubEventPayload::DeploymentStatus(
                    serde_json::from_value(v).map_err(serde::de::Error::custom)?,
                ),
                "check_run" => GitHubEventPayload::CheckRun(
                    serde_json::from_value(v).map_err(serde::de::Error::custom)?,
                ),
                "check_suite" => GitHubEventPayload::CheckSuite(
                    serde_json::from_value(v).map_err(serde::de::Error::custom)?,
                ),
                "status" => GitHubEventPayload::Status(
                    serde_json::from_value(v).map_err(serde::de::Error::custom)?,
                ),
                "merge_group" => GitHubEventPayload::MergeGroup(
                    serde_json::from_value(v).map_err(serde::de::Error::custom)?,
                ),
                "installation" => GitHubEventPayload::Installation(
                    serde_json::from_value(v).map_err(serde::de::Error::custom)?,
                ),
                "installation_repositories" => GitHubEventPayload::InstallationRepositories(
                    serde_json::from_value(v).map_err(serde::de::Error::custom)?,
                ),
                _ => GitHubEventPayload::Other(
                    serde_json::from_value(v).map_err(serde::de::Error::custom)?,
                ),
            };
            Ok(payload)
        };
        from_value(value)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlackAppMentionEventPayload {
    #[serde(flatten)]
    pub common: SlackEventCommon,
    pub channel: Option<String>,
    pub user: Option<String>,
    pub text: Option<String>,
    pub ts: Option<String>,
    pub thread_ts: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlackReactionAddedEventPayload {
    #[serde(flatten)]
    pub common: SlackEventCommon,
    pub reaction: Option<String>,
    pub item_user: Option<String>,
    pub item: JsonValue,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlackAppHomeOpenedEventPayload {
    #[serde(flatten)]
    pub common: SlackEventCommon,
    pub user: Option<String>,
    pub channel: Option<String>,
    pub tab: Option<String>,
    pub view: JsonValue,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlackAssistantThreadStartedEventPayload {
    #[serde(flatten)]
    pub common: SlackEventCommon,
    pub assistant_thread: JsonValue,
    pub thread_ts: Option<String>,
    pub context: JsonValue,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SlackEventPayload {
    Message(SlackMessageEventPayload),
    AppMention(SlackAppMentionEventPayload),
    ReactionAdded(SlackReactionAddedEventPayload),
    AppHomeOpened(SlackAppHomeOpenedEventPayload),
    AssistantThreadStarted(SlackAssistantThreadStartedEventPayload),
    Other(SlackEventCommon),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinearIssueCommentEventPayload {
    #[serde(flatten)]
    pub common: LinearEventCommon,
    pub comment: JsonValue,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinearIssueLabelEventPayload {
    #[serde(flatten)]
    pub common: LinearEventCommon,
    pub label: JsonValue,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinearProjectEventPayload {
    #[serde(flatten)]
    pub common: LinearEventCommon,
    pub project: JsonValue,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinearCycleEventPayload {
    #[serde(flatten)]
    pub common: LinearEventCommon,
    pub cycle: JsonValue,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinearCustomerEventPayload {
    #[serde(flatten)]
    pub common: LinearEventCommon,
    pub customer: JsonValue,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotionPolledChangeEvent {
    pub resource: String,
    pub source_id: String,
    pub entity_id: String,
    pub high_water_mark: String,
    pub before: Option<JsonValue>,
    pub after: JsonValue,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CronEventPayload {
    pub cron_id: Option<String>,
    pub schedule: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub tick_at: OffsetDateTime,
    pub raw: JsonValue,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GenericWebhookPayload {
    pub source: Option<String>,
    pub content_type: Option<String>,
    pub raw: JsonValue,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct A2aPushPayload {
    pub task_id: Option<String>,
    pub task_state: Option<String>,
    pub artifact: Option<JsonValue>,
    pub sender: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor_chain: Option<JsonValue>,
    pub raw: JsonValue,
    pub kind: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamEventPayload {
    pub event: String,
    pub source: Option<String>,
    pub stream: Option<String>,
    pub partition: Option<String>,
    pub offset: Option<String>,
    pub key: Option<String>,
    pub timestamp: Option<String>,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    pub raw: JsonValue,
}

/// Payload emitted by `emit_channel(...)` to channel-source triggers.
///
/// The dispatcher fan-out path (CH-02 / #1872) synthesizes one
/// [`TriggerEvent`](crate::triggers::TriggerEvent) per matching channel
/// trigger using `provider = "channel"` and `kind = "channel.emit"`. The
/// resolved channel coordinates (`scope`, `scope_id`, `name`, fully
/// qualified `name_resolved`) and the user-supplied `payload` ride along
/// here so handlers can read them off the trigger event without re-parsing
/// the resolved name string.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelEventPayload {
    pub id: String,
    pub name: String,
    pub name_resolved: String,
    pub scope: String,
    pub scope_id: String,
    pub payload: JsonValue,
    pub emitted_by: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tenant_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pipeline_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "provider")]
pub enum KnownProviderPayload {
    #[serde(rename = "github")]
    GitHub(Box<GitHubEventPayload>),
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
    #[serde(rename = "kafka")]
    Kafka(StreamEventPayload),
    #[serde(rename = "nats")]
    Nats(StreamEventPayload),
    #[serde(rename = "pulsar")]
    Pulsar(StreamEventPayload),
    #[serde(rename = "postgres-cdc")]
    PostgresCdc(StreamEventPayload),
    #[serde(rename = "email")]
    Email(StreamEventPayload),
    #[serde(rename = "websocket")]
    Websocket(StreamEventPayload),
    #[serde(rename = "channel")]
    Channel(ChannelEventPayload),
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
            Self::Kafka(_) => "kafka",
            Self::Nats(_) => "nats",
            Self::Pulsar(_) => "pulsar",
            Self::PostgresCdc(_) => "postgres-cdc",
            Self::Email(_) => "email",
            Self::Websocket(_) => "websocket",
            Self::Channel(_) => "channel",
        }
    }
}
