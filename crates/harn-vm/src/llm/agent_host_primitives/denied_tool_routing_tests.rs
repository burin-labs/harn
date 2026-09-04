//! `agent_primitive_denied_tool` must pick its model-facing result body by
//! category: RECOVERABLE rejections (schema/argument validation, malformed
//! tool name) coach a retry-with-correction, while TRUE policy/permission
//! denials keep the don't-retry body. Reverting the split (sending every
//! category through `denied_tool_result`) fails the recoverable assertions.
use super::{agent_primitive_denied_tool, deny_tool_call, DenialEvidence};
use crate::agent_events::ToolCallErrorCategory;

#[test]
fn schema_validation_missing_param_yields_invalid_arguments_retry_positive() {
    let envelope = agent_primitive_denied_tool(
        "edit",
        "call_1",
        &serde_json::json!({ "content": "x" }),
        "Tool 'edit' is missing required parameter(s): path. \
         Provide all required parameters and try again.",
        ToolCallErrorCategory::SchemaValidation,
        None,
        None,
    );
    // Envelope-level category is still schema_validation for the wire...
    assert_eq!(envelope["error_category"], "schema_validation");
    // ...but the inner model-facing result is retry-positive, NOT a denial.
    let result = &envelope["result"];
    assert_eq!(result["error"], serde_json::json!("invalid_arguments"));
    assert_ne!(result["error"], serde_json::json!("permission_denied"));
    let observation = envelope["observation"]
        .as_str()
        .expect("recoverable rejection should carry model-facing observation");
    assert!(
        observation.starts_with("[result of edit]\n")
            && observation.contains("[end of edit result]"),
        "recoverable argument rejection must use normal tool-result framing, got: {observation}"
    );
    let next = result["next_step"].as_str().expect("next_step");
    assert!(
        !next.contains("Do not retry"),
        "schema rejection must be retry-positive: {next}"
    );
    assert!(
        next.contains("Re-call") && next.contains("edit") && next.contains("path"),
        "next_step should re-call the named tool with the missing param: {next}"
    );
}

#[test]
fn empty_tool_name_yields_recoverable_retry_positive_feedback() {
    let envelope = agent_primitive_denied_tool(
        "<unnamed>",
        "call_2",
        &serde_json::json!({}),
        "Tool call is missing a name. Emit one tool call per turn as \
         `name({ ... })` using a non-empty tool name from the allowed list, then retry.",
        ToolCallErrorCategory::SchemaValidation,
        None,
        None,
    );
    let result = &envelope["result"];
    assert_eq!(result["error"], serde_json::json!("invalid_arguments"));
    let next = result["next_step"].as_str().expect("next_step");
    assert!(
        !next.contains("Do not retry"),
        "empty-name slip must be retry-positive: {next}"
    );
}

#[test]
fn retryable_arg_constraint_denial_is_coached_as_recoverable() {
    use crate::agent_events::{DenialGate, ToolDenial};
    // A sub-agent scoped to `test/users.*` that tried to edit the shared
    // reference file: the tool is permitted, only this path is out of scope.
    let denial = ToolDenial::retryable(
        DenialGate::ArgConstraint,
        None,
        "tool 'edit' path 'test/accounts.integration.test.ts' is outside your allowed \
         scope. Allowed path pattern(s): [\"test/users.*\"]. This is fixable: re-issue \
         the call with a path that matches one of those patterns.",
    );
    let envelope = agent_primitive_denied_tool(
        "edit",
        "call_3",
        &serde_json::json!({ "path": "test/accounts.integration.test.ts" }),
        denial.reason.clone(),
        ToolCallErrorCategory::PermissionDenied,
        Some(&denial),
        None,
    );
    let result = &envelope["result"];
    // Retry-positive body, NOT a hard permission denial.
    assert_eq!(result["error"], serde_json::json!("invalid_arguments"));
    assert_ne!(result["error"], serde_json::json!("permission_denied"));
    let next = result["next_step"].as_str().expect("next_step");
    assert!(
        !next.contains("Do not retry"),
        "retryable arg-scope denial must coach a correction, not give-up: {next}"
    );
    // The structured denial still records the precise gate + retryable flag.
    assert_eq!(envelope["denial"]["gate"], "arg_constraint");
    assert_eq!(envelope["denial"]["retryable"], true);
}

