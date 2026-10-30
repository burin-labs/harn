use super::*;
use crate::llm::mock::reset_llm_mock_state;
use crate::value::VmDictExt;
use crate::NativeBuiltinCtxExt;

#[path = "agents_sub_agent_physical_usage_tests.rs"]
mod physical_usage_tests;

struct ExecutionPolicyGuard;

impl Drop for ExecutionPolicyGuard {
    fn drop(&mut self) {
        crate::orchestration::clear_execution_policy_stacks();
    }
}

fn push_recursion_policy(limit: usize) -> ExecutionPolicyGuard {
    crate::orchestration::clear_execution_policy_stacks();
    crate::orchestration::push_execution_policy(CapabilityPolicy {
        recursion_limit: Some(limit),
        ..CapabilityPolicy::default()
    });
    ExecutionPolicyGuard
}

#[test]
fn stop_details_preserve_cancel_and_timeout_provenance() {
    let cancelled = stop_details_for_error(&VmError::CategorizedError {
        message: "parent cancelled".to_string(),
        category: crate::value::ErrorCategory::Cancelled,
    });
    assert_eq!(
        cancelled.status,
        crate::agent_events::SubagentTerminalStatus::Cancellation
    );
    assert!(cancelled.cancellation.is_some());
    assert!(cancelled.timeout.is_none());

    let timed_out = stop_details_for_error(&VmError::CategorizedError {
        message: "deadline elapsed".to_string(),
        category: crate::value::ErrorCategory::Timeout,
    });
    assert_eq!(
        timed_out.status,
        crate::agent_events::SubagentTerminalStatus::Timeout
    );
    assert!(timed_out.timeout.is_some());
    assert!(timed_out.cancellation.is_none());

    let failed = stop_details_for_result(&serde_json::json!({
        "status": "provider_error",
        "stop_reason": "provider_error",
        "error": {"category": "provider"}
    }));
    assert_eq!(
        failed.status,
        crate::agent_events::SubagentTerminalStatus::Failure
    );
    assert_eq!(failed.terminal_class, "provider_error");

    let succeeded = stop_details_for_result(&serde_json::json!({
        "status": "done",
        "stop_reason": "completed"
    }));
    assert_eq!(
        succeeded.status,
        crate::agent_events::SubagentTerminalStatus::Success
    );
}

#[test]
fn base_envelope_carries_files_written_and_usage_then_drains() {
    let session = format!("test-files-written-{}", uuid::Uuid::now_v7());
    crate::agent_sessions::clear_session_changed_paths(&session);
    crate::agent_sessions::record_session_changed_path(&session, "src/alpha.rs");
    crate::agent_sessions::record_session_changed_path(&session, "src/beta.rs");

    let envelope = sub_agent_base_envelope(
        "did the work".to_string(),
        VmValue::list(Vec::new()),
        0,
        1234,
        false,
        &session,
    );

    let files = envelope
        .get("files_written")
        .and_then(|value| match value {
            VmValue::List(list) => Some(list.clone()),
            _ => None,
        })
        .expect("files_written is a list");
    let paths: Vec<String> = files
        .iter()
        .filter_map(|value| match value {
            VmValue::String(text) => Some(text.to_string()),
            _ => None,
        })
        .collect();
    assert!(
        paths.contains(&"src/alpha.rs".to_string()),
        "alpha written: {paths:?}"
    );
    assert!(
        paths.contains(&"src/beta.rs".to_string()),
        "beta written: {paths:?}"
    );

    let total = envelope
        .get("usage")
        .and_then(VmValue::as_dict)
        .and_then(|usage| usage.get("total_tokens"))
        .and_then(VmValue::as_int)
        .expect("usage.total_tokens present");
    assert_eq!(total, 1234, "usage carries the child's total tokens");

    // The base envelope DRAINS the session record, so a second build sees no
    // double-counted writes.
    let again = sub_agent_base_envelope(
        "again".to_string(),
        VmValue::list(Vec::new()),
        0,
        0,
        false,
        &session,
    );
    let again_files = again.get("files_written").and_then(|value| match value {
        VmValue::List(list) => Some(list.len()),
        _ => None,
    });
    assert_eq!(
        again_files,
        Some(0),
        "files_written drains after the first build"
    );
}

fn assistant_message(text: &str) -> VmValue {
    VmValue::dict([
        ("role", VmValue::string("assistant")),
        ("content", VmValue::string(text)),
    ])
}

fn normalized_request(extra: Vec<(&str, VmValue)>) -> VmValue {
    let mut request = crate::value::DictMap::from_iter([
        (
            crate::value::intern_key("_type"),
            VmValue::string("sub_agent_request"),
        ),
        (
            crate::value::intern_key("task"),
            VmValue::string("summarize"),
        ),
    ]);
    for (key, value) in extra {
        request.insert(crate::value::intern_key(key), value);
    }
    VmValue::dict(request)
}

