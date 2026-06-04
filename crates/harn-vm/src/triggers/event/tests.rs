use super::util::parse_rfc3339;
use super::*;
use crate::redact::REDACTED_HEADER_VALUE;
use serde_json::Value as JsonValue;
use std::collections::BTreeMap;
use std::sync::Arc;

struct OwnedProviderSchema {
    metadata: ProviderMetadata,
}

impl OwnedProviderSchema {
    fn new(provider: &str, schema_name: &str) -> Self {
        Self {
            metadata: ProviderMetadata {
                provider: provider.to_string(),
                kinds: vec!["webhook".to_string()],
                schema_name: schema_name.to_string(),
                runtime: ProviderRuntimeMetadata::Placeholder,
                ..ProviderMetadata::default()
            },
        }
    }
}

impl ProviderSchema for OwnedProviderSchema {
    fn provider_id(&self) -> &str {
        &self.metadata.provider
    }

    fn harn_schema_name(&self) -> &str {
        &self.metadata.schema_name
    }

    fn metadata(&self) -> ProviderMetadata {
        self.metadata.clone()
    }

    fn normalize(
        &self,
        _kind: &str,
        _headers: &BTreeMap<String, String>,
        raw: JsonValue,
    ) -> Result<ProviderPayload, ProviderCatalogError> {
        Ok(ProviderPayload::Extension(ExtensionProviderPayload {
            provider: self.metadata.provider.clone(),
            schema_name: self.metadata.schema_name.clone(),
            raw,
        }))
    }
}

fn owned_provider_schema(provider: &str, schema_name: &str) -> Arc<dyn ProviderSchema> {
    Arc::new(OwnedProviderSchema::new(provider, schema_name))
}

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
        .register(owned_provider_schema("github", "GitHubEventPayload"))
        .unwrap();
    let error = catalog
        .register(owned_provider_schema("github", "GitHubEventPayload"))
        .unwrap_err();
    assert_eq!(
        error,
        ProviderCatalogError::DuplicateProvider("github".to_string())
    );
}

#[test]
fn provider_catalog_builds_independent_owned_dynamic_catalogs() {
    let first = ProviderCatalog::with_defaults_and(vec![owned_provider_schema(
        "runtime-a",
        "RuntimeAPayload",
    )])
    .unwrap();
    assert_eq!(
        first
            .metadata_for("runtime-a")
            .expect("first dynamic provider")
            .schema_name,
        "RuntimeAPayload"
    );
    assert!(first.metadata_for("runtime-b").is_none());

    let second = ProviderCatalog::with_defaults_and(vec![owned_provider_schema(
        "runtime-b",
        "RuntimeBPayload",
    )])
    .unwrap();
    assert!(second.metadata_for("runtime-a").is_none());
    assert_eq!(
        second
            .metadata_for("runtime-b")
            .expect("second dynamic provider")
            .schema_name,
        "RuntimeBPayload"
    );
    assert!(first.metadata_for("runtime-a").is_some());
}

