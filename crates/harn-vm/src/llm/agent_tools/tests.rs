//! Harn#691: every dispatch path tags `ToolCallUpdate.executor` with
//! the backend that ran the tool. These tests exercise each branch
//! of `dispatch_tool_execution` without spinning up the full agent
//! loop.

use super::*;
use crate::value::VmDictExt;

use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use tokio::sync::Mutex;

fn embedded_call_repair_result(
    tool_name: &str,
    tool_args: &serde_json::Value,
) -> Option<serde_json::Value> {
    futures::executor::block_on(super::embedded_call_repair_result(
        None, tool_name, tool_args,
    ))
}

// D5: a permission denial must tell the model what to do instead, not just
// report `{"error":"permission_denied"}`. The bare object made the model
// retry the same blocked call; the `next_step` field gives it an out.
#[test]
fn denied_tool_result_includes_actionable_next_step() {
    let result = denied_tool_result("run", "shell access is disabled");
    assert_eq!(result["error"], serde_json::json!("permission_denied"));
    assert_eq!(result["tool"], serde_json::json!("run"));
    assert_eq!(
        result["reason"],
        serde_json::json!("shell access is disabled")
    );
    let next = result["next_step"]
        .as_str()
        .expect("denial should carry a next_step string");
    assert!(
        next.contains("Do not retry"),
        "next_step should steer the model off a retry loop: {next}"
    );
    assert!(
        next.contains("run"),
        "next_step should name the denied tool: {next}"
    );
    // No active policy → no allow-all list to assert, so the "Available
    // tools:" clause is omitted rather than claiming a misleading set.
    assert!(
        !next.contains("Available tools:"),
        "with no active policy the denial should not assert an allow list: {next}"
    );
}

// F3: when an execution policy advertises an explicit tool allowlist, the
// denial must NAME those tools so a cheap model can self-correct in one turn
// instead of re-emitting an unlisted name. Grounded in fw-gpt-oss-120b
// transcripts where the model called Codex/container vocab it was never
// shown and only saw a bare "not permitted" denial.
#[test]
fn denied_tool_result_names_available_tools_under_policy() {
    use crate::orchestration::{pop_execution_policy, push_execution_policy, CapabilityPolicy};

    push_execution_policy(CapabilityPolicy {
        tools: vec![
            "look".to_string(),
            "search".to_string(),
            "edit".to_string(),
            "run".to_string(),
            "read_command_output".to_string(),
        ],
        ..Default::default()
    });
    let result = denied_tool_result("repo_browser.open_file", "tool exceeds tool ceiling");
    pop_execution_policy();

    let next = result["next_step"]
        .as_str()
        .expect("denial should carry a next_step string");
    assert!(
        next.contains("Available tools:"),
        "next_step should name the allowed tools under an active policy: {next}"
    );
    for tool in ["look", "search", "edit", "run", "read_command_output"] {
        assert!(
            next.contains(tool),
            "next_step should list the allowed tool {tool}: {next}"
        );
    }
}

// A NAME-RESOLUTION failure (tool-ceiling denial) must never use permission
// framing: a headless model that reads "tell the user what you need
// permission for" petitions a user that does not exist and stalls. The body
// names the failure class, lists the callable tools, and shows the call
// shape instead.
#[test]
fn unavailable_tool_result_is_action_oriented_without_permission_framing() {
    use crate::orchestration::{pop_execution_policy, push_execution_policy, CapabilityPolicy};

    push_execution_policy(CapabilityPolicy {
        tools: vec!["look".to_string(), "search".to_string(), "edit".to_string()],
        ..Default::default()
    });
    let result = unavailable_tool_result("container.upload", "tool exceeds tool ceiling");
    pop_execution_policy();

    assert_eq!(result["error"], serde_json::json!("unknown_tool"));
    let next = result["next_step"]
        .as_str()
        .expect("unavailable-tool result should carry a next_step string");
    assert!(
        !next.to_lowercase().contains("permission") && !next.contains("not permitted"),
        "name-resolution feedback must not use permission framing: {next}"
    );
    assert!(
        next.contains("Available tools:") && next.contains("look"),
        "next_step should list the callable tools: {next}"
    );
    assert!(
        next.contains("re-sending this call will fail"),
        "next_step should steer off an identical re-send: {next}"
    );
}

