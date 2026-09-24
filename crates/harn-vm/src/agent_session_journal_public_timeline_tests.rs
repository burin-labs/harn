use super::*;
use crate::session_timeline::{query_session_store_timeline, SessionTimelineQuery};
use serde_json::json;

#[tokio::test]
async fn directive_frames_keep_internal_visibility_in_the_persisted_timeline() {
    let store =
        crate::stdlib::canonical_store::CanonicalStore::in_memory().expect("canonical store");
    store
        .create(harn_session_store::CreateSession {
            id: Some("directive-visibility".to_string()),
            ..Default::default()
        })
        .await
        .expect("create session");
    let config = JournalConfig {
        store: store.clone(),
        run_id: "run-visibility".to_string(),
        turn_id: "turn-visibility".to_string(),
        workflow_learning_eligibility: WORKFLOW_LEARNING_EXCLUDED.to_string(),
    };
    // Identical prose is deliberately public when genuinely supplied by the
    // person. Visibility must come from the typed producer marker.
    for raw_message in [
        json!({"role": "user", "content": "Follow this instruction."}),
        json!({
            "role": "user",
            "content": "Follow this instruction.",
            "_harn_directive_ids": ["directive-1"],
        }),
    ] {
        let transcript_event = crate::llm::helpers::vm_value_to_json(
            &crate::llm::helpers::transcript_event_from_message(&crate::stdlib::json_to_vm_value(
                &raw_message,
            )),
        );
        let event = append_event_for_mutation(
            &config,
            TranscriptMutation::MessageAdded {
                transcript_event,
                raw_message,
            },
        )
        .expect("map message");
        store
            .append("directive-visibility", event)
            .await
            .expect("persist message");
    }
    let snapshot = query_session_store_timeline(
        &store,
        SessionTimelineQuery::for_session("directive-visibility"),
    )
    .await
    .expect("query timeline")
    .expect("existing session");
    assert_eq!(snapshot.nodes.len(), 2);
    let events = snapshot
        .nodes
        .iter()
        .map(|node| &node.attributes["transcript_event"])
        .collect::<Vec<_>>();
    assert_eq!(events[0]["visibility"], "public");
    assert_eq!(events[1]["visibility"], "internal");
    assert_eq!(events[1]["blocks"][0]["visibility"], "internal");
    assert_eq!(events[0]["text"], events[1]["text"]);
}

