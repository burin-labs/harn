use super::*;

/// The synthesized result must say the HARNESS wrote it (harn#7757).
///
/// It carries the orphaned call's own name, id and an error outcome, so without
/// this tag it is indistinguishable from a real failed call of that tool, and
/// the completion judge reads the injected veto text back as an observation of
/// the workspace. This is the production write side of that tag; the reading
/// side is `tests/agent/judge_self_citation_test.harn`.
#[test]
fn orphan_repair_result_is_tagged_as_harness_authored() {
    let llm_result = crate::stdlib::json_to_vm_value(&json!({
        "provider": "local",
        "model": "Qwen/Qwen3.6-35B-A3B",
        "text": "",
        "_agent_tool_format": "native",
        "native_tool_calls": [{
            "id": "call_9",
            "name": "read",
            "arguments": {"path": "main.rs"}
        }],
    }));
    let assistant = assistant_message_from_llm_result(&llm_result);
    let synthetic = synthesize_orphan_tool_results(
        &assistant,
        "the required files were never listed",
        &std::collections::BTreeSet::new(),
    );
    assert_eq!(synthetic.len(), 1);
    let msg = vm_to_json(&synthetic[0]);
    assert_eq!(
        msg["_harn"]["origin"], "harness_repair",
        "the repair result must carry its authorship: {msg}"
    );
    // The tag rides on the storage-only envelope, so the message the provider
    // sees is unchanged.
    assert_eq!(msg["name"], "read");
    assert_eq!(msg["tool_call_id"], "call_9");
    assert_eq!(msg["content"], "the required files were never listed");
}

/// NEGATIVE CONTROL: an ordinary dispatched result carries no origin key at
/// all, so every message written before this tag existed reads back unchanged.
#[test]
fn a_dispatched_tool_result_carries_no_origin_tag() {
    let msg = vm_to_json(&tool_result_message(ToolResultMessageInput {
        channel: crate::llm_config::ToolFormatChannel::Native,
        name: "read",
        tool_call_id: "call_9",
        observation: "file contents",
        ok: true,
        screenshots: &[],
        data: None,
        origin: ToolResultOrigin::Dispatch,
    }));
    assert_eq!(msg["_harn"]["kind"], "tool_result");
    assert!(
        msg["_harn"].get("origin").is_none(),
        "a dispatched result must be byte-identical to before: {msg}"
    );
}
