use std::collections::BTreeMap;

use serde_json::Value;

use crate::support::{run, EnvironmentGuard};

fn provider_events(path: &std::path::Path, kind: &str) -> Vec<Value> {
    std::fs::read_to_string(path.join("llm_transcript.jsonl"))
        .expect("provider transcript")
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).expect("valid transcript JSON"))
        .filter(|event| event.get("type").and_then(Value::as_str) == Some(kind))
        .collect()
}

#[test]
fn real_agent_loop_calls_are_stage_attributed_and_raw_calls_remain_unattributed() {
    let loop_dir = tempfile::tempdir().expect("loop transcript directory");
    {
        let _transcript = EnvironmentGuard::set(
            "HARN_LLM_TRANSCRIPT_DIR",
            loop_dir.path().to_str().expect("UTF-8 temp path"),
        );
        run(r#"
import { agent_loop } from "std/agent/loop"

pipeline main(harness: Harness) {
  const result = agent_loop(
    harness,
    "Return a short answer.",
    nil,
    {provider: "mock", max_iterations: 1},
  )
  require result.llm.iterations == 1, "the smoke must make one loop turn"
}
"#)
        .expect("agent loop smoke");
    }

    let requests = provider_events(loop_dir.path(), "provider_call_request");
    let responses = provider_events(loop_dir.path(), "provider_call_response");
    assert_eq!(
        requests.len(),
        1,
        "the smoke must observe one provider request"
    );
    assert_eq!(
        responses.len(),
        1,
        "the smoke must observe one provider response"
    );
    assert_eq!(requests[0]["stage"], "work");
    assert_eq!(responses[0]["stage"], "work");
    assert_eq!(requests[0]["call_id"], responses[0]["call_id"]);

    let mut by_stage = BTreeMap::<&str, usize>::new();
    for response in &responses {
        let stage = response["stage"].as_str().expect("agent call stage");
        *by_stage.entry(stage).or_default() += 1;
    }
    assert!(!by_stage.is_empty());
    assert_eq!(by_stage.values().sum::<usize>(), responses.len());

    let raw_dir = tempfile::tempdir().expect("raw transcript directory");
    {
        let _transcript = EnvironmentGuard::set(
            "HARN_LLM_TRANSCRIPT_DIR",
            raw_dir.path().to_str().expect("UTF-8 temp path"),
        );
        run(r#"
pipeline main(harness: Harness) {
  const _ = harness.llm.call("Hello", nil, {provider: "mock"})
}
"#)
        .expect("raw LLM smoke");
    }
    let raw_requests = provider_events(raw_dir.path(), "provider_call_request");
    let raw_responses = provider_events(raw_dir.path(), "provider_call_response");
    assert_eq!(raw_requests.len(), 1);
    assert_eq!(raw_responses.len(), 1);
    assert!(raw_requests[0].get("stage").is_none());
    assert!(raw_responses[0].get("stage").is_none());
}

/// One agent turn in `child`, and the `actor_chain` its provider request
/// carried.
fn child_request_actor_chain(open_child: &str) -> Value {
    let dir = tempfile::tempdir().expect("child transcript directory");
    {
        let _transcript = EnvironmentGuard::set(
            "HARN_LLM_TRANSCRIPT_DIR",
            dir.path().to_str().expect("UTF-8 temp path"),
        );
        run(&format!(
            r#"
import {{ agent_loop }} from "std/agent/loop"

pipeline main(harness: Harness) {{
  const root = harness.agent.open(nil)
  const child = {open_child}
  const result = agent_loop(
    harness,
    "Return a short answer.",
    nil,
    {{provider: "mock", max_iterations: 1, session_id: child}},
  )
  require result.llm.iterations == 1, "the child must make one loop turn"
}}
"#
        ))
        .expect("child agent loop");
    }
    let requests = provider_events(dir.path(), "provider_call_request");
    assert_eq!(
        requests.len(),
        1,
        "the child must make one provider request"
    );
    requests[0]["actor_chain"].clone()
}

#[test]
fn a_child_opened_with_an_actor_stamps_its_hop_on_every_provider_request() {
    // harn#8927: a delegated child opened with plain `open(nil)` inherits the
    // parent's chain, so its provider requests are indistinguishable from the
    // primary agent's.
    assert_eq!(
        child_request_actor_chain("harness.agent.open(nil)"),
        Value::Null,
        "a plain open keeps its inherited, here absent, chain"
    );
    assert_eq!(
        child_request_actor_chain(r#"harness.agent.open(nil, {parent: root, actor: "planner"})"#),
        serde_json::json!({"sub": "anonymous", "act": {"sub": "planner"}}),
    );
}