// A call NAMED `tool_call` whose arguments carry one valid text-format
// call is a wrapper-addressing slip, not an unknown tool: the repair body
// must name the embedded call and show the direct invocation.
#[test]
fn embedded_call_repair_names_the_inner_call() {
    use crate::orchestration::{pop_execution_policy, push_execution_policy, CapabilityPolicy};

    let args = serde_json::json!(
        "<tool_call>\nlook({ file: \"src/main.rs\", intent: \"read\" })\n</tool_call>"
    );
    push_execution_policy(CapabilityPolicy {
        tools: vec!["look".to_string(), "search".to_string()],
        ..Default::default()
    });
    let result = embedded_call_repair_result("tool_call", &args);
    pop_execution_policy();
    let result = result.expect("a wrapper carrying one valid call should yield repair feedback");
    assert_eq!(result["error"], serde_json::json!("invalid_arguments"));
    let reason = result["reason"].as_str().expect("reason");
    assert!(
        reason.contains("wrapper tag") && reason.contains("`look`"),
        "reason should explain the wrapper slip and name the embedded call: {reason}"
    );
    let next = result["next_step"].as_str().expect("next_step");
    assert!(
        next.contains("look(") && next.contains("src/main.rs"),
        "next_step should show the corrected direct invocation: {next}"
    );
    assert!(
        !next.to_lowercase().contains("permission"),
        "repair feedback must not use permission framing: {next}"
    );
}

#[test]
fn call_shaped_tool_name_gets_parse_repair_feedback() {
    use crate::orchestration::{pop_execution_policy, push_execution_policy, CapabilityPolicy};

    push_execution_policy(CapabilityPolicy {
        tools: vec!["look".to_string(), "search".to_string()],
        ..Default::default()
    });
    let result = embedded_call_repair_result(
        "look({ file: \"src/main.rs\", intent: \"read\" })",
        &serde_json::json!({}),
    );
    pop_execution_policy();
    let result = result.expect("call-shaped tool name should be repairable");
    assert_eq!(result["error"], serde_json::json!("invalid_arguments"));
    let reason = result["reason"].as_str().expect("reason");
    assert!(
        reason.contains("tool-name field") && reason.contains("`look`"),
        "reason should classify this as a name-field parse slip: {reason}"
    );
    let next = result["next_step"].as_str().expect("next_step");
    assert!(
        next.contains("look(") && !next.to_lowercase().contains("permission"),
        "repair should show the corrected invocation with no permission framing: {next}"
    );
}

#[test]
fn malformed_call_shaped_tool_name_is_not_unknown_tool_feedback() {
    use crate::orchestration::{pop_execution_policy, push_execution_policy, CapabilityPolicy};

    push_execution_policy(CapabilityPolicy {
        tools: vec!["look".to_string(), "search".to_string()],
        ..Default::default()
    });
    let result =
        embedded_call_repair_result("look({ file: \"src/main.rs\"", &serde_json::json!({}));
    pop_execution_policy();
    let result = result.expect("malformed call-shaped name should still be parse repair");
    assert_eq!(result["error"], serde_json::json!("invalid_arguments"));
    let reason = result["reason"].as_str().expect("reason");
    assert!(
        reason.contains("tool-name field"),
        "reason should name the tool-name parse slip: {reason}"
    );
    assert_ne!(result["error"], serde_json::json!("unknown_tool"));
}

// The streamed-arguments fallback rewraps non-JSON arguments as
// `{"__parse_error": "... Raw input: <raw>"}`; the repair must see through
// that carrier. A single string-valued field carrier works the same way.
#[test]
fn embedded_call_repair_recovers_alternate_argument_carriers() {
    use crate::orchestration::{pop_execution_policy, push_execution_policy, CapabilityPolicy};

    push_execution_policy(CapabilityPolicy {
        tools: vec!["look".to_string(), "search".to_string()],
        ..Default::default()
    });
    let parse_error_args = serde_json::json!({
        "__parse_error": "Could not parse streamed tool arguments as JSON or Harn \
         text-tool arguments: JSON error: expected value; Harn text-tool error: x. \
         Raw input: <tool_call>\nlook({ file: \"a.rs\", intent: \"read\" })\n</tool_call>"
    });
    let parse_error_repair = embedded_call_repair_result("tool_call", &parse_error_args);
    let single_field_args = serde_json::json!({
        "input": "<tool_call>\nsearch({ query: \"fn parse\" })\n</tool_call>"
    });
    let single_field_repair = embedded_call_repair_result("tool_call", &single_field_args);
    pop_execution_policy();

    let repaired = parse_error_repair.expect("__parse_error carrier should be recovered");
    assert!(repaired["next_step"]
        .as_str()
        .expect("next_step")
        .contains("look("));
    let repaired = single_field_repair.expect("single string-field carrier should be recovered");
    assert!(repaired["next_step"]
        .as_str()
        .expect("next_step")
        .contains("search("));
}