#[test]
fn parse_sub_agent_request_rejects_unknown_top_level_options() {
    let request = normalized_request(vec![("backgrund", VmValue::Bool(true))]);

    let err = match parse_sub_agent_request(&[request]) {
        Ok(_) => panic!("expected unknown option failure"),
        Err(err) => err,
    };

    match err {
        VmError::Runtime(message) => assert!(message.contains("backgrund"), "got: {message}"),
        other => panic!("expected Runtime error, got {other:?}"),
    }
}

#[test]
fn parse_sub_agent_request_preserves_session_id_whitespace() {
    let request = normalized_request(vec![("session_id", VmValue::string("  child-session  "))]);

    let parsed = parse_sub_agent_request(&[request]).unwrap();

    assert_eq!(parsed.spec.session_id, "  child-session  ");
}

fn anchor_dict(primary: &str, additional: Vec<(&str, &str)>) -> VmValue {
    let mut roots = Vec::new();
    for (path, mount_mode) in additional {
        roots.push(VmValue::dict([
            ("path", VmValue::string(path)),
            ("mount_mode", VmValue::string(mount_mode)),
            ("mounted_at", VmValue::string("2026-05-24T00:00:00Z")),
        ]));
    }
    let mut anchor = crate::value::DictMap::from_iter([
        (
            crate::value::intern_key("primary"),
            VmValue::string(primary),
        ),
        (
            crate::value::intern_key("anchored_at"),
            VmValue::string("2026-05-24T00:00:00Z"),
        ),
    ]);
    if !roots.is_empty() {
        anchor.put("additional_roots", VmValue::list(roots));
    }
    VmValue::dict(anchor)
}

fn path_string(path: &std::path::Path) -> String {
    path.to_string_lossy().into_owned()
}

#[test]
fn parse_sub_agent_request_inherits_parent_anchor_as_execution_cwd() {
    crate::agent_sessions::reset_session_store();
    let dir = tempfile::tempdir().expect("tempdir");
    let project = dir.path().join("project");
    let canonical_project = crate::workspace_anchor::WorkspaceAnchor::canonicalize_path(&project)
        .expect("canonical project path");
    let project_text = path_string(&canonical_project);
    let parent_id =
        crate::agent_sessions::open_or_create_for_test(Some("anchor-inherit-parent".to_string()));
    crate::agent_sessions::set_workspace_anchor(
        &parent_id,
        Some(crate::workspace_anchor::WorkspaceAnchor {
            primary: project,
            additional_roots: Vec::new(),
            anchored_at: "2026-05-24T00:00:00Z".to_string(),
        }),
    )
    .unwrap();
    let _guard = crate::agent_sessions::enter_current_session(parent_id.clone());

    let parsed = parse_sub_agent_request(&[normalized_request(Vec::new())]).unwrap();

    assert_eq!(
        parsed.spec.parent_session_id.as_deref(),
        Some(parent_id.as_str())
    );
    assert_eq!(
        parsed.spec.workspace_anchor.as_ref().unwrap().primary,
        canonical_project
    );
    assert_eq!(
        parsed.execution.cwd.as_deref(),
        Some(project_text.as_str()),
        "unanchored child workers default their sandbox execution cwd to the parent workspace"
    );
}

#[test]
fn parse_sub_agent_request_preserves_explicit_execution_cwd() {
    crate::agent_sessions::reset_session_store();
    let dir = tempfile::tempdir().expect("tempdir");
    let project = dir.path().join("project");
    let child = project.join("child");
    let explicit_cwd = dir.path().join("explicit-cwd");
    let explicit_cwd_text = path_string(&explicit_cwd);
    let canonical_child = crate::workspace_anchor::WorkspaceAnchor::canonicalize_path(&child)
        .expect("canonical child path");
    let parent_id = crate::agent_sessions::open_or_create_for_test(Some(
        "anchor-explicit-cwd-parent".to_string(),
    ));
    crate::agent_sessions::set_workspace_anchor(
        &parent_id,
        Some(crate::workspace_anchor::WorkspaceAnchor {
            primary: project,
            additional_roots: Vec::new(),
            anchored_at: "2026-05-24T00:00:00Z".to_string(),
        }),
    )
    .unwrap();
    let _guard = crate::agent_sessions::enter_current_session(parent_id);

    let parsed = parse_sub_agent_request(&[normalized_request(vec![
        ("anchor", anchor_dict(&path_string(&child), Vec::new())),
        (
            "execution",
            VmValue::dict([("cwd", VmValue::string(explicit_cwd_text.clone()))]),
        ),
    ])])
    .unwrap();

    assert_eq!(
        parsed.spec.workspace_anchor.as_ref().unwrap().primary,
        canonical_child
    );
    assert_eq!(
        parsed.execution.cwd.as_deref(),
        Some(explicit_cwd_text.as_str()),
        "explicit execution.cwd remains authoritative"
    );
}

