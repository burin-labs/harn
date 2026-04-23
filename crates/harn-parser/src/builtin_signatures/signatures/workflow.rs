//! Workflow, artifact, and run-record builtin signatures.

use super::{BuiltinReturn, BuiltinSig, UNION_DICT_NIL};

pub(crate) const SIGNATURES: &[BuiltinSig] = &[
    BuiltinSig {
        name: "artifact",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "artifact_apply_intent",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "artifact_command_result",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "artifact_context",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "artifact_derive",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "artifact_diff",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "artifact_diff_review",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "artifact_editor_selection",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "artifact_git_diff",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "artifact_patch_proposal",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "artifact_review_decision",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "artifact_select",
        return_type: Some(BuiltinReturn::Named("list")),
    },
    BuiltinSig {
        name: "artifact_test_result",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "artifact_verification_bundle",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "artifact_verification_result",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "artifact_workspace_file",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "artifact_workspace_snapshot",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "continue_as_new",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "eval_metric",
        return_type: Some(BuiltinReturn::Named("nil")),
    },
    BuiltinSig {
        name: "eval_metrics",
        return_type: Some(BuiltinReturn::Named("list")),
    },
    BuiltinSig {
        name: "eval_suite_manifest",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "eval_suite_run",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "handoff",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "handoff_context",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "load_run_tree",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "render_always_on_catalog",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "run_record",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "run_record_diff",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "run_record_eval",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "run_record_eval_suite",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "run_record_fixture",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "run_record_load",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "run_record_save",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "select_artifacts_adaptive",
        return_type: None,
    },
    BuiltinSig {
        name: "workflow.continue_as_new",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "workflow.pause",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "workflow.publish_query",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "workflow.query",
        return_type: None,
    },
    BuiltinSig {
        name: "workflow.receive",
        return_type: Some(BuiltinReturn::Union(UNION_DICT_NIL)),
    },
    BuiltinSig {
        name: "workflow.respond_update",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "workflow.resume",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "workflow.signal",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "workflow.status",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "workflow.update",
        return_type: None,
    },
    BuiltinSig {
        name: "workflow_clone",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "workflow_commit",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "workflow_diff",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "workflow_execute",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "workflow_graph",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "workflow_insert_node",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "workflow_inspect",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "workflow_policy_report",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "workflow_replace_node",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "workflow_rewire",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "workflow_set_auto_compact",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "workflow_set_context_policy",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "workflow_set_model_policy",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "workflow_set_output_visibility",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "workflow_validate",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
];
