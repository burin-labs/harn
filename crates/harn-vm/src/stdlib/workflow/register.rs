//! Top-level workflow executor and builtin registration.

use crate::stdlib::harn_entry::register_harn_entrypoint_category;
use crate::stdlib::registration::{
    async_builtin, register_builtin_group, AsyncBuiltin, BuiltinGroup, SyncBuiltin,
};
use crate::vm::{Vm, VmBuiltinArity};

use super::compact::*;
use super::hooks::*;
use super::host::*;
use super::inspect::*;

const WORKFLOW_STDLIB_ENTRYPOINT_CATEGORY: &str = "workflow.stdlib";

const WORKFLOW_SYNC_PRIMITIVES: &[SyncBuiltin] = &[
    SyncBuiltin::new("workflow_graph", workflow_graph_builtin)
        .signature("workflow_graph(input?)")
        .arity(VmBuiltinArity::Range { min: 0, max: 1 })
        .doc("Normalize a workflow value and return the canonical workflow graph dict."),
    SyncBuiltin::new("workflow_validate", workflow_validate_builtin)
        .signature("workflow_validate(input?, ceiling?)")
        .arity(VmBuiltinArity::Range { min: 0, max: 2 })
        .doc("Validate a workflow graph against a capability policy ceiling."),
    SyncBuiltin::new("workflow_inspect", workflow_inspect_builtin)
        .signature("workflow_inspect(input?, ceiling?)")
        .arity(VmBuiltinArity::Range { min: 0, max: 2 })
        .doc("Return normalized workflow graph shape and validation details."),
    SyncBuiltin::new("workflow_policy_report", workflow_policy_report_builtin)
        .signature("workflow_policy_report(graph, ceiling?)")
        .arity(VmBuiltinArity::Range { min: 1, max: 2 })
        .doc("Report workflow and node policies against an effective ceiling."),
    SyncBuiltin::new("workflow_clone", workflow_clone_builtin)
        .signature("workflow_clone(graph)")
        .arity(VmBuiltinArity::Exact(1))
        .doc("Clone a workflow graph and append audit metadata."),
    SyncBuiltin::new("workflow_insert_node", workflow_insert_node_builtin)
        .signature("workflow_insert_node(graph, node, edge?)")
        .arity(VmBuiltinArity::Range { min: 2, max: 3 })
        .doc("Insert a node and optional edge into a workflow graph."),
    SyncBuiltin::new("workflow_replace_node", workflow_replace_node_builtin)
        .signature("workflow_replace_node(graph, node_id, node)")
        .arity(VmBuiltinArity::Exact(3))
        .doc("Replace one node in a workflow graph."),
    SyncBuiltin::new("workflow_rewire", workflow_rewire_builtin)
        .signature("workflow_rewire(graph, from, to, branch?)")
        .arity(VmBuiltinArity::Range { min: 3, max: 4 })
        .doc("Replace outgoing edge wiring for one workflow graph node."),
    SyncBuiltin::new(
        "workflow_set_model_policy",
        workflow_set_model_policy_builtin,
    )
    .signature("workflow_set_model_policy(graph, node_id, policy)")
    .arity(VmBuiltinArity::Exact(3))
    .doc("Set one node's model policy."),
    SyncBuiltin::new(
        "workflow_set_context_policy",
        workflow_set_context_policy_builtin,
    )
    .signature("workflow_set_context_policy(graph, node_id, policy)")
    .arity(VmBuiltinArity::Exact(3))
    .doc("Set one node's context policy."),
    SyncBuiltin::new(
        "workflow_set_auto_compact",
        workflow_set_auto_compact_builtin,
    )
    .signature("workflow_set_auto_compact(graph, node_id, policy)")
    .arity(VmBuiltinArity::Exact(3))
    .doc("Set one node's auto-compaction policy."),
    SyncBuiltin::new(
        "workflow_set_output_visibility",
        workflow_set_output_visibility_builtin,
    )
    .signature("workflow_set_output_visibility(graph, node_id, visibility)")
    .arity(VmBuiltinArity::Exact(3))
    .doc("Set one node's output visibility policy."),
    SyncBuiltin::new("workflow_diff", workflow_diff_builtin)
        .signature("workflow_diff(left, right)")
        .arity(VmBuiltinArity::Exact(2))
        .doc("Compare two workflow graph values for canonical JSON changes."),
    SyncBuiltin::new("workflow_commit", workflow_commit_builtin)
        .signature("workflow_commit(graph, reason?)")
        .arity(VmBuiltinArity::Range { min: 1, max: 2 })
        .doc("Validate and commit workflow graph audit metadata."),
    SyncBuiltin::new("register_tool_hook", register_tool_hook_builtin)
        .signature("register_tool_hook(config?)")
        .arity(VmBuiltinArity::Range { min: 0, max: 1 })
        .doc("Register low-level pre/post tool hooks for workflow execution."),
    SyncBuiltin::new("clear_tool_hooks", clear_tool_hooks_builtin)
        .signature("clear_tool_hooks()")
        .arity(VmBuiltinArity::Exact(0))
        .doc("Clear registered low-level workflow tool hooks."),
    SyncBuiltin::new("register_persona_hook", register_persona_hook_builtin)
        .signature("register_persona_hook(persona_pattern, event, handler)")
        .arity(VmBuiltinArity::Exact(3))
        .doc("Register a persona lifecycle hook for matching persona names."),
    SyncBuiltin::new("register_step_hook", register_step_hook_builtin)
        .signature("register_step_hook(persona_pattern, step_name, event, handler)")
        .arity(VmBuiltinArity::Exact(4))
        .doc("Register a persona step lifecycle hook for one named step."),
    SyncBuiltin::new("clear_persona_hooks", clear_persona_hooks_builtin)
        .signature("clear_persona_hooks()")
        .arity(VmBuiltinArity::Exact(0))
        .doc("Clear registered persona and step lifecycle hooks."),
    SyncBuiltin::new("register_session_hook", register_session_hook_builtin)
        .signature("register_session_hook(event, pattern?, handler)")
        .arity(VmBuiltinArity::Range { min: 2, max: 3 })
        .doc("Register a session-level lifecycle hook (session_start, session_end, user_prompt_submit, pre_compact, post_compact, post_turn, permission_asked, permission_replied, file_edited, session_error, session_idle)."),
    SyncBuiltin::new("clear_session_hooks", clear_session_hooks_builtin)
        .signature("clear_session_hooks()")
        .arity(VmBuiltinArity::Exact(0))
        .doc("Clear registered session-level lifecycle hooks."),
    SyncBuiltin::new("notify_file_edited", notify_file_edited_builtin)
        .signature("notify_file_edited(path, metadata?)")
        .arity(VmBuiltinArity::Range { min: 1, max: 2 })
        .doc("Queue a `FileEdited` notification; hooks fire on the next agent-loop boundary."),
    SyncBuiltin::new(
        "select_artifacts_adaptive",
        select_artifacts_adaptive_builtin,
    )
    .signature("select_artifacts_adaptive(artifacts?, policy?)")
    .arity(VmBuiltinArity::Range { min: 0, max: 2 })
    .doc("Select workflow artifacts according to a context policy."),
    SyncBuiltin::new("estimate_tokens", estimate_tokens_builtin)
        .signature("estimate_tokens(messages?)")
        .arity(VmBuiltinArity::Range { min: 0, max: 1 })
        .doc("Estimate tokens for a list of message objects."),
    SyncBuiltin::new("microcompact", microcompact_builtin)
        .signature("microcompact(text, max_chars?)")
        .arity(VmBuiltinArity::Range { min: 1, max: 2 })
        .doc("Compact long tool output with the host microcompaction primitive."),
    SyncBuiltin::new(
        HOST_WORKFLOW_PREPARE_RUN_BUILTIN,
        host_workflow_prepare_run_builtin,
    )
    .signature("__host_workflow_prepare_run(task, graph, artifacts?, options?)")
    .arity(VmBuiltinArity::Range { min: 2, max: 4 })
    .doc("Prepare low-level workflow run state for the Harn stdlib workflow executor."),
    SyncBuiltin::new(
        HOST_WORKFLOW_RECORD_TRANSITIONS_BUILTIN,
        host_workflow_record_transitions_builtin,
    )
    .signature("__host_workflow_record_transitions(state_id, ready_nodes, stage, edges)")
    .arity(VmBuiltinArity::Exact(4))
    .doc("Record workflow stage transitions and checkpoint low-level run state."),
    SyncBuiltin::new(
        HOST_WORKFLOW_FINALIZE_RUN_BUILTIN,
        host_workflow_finalize_run_builtin,
    )
    .signature("__host_workflow_finalize_run(state_id, ready_nodes)")
    .arity(VmBuiltinArity::Exact(2))
    .doc("Finalize low-level workflow run state and persist the final checkpoint."),
    SyncBuiltin::new(
        HOST_WORKFLOW_MAP_BRANCH_ARTIFACT_BUILTIN,
        host_workflow_map_branch_artifact_builtin,
    )
    .signature("__host_workflow_map_branch_artifact(node_id, item, lineage)")
    .arity(VmBuiltinArity::Exact(3))
    .doc("Build the synthesized input artifact for one Harn-owned workflow map branch."),
];

