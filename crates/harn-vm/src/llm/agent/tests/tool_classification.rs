use super::*;

#[test]
fn read_only_classification_follows_tool_kind_annotations() {
    // Tools declared with ACP Read|Search|Think|Fetch kinds are read-only;
    // Edit|Delete|Move|Execute|Other are not. The VM reads from the
    // active policy's tool_annotations registry — no hardcoded names.
    let mut registry = std::collections::BTreeMap::new();
    for (name, kind) in [
        ("read", ToolKind::Read),
        ("lookup", ToolKind::Read),
        ("search", ToolKind::Search),
        ("outline", ToolKind::Search),
        ("web_search", ToolKind::Search),
        ("web_fetch", ToolKind::Fetch),
        ("think", ToolKind::Think),
        ("write", ToolKind::Edit),
        ("edit", ToolKind::Edit),
        ("delete", ToolKind::Delete),
        ("exec", ToolKind::Execute),
        ("other", ToolKind::Other),
    ] {
        registry.insert(
            name.to_string(),
            ToolAnnotations {
                kind,
                ..Default::default()
            },
        );
    }
    let policy = crate::orchestration::CapabilityPolicy {
        tool_annotations: registry,
        ..Default::default()
    };
    push_execution_policy(policy);

    let is_ro = |name: &str| {
        crate::orchestration::current_tool_annotations(name)
            .map(|a| a.kind.is_read_only())
            .unwrap_or(false)
    };
    assert!(is_ro("read"));
    assert!(is_ro("lookup"));
    assert!(is_ro("search"));
    assert!(is_ro("outline"));
    assert!(is_ro("web_search"));
    assert!(is_ro("web_fetch"));
    assert!(is_ro("think"));
    assert!(!is_ro("write"));
    assert!(!is_ro("edit"));
    assert!(!is_ro("delete"));
    assert!(!is_ro("exec"));
    // Other is NOT read-only (fail-safe).
    assert!(!is_ro("other"));
    // Unannotated tools are NOT read-only.
    assert!(!is_ro("unknown_tool"));
    assert!(!is_ro(""));

    pop_execution_policy();
}

#[test]
fn stop_after_successful_tools_matches_successful_turn() {
    let stop_tools = vec!["edit".to_string(), "scaffold".to_string()];
    let tool_results = vec![
        json!({"tool_name": "read", "status": "ok"}),
        json!({"tool_name": "edit", "status": "ok"}),
    ];
    assert!(should_stop_after_successful_tools(
        &tool_results,
        &stop_tools
    ));
}

#[test]
fn stop_after_successful_tools_ignores_failed_or_unlisted_tools() {
    let stop_tools = vec!["edit".to_string()];
    let failed_results = vec![json!({"tool_name": "edit", "status": "error"})];
    assert!(!should_stop_after_successful_tools(
        &failed_results,
        &stop_tools
    ));

    let unrelated_results = vec![json!({"tool_name": "read", "status": "ok"})];
    assert!(!should_stop_after_successful_tools(
        &unrelated_results,
        &stop_tools
    ));
}

#[test]
fn has_successful_tools_matches_any_required_tool() {
    let required_tools = vec!["edit".to_string(), "create".to_string()];
    let tool_results = vec![
        json!({"tool_name": "lookup", "status": "ok"}),
        json!({"tool_name": "edit", "status": "ok"}),
    ];
    assert!(has_successful_tools(&tool_results, &required_tools));
}

#[test]
fn has_successful_tools_ignores_failed_turns() {
    let required_tools = vec!["edit".to_string()];
    let tool_results = vec![json!({"tool_name": "edit", "status": "error"})];
    assert!(!has_successful_tools(&tool_results, &required_tools));
}

#[tokio::test(flavor = "current_thread")]
async fn require_successful_tools_marks_loop_failed_when_no_write_succeeds() {
    let mut opts = base_opts(vec![serde_json::json!({
        "role": "user",
        "content": "make a deterministic write",
    })]);
    let mut config = base_agent_config();
    config.require_successful_tools = Some(vec!["edit".to_string()]);

    let result = run_agent_loop_internal(&mut opts, config).await.unwrap();
    assert_eq!(result["status"], "failed");
    assert_eq!(result["successful_tools"], json!([]));
}

#[test]
fn tool_kind_is_read_only_excludes_other() {
    // Regression for invariant #5 of the ACP refactor: ToolKind::Other
    // must NOT auto-classify as read-only. Unannotated tools stay out
    // of the concurrent-dispatch fast path by design.
    let annotations = ToolAnnotations {
        kind: ToolKind::Other,
        ..Default::default()
    };
    assert!(!annotations.kind.is_read_only());
    for kind in [
        ToolKind::Read,
        ToolKind::Search,
        ToolKind::Think,
        ToolKind::Fetch,
    ] {
        assert!(kind.is_read_only(), "{:?} must be read-only", kind);
    }
    for kind in [
        ToolKind::Edit,
        ToolKind::Delete,
        ToolKind::Move,
        ToolKind::Execute,
    ] {
        assert!(
            !kind.is_read_only(),
            "{:?} must NOT be read-only (has side effect)",
            kind
        );
    }
}