// Anything ambiguous must fall back to the unavailable-tool feedback:
// a non-wrapper name, more than one embedded call, unparseable text, or an
// embedded target outside the active policy's tool set.
#[test]
fn embedded_call_repair_rejects_ambiguous_payloads() {
    use crate::orchestration::{pop_execution_policy, push_execution_policy, CapabilityPolicy};

    let valid =
        serde_json::json!("<tool_call>\nlook({ file: \"a.rs\", intent: \"read\" })\n</tool_call>");
    push_execution_policy(CapabilityPolicy {
        tools: vec!["look".to_string()],
        ..Default::default()
    });
    let non_wrapper_repair = embedded_call_repair_result("repo_browser.open_file", &valid);
    let two_calls = serde_json::json!(
        "<tool_call>\nlook({ file: \"a.rs\" })\n</tool_call>\n\
         <tool_call>\nlook({ file: \"b.rs\" })\n</tool_call>"
    );
    let two_calls_repair = embedded_call_repair_result("tool_call", &two_calls);
    let prose_repair =
        embedded_call_repair_result("tool_call", &serde_json::json!("just some prose"));
    pop_execution_policy();
    assert!(
        non_wrapper_repair.is_none(),
        "a non-wrapper tool name must not trigger the repair"
    );
    assert!(
        two_calls_repair.is_none(),
        "more than one embedded call is ambiguous"
    );
    assert!(
        prose_repair.is_none(),
        "prose without a parseable call must not trigger the repair"
    );

    push_execution_policy(CapabilityPolicy {
        tools: vec!["search".to_string()],
        ..Default::default()
    });
    let repaired = embedded_call_repair_result("tool_call", &valid);
    pop_execution_policy();
    assert!(
        repaired.is_none(),
        "an embedded target outside the policy's tool set must not be coached"
    );
}

// A RECOVERABLE schema/argument rejection must coach a retry WITH the
// correction — `error: "invalid_arguments"` (not permission_denied) and a
// next_step that re-calls the tool naming the missing param. Reverting the
// recoverable/denied split (routing this through `denied_tool_result`) makes
// every assertion below fail: the error flips to permission_denied and the
// next_step says "Do not retry the same call".
#[test]
fn recoverable_tool_result_coaches_retry_with_named_missing_param() {
    let result = recoverable_tool_result(
        "edit",
        "Tool 'edit' is missing required parameter(s): path. \
         Provide all required parameters and try again.",
    );
    assert_eq!(result["error"], serde_json::json!("invalid_arguments"));
    assert_ne!(result["error"], serde_json::json!("permission_denied"));
    assert_eq!(result["tool"], serde_json::json!("edit"));
    let next = result["next_step"]
        .as_str()
        .expect("recoverable result should carry a next_step string");
    assert!(
        !next.contains("Do not retry"),
        "recoverable next_step must be retry-positive, not a don't-retry denial: {next}"
    );
    assert!(
        next.contains("Re-call") && next.contains("edit"),
        "next_step should tell the model to re-call the named tool: {next}"
    );
    assert!(
        next.contains("path"),
        "next_step should name the specific missing parameter: {next}"
    );
}

// The empty/malformed tool-name slip (harn#3194) is also recoverable: it
// must get retry-positive feedback, never the permission-denial body.
#[test]
fn recoverable_tool_result_handles_empty_tool_name() {
    let result = recoverable_tool_result(
        "<unnamed>",
        "Tool call is missing a name. Emit one tool call per turn as \
         `name({ ... })` using a non-empty tool name from the allowed list, then retry.",
    );
    assert_eq!(result["error"], serde_json::json!("invalid_arguments"));
    let next = result["next_step"]
        .as_str()
        .expect("recoverable result should carry a next_step string");
    assert!(
        !next.contains("Do not retry"),
        "empty-name feedback must be retry-positive: {next}"
    );
    assert!(
        next.contains("fixable") && next.contains("name"),
        "next_step should frame the missing name as a fixable slip: {next}"
    );
}

