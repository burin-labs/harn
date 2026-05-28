//! Top-level workflow executor and builtin registration.

use crate::stdlib::harn_entry::register_harn_entrypoint_category;
use crate::stdlib::macros::VmBuiltinDef;
use crate::stdlib::registration::{
    async_builtin, register_builtin_group, AsyncBuiltin, BuiltinGroup, SyncBuiltin,
};
use crate::vm::{Vm, VmBuiltinArity};

use super::compact::{
    ESTIMATE_TOKENS_BUILTIN_DEF, MICROCOMPACT_BUILTIN_DEF, SELECT_ARTIFACTS_ADAPTIVE_BUILTIN_DEF,
    TRANSCRIPT_AUTO_COMPACT_BUILTIN_DEF,
};
use super::hooks::*;
use super::host::{
    HOST_WORKFLOW_FINALIZE_RUN_BUILTIN_DEF, HOST_WORKFLOW_MAP_BRANCH_ARTIFACT_BUILTIN_DEF,
    HOST_WORKFLOW_MAP_EXECUTE_BRANCH_BUILTIN_DEF, HOST_WORKFLOW_MAP_FINALIZE_BUILTIN_DEF,
    HOST_WORKFLOW_MAP_PLAN_BUILTIN_DEF, HOST_WORKFLOW_PREPARE_RUN_BUILTIN_DEF,
    HOST_WORKFLOW_RECORD_TRANSITIONS_BUILTIN_DEF, HOST_WORKFLOW_STAGE_COMPLETE_BUILTIN_DEF,
    HOST_WORKFLOW_STAGE_PREPARE_BUILTIN_DEF,
};
use super::inspect::{
    WORKFLOW_CLONE_BUILTIN_DEF, WORKFLOW_COMMIT_BUILTIN_DEF, WORKFLOW_DIFF_BUILTIN_DEF,
    WORKFLOW_GRAPH_BUILTIN_DEF, WORKFLOW_INSERT_NODE_BUILTIN_DEF, WORKFLOW_INSPECT_BUILTIN_DEF,
    WORKFLOW_POLICY_REPORT_BUILTIN_DEF, WORKFLOW_REPLACE_NODE_BUILTIN_DEF,
    WORKFLOW_REWIRE_BUILTIN_DEF, WORKFLOW_SET_AUTO_COMPACT_BUILTIN_DEF,
    WORKFLOW_SET_CONTEXT_POLICY_BUILTIN_DEF, WORKFLOW_SET_MODEL_POLICY_BUILTIN_DEF,
    WORKFLOW_SET_OUTPUT_VISIBILITY_BUILTIN_DEF, WORKFLOW_VALIDATE_BUILTIN_DEF,
};

const WORKFLOW_STDLIB_ENTRYPOINT_CATEGORY: &str = "workflow.stdlib";

const WORKFLOW_SYNC_PRIMITIVES: &[SyncBuiltin] = &[
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
        .doc("Register a session-level lifecycle hook (session_start, session_end, user_prompt_submit, pre_compact, post_compact, post_turn, permission_asked, permission_replied, file_edited, session_error, session_idle, loop_checkpoint)."),
    SyncBuiltin::new("clear_session_hooks", clear_session_hooks_builtin)
        .signature("clear_session_hooks()")
        .arity(VmBuiltinArity::Exact(0))
        .doc("Clear registered session-level lifecycle hooks."),
    SyncBuiltin::new("register_checkpoint_hook", register_checkpoint_hook_builtin)
        .signature("register_checkpoint_hook(kinds, handler)")
        .arity(VmBuiltinArity::Exact(2))
        .doc("Register a hook covering one or more agent-loop checkpoint seams. `kinds` is a list of seam names (iteration_start, pre_tool_dispatch, post_tool_dispatch, iteration_end, pre_compact, post_compact, daemon_idle_pre, daemon_idle_post, loop_exit), a single name, or `*` / nil for every seam."),
    SyncBuiltin::new("register_reminder_provider", register_reminder_provider_builtin)
        .signature("register_reminder_provider(config)")
        .arity(VmBuiltinArity::Exact(1))
        .doc("Register a system-reminder provider closure for agent lifecycle events."),
    SyncBuiltin::new("clear_reminder_providers", clear_reminder_providers_builtin)
        .signature("clear_reminder_providers()")
        .arity(VmBuiltinArity::Exact(0))
        .doc("Clear registered user-defined system-reminder providers."),
    SyncBuiltin::new("pipeline_on_finish", pipeline_on_finish_builtin)
        .signature("pipeline_on_finish(callback)")
        .arity(VmBuiltinArity::Exact(1))
        .doc("Register a callback invoked after the pipeline's declared steps complete. Signature: fn(harness, return_value). Last-write-wins; the callback's return replaces the pipeline's return value."),
    SyncBuiltin::new(
        "pipeline_lifecycle_audit_log_take",
        pipeline_lifecycle_audit_log_take_builtin,
    )
    .signature("pipeline_lifecycle_audit_log_take()")
    .arity(VmBuiltinArity::Exact(0))
    .doc("Return and clear every entry recorded via harness.emit_audit during this pipeline run."),
    SyncBuiltin::new(
        "pipeline_lifecycle_audit_log_snapshot",
        pipeline_lifecycle_audit_log_snapshot_builtin,
    )
    .signature("pipeline_lifecycle_audit_log_snapshot()")
    .arity(VmBuiltinArity::Exact(0))
    .doc("Return every entry recorded via harness.emit_audit without clearing the log."),
    SyncBuiltin::new(
        "__host_settlement_agent_active",
        settlement_agent_active_builtin,
    )
    .signature("__host_settlement_agent_active()")
    .arity(VmBuiltinArity::Exact(0))
    .doc("Return true while the settlement-agent drain loop (#1856) is running on this thread; false otherwise."),
    SyncBuiltin::new("notify_file_edited", notify_file_edited_builtin)
        .signature("notify_file_edited(path, metadata?)")
        .arity(VmBuiltinArity::Range { min: 1, max: 2 })
        .doc("Queue a `FileEdited` notification; hooks fire on the next agent-loop boundary."),
];

