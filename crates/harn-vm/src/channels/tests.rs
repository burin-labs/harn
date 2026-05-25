use super::*;

fn context() -> ChannelContext {
    ChannelContext {
        task_id: Some("task".to_string()),
        root_task_id: Some("root".to_string()),
        ..ChannelContext::default()
    }
}

#[test]
fn resolves_bare_name_to_default_tenant() {
    let resolved = resolve_channel("pr.merged", &ChannelOptions::default(), &context()).unwrap();
    assert_eq!(resolved.scope, ChannelScope::Tenant);
    assert_eq!(resolved.resolved_name, "tenant:default:pr.merged");
    assert_eq!(resolved.topic.as_str(), "channels.tenant.default.pr.merged");
}

#[test]
fn resolves_session_prefix_from_context() {
    let resolved =
        resolve_channel("session:agent.done", &ChannelOptions::default(), &context()).unwrap();
    assert_eq!(resolved.scope, ChannelScope::Session);
    assert_eq!(resolved.resolved_name, "session:root:agent.done");
}

#[test]
fn missing_pipeline_context_reports_channel_error() {
    let err = resolve_channel(
        "pipeline:stage.done",
        &ChannelOptions::default(),
        &context(),
    )
    .unwrap_err();
    assert!(err.0.contains("HARN-CHN-001"));
}

#[test]
fn org_scope_is_disabled() {
    let err = resolve_channel(
        "org:burin-labs:pr.merged",
        &ChannelOptions::default(),
        &context(),
    )
    .unwrap_err();
    assert!(err.0.contains("HARN-CHN-002"));
}

#[test]
fn explicit_session_id_matching_context_resolves() {
    let ctx = ChannelContext {
        agent_session_id: Some("sess-A".to_string()),
        ..context()
    };
    let options = ChannelOptions {
        session_id: Some("sess-A".to_string()),
        ..ChannelOptions::default()
    };
    let resolved = resolve_channel("session:agent.done", &options, &ctx).unwrap();
    assert_eq!(resolved.scope, ChannelScope::Session);
    assert_eq!(resolved.resolved_name, "session:sess-A:agent.done");
}

#[test]
fn explicit_session_id_conflict_reports_ambiguity() {
    let ctx = ChannelContext {
        agent_session_id: Some("sess-A".to_string()),
        ..context()
    };
    let options = ChannelOptions {
        session_id: Some("sess-B".to_string()),
        ..ChannelOptions::default()
    };
    let err = resolve_channel("session:agent.done", &options, &ctx).unwrap_err();
    assert!(
        err.0.contains("HARN-CHN-004"),
        "expected HARN-CHN-004, got: {}",
        err.0
    );
}

#[test]
fn explicit_pipeline_id_conflict_reports_ambiguity() {
    let ctx = ChannelContext {
        workflow_id: Some("pipe-A".to_string()),
        ..context()
    };
    let options = ChannelOptions {
        pipeline_id: Some("pipe-B".to_string()),
        ..ChannelOptions::default()
    };
    let err = resolve_channel("pipeline:stage.done", &options, &ctx).unwrap_err();
    assert!(
        err.0.contains("HARN-CHN-004"),
        "expected HARN-CHN-004, got: {}",
        err.0
    );
}

#[test]
fn explicit_tenant_mismatch_reports_cross_tenant() {
    let ctx = ChannelContext {
        tenant_id: Some("tenant-A".to_string()),
        ..context()
    };
    let options = ChannelOptions {
        tenant_id: Some("tenant-B".to_string()),
        ..ChannelOptions::default()
    };
    let err = resolve_channel("pr.merged", &options, &ctx).unwrap_err();
    assert!(
        err.0.contains("HARN-CHN-002"),
        "expected HARN-CHN-002, got: {}",
        err.0
    );
}

#[test]
fn explicit_tenant_in_name_matching_context_resolves() {
    let ctx = ChannelContext {
        tenant_id: Some("tenant-A".to_string()),
        ..context()
    };
    let resolved = resolve_channel(
        "tenant:tenant-A:pr.merged",
        &ChannelOptions::default(),
        &ctx,
    )
    .unwrap();
    assert_eq!(resolved.scope, ChannelScope::Tenant);
    assert_eq!(resolved.resolved_name, "tenant:tenant-A:pr.merged");
}

#[test]
fn cross_tenant_via_name_prefix_is_rejected() {
    let ctx = ChannelContext {
        tenant_id: Some("tenant-A".to_string()),
        ..context()
    };
    let err = resolve_channel(
        "tenant:tenant-B:pr.merged",
        &ChannelOptions::default(),
        &ctx,
    )
    .unwrap_err();
    assert!(err.0.contains("HARN-CHN-002"));
}