#[test]
fn recoverable_tool_result_is_not_a_denial() {
    // The loop keys flow control off `is_denied_tool_result`; a fixable
    // argument error must NOT trip the denial detector.
    let result = recoverable_tool_result(
        "edit",
        "Tool 'edit' is missing required parameter(s): path. \
         Provide all required parameters and try again.",
    );
    assert!(
        !is_denied_tool_result(&result),
        "recoverable invalid_arguments result must not read as a denial"
    );
    assert!(!is_denied_tool_result(&serde_json::Value::String(
        result.to_string()
    )));
}

#[test]
fn extract_missing_params_pulls_named_list() {
    assert_eq!(
        extract_missing_params(
            "Tool 'edit' is missing required parameter(s): path, mode. \
             Provide all required parameters and try again."
        ),
        Some("path, mode".to_string())
    );
    assert_eq!(extract_missing_params("Tool call is missing a name."), None);
}

fn tools_dict(entries: Vec<(&str, crate::value::DictMap)>) -> VmValue {
    let list: Vec<VmValue> = entries
        .into_iter()
        .map(|(name, mut entry)| {
            entry
                .entry(crate::value::intern_key("name"))
                .or_insert_with(|| VmValue::String(arcstr::ArcStr::from(name.to_string())));
            VmValue::dict(entry)
        })
        .collect();
    let mut dict = crate::value::DictMap::new();
    dict.insert(
        crate::value::intern_key("tools"),
        VmValue::List(std::sync::Arc::new(list)),
    );
    VmValue::dict(dict)
}

#[test]
fn denied_tool_result_detects_rendered_blocked_json() {
    let blocked = serde_json::json!({
        "blocked": true,
        "status": "blocked",
        "reason": "policy rejected command"
    });
    assert!(is_denied_tool_result(&blocked));
    assert!(is_denied_tool_result(&serde_json::Value::String(
        blocked.to_string()
    )));
    assert!(!is_denied_tool_result(&serde_json::json!({
        "status": "completed",
        "stdout": "ok"
    })));
}