const WORKFLOW_ASYNC_PRIMITIVES: &[AsyncBuiltin] = &[
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

pub(crate) const MODULE_BUILTINS: &[&VmBuiltinDef] = &[
    // compact (already migrated to #[harn_builtin] in stdlib/workflow/compact.rs)
    &SELECT_ARTIFACTS_ADAPTIVE_BUILTIN_DEF,
    &ESTIMATE_TOKENS_BUILTIN_DEF,
    &MICROCOMPACT_BUILTIN_DEF,
    &TRANSCRIPT_AUTO_COMPACT_BUILTIN_DEF,
    // inspect (graph-shape builders + structural manipulation)
    &WORKFLOW_GRAPH_BUILTIN_DEF,
    &WORKFLOW_VALIDATE_BUILTIN_DEF,
    &WORKFLOW_INSPECT_BUILTIN_DEF,
    &WORKFLOW_POLICY_REPORT_BUILTIN_DEF,
    &WORKFLOW_CLONE_BUILTIN_DEF,
    &WORKFLOW_INSERT_NODE_BUILTIN_DEF,
    &WORKFLOW_REPLACE_NODE_BUILTIN_DEF,
    &WORKFLOW_REWIRE_BUILTIN_DEF,
    &WORKFLOW_SET_MODEL_POLICY_BUILTIN_DEF,
    &WORKFLOW_SET_CONTEXT_POLICY_BUILTIN_DEF,
    &WORKFLOW_SET_AUTO_COMPACT_BUILTIN_DEF,
    &WORKFLOW_SET_OUTPUT_VISIBILITY_BUILTIN_DEF,
    &WORKFLOW_DIFF_BUILTIN_DEF,
    &WORKFLOW_COMMIT_BUILTIN_DEF,
    // host (low-level workflow runtime helpers, all runtime_only)
    &HOST_WORKFLOW_PREPARE_RUN_BUILTIN_DEF,
    &HOST_WORKFLOW_RECORD_TRANSITIONS_BUILTIN_DEF,
    &HOST_WORKFLOW_FINALIZE_RUN_BUILTIN_DEF,
    &HOST_WORKFLOW_MAP_BRANCH_ARTIFACT_BUILTIN_DEF,
    &HOST_WORKFLOW_STAGE_PREPARE_BUILTIN_DEF,
    &HOST_WORKFLOW_STAGE_COMPLETE_BUILTIN_DEF,
    &HOST_WORKFLOW_MAP_PLAN_BUILTIN_DEF,
    &HOST_WORKFLOW_MAP_EXECUTE_BRANCH_BUILTIN_DEF,
    &HOST_WORKFLOW_MAP_FINALIZE_BUILTIN_DEF,
];

pub(crate) fn register_workflow_builtins(vm: &mut Vm) {
    register_builtin_group(vm, WORKFLOW_PRIMITIVES);
    for def in MODULE_BUILTINS {
        vm.register_builtin_def(def);
    }
    register_harn_entrypoint_category(vm, WORKFLOW_STDLIB_ENTRYPOINT_CATEGORY);
}
