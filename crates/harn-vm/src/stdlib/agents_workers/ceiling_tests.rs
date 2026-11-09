//! Worker loops under an effect ceiling inherited from a delegating tool call.

use super::*;

/// A ceiling shaped like one derived from a delegating tool's declared
/// capabilities: it lets an agent run its model and edit the workspace, and
/// grants no worker capability.
fn tool_ceiling_without_worker_capability() -> CapabilityPolicy {
    let ops = |names: &[&str]| names.iter().map(|name| name.to_string()).collect();
    CapabilityPolicy {
        capabilities: std::collections::BTreeMap::from_iter([
            (
                "workspace".to_string(),
                ops(&["read_text", "write_text", "delete"]),
            ),
            ("llm".to_string(), ops(&["catalog", "call"])),
            ("state".to_string(), ops(&["read", "write"])),
        ]),
        side_effect_level: Some("workspace_write".to_string()),
        recursion_limit: Some(3),
        ..Default::default()
    }
}

/// A parent that delegates from inside a tool call hands its workers that
/// tool's ceiling. The ceiling bounds what the worker's model may do; it must
/// not stop the worker's loop from running its own bookkeeping, such as
/// reading its own worker record.
#[tokio::test(flavor = "current_thread")]
async fn background_worker_loop_runs_under_a_tool_ceiling_without_worker_capability() {
    use crate::orchestration::{
        clear_execution_policy_stacks, pop_execution_policy, push_execution_policy,
    };

    clear_execution_policy_stacks();
    reset_worker_registry();
    crate::agent_sessions::reset_session_store();
    crate::llm::mock::reset_llm_mock_state();
    push_execution_policy(tool_ceiling_without_worker_capability());

    let request = vm_dict(vec![
        ("_type", vm_string("sub_agent_request")),
        ("name", vm_string("ceiling-child")),
        ("task", vm_string("summarize")),
        ("background", VmValue::Bool(true)),
        (
            "options",
            vm_dict(vec![
                ("provider", vm_string("mock")),
                ("model", vm_string("mock")),
                ("max_iterations", VmValue::Int(1)),
            ]),
        ),
    ]);

    let waited = tokio::task::LocalSet::new()
        .run_until(async {
            let mut vm = crate::new_vm();
            crate::register_vm_stdlib(&mut vm);
            let ctx = crate::vm::AsyncBuiltinCtx::for_test(vm);
            let wait_ctx = ctx.child_ctx();
            let worker = super::super::sub_agent_run_builtin(ctx, vec![request])
                .await
                .expect("background sub-agent must spawn");
            super::super::wait_agent_builtin(wait_ctx, vec![worker]).await
        })
        .await;

    pop_execution_policy();
    clear_execution_policy_stacks();
    reset_worker_registry();
    crate::agent_sessions::reset_session_store();
    crate::llm::mock::reset_llm_mock_state();

    let waited = crate::llm::helpers::vm_value_to_json(
        &waited.expect("the worker's loop must not be refused by its inherited tool ceiling"),
    );
    assert_eq!(waited["status"], serde_json::json!("completed"), "{waited}");
    assert_eq!(
        waited["result"]["status"],
        serde_json::json!("done"),
        "{waited}"
    );
}

/// Negative control for the bookkeeping read above: under the same ceiling,
/// observing the delegated workers is still refused. Only the caller's own
/// record is exempt.
#[test]
fn tool_ceiling_without_worker_capability_still_refuses_listing_workers() {
    use crate::orchestration::{
        clear_execution_policy_stacks, enforce_current_policy_for_capability, push_execution_policy,
    };
    use harn_builtin_meta::CapabilityId;

    clear_execution_policy_stacks();
    push_execution_policy(tool_ceiling_without_worker_capability());
    let listed = enforce_current_policy_for_capability(CapabilityId::Agent, "worker_list", &[]);
    let own = enforce_current_policy_for_capability(CapabilityId::Agent, "current_worker", &[]);
    clear_execution_policy_stacks();

    let error = listed.expect_err("listing delegated workers needs a worker capability");
    assert!(
        error
            .to_string()
            .contains("worker_list exceeds the active effect ceiling"),
        "{error}"
    );
    own.expect("reading the caller's own worker record is loop bookkeeping");
}