#[test]
fn parse_sub_agent_request_accepts_anchor_in_parent_scope() {
    crate::agent_sessions::reset_session_store();
    let dir = tempfile::tempdir().expect("tempdir");
    let parent = dir.path().join("parent");
    let sibling = dir.path().join("sibling");
    let parent_nested = parent.join("nested");
    let sibling_sub = sibling.join("sub");
    let parent_nested_text = path_string(&parent_nested);
    let sibling_sub_text = path_string(&sibling_sub);
    let parent_id =
        crate::agent_sessions::open_or_create_for_test(Some("anchor-parent".to_string()));
    crate::agent_sessions::set_workspace_anchor(
        &parent_id,
        Some(crate::workspace_anchor::WorkspaceAnchor {
            primary: parent,
            additional_roots: vec![crate::workspace_anchor::MountedRoot {
                path: sibling,
                mount_mode: crate::workspace_anchor::MountMode::Extend,
                mounted_at: "2026-05-24T00:00:00Z".to_string(),
            }],
            anchored_at: "2026-05-24T00:00:00Z".to_string(),
        }),
    )
    .unwrap();
    let _guard = crate::agent_sessions::enter_current_session(parent_id);

    for primary in [&parent_nested_text, &sibling_sub_text] {
        let request = normalized_request(vec![("anchor", anchor_dict(primary, Vec::new()))]);
        let parsed = parse_sub_agent_request(&[request])
            .unwrap_or_else(|err| panic!("anchor {primary} should be accepted: {err}"));
        let canonical_primary = crate::workspace_anchor::WorkspaceAnchor::canonicalize_path(
            std::path::Path::new(primary),
        )
        .expect("canonical child primary");
        assert_eq!(
            parsed.spec.workspace_anchor.as_ref().unwrap().primary,
            canonical_primary
        );
    }
}

#[test]
fn parse_sub_agent_request_rejects_anchor_outside_parent_scope() {
    crate::agent_sessions::reset_session_store();
    let dir = tempfile::tempdir().expect("tempdir");
    let parent = dir.path().join("parent");
    let outside = dir.path().join("elsewhere");
    let outside_text = path_string(&outside);
    let parent_id =
        crate::agent_sessions::open_or_create_for_test(Some("anchor-parent".to_string()));
    crate::agent_sessions::set_workspace_anchor(
        &parent_id,
        Some(crate::workspace_anchor::WorkspaceAnchor {
            primary: parent,
            additional_roots: Vec::new(),
            anchored_at: "2026-05-24T00:00:00Z".to_string(),
        }),
    )
    .unwrap();
    let _guard = crate::agent_sessions::enter_current_session(parent_id);

    let request = normalized_request(vec![("anchor", anchor_dict(&outside_text, Vec::new()))]);
    let err = parse_sub_agent_request(&[request])
        .err()
        .expect("expected anchor escape to fail");
    match err {
        VmError::Runtime(message) => {
            assert!(
                message.contains("child anchor escapes parent"),
                "unexpected error: {message}"
            );
        }
        other => panic!("expected Runtime error, got {other:?}"),
    }
}

#[test]
fn parse_sub_agent_request_anchor_without_parent_passes() {
    crate::agent_sessions::reset_session_store();
    let request = normalized_request(vec![(
        "anchor",
        anchor_dict("/workspace/anywhere", Vec::new()),
    )]);
    let parsed = parse_sub_agent_request(&[request]).unwrap();
    assert_eq!(
        parsed.spec.workspace_anchor.as_ref().unwrap().primary,
        std::path::PathBuf::from("/workspace/anywhere")
    );
}

#[test]
fn synthesize_summary_uses_prior_assistant_json_from_transcript() {
    let transcript = crate::llm::helpers::new_transcript_with(
        None,
        vec![
            assistant_message("{\"answer\":\"ok\"}"),
            assistant_message("##DONE##"),
        ],
        None,
        None,
    );
    let result = serde_json::json!({
        "visible_text": "##DONE##",
        "text": "##DONE##",
    });

    let synthesized = synthesize_sub_agent_result(&result, &transcript, true);

    assert_eq!(synthesized.summary, "{\"answer\":\"ok\"}");
    assert_eq!(
        synthesized
            .structured_json
            .as_ref()
            .and_then(|candidate| candidate.value.get("answer"))
            .and_then(|value| value.as_str()),
        Some("ok")
    );
}

