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
    TY_STRING,
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
];