#[test]
fn ok_result_failure_category_detects_failure_bodies() {
    // The pre-fix bug: these all returned Ok(value) from dispatch and were
    // laundered into `ok: true` because they aren't *denials*.
    assert_eq!(
        ok_result_failure_category(&serde_json::json!({"ok": false, "error": "boom"})),
        Some("tool_error")
    );
    assert_eq!(
        ok_result_failure_category(&serde_json::json!({"success": false})),
        Some("tool_error")
    );
    assert_eq!(
        ok_result_failure_category(&serde_json::json!({"status": "error", "stderr": "x"})),
        Some("tool_error")
    );
    assert_eq!(
        ok_result_failure_category(&serde_json::json!({"status": "failed"})),
        Some("tool_error")
    );
    assert_eq!(
        ok_result_failure_category(&serde_json::json!({"isError": true, "content": []})),
        Some("tool_error")
    );
    assert_eq!(
        ok_result_failure_category(&serde_json::json!({"error": "disk full"})),
        Some("tool_error")
    );
    // Stringified envelope (host bridges that stringify) still detected.
    let stringified = serde_json::Value::String(r#"{"ok": false, "error": "boom"}"#.to_string());
    assert_eq!(ok_result_failure_category(&stringified), Some("tool_error"));
}

#[test]
fn ok_result_failure_category_passes_through_successes() {
    assert_eq!(
        ok_result_failure_category(&serde_json::json!({"ok": true, "stdout": "done"})),
        None
    );
    assert_eq!(
        ok_result_failure_category(&serde_json::json!({"status": "completed"})),
        None
    );
    // A null/empty error with positive signals is a success, not a failure.
    assert_eq!(
        ok_result_failure_category(&serde_json::json!({"ok": true, "error": null})),
        None
    );
    assert_eq!(
        ok_result_failure_category(&serde_json::json!({"ok": true, "error": "  "})),
        None
    );
    // Plain string output and arrays are ordinary success.
    assert_eq!(
        ok_result_failure_category(&serde_json::Value::String("file contents".to_string())),
        None
    );
    assert_eq!(
        ok_result_failure_category(&serde_json::json!(["a", "b"])),
        None
    );
    assert_eq!(ok_result_failure_category(&serde_json::Value::Null), None);
}

#[test]
fn mcp_server_for_tool_finds_top_level_annotation() {
    // mcp_list_tools tags every entry with `_mcp_server`. The
    // helper picks that up so the dispatch site can tag the
    // executor as `McpServer { server_name }`.
    let mut entry = crate::value::DictMap::new();
    entry.put_str("_mcp_server", "linear");
    let tools = tools_dict(vec![("create_issue", entry)]);
    assert_eq!(
        mcp_server_for_tool(Some(&tools), "create_issue"),
        Some("linear".to_string())
    );
}

#[test]
fn mcp_server_for_tool_finds_nested_function_annotation() {
    // OpenAI-shape tools nest `_mcp_server` inside a `function`
    // sub-dict; the search must drill down a level.
    let mut function = crate::value::DictMap::new();
    function.put_str("name", "create_issue");
    function.put_str("_mcp_server", "linear");
    let mut entry = crate::value::DictMap::new();
    entry.insert(
        crate::value::intern_key("function"),
        VmValue::dict(function),
    );
    // The outer entry has no `name` — fall back to function.name.
    let mut dict = crate::value::DictMap::new();
    dict.insert(
        crate::value::intern_key("tools"),
        VmValue::List(std::sync::Arc::new(vec![VmValue::Dict(
            std::sync::Arc::new(entry),
        )])),
    );
    let tools = VmValue::dict(dict);
    assert_eq!(
        mcp_server_for_tool(Some(&tools), "create_issue"),
        Some("linear".to_string())
    );
}

#[test]
fn mcp_server_for_tool_returns_none_for_plain_tool() {
    let tools = tools_dict(vec![("read", crate::value::DictMap::new())]);
    assert!(mcp_server_for_tool(Some(&tools), "read").is_none());
    assert!(mcp_server_for_tool(Some(&tools), "missing").is_none());
    assert!(mcp_server_for_tool(None, "read").is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn dispatch_tags_harn_builtin_for_local_short_circuit() {
    // `read_file` is a `handle_tool_locally` short-circuit — the
    // dispatcher resolves it without touching tools_val or the
    // bridge, and tags executor=HarnBuiltin.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("hello.txt");
    std::fs::write(&path, "harn#691").expect("write");
    let args = serde_json::json!({ "path": path.to_string_lossy() });
    let outcome = dispatch_tool_execution("read_file", &args, None, None, 0, 0).await;
    assert!(outcome.result.is_ok(), "got: {:?}", outcome.result);
    assert_eq!(outcome.executor, Some(ToolExecutor::HarnBuiltin));
}

#[tokio::test(flavor = "current_thread")]
async fn dispatch_tags_host_bridge_when_only_bridge_can_serve() {
    // No `handle_tool_locally` short-circuit, no script handler in
    // tools_val — the bridge is the only backend left, so the
    // executor must be `HostBridge`. Use a writer that errors so
    // the call fails fast without needing a real host process.
    let bridge = crate::bridge::HostBridge::from_parts_with_writer(
        Arc::new(Mutex::new(std::collections::HashMap::new())),
        Arc::new(AtomicBool::new(false)),
        Arc::new(|_| Err("test bridge: no host attached".to_string())),
        1,
    );
    let bridge = Arc::new(bridge);
    let args = serde_json::json!({});
    let outcome =
        dispatch_tool_execution("custom_host_tool", &args, None, Some(&bridge), 0, 0).await;
    // The call itself fails (no host responds) but the executor
    // reflects the path that was attempted.
    assert!(outcome.result.is_err());
    assert_eq!(outcome.executor, Some(ToolExecutor::HostBridge));
}

#[tokio::test(flavor = "current_thread")]
async fn dispatch_tags_mcp_server_when_tool_is_mcp_owned_via_bridge() {
    // The bridge is present AND the tool entry carries a
    // `_mcp_server` annotation: the executor must point to the
    // MCP server, not the bridge that proxied the call.
    let bridge = crate::bridge::HostBridge::from_parts_with_writer(
        Arc::new(Mutex::new(std::collections::HashMap::new())),
        Arc::new(AtomicBool::new(false)),
        Arc::new(|_| Err("test bridge".to_string())),
        1,
    );
    let bridge = Arc::new(bridge);
    let mut entry = crate::value::DictMap::new();
    entry.put_str("_mcp_server", "linear");
    let tools = tools_dict(vec![("create_issue", entry)]);
    let args = serde_json::json!({});
    let outcome =
        dispatch_tool_execution("create_issue", &args, Some(&tools), Some(&bridge), 0, 0).await;
    assert_eq!(
        outcome.executor,
        Some(ToolExecutor::McpServer {
            server_name: "linear".to_string()
        })
    );
}

#[tokio::test(flavor = "current_thread")]
async fn dispatch_returns_none_executor_when_no_backend_available() {
    // No local short-circuit, no script handler, no bridge — the
    // dispatcher reports the tool as unavailable and the executor
    // stays `None` so callers don't blame a specific backend.
    let outcome =
        dispatch_tool_execution("nonexistent_tool", &serde_json::json!({}), None, None, 0, 0).await;
    assert!(outcome.result.is_err());
    assert!(outcome.executor.is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn dispatch_honors_declared_host_bridge_executor() {
    // harn#743: when a tool declares `executor: "host_bridge"`, the
    // dispatcher tags the event as HostBridge regardless of the
    // historic handler/`_mcp_server` heuristic.
    let bridge = crate::bridge::HostBridge::from_parts_with_writer(
        Arc::new(Mutex::new(std::collections::HashMap::new())),
        Arc::new(AtomicBool::new(false)),
        Arc::new(|_| Err("test bridge".to_string())),
        1,
    );
    let bridge = Arc::new(bridge);
    let mut entry = crate::value::DictMap::new();
    entry.put_str("executor", "host_bridge");
    entry.put_str("host_capability", "interaction.ask");
    let tools = tools_dict(vec![("ask_user", entry)]);
    let outcome = dispatch_tool_execution(
        "ask_user",
        &serde_json::json!({"prompt": "x"}),
        Some(&tools),
        Some(&bridge),
        0,
        0,
    )
    .await;
    assert_eq!(outcome.executor, Some(ToolExecutor::HostBridge));
}

#[tokio::test(flavor = "current_thread")]
async fn dispatch_honors_declared_provider_native_executor() {
    // Provider-native tools must never reach a runtime backend.
    // The dispatcher rejects with ProviderNative as the executor so
    // the ACP event reflects "model already executed this".
    let mut entry = crate::value::DictMap::new();
    entry.put_str("executor", "provider_native");
    let tools = tools_dict(vec![("tool_search", entry)]);
    let outcome = dispatch_tool_execution(
        "tool_search",
        &serde_json::json!({}),
        Some(&tools),
        None,
        0,
        0,
    )
    .await;
    assert_eq!(outcome.executor, Some(ToolExecutor::ProviderNative));
    assert!(outcome.result.is_err());
}

#[tokio::test(flavor = "current_thread")]
async fn dispatch_honors_declared_mcp_server_executor() {
    // Declared mcp_server uses the configured server name, not the
    // implicit `_mcp_server` annotation.
    let bridge = crate::bridge::HostBridge::from_parts_with_writer(
        Arc::new(Mutex::new(std::collections::HashMap::new())),
        Arc::new(AtomicBool::new(false)),
        Arc::new(|_| Err("test bridge".to_string())),
        1,
    );
    let bridge = Arc::new(bridge);
    let mut entry = crate::value::DictMap::new();
    entry.put_str("executor", "mcp_server");
    entry.put_str("mcp_server", "github");
    let tools = tools_dict(vec![("github_search_issues", entry)]);
    let outcome = dispatch_tool_execution(
        "github_search_issues",
        &serde_json::json!({"query": "x"}),
        Some(&tools),
        Some(&bridge),
        0,
        0,
    )
    .await;
    assert_eq!(
        outcome.executor,
        Some(ToolExecutor::McpServer {
            server_name: "github".to_string()
        })
    );
}