#[test]
fn tool_call_wrapper_ceiling_denial_yields_embedded_call_repair() {
    use crate::agent_events::{DenialGate, ToolDenial};
    // Live headless pathology: the model emitted a native call NAMED
    // `tool_call` whose arguments carried a correct text-format call. The
    // ceiling denial must come back as parse-repair feedback that names
    // the embedded call — never permission vocabulary the model answers
    // by petitioning a user that does not exist.
    use crate::orchestration::{pop_execution_policy, push_execution_policy, CapabilityPolicy};
    let denial = ToolDenial::terminal(
        DenialGate::ToolCeiling,
        None,
        "tool 'tool_call' exceeds tool ceiling",
    );
    // A ToolCeiling denial implies an active policy with a non-empty tool
    // allowlist — mirror that precondition so the embedded call validates.
    push_execution_policy(CapabilityPolicy {
        tools: vec!["look".to_string(), "search".to_string(), "edit".to_string()],
        ..Default::default()
    });
    let envelope = futures::executor::block_on(deny_tool_call(
        None,
        "",
        "tool_call",
        "call_8",
        &serde_json::json!(
            "<tool_call>\nlook({ file: \"src/main.rs\", intent: \"read\" })\n</tool_call>"
        ),
        denial,
        false,
        DenialEvidence::new(None, None),
    ));
    pop_execution_policy();
    let result = &envelope["result"];
    assert_eq!(result["error"], serde_json::json!("invalid_arguments"));
    let next = result["next_step"].as_str().expect("next_step");
    assert!(
        next.contains("look(") && next.contains("src/main.rs"),
        "repair must show the corrected direct invocation: {next}"
    );
    assert!(
        !next.to_lowercase().contains("permission") && !next.contains("Do not retry"),
        "repair must be retry-positive with no permission framing: {next}"
    );
    // The structured denial names the wrapper-syntax cause instead of the
    // lower-level policy gate and flips retryable: re-issuing WITH the
    // correction is exactly the coached next move.
    assert_eq!(envelope["denial"]["gate"], "malformed_tool_wrapper");
    assert_eq!(envelope["denial"]["retryable"], true);
    assert_eq!(result["denial"]["gate"], "malformed_tool_wrapper");
    assert_eq!(result["denial"]["retryable"], true);
    assert!(DenialGate::MalformedToolWrapper.owns_reason(result["reason"].as_str().unwrap()));
    assert_eq!(result["denial"]["reason"], result["reason"]);
    assert_eq!(envelope["error"], result["reason"]);
    // The wire-level category is unchanged for host harnesses.
    assert_eq!(envelope["error_category"], "permission_denied");
}

#[test]
fn unknown_tool_ceiling_denial_drops_permission_framing() {
    use crate::agent_events::{DenialGate, ToolDenial};
    // A plain unknown/excluded name (no embedded call to repair) gets the
    // action-oriented unavailable-tool body: name the failure class, steer
    // off a re-send — never "what you need permission for".
    let denial = ToolDenial::terminal(
        DenialGate::ToolCeiling,
        None,
        "tool 'repo_browser.bundle' exceeds tool ceiling",
    );
    let envelope = agent_primitive_denied_tool(
        "repo_browser.bundle",
        "call_9",
        &serde_json::json!({ "path": "src" }),
        denial.reason.clone(),
        ToolCallErrorCategory::PermissionDenied,
        Some(&denial),
        None,
    );
    let result = &envelope["result"];
    assert_eq!(result["error"], serde_json::json!("unknown_tool"));
    let next = result["next_step"].as_str().expect("next_step");
    assert!(
        !next.to_lowercase().contains("permission") && !next.contains("not permitted"),
        "name-resolution denial must not use permission framing: {next}"
    );
    assert!(
        next.contains("not one of the available tools"),
        "next_step should name the failure class: {next}"
    );
    // Still terminal: re-sending the identical call can never succeed.
    assert_eq!(envelope["denial"]["gate"], "tool_ceiling");
    assert_eq!(envelope["denial"]["retryable"], false);
    assert_eq!(envelope["error_category"], "permission_denied");
}

#[test]
fn hard_capability_denial_stays_terminal() {
    use crate::agent_events::{DenialGate, ToolDenial};
    let denial = ToolDenial::terminal(
        DenialGate::CapabilityCeiling,
        Some("workspace.write_text".to_string()),
        "tool 'edit' exceeds capability ceiling: workspace.write_text",
    );
    let envelope = agent_primitive_denied_tool(
        "edit",
        "call_4",
        &serde_json::json!({ "path": "x" }),
        denial.reason.clone(),
        ToolCallErrorCategory::PermissionDenied,
        Some(&denial),
        None,
    );
    let result = &envelope["result"];
    assert_eq!(result["error"], serde_json::json!("permission_denied"));
    let next = result["next_step"].as_str().expect("next_step");
    assert!(
        next.contains("Do not retry"),
        "a hard capability ceiling must stay terminal: {next}"
    );
}

