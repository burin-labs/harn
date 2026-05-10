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
    BuiltinSignature, Param, Ty, TY_ANY, TY_CLOSURE, TY_DICT, TY_DICT_OR_NIL, TY_INT, TY_LIST,
    TY_NIL, TY_STRING, TY_STRING_OR_NIL,
};

/// `string | dict` — accepted shapes for workflow targets in the
/// `workflow.*` mailbox primitives. Either a workflow id string or a
/// `{workflow_id, base_dir?}` dict.
const TY_WORKFLOW_TARGET: Ty = Ty::Union(&[TY_STRING, TY_DICT]);

/// `dict | Schema<any>` — schema aliases type-check as `Schema<T>` but
/// compile down to JSON-Schema dictionaries at runtime.
const TY_SCHEMA_VALUE: Ty = Ty::Union(&[TY_DICT, Ty::Apply("Schema", &[TY_ANY])]);

pub(crate) const SIGNATURES: &[BuiltinSignature] = &[
    // artifact(payload) — normalize a free-form dict into a canonical
    // ArtifactRecord and emit the corresponding handoff event.
    BuiltinSignature::simple("artifact", &[Param::new("payload", TY_DICT)], TY_DICT),
    // artifact_apply_intent(target, intent, options?) — derive an
    // `apply_intent` artifact lineage-linked to `target`.
    BuiltinSignature::simple(
        "artifact_apply_intent",
        &[
            Param::new("target", TY_DICT),
            Param::new("intent", TY_STRING),
            Param::optional("options", TY_DICT_OR_NIL),
        ],
        TY_DICT,
    ),
    // artifact_command_result(command, output, options?) — wrap a shell
    // command's output as a `command_result` artifact.
    BuiltinSignature::simple(
        "artifact_command_result",
        &[
            Param::new("command", TY_STRING),
            Param::new("output", TY_ANY),
            Param::optional("options", TY_DICT_OR_NIL),
        ],
        TY_DICT,
    ),
    // artifact_context(artifacts, policy?) — render selected artifacts as
    // a single context string.
    BuiltinSignature::simple(
        "artifact_context",
        &[
            Param::new("artifacts", TY_LIST),
            Param::optional("policy", TY_DICT_OR_NIL),
        ],
        TY_STRING,
    ),
    // artifact_derive(parent, kind?, extras?) — clone `parent` with a new
    // kind and lineage-pointing back at it.
    BuiltinSignature::simple(
        "artifact_derive",
        &[
            Param::new("parent", TY_DICT),
            Param::optional("kind", TY_STRING),
            Param::optional("extras", TY_DICT_OR_NIL),
        ],
        TY_DICT,
    ),
    // artifact_diff(path, before, after, options?) — render a unified
    // diff and wrap it as a `diff` artifact.
    BuiltinSignature::simple(
        "artifact_diff",
        &[
            Param::new("path", TY_STRING),
            Param::new("before", TY_STRING),
            Param::new("after", TY_STRING),
            Param::optional("options", TY_DICT_OR_NIL),
        ],
        TY_DICT,
    ),
    // artifact_diff_review(target, summary?, options?) — review-comment
    // artifact lineage-linked to a parent diff/patch artifact.
    BuiltinSignature::simple(
        "artifact_diff_review",
        &[
            Param::new("target", TY_DICT),
            Param::optional("summary", TY_STRING_OR_NIL),
            Param::optional("options", TY_DICT_OR_NIL),
        ],
        TY_DICT,
    ),
    // artifact_editor_selection(path, text, options?).
    BuiltinSignature::simple(
        "artifact_editor_selection",
        &[
            Param::new("path", TY_STRING),
            Param::new("text", TY_STRING),
            Param::optional("options", TY_DICT_OR_NIL),
        ],
        TY_DICT,
    ),
    // artifact_git_diff(diff_text, options?) — wrap a raw `git diff`
    // string as an artifact.
    BuiltinSignature::simple(
        "artifact_git_diff",
        &[
            Param::new("diff_text", TY_STRING),
            Param::optional("options", TY_DICT_OR_NIL),
        ],
        TY_DICT,
    ),
    // artifact_patch_proposal(target, patch, options?).
    BuiltinSignature::simple(
        "artifact_patch_proposal",
        &[
            Param::new("target", TY_DICT),
            Param::new("patch", TY_STRING),
            Param::optional("options", TY_DICT_OR_NIL),
        ],
        TY_DICT,
    ),
    // artifact_review_decision(target, decision, options?).
    BuiltinSignature::simple(
        "artifact_review_decision",
        &[
            Param::new("target", TY_DICT),
            Param::new("decision", TY_STRING),
            Param::optional("options", TY_DICT_OR_NIL),
        ],
        TY_DICT,
    ),
    // artifact_select(artifacts, policy?) -> list of selected artifacts.
    BuiltinSignature::simple(
        "artifact_select",
        &[
            Param::new("artifacts", TY_LIST),
            Param::optional("policy", TY_DICT_OR_NIL),
        ],
        TY_LIST,
    ),
    // artifact_test_result(title, text, options?).
    BuiltinSignature::simple(
        "artifact_test_result",
        &[
            Param::new("title", TY_STRING),
            Param::new("text", TY_STRING),
            Param::optional("options", TY_DICT_OR_NIL),
        ],
        TY_DICT,
    ),
    // artifact_verification_bundle(title, checks, options?).
    BuiltinSignature::simple(
        "artifact_verification_bundle",
        &[
            Param::new("title", TY_STRING),
            Param::new("checks", TY_ANY),
            Param::optional("options", TY_DICT_OR_NIL),
        ],
        TY_DICT,
    ),
    // artifact_verification_result(title, text, options?).
    BuiltinSignature::simple(
        "artifact_verification_result",
        &[
            Param::new("title", TY_STRING),
            Param::new("text", TY_STRING),
            Param::optional("options", TY_DICT_OR_NIL),
        ],
        TY_DICT,
    ),
    // artifact_workspace_file(path, content, options?).
    BuiltinSignature::simple(
        "artifact_workspace_file",
        &[
            Param::new("path", TY_STRING),
            Param::new("content", TY_STRING),
            Param::optional("options", TY_DICT_OR_NIL),
        ],
        TY_DICT,
    ),
    // artifact_workspace_snapshot(paths, summary?, options?).
    BuiltinSignature::simple(
        "artifact_workspace_snapshot",
        &[
            Param::new("paths", TY_ANY),
            Param::optional("summary", TY_STRING_OR_NIL),
            Param::optional("options", TY_DICT_OR_NIL),
        ],
        TY_DICT,
    ),
    // assemble_context(options) — async context-pack builder. The single
    // options dict carries `artifacts`, `strategy`, `query`, optional
    // `ranker_callback`, etc.
    BuiltinSignature::simple(
        "assemble_context",
        &[Param::new("options", TY_DICT)],
        TY_DICT,
    ),
    // context_pack_manifest(payload) — normalize a manifest dict.
    BuiltinSignature::simple(
        "context_pack_manifest",
        &[Param::new("payload", TY_DICT)],
        TY_DICT,
    ),
    // context_pack_manifest_parse(source) — parse a manifest source
    // string into a normalized manifest dict.
    BuiltinSignature::simple(
        "context_pack_manifest_parse",
        &[Param::new("source", TY_STRING)],
        TY_DICT,
    ),
    // context_pack_suggestions(events?, options?) -> list of suggested
    // context-pack adjustments based on recorded friction events.
    BuiltinSignature::simple(
        "context_pack_suggestions",
        &[
            Param::optional("events", Ty::Union(&[TY_LIST, TY_NIL])),
            Param::optional("options", TY_DICT_OR_NIL),
        ],
        TY_LIST,
    ),
    // continue_as_new(target) — bump the workflow generation, clear
    // pending responses, and return updated status.
    BuiltinSignature::simple(
        "continue_as_new",
        &[Param::new("target", TY_WORKFLOW_TARGET)],
        TY_DICT,
    ),
    // eval_metric(name, value, metadata?) — record a metric on the
    // current eval thread.
    BuiltinSignature::simple(
        "eval_metric",
        &[
            Param::new("name", TY_STRING),
            Param::new("value", TY_ANY),
            Param::optional("metadata", TY_DICT_OR_NIL),
        ],
        TY_NIL,
    ),
    // eval_metrics() -> list of `{name, value, metadata?}` dicts.
    BuiltinSignature::simple("eval_metrics", &[], TY_LIST),
    // eval_pack_manifest(payload) — normalize an eval pack manifest dict.
    BuiltinSignature::simple(
        "eval_pack_manifest",
        &[Param::new("payload", TY_DICT)],
        TY_DICT,
    ),
    // eval_pack_run(payload) — execute the eval pack described by
    // `payload` and return its run summary.
    BuiltinSignature::simple("eval_pack_run", &[Param::new("payload", TY_DICT)], TY_DICT),
    // persona_eval_ladder_manifest(payload) — normalize a persona eval
    // ladder manifest dict.
    BuiltinSignature::simple(
        "persona_eval_ladder_manifest",
        &[Param::new("payload", TY_DICT)],
        TY_DICT,
    ),
    // persona_eval_ladder_run(payload) — execute the persona eval
    // ladder described by `payload`.
    BuiltinSignature::simple(
        "persona_eval_ladder_run",
        &[Param::new("payload", TY_DICT)],
        TY_DICT,
    ),
    // eval_suite_manifest(payload) — normalize an eval-suite manifest
    // dict.
    BuiltinSignature::simple(
        "eval_suite_manifest",
        &[Param::new("payload", TY_DICT)],
        TY_DICT,
    ),
    // eval_suite_run(payload) — execute the eval suite described by
    // `payload`.
    BuiltinSignature::simple("eval_suite_run", &[Param::new("payload", TY_DICT)], TY_DICT),
    // friction_clear() — reset the in-memory friction event log.
    BuiltinSignature::simple("friction_clear", &[], TY_NIL),
    // friction_eval_fixture(fixture) — replay a friction fixture and
    // return pass/failure diagnostics.
    BuiltinSignature::simple(
        "friction_eval_fixture",
        &[Param::new("fixture", TY_DICT)],
        TY_DICT,
    ),
    // friction_event(payload) — normalize a friction event payload.
    BuiltinSignature::simple("friction_event", &[Param::new("payload", TY_DICT)], TY_DICT),
    // friction_events() -> list of recorded friction events.
    BuiltinSignature::simple("friction_events", &[], TY_LIST),
    // friction_record(payload, options?) -> `{recorded, sink, event,
    // path?}` summary. Persists to JSONL when `log_path` is set or
    // `HARN_FRICTION_LOG` env var is present.
    BuiltinSignature::simple(
        "friction_record",
        &[
            Param::new("payload", TY_DICT),
            Param::optional("options", TY_DICT_OR_NIL),
        ],
        TY_DICT,
    ),
    // handoff(payload) — normalize a handoff payload (or an artifact
    // dict) into a canonical handoff record.
    BuiltinSignature::simple("handoff", &[Param::new("payload", TY_DICT)], TY_DICT),
    // handoff_context(payload) -> rendered handoff context string.
    BuiltinSignature::simple(
        "handoff_context",
        &[Param::new("payload", TY_DICT)],
        TY_STRING,
    ),
    // handoff_routes() -> runtime handoff route table from harn.toml.
    BuiltinSignature::simple("handoff_routes", &[], TY_LIST),
    // load_run_tree(path) — load a run record tree from disk.
    BuiltinSignature::simple("load_run_tree", &[Param::new("path", TY_STRING)], TY_DICT),
    // render_always_on_catalog(entries_or_registry, budget?) -> rendered
    // catalog string capped at `budget` tokens (defaults to 2000).
    BuiltinSignature::simple(
        "render_always_on_catalog",
        &[
            Param::new("entries_or_registry", Ty::Union(&[TY_LIST, TY_DICT])),
            Param::optional("budget", TY_INT),
        ],
        TY_STRING,
    ),
    // run_record(payload) — normalize a run-record dict.
    BuiltinSignature::simple("run_record", &[Param::new("payload", TY_DICT)], TY_DICT),
    // run_record_diff(left, right) -> diff dict.
    BuiltinSignature::simple(
        "run_record_diff",
        &[Param::new("left", TY_DICT), Param::new("right", TY_DICT)],
        TY_DICT,
    ),
    // run_record_eval(run, fixture?) -> evaluation result.
    BuiltinSignature::simple(
        "run_record_eval",
        &[
            Param::new("run", TY_DICT),
            Param::optional("fixture", TY_DICT_OR_NIL),
        ],
        TY_DICT,
    ),
    // run_record_eval_suite(cases) -> aggregate evaluation result.
    BuiltinSignature::simple(
        "run_record_eval_suite",
        &[Param::new("cases", TY_LIST)],
        TY_DICT,
    ),
    // run_record_fixture(run) -> replay fixture dict derived from a run.
    BuiltinSignature::simple("run_record_fixture", &[Param::new("run", TY_DICT)], TY_DICT),
    // run_record_load(path) -> loaded run-record dict.
    BuiltinSignature::simple("run_record_load", &[Param::new("path", TY_STRING)], TY_DICT),
    // run_record_save(run, path?) -> `{path, run}`.
    BuiltinSignature::simple(
        "run_record_save",
        &[
            Param::new("run", TY_DICT),
            Param::optional("path", TY_STRING_OR_NIL),
        ],
        TY_DICT,
    ),
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
    // workflow.continue_as_new(target) -> updated status dict.
    BuiltinSignature::simple(
        "workflow.continue_as_new",
        &[Param::new("target", TY_WORKFLOW_TARGET)],
        TY_DICT,
    ),
    // workflow.pause(target) -> updated status dict.
    BuiltinSignature::simple(
        "workflow.pause",
        &[Param::new("target", TY_WORKFLOW_TARGET)],
        TY_DICT,
    ),
    // workflow.publish_query(target, name, value) -> publish-summary
    // dict.
    BuiltinSignature::simple(
        "workflow.publish_query",
        &[
            Param::new("target", TY_WORKFLOW_TARGET),
            Param::new("name", TY_STRING),
            Param::new("value", TY_ANY),
        ],
        TY_DICT,
    ),
    // workflow.query(target, name) -> stored query value (any).
    BuiltinSignature::simple(
        "workflow.query",
        &[
            Param::new("target", TY_WORKFLOW_TARGET),
            Param::new("name", TY_STRING),
        ],
        TY_ANY,
    ),
    // workflow.receive(target) -> next message dict | nil.
    BuiltinSignature::simple(
        "workflow.receive",
        &[Param::new("target", TY_WORKFLOW_TARGET)],
        TY_DICT_OR_NIL,
    ),
    // workflow.respond_update(target, request_id, value, name?).
    BuiltinSignature::simple(
        "workflow.respond_update",
        &[
            Param::new("target", TY_WORKFLOW_TARGET),
            Param::new("request_id", TY_STRING),
            Param::new("value", TY_ANY),
            Param::optional("name", TY_STRING_OR_NIL),
        ],
        TY_DICT,
    ),
    // workflow.resume(target) -> updated status dict.
    BuiltinSignature::simple(
        "workflow.resume",
        &[Param::new("target", TY_WORKFLOW_TARGET)],
        TY_DICT,
    ),
    // workflow.signal(target, name, payload?) -> enqueue summary dict.
    BuiltinSignature::simple(
        "workflow.signal",
        &[
            Param::new("target", TY_WORKFLOW_TARGET),
            Param::new("name", TY_STRING),
            Param::optional("payload", TY_ANY),
        ],
        TY_DICT,
    ),
    // workflow.status(target) -> status dict.
    BuiltinSignature::simple(
        "workflow.status",
        &[Param::new("target", TY_WORKFLOW_TARGET)],
        TY_DICT,
    ),
    // workflow.update(target, name, payload?, options?) — async; returns
    // the update response value (caller-defined shape, hence `any`).
    BuiltinSignature::simple(
        "workflow.update",
        &[
            Param::new("target", TY_WORKFLOW_TARGET),
            Param::new("name", TY_STRING),
            Param::optional("payload", TY_ANY),
            Param::optional("options", TY_DICT_OR_NIL),
        ],
        TY_ANY,
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