#[test]
fn synthesize_summary_falls_back_to_assistant_event_history() {
    let transcript = crate::llm::helpers::new_transcript_with_events(
        None,
        Vec::new(),
        None,
        None,
        vec![crate::llm::helpers::transcript_event(
            "message",
            "assistant",
            "public",
            "{\"paths\":[\"src/lib.rs\"]}",
            None,
        )],
        Vec::new(),
        Some("active"),
    );
    let result = serde_json::json!({
        "visible_text": "",
        "text": "",
    });

    let synthesized = synthesize_sub_agent_result(&result, &transcript, true);

    assert_eq!(synthesized.summary, "{\"paths\":[\"src/lib.rs\"]}");
    assert_eq!(
        synthesized
            .structured_json
            .as_ref()
            .and_then(|candidate| candidate.value.get("paths"))
            .and_then(|value| value.as_array())
            .map(Vec::len),
        Some(1)
    );
}

#[test]
fn declared_output_never_falls_back_to_an_earlier_transcript_json() {
    let transcript = crate::llm::helpers::new_transcript_with(
        None,
        vec![assistant_message("{\"answer\":42}")],
        None,
        None,
    );
    let synthesized = synthesize_sub_agent_declared_output(
        &serde_json::json!({
            "visible_text": "{\"answer\":\"wrong\"}",
            "output_valid": false,
        }),
        &transcript,
    );

    assert!(
        synthesized.structured_json.is_none(),
        "an invalid declared terminal output must not borrow transcript JSON"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn execute_sub_agent_propagates_nested_budget_denial() {
    crate::agent_sessions::reset_session_store();
    reset_llm_mock_state();
    let _policy = push_recursion_policy(0);
    let parent = crate::agent_sessions::open_or_create_for_test(Some("parent-budget".into()));
    let mut options = crate::value::DictMap::from_iter([
        (
            crate::value::intern_key("provider"),
            VmValue::string("mock"),
        ),
        (crate::value::intern_key("model"), VmValue::string("mock")),
        (crate::value::intern_key("max_iterations"), VmValue::Int(1)),
    ]);
    annotate_nested_execution_options(
        &mut options,
        NestedExecutionKind::SubAgentRun,
        "budgeted-worker",
    );

    let spec = SubAgentRunSpec {
        name: "budgeted-worker".to_string(),
        task: "inspect the repo".to_string(),
        system: None,
        options,
        returns_schema: None,
        session_id: "child-budget".to_string(),
        run_id: "agent_run_child_budget".to_string(),
        parent_session_id: Some(parent.clone()),
        parent_run_id: Some("agent_run_parent_budget".to_string()),
        reminder_propagation: Vec::new(),
        workspace_anchor: None,
        stop_emitted: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
    };

    let mut vm = crate::new_vm();
    vm.set_harness(crate::Harness::real());
    crate::register_vm_stdlib(&mut vm);
    let ctx = crate::vm::AsyncBuiltinCtx::for_test(vm);
    let result = execute_sub_agent(&ctx, spec).await.unwrap();

    assert_eq!(result.payload["ok"].as_bool(), Some(false));
    assert_eq!(result.payload["budget_exceeded"].as_bool(), Some(true));
    assert_eq!(
        result.payload["usage"]["token_scope"],
        "every_provider_call"
    );
    assert_eq!(result.payload["usage"]["status"], "not_run");
    assert_eq!(result.payload["usage"]["provider_call_count"], 0);
    assert_eq!(result.payload["usage"]["total_tokens"], 0);
    assert_eq!(
        result.payload["error"]["category"].as_str(),
        Some("budget_exceeded")
    );
    assert!(
        result.payload["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("sub_agent_run")),
        "{:?}",
        result.payload
    );
    assert!(result
        .payload
        .get("transcript")
        .and_then(|transcript| transcript.get("events"))
        .and_then(|events| events.as_array())
        .is_some_and(|events| events
            .iter()
            .any(|event| event["kind"] == "nested_execution_budget_denied")));

    let parent_events = crate::agent_sessions::snapshot(&parent)
        .and_then(|value| value.as_dict().cloned())
        .and_then(|dict| dict.get("events").cloned())
        .and_then(|value| match value {
            VmValue::List(list) => Some((*list).clone()),
            _ => None,
        })
        .expect("parent events");
    let result_event = parent_events
        .iter()
        .filter_map(|event| event.as_dict())
        .find(|dict| dict.get("kind").map(VmValue::display).as_deref() == Some("sub_agent_result"))
        .expect("sub_agent_result event");
    let metadata = result_event
        .get("metadata")
        .and_then(|value| value.as_dict())
        .expect("event metadata");
    assert!(matches!(metadata.get("ok"), Some(VmValue::Bool(false))));
    assert!(matches!(
        metadata.get("budget_exceeded"),
        Some(VmValue::Bool(true))
    ));

    reset_llm_mock_state();
    crate::agent_sessions::reset_session_store();
}