#[test]
fn arg_scoped_dynamic_permission_denial_is_coached_as_recoverable() {
    use crate::agent_events::{DenialGate, ToolDenial};
    // A dynamic permission rule denied a specific path while the tool itself
    // is permitted (analogous to ArgConstraint): coach a retry with an
    // allowed value rather than a terminal "do not retry".
    let denial = ToolDenial::retryable(
        DenialGate::DynamicPermission,
        None,
        "permission denied: path 'docs/secret.md' is outside custom path scope",
    );
    let envelope = agent_primitive_denied_tool(
        "edit",
        "call_5",
        &serde_json::json!({ "path": "docs/secret.md" }),
        denial.reason.clone(),
        ToolCallErrorCategory::PermissionDenied,
        Some(&denial),
        None,
    );
    let result = &envelope["result"];
    // Retry-positive body, NOT a hard permission denial.
    assert_eq!(result["error"], serde_json::json!("invalid_arguments"));
    assert_ne!(result["error"], serde_json::json!("permission_denied"));
    let next = result["next_step"].as_str().expect("next_step");
    assert!(
        !next.contains("Do not retry"),
        "arg-scoped dynamic-permission denial must coach a correction: {next}"
    );
    assert_eq!(envelope["denial"]["gate"], "dynamic_permission");
    assert_eq!(envelope["denial"]["retryable"], true);
}

#[test]
fn hard_dynamic_permission_ceiling_stays_terminal() {
    use crate::agent_events::{DenialGate, ToolDenial};
    // The whole tool is denied by the dynamic policy: a retry can't help.
    let denial = ToolDenial::terminal(
        DenialGate::DynamicPermission,
        None,
        "permission denied: tool 'exec' is not allowed by this agent's permissions",
    );
    let envelope = agent_primitive_denied_tool(
        "exec",
        "call_6",
        &serde_json::json!({ "command": "rm -rf /" }),
        denial.reason.clone(),
        ToolCallErrorCategory::PermissionDenied,
        Some(&denial),
        None,
    );
    let result = &envelope["result"];
    assert_eq!(result["error"], serde_json::json!("permission_denied"));
    let next = result["next_step"].as_str().expect("next_step");
    assert!(
        next.contains("Do not retry"),
        "a hard dynamic-permission ceiling must stay terminal: {next}"
    );
    assert_eq!(envelope["denial"]["retryable"], false);
}

#[test]
fn approval_unavailable_and_host_rejected_stay_terminal() {
    use crate::agent_events::{DenialGate, ToolDenial};
    // ApprovalUnavailable means no approver exists; HostRejected means the
    // user said no. A retry yields the same result, so both stay terminal
    // and are never marked recoverable.
    for gate in [DenialGate::ApprovalUnavailable, DenialGate::HostRejected] {
        let denial = ToolDenial::terminal(gate, None, "approval refused");
        assert!(!denial.retryable, "{} must stay terminal", gate.as_str());
        let envelope = agent_primitive_denied_tool(
            "exec",
            "call_7",
            &serde_json::json!({ "command": "ls" }),
            denial.reason.clone(),
            ToolCallErrorCategory::PermissionDenied,
            Some(&denial),
            None,
        );
        let result = &envelope["result"];
        assert_eq!(result["error"], serde_json::json!("permission_denied"));
        let next = result["next_step"].as_str().expect("next_step");
        assert!(
            next.contains("Do not retry"),
            "{} must stay terminal: {next}",
            gate.as_str()
        );
    }
}

use super::arg_delivery_fault_feedback;

#[test]
fn empty_args_with_length_truncation_names_the_truncation_cause() {
    let (reason, cause) =
        arg_delivery_fault_feedback("edit", &serde_json::json!({}), Some("length"))
            .expect("empty args must be cause-named");
    assert_eq!(cause, "empty_arguments_truncated");
    assert!(
        reason.contains("TRUNCATED") && reason.contains("output"),
        "length-truncated empty args must name the output-limit cut: {reason}"
    );
    assert!(
        reason.contains("shorter") || reason.contains("split"),
        "truncation feedback must coach a smaller re-issue: {reason}"
    );
    assert!(
        !reason.contains("missing required parameter"),
        "must not misdiagnose as a missing-parameter slip: {reason}"
    );
    // Anthropic spelling and provider casing route to the same cause.
    let (_, cause) =
        arg_delivery_fault_feedback("edit", &serde_json::Value::Null, Some("MAX_TOKENS"))
            .expect("null args must be cause-named");
    assert_eq!(cause, "empty_arguments_truncated");
}