#[test]
fn registered_provider_metadata_marks_builtin_connectors() {
    let entries = registered_provider_metadata();
    let builtin: Vec<&ProviderMetadata> = entries
        .iter()
        .filter(|entry| matches!(entry.runtime, ProviderRuntimeMetadata::Builtin { .. }))
        .collect();

    assert_eq!(builtin.len(), 9);
    assert!(builtin.iter().any(|entry| entry.provider == "a2a-push"));
    assert!(builtin.iter().any(|entry| entry.provider == "cron"));
    assert!(builtin.iter().any(|entry| entry.provider == "webhook"));
    for provider in ["github", "linear", "notion", "slack"] {
        let entry = entries
            .iter()
            .find(|entry| entry.provider == provider)
            .expect("first-party package-backed provider metadata");
        assert!(matches!(
            entry.runtime,
            ProviderRuntimeMetadata::Placeholder
        ));
    }
    let kafka = entries
        .iter()
        .find(|entry| entry.provider == "kafka")
        .expect("kafka stream provider");
    assert_eq!(kafka.kinds, vec!["stream".to_string()]);
    assert_eq!(kafka.schema_name, "StreamEventPayload");
    assert!(matches!(
        kafka.runtime,
        ProviderRuntimeMetadata::Builtin {
            ref connector,
            default_signature_variant: None
        } if connector == "stream"
    ));
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

fn github_headers(event: &str, delivery: &str) -> BTreeMap<String, String> {
    BTreeMap::from([
        ("X-GitHub-Event".to_string(), event.to_string()),
        ("X-GitHub-Delivery".to_string(), delivery.to_string()),
    ])
}

fn unwrap_github(payload: ProviderPayload) -> GitHubEventPayload {
    match payload {
        ProviderPayload::Known(KnownProviderPayload::GitHub(p)) => p,
        other => panic!("expected GitHub payload, got {other:?}"),
    }
}

/// Mirror of the connector's normalized webhook payload shape: the
/// connector wraps the original GitHub body as `raw` and promotes
/// stable common + event-specific fields.
fn connector_normalized(
    event: &str,
    delivery: &str,
    installation_id: i64,
    action: Option<&str>,
    original: serde_json::Value,
    promoted: serde_json::Value,
) -> serde_json::Value {
    let mut common = serde_json::json!({
        "provider": "github",
        "event": event,
        "topic": match action {
            Some(a) => format!("github.{event}.{a}"),
            None => format!("github.{event}"),
        },
        "delivery_id": delivery,
        "installation_id": installation_id,
        "repository": original.get("repository").cloned().unwrap_or(JsonValue::Null),
        "repo": serde_json::json!({"owner": "octo-org", "name": "octo-repo", "full_name": "octo-org/octo-repo"}),
        "raw": original,
    });
    if let Some(a) = action {
        common["action"] = serde_json::json!(a);
    }
    let common_obj = common.as_object_mut().unwrap();
    if let Some(promoted_obj) = promoted.as_object() {
        for (k, v) in promoted_obj {
            common_obj.insert(k.clone(), v.clone());
        }
    }
    common
}

#[test]
fn github_check_suite_event_promotes_typed_fields() {
    let original = serde_json::json!({
        "action": "requested",
        "check_suite": {
            "id": 8101,
            "status": "queued",
            "conclusion": null,
            "head_sha": "ccccccccccccccccccccccccccccccccccccccc1",
            "head_branch": "feature/x",
        },
        "repository": {"full_name": "octo-org/octo-repo"},
        "installation": {"id": 3001},
    });
    let normalized = connector_normalized(
        "check_suite",
        "delivery-cs",
        3001,
        Some("requested"),
        original.clone(),
        serde_json::json!({
            "check_suite": original["check_suite"].clone(),
            "check_suite_id": 8101,
            "head_sha": "ccccccccccccccccccccccccccccccccccccccc1",
            "head_ref": "feature/x",
            "status": "queued",
        }),
    );
    let provider = ProviderId::from("github");
    let payload = ProviderPayload::normalize(
        &provider,
        "check_suite",
        &github_headers("check_suite", "delivery-cs"),
        normalized,
    )
    .expect("check_suite payload");
    let GitHubEventPayload::CheckSuite(check_suite) = unwrap_github(payload) else {
        panic!("expected CheckSuite variant");
    };
    assert_eq!(check_suite.common.event, "check_suite");
    assert_eq!(check_suite.common.action.as_deref(), Some("requested"));
    assert_eq!(
        check_suite.common.delivery_id.as_deref(),
        Some("delivery-cs")
    );
    assert_eq!(check_suite.common.installation_id, Some(3001));
    assert_eq!(
        check_suite.common.topic.as_deref(),
        Some("github.check_suite.requested")
    );
    assert!(check_suite.common.repository.is_some());
    assert!(check_suite.common.repo.is_some());
    assert_eq!(check_suite.check_suite_id, Some(8101));
    assert_eq!(
        check_suite.head_sha.as_deref(),
        Some("ccccccccccccccccccccccccccccccccccccccc1")
    );
    assert_eq!(check_suite.head_ref.as_deref(), Some("feature/x"));
    assert_eq!(check_suite.status.as_deref(), Some("queued"));
    // Original body is preserved as the escape-hatch raw.
    assert_eq!(check_suite.common.raw, original);
}

#[test]
fn github_status_event_promotes_typed_fields() {
    let original = serde_json::json!({
        "id": 9101,
        "sha": "ccccccccccccccccccccccccccccccccccccccc1",
        "state": "success",
        "context": "legacy/status",
        "target_url": "https://ci.example.test/octo-repo/9101",
        "branches": [{"name": "main"}],
        "repository": {"full_name": "octo-org/octo-repo"},
        "installation": {"id": 3001},
    });
    let normalized = connector_normalized(
        "status",
        "delivery-status",
        3001,
        None,
        original.clone(),
        serde_json::json!({
            "commit_status": original,
            "status_id": 9101,
            "head_sha": "ccccccccccccccccccccccccccccccccccccccc1",
            "head_ref": "main",
            "base_ref": "main",
            "state": "success",
            "context": "legacy/status",
            "target_url": "https://ci.example.test/octo-repo/9101",
        }),
    );
    let provider = ProviderId::from("github");
    let payload = ProviderPayload::normalize(
        &provider,
        "status",
        &github_headers("status", "delivery-status"),
        normalized,
    )
    .expect("status payload");
    let GitHubEventPayload::Status(status) = unwrap_github(payload) else {
        panic!("expected Status variant");
    };
    assert_eq!(status.common.event, "status");
    assert_eq!(status.common.installation_id, Some(3001));
    assert_eq!(status.status_id, Some(9101));
    assert_eq!(status.state.as_deref(), Some("success"));
    assert_eq!(status.context.as_deref(), Some("legacy/status"));
    assert_eq!(
        status.target_url.as_deref(),
        Some("https://ci.example.test/octo-repo/9101")
    );
    assert_eq!(
        status.head_sha.as_deref(),
        Some("ccccccccccccccccccccccccccccccccccccccc1")
    );
    assert!(status.commit_status.is_some());
}

#[test]
fn github_merge_group_event_promotes_typed_fields() {
    let original = serde_json::json!({
        "action": "checks_requested",
        "merge_group": {
            "id": 9201,
            "head_ref": "gh-readonly-queue/main/pr-42",
            "head_sha": "ddddddddddddddddddddddddddddddddddddddd1",
            "base_ref": "main",
            "base_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa1",
            "pull_requests": [{"number": 42}, {"number": 43}],
        },
        "repository": {"full_name": "octo-org/octo-repo"},
        "installation": {"id": 3001},
    });
    let normalized = connector_normalized(
        "merge_group",
        "delivery-mg",
        3001,
        Some("checks_requested"),
        original.clone(),
        serde_json::json!({
            "merge_group": original["merge_group"].clone(),
            "merge_group_id": 9201,
            "head_sha": "ddddddddddddddddddddddddddddddddddddddd1",
            "head_ref": "gh-readonly-queue/main/pr-42",
            "base_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa1",
            "base_ref": "main",
            "pull_requests": [{"number": 42}, {"number": 43}],
            "pull_request_numbers": [42, 43],
        }),
    );
    let provider = ProviderId::from("github");
    let payload = ProviderPayload::normalize(
        &provider,
        "merge_group",
        &github_headers("merge_group", "delivery-mg"),
        normalized,
    )
    .expect("merge_group payload");
    let GitHubEventPayload::MergeGroup(mg) = unwrap_github(payload) else {
        panic!("expected MergeGroup variant");
    };
    assert_eq!(mg.common.event, "merge_group");
    assert_eq!(mg.common.action.as_deref(), Some("checks_requested"));
    assert_eq!(mg.merge_group_id, Some(serde_json::json!(9201)));
    assert_eq!(mg.head_ref.as_deref(), Some("gh-readonly-queue/main/pr-42"));
    assert_eq!(
        mg.base_sha.as_deref(),
        Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa1")
    );
    assert_eq!(mg.base_ref.as_deref(), Some("main"));
    assert_eq!(mg.pull_request_numbers, vec![42i64, 43i64]);
    assert_eq!(mg.pull_requests.len(), 2);
}

#[test]
fn github_installation_event_promotes_typed_fields() {
    let original = serde_json::json!({
        "action": "suspend",
        "installation": {
            "id": 3001,
            "account": {"login": "octo-org"},
            "repository_selection": "selected",
            "suspended_at": "2026-04-20T18:00:00Z",
        },
        "repositories": [{"full_name": "octo-org/octo-repo"}],
    });
    let normalized = connector_normalized(
        "installation",
        "delivery-inst",
        3001,
        Some("suspend"),
        original.clone(),
        serde_json::json!({
            "installation": original["installation"].clone(),
            "account": {"login": "octo-org"},
            "installation_state": "suspended",
            "suspended": true,
            "revoked": false,
            "repositories": original["repositories"].clone(),
        }),
    );
    let provider = ProviderId::from("github");
    let payload = ProviderPayload::normalize(
        &provider,
        "installation",
        &github_headers("installation", "delivery-inst"),
        normalized,
    )
    .expect("installation payload");
    let GitHubEventPayload::Installation(inst) = unwrap_github(payload) else {
        panic!("expected Installation variant");
    };
    assert_eq!(inst.common.event, "installation");
    assert_eq!(inst.common.action.as_deref(), Some("suspend"));
    assert_eq!(inst.installation_state.as_deref(), Some("suspended"));
    assert_eq!(inst.suspended, Some(true));
    assert_eq!(inst.revoked, Some(false));
    assert_eq!(inst.repositories.len(), 1);
    assert!(inst.account.is_some());
}

#[test]
fn github_installation_repositories_event_promotes_typed_fields() {
    let original = serde_json::json!({
        "action": "removed",
        "installation": {"id": 3001, "account": {"login": "octo-org"}},
        "repository_selection": "selected",
        "repositories_added": [],
        "repositories_removed": [
            {"id": 4001, "full_name": "octo-org/octo-repo"},
        ],
    });
    let normalized = connector_normalized(
        "installation_repositories",
        "delivery-inst-repos",
        3001,
        Some("removed"),
        original.clone(),
        serde_json::json!({
            "installation": original["installation"].clone(),
            "account": {"login": "octo-org"},
            "installation_state": "revoked",
            "suspended": false,
            "revoked": true,
            "repository_selection": "selected",
            "repositories_added": [],
            "repositories_removed": original["repositories_removed"].clone(),
        }),
    );
    let provider = ProviderId::from("github");
    let payload = ProviderPayload::normalize(
        &provider,
        "installation_repositories",
        &github_headers("installation_repositories", "delivery-inst-repos"),
        normalized,
    )
    .expect("installation_repositories payload");
    let GitHubEventPayload::InstallationRepositories(repos) = unwrap_github(payload) else {
        panic!("expected InstallationRepositories variant");
    };
    assert_eq!(repos.common.event, "installation_repositories");
    assert_eq!(repos.common.action.as_deref(), Some("removed"));
    assert_eq!(repos.repository_selection.as_deref(), Some("selected"));
    assert!(repos.repositories_added.is_empty());
    assert_eq!(repos.repositories_removed.len(), 1);
    assert_eq!(
        repos.repositories_removed[0]
            .get("full_name")
            .and_then(|v| v.as_str()),
        Some("octo-org/octo-repo"),
    );
    assert_eq!(repos.installation_state.as_deref(), Some("revoked"));
    assert_eq!(repos.revoked, Some(true));
}

#[test]
fn github_legacy_direct_webhook_still_normalizes() {
    // Direct GitHub webhook bodies (no connector wrapper) should
    // continue to populate installation_id from `installation.id` and
    // leave the new common fields unset.
    let provider = ProviderId::from("github");
    let payload = ProviderPayload::normalize(
        &provider,
        "issues",
        &github_headers("issues", "delivery-legacy"),
        serde_json::json!({
            "action": "opened",
            "installation": {"id": 99},
            "issue": {"number": 7},
        }),
    )
    .expect("legacy issues payload");
    let GitHubEventPayload::Issues(issues) = unwrap_github(payload) else {
        panic!("expected Issues variant");
    };
    assert_eq!(issues.common.installation_id, Some(99));
    assert_eq!(
        issues.common.delivery_id.as_deref(),
        Some("delivery-legacy")
    );
    assert!(issues.common.topic.is_none());
    assert!(issues.common.repo.is_none());
    assert_eq!(issues.issue.get("number").and_then(|v| v.as_i64()), Some(7));
}

#[test]
fn github_new_event_variants_round_trip_through_serde() {
    // Untagged enums match in declaration order. Make sure each new
    // event kind serializes and deserializes back to the same variant
    // — i.e. it is not silently absorbed into an earlier variant such
    // as `Push` (whose only required field is defaulted) or `Other`.
    let provider = ProviderId::from("github");
    let cases: &[(&str, serde_json::Value, &str)] = &[
        (
            "check_suite",
            serde_json::json!({
                "event": "check_suite",
                "check_suite": {"id": 1},
                "check_suite_id": 1,
                "raw": {"check_suite": {"id": 1}},
            }),
            "CheckSuite",
        ),
        (
            "status",
            serde_json::json!({
                "event": "status",
                "commit_status": {"id": 9},
                "status_id": 9,
                "state": "success",
                "raw": {"id": 9, "state": "success"},
            }),
            "Status",
        ),
        (
            "merge_group",
            serde_json::json!({
                "event": "merge_group",
                "merge_group": {"id": 1},
                "merge_group_id": 1,
                "raw": {"merge_group": {"id": 1}},
            }),
            "MergeGroup",
        ),
        (
            "installation",
            serde_json::json!({
                "event": "installation",
                "installation": {"id": 1},
                "installation_state": "active",
                "suspended": false,
                "raw": {"installation": {"id": 1}},
            }),
            "Installation",
        ),
        (
            "installation_repositories",
            serde_json::json!({
                "event": "installation_repositories",
                "installation": {"id": 1},
                "repository_selection": "selected",
                "repositories_added": [],
                "repositories_removed": [{"id": 7}],
                "raw": {"installation": {"id": 1}},
            }),
            "InstallationRepositories",
        ),
    ];
    for (kind, raw, want_variant) in cases {
        let payload = ProviderPayload::normalize(
            &provider,
            kind,
            &github_headers(kind, "delivery"),
            raw.clone(),
        )
        .unwrap_or_else(|_| panic!("normalize {kind}"));
        let serialized = serde_json::to_value(&payload).expect("serialize");
        let deserialized: ProviderPayload =
            serde_json::from_value(serialized.clone()).expect("deserialize");
        let actual_variant = match unwrap_github(deserialized) {
            GitHubEventPayload::Issues(_) => "Issues",
            GitHubEventPayload::PullRequest(_) => "PullRequest",
            GitHubEventPayload::IssueComment(_) => "IssueComment",
            GitHubEventPayload::PullRequestReview(_) => "PullRequestReview",
            GitHubEventPayload::Push(_) => "Push",
            GitHubEventPayload::WorkflowRun(_) => "WorkflowRun",
            GitHubEventPayload::DeploymentStatus(_) => "DeploymentStatus",
            GitHubEventPayload::CheckRun(_) => "CheckRun",
            GitHubEventPayload::CheckSuite(_) => "CheckSuite",
            GitHubEventPayload::Status(_) => "Status",
            GitHubEventPayload::MergeGroup(_) => "MergeGroup",
            GitHubEventPayload::Installation(_) => "Installation",
            GitHubEventPayload::InstallationRepositories(_) => "InstallationRepositories",
            GitHubEventPayload::Other(_) => "Other",
        };
        assert_eq!(
            actual_variant, *want_variant,
            "{kind} round-tripped as {actual_variant}, expected {want_variant}; serialized form: {serialized}"
        );
    }
}

#[test]
fn provider_normalizes_stream_payloads() {
    let payload = ProviderPayload::normalize(
        &ProviderId::from("kafka"),
        "quote.tick",
        &BTreeMap::from([("x-source".to_string(), "feed".to_string())]),
        serde_json::json!({
            "topic": "quotes",
            "partition": 7,
            "offset": "42",
            "key": "AAPL",
            "timestamp": "2026-04-21T12:00:00Z"
        }),
    )
    .expect("stream payload");
    let ProviderPayload::Known(KnownProviderPayload::Kafka(payload)) = payload else {
        panic!("expected kafka stream payload")
    };
    assert_eq!(payload.event, "quote.tick");
    assert_eq!(payload.stream.as_deref(), Some("quotes"));
    assert_eq!(payload.partition.as_deref(), Some("7"));
    assert_eq!(payload.offset.as_deref(), Some("42"));
    assert_eq!(payload.key.as_deref(), Some("AAPL"));
    assert_eq!(payload.timestamp.as_deref(), Some("2026-04-21T12:00:00Z"));
}

/// Parity between the hand-written `GitHubEventPayload` family in `payloads`
/// and the structs generated from the canonical Harn schema module by
/// `harn connector-schema-codegen` (see `schemas_generated`).
///
/// This is the test that will later license deleting the hand-written structs:
/// it proves the Harn schema captures the same wire shape, so the same webhook
/// JSON deserializes into either copy to the same logical value.
mod schemas_generated_parity {
    use crate::triggers::event::schemas_generated as gen;
    use serde_json::{json, Value as JsonValue};

    /// One representative normalized payload per GitHub event kind, shaped like
    /// the connector's normalized output: the flattened common block
    /// (`event`/`action`/`delivery_id`/`installation_id`/`topic`/`repository`/
    /// `repo`/`raw`) plus the event-specific promoted fields.
    fn github_fixtures() -> Vec<(&'static str, JsonValue)> {
        let common = |event: &str| {
            json!({
                "event": event,
                "action": "opened",
                "delivery_id": "delivery-1",
                "installation_id": 4242,
                "topic": format!("github.{event}.opened"),
                "repository": {"full_name": "octo-org/octo-repo"},
                "repo": {"owner": "octo-org", "name": "octo-repo"},
                "raw": {"event": event},
            })
        };
        let with = |event: &str, extra: JsonValue| {
            let mut base = common(event);
            let obj = base.as_object_mut().unwrap();
            for (k, v) in extra.as_object().unwrap() {
                obj.insert(k.clone(), v.clone());
            }
            base
        };
        vec![
            ("Issues", with("issues", json!({"issue": {"number": 1}}))),
            (
                "PullRequest",
                with("pull_request", json!({"pull_request": {"number": 2}})),
            ),
            (
                "IssueComment",
                with(
                    "issue_comment",
                    json!({"issue": {"number": 1}, "comment": {"id": 9}}),
                ),
            ),
            (
                "PullRequestReview",
                with(
                    "pull_request_review",
                    json!({"pull_request": {"number": 2}, "review": {"state": "approved"}}),
                ),
            ),
            (
                "Push",
                with(
                    "push",
                    json!({"commits": [{"id": "abc"}], "distinct_size": 1}),
                ),
            ),
            (
                "WorkflowRun",
                with("workflow_run", json!({"workflow_run": {"id": 7}})),
            ),
            (
                "DeploymentStatus",
                with(
                    "deployment_status",
                    json!({"deployment_status": {"state": "success"}, "deployment": {"id": 5}}),
                ),
            ),
            (
                "CheckRun",
                with("check_run", json!({"check_run": {"id": 3}})),
            ),
            (
                "CheckSuite",
                with(
                    "check_suite",
                    json!({
                        "check_suite": {"id": 8101},
                        "check_suite_id": 8101,
                        "head_sha": "deadbeef",
                        "head_ref": "feature/x",
                        "status": "queued",
                    }),
                ),
            ),
            (
                "Status",
                with(
                    "status",
                    json!({
                        "commit_status": {"id": 9},
                        "status_id": 9,
                        "state": "success",
                        "context": "ci",
                    }),
                ),
            ),
            (
                "MergeGroup",
                with(
                    "merge_group",
                    json!({
                        "merge_group": {"id": 1},
                        "merge_group_id": 9201,
                        "head_sha": "cafef00d",
                        "pull_requests": [{"number": 2}],
                        "pull_request_numbers": [2],
                    }),
                ),
            ),
            (
                "Installation",
                with(
                    "installation",
                    json!({
                        "installation": {"id": 1},
                        "installation_state": "active",
                        "suspended": false,
                        "repositories": [{"id": 7}],
                    }),
                ),
            ),
            (
                "InstallationRepositories",
                with(
                    "installation_repositories",
                    json!({
                        "installation": {"id": 1},
                        "repository_selection": "selected",
                        "repositories_added": [{"id": 7}],
                        "repositories_removed": [],
                    }),
                ),
            ),
            // Unknown event kind -> the `Other` common-record fallback.
            ("Other", common("label")),
        ]
    }

    /// The variant name the generated enum resolves to, for a parity assertion
    /// against the expected kind.
    fn gen_variant(payload: &gen::GitHubEventPayload) -> &'static str {
        match payload {
            gen::GitHubEventPayload::Issues(_) => "Issues",
            gen::GitHubEventPayload::PullRequest(_) => "PullRequest",
            gen::GitHubEventPayload::IssueComment(_) => "IssueComment",
            gen::GitHubEventPayload::PullRequestReview(_) => "PullRequestReview",
            gen::GitHubEventPayload::Push(_) => "Push",
            gen::GitHubEventPayload::WorkflowRun(_) => "WorkflowRun",
            gen::GitHubEventPayload::DeploymentStatus(_) => "DeploymentStatus",
            gen::GitHubEventPayload::CheckRun(_) => "CheckRun",
            gen::GitHubEventPayload::CheckSuite(_) => "CheckSuite",
            gen::GitHubEventPayload::Status(_) => "Status",
            gen::GitHubEventPayload::MergeGroup(_) => "MergeGroup",
            gen::GitHubEventPayload::Installation(_) => "Installation",
            gen::GitHubEventPayload::InstallationRepositories(_) => "InstallationRepositories",
            gen::GitHubEventPayload::Other(_) => "Other",
        }
    }

    fn handwritten_variant(payload: &super::super::GitHubEventPayload) -> &'static str {
        use super::super::GitHubEventPayload as P;
        match payload {
            P::Issues(_) => "Issues",
            P::PullRequest(_) => "PullRequest",
            P::IssueComment(_) => "IssueComment",
            P::PullRequestReview(_) => "PullRequestReview",
            P::Push(_) => "Push",
            P::WorkflowRun(_) => "WorkflowRun",
            P::DeploymentStatus(_) => "DeploymentStatus",
            P::CheckRun(_) => "CheckRun",
            P::CheckSuite(_) => "CheckSuite",
            P::Status(_) => "Status",
            P::MergeGroup(_) => "MergeGroup",
            P::Installation(_) => "Installation",
            P::InstallationRepositories(_) => "InstallationRepositories",
            P::Other(_) => "Other",
        }
    }

    /// (a) The generated structs deserialize the normalized GitHub webhook JSON
    /// without loss, and (b) the SAME JSON deserializes into the generated and
    /// the hand-written payloads, landing on the same variant and serializing
    /// back to the same logical JSON value.
    #[test]
    fn generated_github_payloads_match_handwritten() {
        for (want_variant, normalized) in github_fixtures() {
            let gen_payload: gen::GitHubEventPayload = serde_json::from_value(normalized.clone())
                .unwrap_or_else(|e| panic!("generated deserialize {want_variant}: {e}"));
            let hand_payload: super::super::GitHubEventPayload =
                serde_json::from_value(normalized.clone())
                    .unwrap_or_else(|e| panic!("hand-written deserialize {want_variant}: {e}"));

            assert_eq!(
                gen_variant(&gen_payload),
                want_variant,
                "generated enum picked the wrong variant for {want_variant}"
            );
            assert_eq!(
                handwritten_variant(&hand_payload),
                want_variant,
                "hand-written enum picked the wrong variant for {want_variant}"
            );

            // Logical-value parity: both serialize back to the same JSON. The
            // fixtures populate every common field, so the one intentional
            // serde divergence (generated common optionals carry
            // `skip_serializing_if`, the hand-written ones do not) does not
            // manifest here — every field is present.
            let gen_json = serde_json::to_value(&gen_payload).expect("serialize generated");
            let hand_json = serde_json::to_value(&hand_payload).expect("serialize hand-written");
            assert_eq!(
                gen_json, hand_json,
                "generated vs hand-written JSON diverged for {want_variant}"
            );
        }
    }

    /// The canonical schema module embedded in `harn-stdlib` parses and yields
    /// exactly the GitHub type declarations the generated file is built from —
    /// a cheap guard that the source of truth stays well-formed.
    #[test]
    fn canonical_schema_module_parses() {
        let program = harn_parser::parse_source(harn_stdlib::CONNECTOR_EVENT_SCHEMAS_SOURCE)
            .expect("canonical connector schema module parses");
        let type_decls = program
            .iter()
            .filter(|node| matches!(node.node, harn_parser::Node::TypeDecl { .. }))
            .count();
        // 1 common + 13 GitHub payload records + 1 GitHub union enum + 4
        // forge-agnostic records.
        assert_eq!(type_decls, 19, "unexpected schema type-declaration count");
    }
}