#[tokio::test(flavor = "current_thread")]
async fn native_tool_narration_survives_the_live_journal_and_result_merge() {
    crate::agent_sessions::reset_session_store();
    crate::reset_thread_local_state();
    let root = tempfile::tempdir().expect("project root");
    let root_literal =
        serde_json::to_string(root.path().to_str().expect("UTF-8 root")).expect("root literal");
    let source = format!(
        r###"
import {{ agent_loop }} from "std/agent/loop"

pipeline main(harness: Harness, task: unknown) {{
  harness.llm.mock_enqueue({{text: "I will inspect the source.", tool_calls: [{{
    id: "narrated-call", name: "noop", arguments: {{path: "first.rs"}},
  }}]}})
  harness.llm.mock_enqueue({{text: "I will inspect both related files.", tool_calls: [
    {{id: "related-call-1", name: "noop", arguments: {{path: "second.rs"}}}},
    {{id: "related-call-2", name: "noop", arguments: {{path: "third.rs"}}}},
  ]}})
  harness.llm.mock_enqueue({{text: "##DONE##"}})
  let tools = tool_registry()
  tools = tool_define(tools, "noop", "Read the deterministic source.", {{
    handler: {{ _args -> "known-source-contents" }},
    parameters: {{path: {{type: "string"}}}}, returns: {{type: "string"}},
  }})
  const result = agent_loop(harness, "Use noop, then finish.", nil, {{
    provider: "mock", model: "mock", root: {root_literal},
    session_id: "narrated-native-tool", tools: tools, progress_tool: {{}},
    tool_format: "native", loop_until_done: true, max_iterations: 4,
  }})
  harness.stdio.println(result.status)
}}
"###,
    );
    let chunk = crate::compile_source(&source).expect("compile agent loop");
    let local = tokio::task::LocalSet::new();
    let output = local
        .run_until(async {
            let mut vm = crate::Vm::new();
            crate::register_vm_stdlib(&mut vm);
            vm.execute(&chunk).await.expect("run mocked agent loop");
            vm.output().to_string()
        })
        .await;
    assert!(output.lines().any(|line| line.ends_with("done")));
    let snapshot = crate::session_timeline::query_persisted_session_timeline(
        root.path(),
        SessionTimelineQuery::for_session("narrated-native-tool"),
    )
    .await
    .expect("query canonical timeline")
    .expect("persisted agent session");
    let tools = snapshot
        .nodes
        .iter()
        .filter(|node| node.category == "tool")
        .collect::<Vec<_>>();
    assert_eq!(tools.len(), 3, "all three native calls must really execute");
    for tool in &tools {
        assert_eq!(tool.name, "noop");
        assert_eq!(tool.status, "completed");
        assert!(tool.start_ms.is_some());
        assert!(tool.duration_ms.is_some());
        assert!(tool.attributes["output"]
            .to_string()
            .contains("known-source-contents"));
    }
    let narration = snapshot
        .nodes
        .iter()
        .filter(|node| node.category == "message" && node.name == "I will inspect the source.")
        .collect::<Vec<_>>();
    assert_eq!(
        narration.len(),
        1,
        "public narration must survive exactly once"
    );
    assert!(narration[0].order < tools[0].order);
    assert_eq!(narration[0].attributes["role"], "assistant");
    let related = snapshot
        .nodes
        .iter()
        .filter(|node| {
            node.category == "message" && node.name == "I will inspect both related files."
        })
        .collect::<Vec<_>>();
    assert_eq!(
        related.len(),
        1,
        "one message can narrate multiple tool calls"
    );
    assert!(related[0].order > tools[0].order);
    assert!(related[0].order < tools[1].order);
    for (tool, path) in tools.iter().zip(["first.rs", "second.rs", "third.rs"]) {
        assert_eq!(tool.attributes["input"], json!({"path": path}));
    }

    let narration_event = narration[0]
        .references
        .iter()
        .find(|reference| reference.kind == "session_event")
        .and_then(|reference| reference.event_id)
        .expect("narration must retain its canonical source event");
    let mut incremental_query = SessionTimelineQuery::for_session("narrated-native-tool");
    incremental_query.from_cursor.topics.insert(
        "session-store:narrated-native-tool".to_string(),
        narration_event,
    );
    let incremental =
        crate::session_timeline::query_persisted_session_timeline(root.path(), incremental_query)
            .await
            .expect("query incremental timeline")
            .expect("persisted agent session");
    assert_eq!(
        incremental
            .nodes
            .iter()
            .filter(|node| node.id == related[0].id)
            .count(),
        1,
    );
    for tool in tools {
        let updated = incremental
            .nodes
            .iter()
            .find(|node| node.id == tool.id)
            .expect("tool identity must agree across complete and incremental reads");
        assert_eq!(updated.status, tool.status);
        assert_eq!(updated.attributes["output"], tool.attributes["output"]);
    }
}

#[tokio::test]
async fn raw_tool_arguments_survive_canonical_result_merging() {
    let store =
        crate::stdlib::canonical_store::CanonicalStore::in_memory().expect("canonical store");
    store
        .create(harn_session_store::CreateSession {
            id: Some("raw-arguments".to_string()),
            ..Default::default()
        })
        .await
        .expect("create session");
    let config = JournalConfig {
        store: store.clone(),
        run_id: "run-arguments".to_string(),
        turn_id: "turn-arguments".to_string(),
        workflow_learning_eligibility: WORKFLOW_LEARNING_EXCLUDED.to_string(),
    };
    for transcript_event in [
        json!({
            "id": "call-event", "kind": "tool_call", "role": "assistant",
            "metadata": {
                "tool_call_id": "call-1", "tool_name": "read_file",
                "raw_input": {"path": "known-source.rs"},
            },
        }),
        json!({
            "id": "result-event", "kind": "tool_result", "role": "tool",
            "metadata": {
                "tool_call_id": "call-1", "output": "known source contents",
            },
        }),
    ] {
        let event = append_event_for_mutation(
            &config,
            TranscriptMutation::AuditEventAdded { transcript_event },
        )
        .expect("map tool event");
        store
            .append("raw-arguments", event)
            .await
            .expect("journal tool event");
    }
    let snapshot =
        query_session_store_timeline(&store, SessionTimelineQuery::for_session("raw-arguments"))
            .await
            .expect("query timeline")
            .expect("existing session");
    assert_eq!(snapshot.nodes.len(), 1);
    assert_eq!(snapshot.nodes[0].status, "completed");
    assert_eq!(
        snapshot.nodes[0].attributes["output"],
        "known source contents"
    );
    assert_eq!(
        snapshot.nodes[0].attributes["input"],
        json!({"path": "known-source.rs"})
    );
}
