use super::{agent_primitive_denied_tool, host_agent_dispatch_tool_call};

#[test]
fn approval_unavailable_names_terminal_risk_class() {
    use crate::agent_events::{DenialGate, ToolCallErrorCategory, ToolDenial};

    let denial = ToolDenial::terminal(
        DenialGate::ApprovalUnavailable,
        None,
        "approval required but no host bridge is available",
    )
    .with_denial_class("approval_required+package_install", 2);
    let envelope = agent_primitive_denied_tool(
        "exec",
        "call_8",
        &serde_json::json!({ "command": "pip install pytest" }),
        denial.reason.clone(),
        ToolCallErrorCategory::PermissionDenied,
        Some(&denial),
        None,
    );
    let result = &envelope["result"];
    assert_eq!(result["error"], serde_json::json!("permission_denied"));
    let human_summary = result["human_summary"]
        .as_str()
        .expect("human_summary is a top-level plain-English string for hosts that skip the structured fields");
    assert!(
        human_summary.contains("exec"),
        "human_summary should name the blocked tool: {human_summary}"
    );
    assert!(
        human_summary.contains("approval required but no host bridge is available"),
        "human_summary should carry the reason for hosts that render it alone: {human_summary}"
    );
    assert_eq!(
        result["denial_class"],
        serde_json::json!("approval_required+package_install")
    );
    assert_eq!(result["class_repeat_count"], serde_json::json!(2));
    let next = result["next_step"].as_str().expect("next_step");
    assert!(
        next.contains("already marked terminal"),
        "repeated class denial should not invite more variants: {next}"
    );
    assert!(
        next.contains("approval_required+package_install"),
        "next_step should name the terminal risk class: {next}"
    );
    assert_eq!(
        result["denial"]["denial_class"],
        serde_json::json!("approval_required+package_install")
    );
    assert_eq!(result["denial"]["class_repeat_count"], serde_json::json!(2));
}

#[tokio::test]
async fn approval_unavailable_denials_share_terminal_risk_class_across_arg_variants() {
    crate::orchestration::clear_execution_policy_stacks();
    crate::orchestration::clear_all_approval_policy_repeat_counts();
    let policy: crate::orchestration::ToolApprovalPolicy =
        serde_json::from_value(serde_json::json!({
            "rules": [{
                "ask": {"tool": "exec", "command_identity": "pip"},
                "reason": "package installs require approval"
            }]
        }))
        .expect("approval policy");
    crate::orchestration::push_approval_policy(policy);

    let mut options = crate::value::DictMap::new();
    options.insert(
        crate::value::intern_key("session_id"),
        crate::stdlib::json_to_vm_value(&serde_json::json!(
            "approval-unavailable-terminal-class-test"
        )),
    );

    let first = dispatch_exec("pip install pytest", &options).await;
    let second = dispatch_exec("pip install requests --break-system-packages", &options).await;

    crate::orchestration::pop_approval_policy();
    crate::orchestration::clear_all_approval_policy_repeat_counts();

    assert_eq!(
        first["result"]["error"],
        serde_json::json!("permission_denied")
    );
    assert_eq!(
        first["result"]["denial"]["gate"],
        serde_json::json!("approval_unavailable")
    );
    assert_eq!(
        first["result"]["denial_class"],
        serde_json::json!("approval_required+command_rule")
    );
    assert_eq!(first["result"]["class_repeat_count"], serde_json::json!(1));
    assert_eq!(
        second["result"]["denial_class"],
        serde_json::json!("approval_required+command_rule")
    );
    assert_eq!(
        second["result"]["class_repeat_count"],
        serde_json::json!(2),
        "same session + same policy risk class should be repeat-counted even when args differ"
    );
    let next_step = second["result"]["next_step"].as_str().expect("next_step");
    assert!(
        next_step.contains("already marked terminal"),
        "repeat feedback must suppress approval-required arg churn: {next_step}"
    );
}

async fn dispatch_exec(command: &str, options: &crate::value::DictMap) -> serde_json::Value {
    let call = crate::stdlib::json_to_vm_value(&serde_json::json!({
        "id": format!("exec-{command}"),
        "name": "exec",
        "arguments": {"command": command},
    }));
    let result = host_agent_dispatch_tool_call(
        crate::vm::AsyncBuiltinCtx::for_test(crate::vm::Vm::new()),
        call,
        None,
        options,
    )
    .await
    .expect("approval-unavailable denial is a normal dispatch result");
    crate::llm::helpers::vm_value_to_json(&result)
}
