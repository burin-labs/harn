use super::*;

#[test]
fn prose_budget_detection_and_trimming_work() {
    let policy = TurnPolicy {
        require_action_or_yield: true,
        allow_done_sentinel: true,
        max_prose_chars: Some(12),
    };
    assert!(prose_exceeds_budget(
        "This prose is definitely too long.",
        Some(&policy)
    ));
    let trimmed = trim_prose_for_history("This prose is definitely too long.", Some(&policy));
    assert!(trimmed.contains("assistant prose truncated by turn policy"));
}

#[test]
fn action_turn_nudge_mentions_action_or_yield() {
    let policy = TurnPolicy {
        require_action_or_yield: true,
        allow_done_sentinel: true,
        max_prose_chars: Some(120),
    };
    let msg = action_turn_nudge("text", Some(&policy), true).expect("nudge");
    assert!(
        msg.contains("either make concrete progress with a well-formed <tool_call> block, switch phase, or emit a <done> block")
    );
    assert!(msg.contains("120"));
    assert!(msg.contains("too much budget on prose"));
}

#[test]
fn sentinel_without_action_nudge_stays_stage_agnostic() {
    let policy = TurnPolicy {
        require_action_or_yield: true,
        allow_done_sentinel: true,
        max_prose_chars: Some(180),
    };
    let msg = sentinel_without_action_nudge("text", Some(&policy));
    assert!(msg.contains("without taking any tool action"));
    assert!(msg.contains("Make concrete progress with an available tool now, or switch phase"));
    assert!(!msg.contains("lookup() or read()"));
    assert!(msg.contains("Keep prose to at most 180 visible characters"));
}

#[test]
fn action_turn_nudge_omits_done_sentinel_when_stage_disallows_it() {
    let policy = TurnPolicy {
        require_action_or_yield: true,
        allow_done_sentinel: false,
        max_prose_chars: Some(90),
    };
    let msg = action_turn_nudge("text", Some(&policy), false).expect("nudge");
    assert!(!msg.contains("<done>"));
    assert!(msg.contains(
        "either make concrete progress with a well-formed <tool_call> block or switch phase"
    ));
}

#[test]
fn sentinel_without_action_nudge_explains_workflow_owned_stage_rule() {
    let policy = TurnPolicy {
        require_action_or_yield: true,
        allow_done_sentinel: false,
        max_prose_chars: Some(90),
    };
    let msg = sentinel_without_action_nudge("text", Some(&policy));
    assert!(msg.contains("workflow-owned action stage"));
    assert!(msg.contains("Do not output a <done> block in this stage"));
}

#[test]
fn native_action_turn_nudge_mentions_native_channel() {
    let policy = TurnPolicy {
        require_action_or_yield: true,
        allow_done_sentinel: false,
        max_prose_chars: Some(90),
    };
    let msg = action_turn_nudge("native", Some(&policy), false).expect("nudge");
    assert!(msg.contains("provider tool channel only"));
    assert!(msg.contains("handwritten tool-call text is invalid"));
}

#[tokio::test(flavor = "current_thread")]
async fn persistent_prompt_omits_done_sentinel_when_stage_disallows_it() {
    reset_llm_mock_state();
    let mut opts = base_opts(vec![serde_json::json!({
        "role": "user",
        "content": "repair the attached file",
    })]);
    let mut config = base_agent_config();
    config.persistent = true;
    config.turn_policy = Some(TurnPolicy {
        require_action_or_yield: false,
        allow_done_sentinel: false,
        max_prose_chars: None,
    });

    let _ = run_agent_loop_internal(&mut opts, config).await.unwrap();
    let calls = get_llm_mock_calls();
    let system = calls
        .last()
        .and_then(|call| call.system.as_ref())
        .expect("mock call system prompt");
    assert!(system.contains("take action with tool calls"));
    assert!(!system.contains("##DONE##"));
    reset_llm_mock_state();
}

#[tokio::test(flavor = "current_thread")]
async fn native_persistent_prompt_stays_on_native_tool_contract() {
    reset_llm_mock_state();
    let mut opts = base_opts(vec![serde_json::json!({
        "role": "user",
        "content": "inspect the workspace",
    })]);
    opts.native_tools = Some(vec![serde_json::json!({
        "type": "function",
        "function": {
            "name": "read",
            "description": "Read a file",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": {"type": "string"}
                },
                "required": ["path"]
            }
        }
    })]);

    let mut config = base_agent_config();
    config.persistent = true;
    config.tool_format = "native".to_string();

    let _ = run_agent_loop_internal(&mut opts, config).await.unwrap();
    let calls = get_llm_mock_calls();
    let system = calls
        .last()
        .and_then(|call| call.system.as_ref())
        .expect("mock call system prompt");
    assert!(system.contains("## Native tool protocol"));
    assert!(!system.contains("## Task ledger"));
    assert!(!system.contains("## Response protocol"));
    assert!(!system.contains("declare function read(args:"));
    assert!(!system.contains("<tool_call>"));
    reset_llm_mock_state();
}

#[tokio::test(flavor = "current_thread")]
async fn native_persistent_prompt_includes_task_ledger_only_when_active() {
    reset_llm_mock_state();
    let mut opts = base_opts(vec![serde_json::json!({
        "role": "user",
        "content": "inspect the workspace",
    })]);
    opts.native_tools = Some(vec![serde_json::json!({
        "type": "function",
        "function": {
            "name": "read",
            "description": "Read a file",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": {"type": "string"}
                },
                "required": ["path"]
            }
        }
    })]);

    let mut config = base_agent_config();
    config.persistent = true;
    config.tool_format = "native".to_string();
    config.task_ledger = crate::llm::ledger::TaskLedger {
        root_task: "Inspect the workspace".to_string(),
        deliverables: vec![crate::llm::ledger::Deliverable {
            id: "deliverable-1".to_string(),
            text: "Read the target file".to_string(),
            status: crate::llm::ledger::DeliverableStatus::Open,
            note: None,
        }],
        rationale: String::new(),
        observations: Vec::new(),
    };

    let _ = run_agent_loop_internal(&mut opts, config).await.unwrap();
    let calls = get_llm_mock_calls();
    let system = calls
        .last()
        .and_then(|call| call.system.as_ref())
        .expect("mock call system prompt");
    assert!(system.contains("## Task ledger"));
    reset_llm_mock_state();
}
