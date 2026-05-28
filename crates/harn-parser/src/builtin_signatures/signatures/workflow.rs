//! Workflow, artifact, and run-record builtin signatures.
//!
//! Three sub-areas:
//!
//! - `artifact_*` / `handoff*` / `assemble_context` — typed-record
//!   constructors and selectors that produce `ArtifactRecord`-shaped
//!   dicts the orchestration runtime understands.
//! - `workflow_*` graph-shape builders/validators (`workflow_graph`,
//!   `workflow_validate`, ...) plus `workflow.*` mailbox primitives
//!   (signal/query/update/...).
//! - `run_record_*` and `eval_*` for replay fixtures and eval suites.

use super::{
    BuiltinSignature, Param, Ty, TY_ANY, TY_CLOSURE, TY_DICT, TY_DICT_OR_NIL, TY_LIST, TY_NIL,
    TY_STRING, TY_STRING_OR_NIL,
};

/// `dict | Schema<any>` — schema aliases type-check as `Schema<T>` but
/// compile down to JSON-Schema dictionaries at runtime.
const TY_SCHEMA_VALUE: Ty = Ty::Union(&[TY_DICT, Ty::Apply("Schema", &[TY_ANY])]);

pub(crate) const SIGNATURES: &[BuiltinSignature] = &[
    // assemble_context(options) — async context-pack builder. The single
    // options dict carries `artifacts`, `strategy`, `query`, optional
    // `ranker_callback`, etc.
    BuiltinSignature::simple(
        "assemble_context",
        &[Param::new("options", TY_DICT)],
        TY_DICT,
    ),
    // handoff_effects(source, ceiling?) -> typed effect set computed from a
    // child agent's entrypoint Harn source,
    // select_artifacts_adaptive(artifacts, policy?) -> list of selected
    // artifacts after dedup, microcompaction, and policy-driven
    // selection.
    BuiltinSignature::simple(
        "select_artifacts_adaptive",
        &[
            Param::optional("artifacts", Ty::Union(&[TY_LIST, TY_NIL])),
            Param::optional("policy", TY_DICT_OR_NIL),
        ],
        TY_LIST,
    ),
    // workflow_clone(workflow) -> cloned workflow graph dict.
    BuiltinSignature::simple(
        "workflow_clone",
        &[Param::new("workflow", TY_DICT)],
        TY_DICT,
    ),
    // workflow_commit(workflow, reason?) -> committed graph dict.
    BuiltinSignature::simple(
        "workflow_commit",
        &[
            Param::new("workflow", TY_DICT),
            Param::optional("reason", TY_STRING_OR_NIL),
        ],
        TY_DICT,
    ),
    // workflow_diff(left, right) -> `{changed, left, right}` dict.
    BuiltinSignature::simple(
        "workflow_diff",
        &[Param::new("left", TY_DICT), Param::new("right", TY_DICT)],
        TY_DICT,
    ),
    // workflow_execute(task, workflow, artifacts?, options?) — async;
    // runs the workflow and returns `{status, run, artifacts,
    // transcript, path}`.
    BuiltinSignature::simple(
        "workflow_execute",
        &[
            Param::new("task", TY_STRING),
            Param::new("workflow", TY_DICT),
            Param::optional("artifacts", Ty::Union(&[TY_LIST, TY_NIL])),
            Param::optional("options", TY_DICT_OR_NIL),
        ],
        TY_DICT,
    ),
    // workflow_graph(workflow?) -> normalized graph dict.
    BuiltinSignature::simple(
        "workflow_graph",
        &[Param::optional("workflow", TY_DICT_OR_NIL)],
        TY_DICT,
    ),
    // workflow_insert_node(workflow, node, edge?) -> updated graph dict.
    BuiltinSignature::simple(
        "workflow_insert_node",
        &[
            Param::new("workflow", TY_DICT),
            Param::new("node", TY_DICT),
            Param::optional("edge", TY_DICT_OR_NIL),
        ],
        TY_DICT,
    ),
    // workflow_inspect(workflow, ceiling?) -> `{graph, validation,
    // node_count, edge_count}` dict.
    BuiltinSignature::simple(
        "workflow_inspect",
        &[
            Param::optional("workflow", TY_DICT_OR_NIL),
            Param::optional("ceiling", TY_DICT_OR_NIL),
        ],
        TY_DICT,
    ),
    // workflow_policy_report(workflow, ceiling?) -> per-node policy
    // report dict.
    BuiltinSignature::simple(
        "workflow_policy_report",
        &[
            Param::optional("workflow", TY_DICT_OR_NIL),
            Param::optional("ceiling", TY_DICT_OR_NIL),
        ],
        TY_DICT,
    ),
    // workflow_replace_node(workflow, node_id, node) -> updated graph
    // dict.
    BuiltinSignature::simple(
        "workflow_replace_node",
        &[
            Param::new("workflow", TY_DICT),
            Param::new("node_id", TY_STRING),
            Param::new("node", TY_DICT),
        ],
        TY_DICT,
    ),
    // workflow_rewire(workflow, from, to, branch?) -> updated graph dict.
    BuiltinSignature::simple(
        "workflow_rewire",
        &[
            Param::new("workflow", TY_DICT),
            Param::new("from", TY_STRING),
            Param::new("to", TY_STRING),
            Param::optional("branch", TY_STRING_OR_NIL),
        ],
        TY_DICT,
    ),
    // workflow_set_auto_compact(workflow, node_id, policy) -> updated
    // graph dict.
    BuiltinSignature::simple(
        "workflow_set_auto_compact",
        &[
            Param::new("workflow", TY_DICT),
            Param::new("node_id", TY_STRING),
            Param::new("policy", TY_DICT),
        ],
        TY_DICT,
    ),
    // workflow_set_context_policy(workflow, node_id, policy) -> updated
    // graph dict.
    BuiltinSignature::simple(
        "workflow_set_context_policy",
        &[
            Param::new("workflow", TY_DICT),
            Param::new("node_id", TY_STRING),
            Param::new("policy", TY_DICT),
        ],
        TY_DICT,
    ),
    // workflow_set_model_policy(workflow, node_id, policy) -> updated
    // graph dict.
    BuiltinSignature::simple(
        "workflow_set_model_policy",
        &[
            Param::new("workflow", TY_DICT),
            Param::new("node_id", TY_STRING),
            Param::new("policy", TY_DICT),
        ],
        TY_DICT,
    ),
    // workflow_set_output_visibility(workflow, node_id, visibility) ->
    // updated graph dict. `visibility` is `string | nil`.
    BuiltinSignature::simple(
        "workflow_set_output_visibility",
        &[
            Param::new("workflow", TY_DICT),
            Param::new("node_id", TY_STRING),
            Param::new("visibility", TY_STRING_OR_NIL),
        ],
        TY_DICT,
    ),
    BuiltinSignature::simple(
        "workflow_typed_output_checkpoint",
        &[
            Param::new("name", TY_STRING),
            Param::new("prompt", TY_STRING),
            Param::new("schema", TY_SCHEMA_VALUE),
            Param::optional("options", TY_DICT_OR_NIL),
            Param::optional("validator", Ty::Union(&[TY_CLOSURE, TY_NIL])),
        ],
        TY_DICT,
    ),
    // workflow_validate(workflow?, ceiling?) -> validation report dict
    // (`{valid, errors, warnings, ...}`).
    BuiltinSignature::simple(
        "workflow_validate",
        &[
            Param::optional("workflow", TY_DICT_OR_NIL),
            Param::optional("ceiling", TY_DICT_OR_NIL),
        ],
        TY_DICT,
    ),
];