const WORKFLOW_ASYNC_PRIMITIVES: &[AsyncBuiltin] = &[
    async_builtin!(
        HOST_WORKFLOW_STAGE_PREPARE_BUILTIN,
        host_workflow_stage_prepare_builtin
    )
    .signature("__host_workflow_stage_prepare(state_id, node_id, ready_nodes, options?)")
    .arity(VmBuiltinArity::Range { min: 3, max: 4 })
    .doc("Prepare one low-level workflow stage and install its execution scope."),
    async_builtin!(
        HOST_WORKFLOW_STAGE_COMPLETE_BUILTIN,
        host_workflow_stage_complete_builtin
    )
    .signature("__host_workflow_stage_complete(state_id, node_id, llm_result)")
    .arity(VmBuiltinArity::Exact(3))
    .doc("Complete one prepared low-level workflow stage and tear down its execution scope."),
    async_builtin!(
        HOST_WORKFLOW_MAP_PLAN_BUILTIN,
        host_workflow_map_plan_builtin
    )
    .signature("__host_workflow_map_plan(node, artifacts)")
    .arity(VmBuiltinArity::Exact(2))
    .doc("Return the host-normalized execution plan for a workflow map stage."),
    async_builtin!(
        HOST_WORKFLOW_MAP_EXECUTE_BRANCH_BUILTIN,
        host_workflow_map_execute_branch_builtin
    )
    .signature("__host_workflow_map_execute_branch(node_id, plan, item, branch_artifact, options?)")
    .arity(VmBuiltinArity::Range { min: 4, max: 5 })
    .doc("Execute one workflow map branch while Harn owns branch scheduling."),
    async_builtin!(
        HOST_WORKFLOW_MAP_FINALIZE_BUILTIN,
        host_workflow_map_finalize_builtin
    )
    .signature("__host_workflow_map_finalize(strategy, total_items, completed, failures, produced)")
    .arity(VmBuiltinArity::Exact(5))
    .doc("Finalize a Harn-owned workflow map stage after branch settlement."),
    async_builtin!("transcript_auto_compact", transcript_auto_compact_builtin)
        .signature("transcript_auto_compact(messages, options?)")
        .arity(VmBuiltinArity::Range { min: 1, max: 2 })
        .doc("Apply the workflow/agent transcript auto-compaction primitive to a message list."),
    async_builtin!("__host_fire_session_hook", fire_session_hook_builtin)
        .signature("__host_fire_session_hook(event, payload?)")
        .arity(VmBuiltinArity::Range { min: 1, max: 2 })
        .doc("Fire a session-level lifecycle hook and return its control flow."),
    async_builtin!("__host_drain_file_edits", drain_file_edits_builtin)
        .signature("__host_drain_file_edits(session_id?)")
        .arity(VmBuiltinArity::Range { min: 0, max: 1 })
        .doc("Drain the FileEdited queue, fire matching hooks, return the drained paths."),
];

const WORKFLOW_PRIMITIVES: BuiltinGroup<'static> = BuiltinGroup::new()
    .category("workflow.host")
    .sync(WORKFLOW_SYNC_PRIMITIVES)
    .async_(WORKFLOW_ASYNC_PRIMITIVES);

pub(crate) fn register_workflow_builtins(vm: &mut Vm) {
    register_builtin_group(vm, WORKFLOW_PRIMITIVES);
    register_harn_entrypoint_category(vm, WORKFLOW_STDLIB_ENTRYPOINT_CATEGORY);
}