#[test]
fn channel_selector_parses_tenant_default_shorthand() {
    let selector = ChannelSelector::parse("channel:pr.merged").expect("parses");
    assert_eq!(selector.scope(), "tenant");
    assert_eq!(selector.name(), "pr.merged");
    assert!(selector.matches("tenant", "default", "pr.merged", "default"));
    assert!(!selector.matches("tenant", "default", "other.event", "default"));
    assert!(!selector.matches("session", "default", "pr.merged", "default"));
}

#[test]
fn channel_selector_parses_session_scope() {
    let selector = ChannelSelector::parse("channel:session:my-event").expect("parses");
    assert_eq!(selector.scope(), "session");
    assert_eq!(selector.name(), "my-event");
    assert!(selector.matches("session", "any-session-id", "my-event", "any-session-id"));
    assert!(!selector.matches("tenant", "any", "my-event", "any"));
}

#[test]
fn channel_selector_parses_explicit_tenant() {
    let selector = ChannelSelector::parse("channel:tenant:burin-labs:pr.merged").expect("parses");
    assert!(selector.matches("tenant", "burin-labs", "pr.merged", "default"));
    assert!(!selector.matches("tenant", "other-tenant", "pr.merged", "default"));
}

#[test]
fn channel_selector_parses_tenant_wildcard() {
    let selector = ChannelSelector::parse("channel:tenant:*:pr.merged").expect("parses");
    assert!(selector.matches("tenant", "burin-labs", "pr.merged", "default"));
    assert!(selector.matches("tenant", "other-tenant", "pr.merged", "default"));
    assert!(!selector.matches("tenant", "any", "different-event", "default"));
    assert!(!selector.matches("session", "any", "pr.merged", "default"));
}

#[test]
fn channel_selector_rejects_malformed_inputs() {
    assert!(ChannelSelector::parse("not-a-channel").is_err());
    assert!(ChannelSelector::parse("channel:").is_err());
    assert!(ChannelSelector::parse("channel:session:").is_err());
    assert!(ChannelSelector::parse("channel:session:has:extra:colons").is_err());
    assert!(ChannelSelector::parse("channel:org:no-name").is_err());
    assert!(ChannelSelector::parse("channel:tenant::missing-id").is_err());
    assert!(ChannelSelector::parse("channel:tenant:foo:").is_err());
}

#[test]
fn channel_filter_matches_equality_paths() {
    let payload = serde_json::json!({"repo": "harn", "nested": {"k": "v"}});
    assert!(channel_filter_matches("{\"repo\": \"harn\"}", &payload));
    assert!(!channel_filter_matches("{\"repo\": \"other\"}", &payload));
    assert!(channel_filter_matches("{\"nested.k\": \"v\"}", &payload));
    assert!(!channel_filter_matches("{\"nested.k\": \"x\"}", &payload));
    // Missing path → no match.
    assert!(!channel_filter_matches("{\"missing\": \"x\"}", &payload));
    // Empty filter → permissive.
    assert!(channel_filter_matches("", &payload));
    // Non-dict filter → permissive (back-compat with legacy `filter:` field).
    assert!(channel_filter_matches("just-a-string", &payload));
}

// CH-07 (#1878): `channel_payload_hash` is the byte signal the
// replay-oracle's `HARN-REP-CHN-002` diagnostic compares across
// runs. The hash MUST be deterministic across object-key iteration
// order (so the test in `replay_oracle` and the conformance fixture
// both rely on canonical ordering) and MUST change when the payload
// changes.
#[test]
fn channel_payload_hash_is_deterministic_across_key_order() {
    let a = serde_json::json!({"a": 1, "b": 2, "nested": {"x": 10, "y": 20}});
    let b = serde_json::json!({"nested": {"y": 20, "x": 10}, "b": 2, "a": 1});
    assert_eq!(channel_payload_hash(&a), channel_payload_hash(&b));
}

#[test]
fn channel_payload_hash_changes_with_value_drift() {
    let baseline = serde_json::json!({"repo": "harn", "attempt": 1});
    let drifted = serde_json::json!({"repo": "harn", "attempt": 2});
    assert_ne!(
        channel_payload_hash(&baseline),
        channel_payload_hash(&drifted)
    );
}

#[test]
fn channel_payload_hash_is_sha256_prefixed_hex() {
    let value = serde_json::json!({"k": "v"});
    let hash = channel_payload_hash(&value);
    assert!(hash.starts_with("sha256:"));
    assert_eq!(hash.len(), "sha256:".len() + 64);
}