#[test]
fn empty_args_with_clean_stop_names_the_provider_fault_cause() {
    for stop_reason in [Some("stop"), Some("tool_calls"), None] {
        let (reason, cause) =
            arg_delivery_fault_feedback("edit", &serde_json::json!({}), stop_reason)
                .expect("empty args must be cause-named");
        assert_eq!(cause, "empty_arguments_dropped");
        assert!(
            reason.contains("EMPTY arguments") && reason.contains("provider"),
            "clean-stop empty args must name the provider fault: {reason}"
        );
        assert!(
            reason.contains("Re-issue the same call"),
            "provider-fault feedback must coach an identical re-issue: {reason}"
        );
    }
}

// REAL captured bytes from a live llamacpp qwen3.6-35b turn
// (burin-examples/swift conversation 1b42844f): the model authored an
// `edit(action=create, content=<large file>)` call whose native
// `function.arguments` stream was cut mid-`content`, so the streamed-arg
// parser handed dispatch a `{"__parse_error": "..."}` carrier. Validation
// then reported "missing required parameter: path" and the model, told to
// "re-call with path" (which it HAD supplied), re-issued the same oversized
// edit and truncated again — 21 llm calls, 28 failed tool calls, idle with
// no visible reply. The carrier must be named as a truncation, not a slip.
const CAPTURED_TRUNCATION_CARRIER: &str = "Could not parse streamed tool arguments as JSON \
    or Harn text-tool arguments: JSON error: EOF while parsing a value at line 1 column 1401; \
    Harn text-tool error: TOOL CALL PARSE ERROR: `edit{...}` — unexpected end of input. Tool \
    arguments must be a TypeScript object literal. Raw: {\"path\":\"Sources/SysMonCore/\
    Providers/LiveSystemProvider.swift\",\"action\":\"create\",\"content\":\"import Foundation";

#[test]
fn truncated_toolcall_carrier_names_the_truncation_not_a_missing_param() {
    let carrier = serde_json::json!({ "__parse_error": CAPTURED_TRUNCATION_CARRIER });
    let (reason, cause) = arg_delivery_fault_feedback("edit", &carrier, Some("tool_calls"))
        .expect("a __parse_error carrier must be cause-named, not left to the validator");
    assert_eq!(cause, "arguments_truncated");
    assert!(
        reason.contains("TRUNCATED") || reason.contains("cut off"),
        "the carrier must be named as a truncated call: {reason}"
    );
    assert!(
        reason.contains("shorter") || reason.contains("split") || reason.contains("smaller"),
        "truncation feedback must coach a smaller re-issue: {reason}"
    );
    assert!(
        !reason.contains("missing required parameter"),
        "must NOT repeat the misdiagnosing missing-parameter message: {reason}"
    );
}

#[test]
fn malformed_toolcall_carrier_stays_a_clean_parse_error() {
    // A genuinely malformed (non-truncation) carrier must NOT be silently
    // accepted or mislabeled as a truncation — it stays a clean parse error
    // coaching valid JSON. Negative control against over-permissive repair.
    let carrier = serde_json::json!({
        "__parse_error": "Could not parse streamed tool arguments as JSON or Harn \
            text-tool arguments: JSON error: key must be a string at line 1 column 5. \
            Raw input: {path: not-json @#$}"
    });
    let (reason, cause) = arg_delivery_fault_feedback("edit", &carrier, None)
        .expect("a malformed carrier is still a named parse fault");
    assert_eq!(cause, "arguments_malformed");
    assert!(
        !reason.contains("TRUNCATED"),
        "a non-EOF parse error must not be labeled a truncation: {reason}"
    );
    assert!(
        reason.contains("JSON"),
        "malformed feedback must coach valid JSON: {reason}"
    );
}

#[test]
fn non_empty_args_keep_the_precise_validator_message() {
    assert!(
        arg_delivery_fault_feedback(
            "edit",
            &serde_json::json!({ "content": "x" }),
            Some("length")
        )
        .is_none(),
        "a call that DID deliver arguments must keep the missing-parameter message"
    );
}

#[test]
fn permission_denied_keeps_do_not_retry_body() {
    let envelope = agent_primitive_denied_tool(
        "run",
        "call_3",
        &serde_json::json!({ "command": "rm -rf /" }),
        "shell access is disabled by policy",
        ToolCallErrorCategory::PermissionDenied,
        None,
        None,
    );
    assert_eq!(envelope["error_category"], "permission_denied");
    let result = &envelope["result"];
    assert_eq!(result["error"], serde_json::json!("permission_denied"));
    let next = result["next_step"].as_str().expect("next_step");
    assert!(
        next.contains("Do not retry the same call"),
        "true denial must still steer off a retry loop: {next}"
    );
}
