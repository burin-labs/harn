use std::collections::BTreeMap;

use serde_json::Value as JsonValue;
use time::OffsetDateTime;

use super::payloads::*;
use super::util::{
    header_value, json_stringish, parse_json_i64ish, parse_rfc3339, parse_string_array,
};

pub(super) fn github_payload(
    kind: &str,
    headers: &BTreeMap<String, String>,
    raw: JsonValue,
) -> ProviderPayload {
    // The connector emits a normalized payload that wraps the original
    // GitHub webhook body inside its own `raw` field. When we see that
    // wrapper shape, prefer it as the escape-hatch raw so callers don't
    // have to traverse two levels of `raw` to reach the upstream payload.
    let original_raw = raw
        .get("raw")
        .filter(|value| value.is_object())
        .cloned()
        .unwrap_or_else(|| raw.clone());
    let common = GitHubEventCommon {
        event: kind.to_string(),
        action: raw
            .get("action")
            .and_then(JsonValue::as_str)
            .map(ToString::to_string),
        delivery_id: raw
            .get("delivery_id")
            .and_then(JsonValue::as_str)
            .map(ToString::to_string)
            .or_else(|| headers.get("X-GitHub-Delivery").cloned()),
        installation_id: raw
            .get("installation_id")
            .and_then(JsonValue::as_i64)
            .or_else(|| {
                raw.get("installation")
                    .and_then(|value| value.get("id"))
                    .and_then(JsonValue::as_i64)
            }),
        topic: raw
            .get("topic")
            .and_then(JsonValue::as_str)
            .map(ToString::to_string),
        repository: raw.get("repository").cloned(),
        repo: raw.get("repo").cloned(),
        raw: original_raw,
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
        "check_suite" => {
            let check_suite = raw.get("check_suite").cloned().unwrap_or(JsonValue::Null);
            GitHubEventPayload::CheckSuite(GitHubCheckSuiteEventPayload {
                check_suite_id: github_promoted_i64(&raw, "check_suite_id")
                    .or_else(|| check_suite.get("id").and_then(JsonValue::as_i64)),
                pull_request_number: github_promoted_i64(&raw, "pull_request_number"),
                head_sha: github_promoted_string(&raw, "head_sha"),
                head_ref: github_promoted_string(&raw, "head_ref"),
                base_ref: github_promoted_string(&raw, "base_ref"),
                status: github_promoted_string(&raw, "status"),
                conclusion: github_promoted_string(&raw, "conclusion"),
                common,
                check_suite,
            })
        }
        "status" => GitHubEventPayload::Status(GitHubStatusEventPayload {
            commit_status: raw
                .get("commit_status")
                .cloned()
                .or_else(|| Some(common.raw.clone())),
            status_id: github_promoted_i64(&raw, "status_id")
                .or_else(|| common.raw.get("id").and_then(JsonValue::as_i64)),
            head_sha: github_promoted_string(&raw, "head_sha").or_else(|| {
                common
                    .raw
                    .get("sha")
                    .and_then(JsonValue::as_str)
                    .map(ToString::to_string)
            }),
            head_ref: github_promoted_string(&raw, "head_ref"),
            base_ref: github_promoted_string(&raw, "base_ref"),
            state: github_promoted_string(&raw, "state"),
            context: github_promoted_string(&raw, "context"),
            target_url: github_promoted_string(&raw, "target_url"),
            common,
        }),
        "merge_group" => {
            let merge_group = raw.get("merge_group").cloned().unwrap_or(JsonValue::Null);
            GitHubEventPayload::MergeGroup(GitHubMergeGroupEventPayload {
                merge_group_id: raw
                    .get("merge_group_id")
                    .cloned()
                    .or_else(|| merge_group.get("id").cloned()),
                head_sha: github_promoted_string(&raw, "head_sha").or_else(|| {
                    merge_group
                        .get("head_sha")
                        .and_then(JsonValue::as_str)
                        .map(ToString::to_string)
                }),
                head_ref: github_promoted_string(&raw, "head_ref").or_else(|| {
                    merge_group
                        .get("head_ref")
                        .and_then(JsonValue::as_str)
                        .map(ToString::to_string)
                }),
                base_sha: github_promoted_string(&raw, "base_sha").or_else(|| {
                    merge_group
                        .get("base_sha")
                        .and_then(JsonValue::as_str)
                        .map(ToString::to_string)
                }),
                base_ref: github_promoted_string(&raw, "base_ref").or_else(|| {
                    merge_group
                        .get("base_ref")
                        .and_then(JsonValue::as_str)
                        .map(ToString::to_string)
                }),
                pull_requests: raw
                    .get("pull_requests")
                    .and_then(JsonValue::as_array)
                    .cloned()
                    .unwrap_or_default(),
                pull_request_numbers: raw
                    .get("pull_request_numbers")
                    .and_then(JsonValue::as_array)
                    .map(|values| {
                        values
                            .iter()
                            .filter_map(JsonValue::as_i64)
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default(),
                common,
                merge_group,
            })
        }
        "installation" => GitHubEventPayload::Installation(GitHubInstallationEventPayload {
            installation: raw.get("installation").cloned(),
            account: raw.get("account").cloned(),
            installation_state: github_promoted_string(&raw, "installation_state"),
            suspended: raw.get("suspended").and_then(JsonValue::as_bool),
            revoked: raw.get("revoked").and_then(JsonValue::as_bool),
            repositories: raw
                .get("repositories")
                .and_then(JsonValue::as_array)
                .cloned()
                .unwrap_or_default(),
            common,
        }),
        "installation_repositories" => GitHubEventPayload::InstallationRepositories(
            GitHubInstallationRepositoriesEventPayload {
                installation: raw.get("installation").cloned(),
                account: raw.get("account").cloned(),
                installation_state: github_promoted_string(&raw, "installation_state"),
                suspended: raw.get("suspended").and_then(JsonValue::as_bool),
                revoked: raw.get("revoked").and_then(JsonValue::as_bool),
                repository_selection: github_promoted_string(&raw, "repository_selection"),
                repositories_added: raw
                    .get("repositories_added")
                    .and_then(JsonValue::as_array)
                    .cloned()
                    .unwrap_or_default(),
                repositories_removed: raw
                    .get("repositories_removed")
                    .and_then(JsonValue::as_array)
                    .cloned()
                    .unwrap_or_default(),
                common,
            },
        ),
        _ => GitHubEventPayload::Other(common),
    };
    ProviderPayload::Known(KnownProviderPayload::GitHub(payload))
}

fn github_promoted_string(raw: &JsonValue, field: &str) -> Option<String> {
    raw.get(field)
        .and_then(JsonValue::as_str)
        .map(ToString::to_string)
}

fn github_promoted_i64(raw: &JsonValue, field: &str) -> Option<i64> {
    raw.get(field).and_then(JsonValue::as_i64)
}

pub(super) fn slack_payload(
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

pub(super) fn linear_payload(
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

pub(super) fn notion_payload(
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

pub(super) fn cron_payload(
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

pub(super) fn webhook_payload(
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

pub(super) fn a2a_push_payload(
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
    let task_state = raw
        .pointer("/status/state")
        .or_else(|| raw.pointer("/statusUpdate/status/state"))
        .and_then(JsonValue::as_str)
        .map(|state| match state {
            "cancelled" => "canceled".to_string(),
            other => other.to_string(),
        });
    let artifact = raw
        .pointer("/artifactUpdate/artifact")
        .or_else(|| raw.get("artifact"))
        .cloned();
    let kind = task_state
        .as_deref()
        .map(|state| format!("a2a.task.{state}"))
        .unwrap_or_else(|| "a2a.task.update".to_string());
    ProviderPayload::Known(KnownProviderPayload::A2aPush(A2aPushPayload {
        task_id,
        task_state,
        artifact,
        sender,
        raw,
        kind,
    }))
}

pub(super) fn kafka_payload(
    kind: &str,
    headers: &BTreeMap<String, String>,
    raw: JsonValue,
) -> ProviderPayload {
    ProviderPayload::Known(KnownProviderPayload::Kafka(stream_payload(
        kind, headers, raw,
    )))
}

pub(super) fn nats_payload(
    kind: &str,
    headers: &BTreeMap<String, String>,
    raw: JsonValue,
) -> ProviderPayload {
    ProviderPayload::Known(KnownProviderPayload::Nats(stream_payload(
        kind, headers, raw,
    )))
}

pub(super) fn pulsar_payload(
    kind: &str,
    headers: &BTreeMap<String, String>,
    raw: JsonValue,
) -> ProviderPayload {
    ProviderPayload::Known(KnownProviderPayload::Pulsar(stream_payload(
        kind, headers, raw,
    )))
}

pub(super) fn postgres_cdc_payload(
    kind: &str,
    headers: &BTreeMap<String, String>,
    raw: JsonValue,
) -> ProviderPayload {
    ProviderPayload::Known(KnownProviderPayload::PostgresCdc(stream_payload(
        kind, headers, raw,
    )))
}

pub(super) fn email_payload(
    kind: &str,
    headers: &BTreeMap<String, String>,
    raw: JsonValue,
) -> ProviderPayload {
    ProviderPayload::Known(KnownProviderPayload::Email(stream_payload(
        kind, headers, raw,
    )))
}

pub(super) fn websocket_payload(
    kind: &str,
    headers: &BTreeMap<String, String>,
    raw: JsonValue,
) -> ProviderPayload {
    ProviderPayload::Known(KnownProviderPayload::Websocket(stream_payload(
        kind, headers, raw,
    )))
}

fn stream_payload(
    kind: &str,
    headers: &BTreeMap<String, String>,
    raw: JsonValue,
) -> StreamEventPayload {
    StreamEventPayload {
        event: kind.to_string(),
        source: json_stringish(&raw, &["source", "connector", "origin"]),
        stream: json_stringish(
            &raw,
            &["stream", "topic", "subject", "channel", "mailbox", "slot"],
        ),
        partition: json_stringish(&raw, &["partition", "shard", "consumer"]),
        offset: json_stringish(&raw, &["offset", "sequence", "lsn", "message_id"]),
        key: json_stringish(&raw, &["key", "message_key", "id", "event_id"]),
        timestamp: json_stringish(&raw, &["timestamp", "occurred_at", "received_at", "ts"]),
        headers: headers.clone(),
        raw,
    }
}
