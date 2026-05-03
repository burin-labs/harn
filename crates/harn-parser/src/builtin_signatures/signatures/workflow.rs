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
    BuiltinSignature, Param, Ty, TY_ANY, TY_DICT, TY_DICT_OR_NIL, TY_INT, TY_LIST, TY_NIL,
    TY_STRING, TY_STRING_OR_NIL,
};

/// `string | dict` — accepted shapes for workflow targets in the
/// `workflow.*` mailbox primitives. Either a workflow id string or a
/// `{workflow_id, base_dir?}` dict.
const TY_WORKFLOW_TARGET: Ty = Ty::Union(&[TY_STRING, TY_DICT]);

pub(crate) const SIGNATURES: &[BuiltinSignature] = &[
    // artifact(payload) — normalize a free-form dict into a canonical
    // ArtifactRecord and emit the corresponding handoff event.
    BuiltinSignature {
        name: "artifact",
        params: &[Param::new("payload", TY_DICT)],
        returns: TY_DICT,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    // artifact_apply_intent(target, intent, options?) — derive an
    // `apply_intent` artifact lineage-linked to `target`.
    BuiltinSignature {
        name: "artifact_apply_intent",
        params: &[
            Param::new("target", TY_DICT),
            Param::new("intent", TY_STRING),
            Param::optional("options", TY_DICT_OR_NIL),
        ],
        returns: TY_DICT,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    // artifact_command_result(command, output, options?) — wrap a shell
    // command's output as a `command_result` artifact.
    BuiltinSignature {
        name: "artifact_command_result",
        params: &[
            Param::new("command", TY_STRING),
            Param::new("output", TY_ANY),
            Param::optional("options", TY_DICT_OR_NIL),
        ],
        returns: TY_DICT,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    // artifact_context(artifacts, policy?) — render selected artifacts as
    // a single context string.
    BuiltinSignature {
        name: "artifact_context",
        params: &[
            Param::new("artifacts", TY_LIST),
            Param::optional("policy", TY_DICT_OR_NIL),
        ],
        returns: TY_STRING,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    // artifact_derive(parent, kind?, extras?) — clone `parent` with a new
    // kind and lineage-pointing back at it.
    BuiltinSignature {
        name: "artifact_derive",
        params: &[
            Param::new("parent", TY_DICT),
            Param::optional("kind", TY_STRING),
            Param::optional("extras", TY_DICT_OR_NIL),
        ],
        returns: TY_DICT,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    // artifact_diff(path, before, after, options?) — render a unified
    // diff and wrap it as a `diff` artifact.
    BuiltinSignature {
        name: "artifact_diff",
        params: &[
            Param::new("path", TY_STRING),
            Param::new("before", TY_STRING),
            Param::new("after", TY_STRING),
            Param::optional("options", TY_DICT_OR_NIL),
        ],
        returns: TY_DICT,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    // artifact_diff_review(target, summary?, options?) — review-comment
    // artifact lineage-linked to a parent diff/patch artifact.
    BuiltinSignature {
        name: "artifact_diff_review",
        params: &[
            Param::new("target", TY_DICT),
            Param::optional("summary", TY_STRING_OR_NIL),
            Param::optional("options", TY_DICT_OR_NIL),
        ],
        returns: TY_DICT,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    // artifact_editor_selection(path, text, options?).
    BuiltinSignature {
        name: "artifact_editor_selection",
        params: &[
            Param::new("path", TY_STRING),
            Param::new("text", TY_STRING),
            Param::optional("options", TY_DICT_OR_NIL),
        ],
        returns: TY_DICT,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    // artifact_git_diff(diff_text, options?) — wrap a raw `git diff`
    // string as an artifact.
    BuiltinSignature {
        name: "artifact_git_diff",
        params: &[
            Param::new("diff_text", TY_STRING),
            Param::optional("options", TY_DICT_OR_NIL),
        ],
        returns: TY_DICT,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    // artifact_patch_proposal(target, patch, options?).
    BuiltinSignature {
        name: "artifact_patch_proposal",
        params: &[
            Param::new("target", TY_DICT),
            Param::new("patch", TY_STRING),
            Param::optional("options", TY_DICT_OR_NIL),
        ],
        returns: TY_DICT,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    // artifact_review_decision(target, decision, options?).
    BuiltinSignature {
        name: "artifact_review_decision",
        params: &[
            Param::new("target", TY_DICT),
            Param::new("decision", TY_STRING),
            Param::optional("options", TY_DICT_OR_NIL),
        ],
        returns: TY_DICT,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    // artifact_select(artifacts, policy?) -> list of selected artifacts.
    BuiltinSignature {
        name: "artifact_select",
        params: &[
            Param::new("artifacts", TY_LIST),
            Param::optional("policy", TY_DICT_OR_NIL),
        ],
        returns: TY_LIST,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    // artifact_test_result(title, text, options?).
    BuiltinSignature {
        name: "artifact_test_result",
        params: &[
            Param::new("title", TY_STRING),
            Param::new("text", TY_STRING),
            Param::optional("options", TY_DICT_OR_NIL),
        ],
        returns: TY_DICT,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    // artifact_verification_bundle(title, checks, options?).
    BuiltinSignature {
        name: "artifact_verification_bundle",
        params: &[
            Param::new("title", TY_STRING),
            Param::new("checks", TY_ANY),
            Param::optional("options", TY_DICT_OR_NIL),
        ],
        returns: TY_DICT,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    // artifact_verification_result(title, text, options?).
    BuiltinSignature {
        name: "artifact_verification_result",
        params: &[
            Param::new("title", TY_STRING),
            Param::new("text", TY_STRING),
            Param::optional("options", TY_DICT_OR_NIL),
        ],
        returns: TY_DICT,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    // artifact_workspace_file(path, content, options?).
    BuiltinSignature {
        name: "artifact_workspace_file",
        params: &[
            Param::new("path", TY_STRING),
            Param::new("content", TY_STRING),
            Param::optional("options", TY_DICT_OR_NIL),
        ],
        returns: TY_DICT,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    // artifact_workspace_snapshot(paths, summary?, options?).
    BuiltinSignature {
        name: "artifact_workspace_snapshot",
        params: &[
            Param::new("paths", TY_ANY),
            Param::optional("summary", TY_STRING_OR_NIL),
            Param::optional("options", TY_DICT_OR_NIL),
        ],
        returns: TY_DICT,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    // assemble_context(options) — async context-pack builder. The single
    // options dict carries `artifacts`, `strategy`, `query`, optional
    // `ranker_callback`, etc.
    BuiltinSignature {
        name: "assemble_context",
        params: &[Param::new("options", TY_DICT)],
        returns: TY_DICT,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    // context_pack_manifest(payload) — normalize a manifest dict.
    BuiltinSignature {
        name: "context_pack_manifest",
        params: &[Param::new("payload", TY_DICT)],
        returns: TY_DICT,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    // context_pack_manifest_parse(source) — parse a manifest source
    // string into a normalized manifest dict.
    BuiltinSignature {
        name: "context_pack_manifest_parse",
        params: &[Param::new("source", TY_STRING)],
        returns: TY_DICT,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    // context_pack_suggestions(events?, options?) -> list of suggested
    // context-pack adjustments based on recorded friction events.
    BuiltinSignature {
        name: "context_pack_suggestions",
        params: &[
            Param::optional("events", Ty::Union(&[TY_LIST, TY_NIL])),
            Param::optional("options", TY_DICT_OR_NIL),
        ],
        returns: TY_LIST,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    // continue_as_new(target) — bump the workflow generation, clear
    // pending responses, and return updated status.
    BuiltinSignature {
        name: "continue_as_new",
        params: &[Param::new("target", TY_WORKFLOW_TARGET)],
        returns: TY_DICT,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    // eval_metric(name, value, metadata?) — record a metric on the
    // current eval thread.
    BuiltinSignature {
        name: "eval_metric",
        params: &[
            Param::new("name", TY_STRING),
            Param::new("value", TY_ANY),
            Param::optional("metadata", TY_DICT_OR_NIL),
        ],
        returns: TY_NIL,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    // eval_metrics() -> list of `{name, value, metadata?}` dicts.
    BuiltinSignature {
        name: "eval_metrics",
        params: &[],
        returns: TY_LIST,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    // eval_pack_manifest(payload) — normalize an eval pack manifest dict.
    BuiltinSignature {
        name: "eval_pack_manifest",
        params: &[Param::new("payload", TY_DICT)],
        returns: TY_DICT,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    // eval_pack_run(payload) — execute the eval pack described by
    // `payload` and return its run summary.
    BuiltinSignature {
        name: "eval_pack_run",
        params: &[Param::new("payload", TY_DICT)],
        returns: TY_DICT,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    // persona_eval_ladder_manifest(payload) — normalize a persona eval
    // ladder manifest dict.
    BuiltinSignature {
        name: "persona_eval_ladder_manifest",
        params: &[Param::new("payload", TY_DICT)],
        returns: TY_DICT,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    // persona_eval_ladder_run(payload) — execute the persona eval
    // ladder described by `payload`.
    BuiltinSignature {
        name: "persona_eval_ladder_run",
        params: &[Param::new("payload", TY_DICT)],
        returns: TY_DICT,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    // eval_suite_manifest(payload) — normalize an eval-suite manifest
    // dict.
    BuiltinSignature {
        name: "eval_suite_manifest",
        params: &[Param::new("payload", TY_DICT)],
        returns: TY_DICT,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    // eval_suite_run(payload) — execute the eval suite described by
    // `payload`.
    BuiltinSignature {
        name: "eval_suite_run",
        params: &[Param::new("payload", TY_DICT)],
        returns: TY_DICT,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    // friction_clear() — reset the in-memory friction event log.
    BuiltinSignature {
        name: "friction_clear",
        params: &[],
        returns: TY_NIL,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    // friction_eval_fixture(fixture) — replay a friction fixture and
    // return pass/failure diagnostics.
    BuiltinSignature {
        name: "friction_eval_fixture",
        params: &[Param::new("fixture", TY_DICT)],
        returns: TY_DICT,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    // friction_event(payload) — normalize a friction event payload.
    BuiltinSignature {
        name: "friction_event",
        params: &[Param::new("payload", TY_DICT)],
        returns: TY_DICT,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    // friction_events() -> list of recorded friction events.
    BuiltinSignature {
        name: "friction_events",
        params: &[],
        returns: TY_LIST,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    // friction_record(payload, options?) -> `{recorded, sink, event,
    // path?}` summary. Persists to JSONL when `log_path` is set or
    // `HARN_FRICTION_LOG` env var is present.
    BuiltinSignature {
        name: "friction_record",
        params: &[
            Param::new("payload", TY_DICT),
            Param::optional("options", TY_DICT_OR_NIL),
        ],
        returns: TY_DICT,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    // handoff(payload) — normalize a handoff payload (or an artifact
    // dict) into a canonical handoff record.
    BuiltinSignature {
        name: "handoff",
        params: &[Param::new("payload", TY_DICT)],
        returns: TY_DICT,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    // handoff_context(payload) -> rendered handoff context string.
    BuiltinSignature {
        name: "handoff_context",
        params: &[Param::new("payload", TY_DICT)],
        returns: TY_STRING,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    // load_run_tree(path) — load a run record tree from disk.
    BuiltinSignature {
        name: "load_run_tree",
        params: &[Param::new("path", TY_STRING)],
        returns: TY_DICT,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    // render_always_on_catalog(entries_or_registry, budget?) -> rendered
    // catalog string capped at `budget` tokens (defaults to 2000).
    BuiltinSignature {
        name: "render_always_on_catalog",
        params: &[
            Param::new("entries_or_registry", Ty::Union(&[TY_LIST, TY_DICT])),
            Param::optional("budget", TY_INT),
        ],
        returns: TY_STRING,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    // run_record(payload) — normalize a run-record dict.
    BuiltinSignature {
        name: "run_record",
        params: &[Param::new("payload", TY_DICT)],
        returns: TY_DICT,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    // run_record_diff(left, right) -> diff dict.
    BuiltinSignature {
        name: "run_record_diff",
        params: &[Param::new("left", TY_DICT), Param::new("right", TY_DICT)],
        returns: TY_DICT,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    // run_record_eval(run, fixture?) -> evaluation result.
    BuiltinSignature {
        name: "run_record_eval",
        params: &[
            Param::new("run", TY_DICT),
            Param::optional("fixture", TY_DICT_OR_NIL),
        ],
        returns: TY_DICT,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    // run_record_eval_suite(cases) -> aggregate evaluation result.
    BuiltinSignature {
        name: "run_record_eval_suite",
        params: &[Param::new("cases", TY_LIST)],
        returns: TY_DICT,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    // run_record_fixture(run) -> replay fixture dict derived from a run.
    BuiltinSignature {
        name: "run_record_fixture",
        params: &[Param::new("run", TY_DICT)],
        returns: TY_DICT,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    // run_record_load(path) -> loaded run-record dict.
    BuiltinSignature {
        name: "run_record_load",
        params: &[Param::new("path", TY_STRING)],
        returns: TY_DICT,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    // run_record_save(run, path?) -> `{path, run}`.
    BuiltinSignature {
        name: "run_record_save",
        params: &[
            Param::new("run", TY_DICT),
            Param::optional("path", TY_STRING_OR_NIL),
        ],
        returns: TY_DICT,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    // select_artifacts_adaptive(artifacts, policy?) -> list of selected
    // artifacts after dedup, microcompaction, and policy-driven
    // selection.
    BuiltinSignature {
        name: "select_artifacts_adaptive",
        params: &[
            Param::optional("artifacts", Ty::Union(&[TY_LIST, TY_NIL])),
            Param::optional("policy", TY_DICT_OR_NIL),
        ],
        returns: TY_LIST,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    // workflow.continue_as_new(target) -> updated status dict.
    BuiltinSignature {
        name: "workflow.continue_as_new",
        params: &[Param::new("target", TY_WORKFLOW_TARGET)],
        returns: TY_DICT,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    // workflow.pause(target) -> updated status dict.
    BuiltinSignature {
        name: "workflow.pause",
        params: &[Param::new("target", TY_WORKFLOW_TARGET)],
        returns: TY_DICT,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    // workflow.publish_query(target, name, value) -> publish-summary
    // dict.
    BuiltinSignature {
        name: "workflow.publish_query",
        params: &[
            Param::new("target", TY_WORKFLOW_TARGET),
            Param::new("name", TY_STRING),
            Param::new("value", TY_ANY),
        ],
        returns: TY_DICT,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    // workflow.query(target, name) -> stored query value (any).
    BuiltinSignature {
        name: "workflow.query",
        params: &[
            Param::new("target", TY_WORKFLOW_TARGET),
            Param::new("name", TY_STRING),
        ],
        returns: TY_ANY,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    // workflow.receive(target) -> next message dict | nil.
    BuiltinSignature {
        name: "workflow.receive",
        params: &[Param::new("target", TY_WORKFLOW_TARGET)],
        returns: TY_DICT_OR_NIL,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    // workflow.respond_update(target, request_id, value, name?).
    BuiltinSignature {
        name: "workflow.respond_update",
        params: &[
            Param::new("target", TY_WORKFLOW_TARGET),
            Param::new("request_id", TY_STRING),
            Param::new("value", TY_ANY),
            Param::optional("name", TY_STRING_OR_NIL),
        ],
        returns: TY_DICT,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    // workflow.resume(target) -> updated status dict.
    BuiltinSignature {
        name: "workflow.resume",
        params: &[Param::new("target", TY_WORKFLOW_TARGET)],
        returns: TY_DICT,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    // workflow.signal(target, name, payload?) -> enqueue summary dict.
    BuiltinSignature {
        name: "workflow.signal",
        params: &[
            Param::new("target", TY_WORKFLOW_TARGET),
            Param::new("name", TY_STRING),
            Param::optional("payload", TY_ANY),
        ],
        returns: TY_DICT,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    // workflow.status(target) -> status dict.
    BuiltinSignature {
        name: "workflow.status",
        params: &[Param::new("target", TY_WORKFLOW_TARGET)],
        returns: TY_DICT,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    // workflow.update(target, name, payload?, options?) — async; returns
    // the update response value (caller-defined shape, hence `any`).
    BuiltinSignature {
        name: "workflow.update",
        params: &[
            Param::new("target", TY_WORKFLOW_TARGET),
            Param::new("name", TY_STRING),
            Param::optional("payload", TY_ANY),
            Param::optional("options", TY_DICT_OR_NIL),
        ],
        returns: TY_ANY,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    // workflow_clone(workflow) -> cloned workflow graph dict.
    BuiltinSignature {
        name: "workflow_clone",
        params: &[Param::new("workflow", TY_DICT)],
        returns: TY_DICT,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    // workflow_commit(workflow, reason?) -> committed graph dict.
    BuiltinSignature {
        name: "workflow_commit",
        params: &[
            Param::new("workflow", TY_DICT),
            Param::optional("reason", TY_STRING_OR_NIL),
        ],
        returns: TY_DICT,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    // workflow_diff(left, right) -> `{changed, left, right}` dict.
    BuiltinSignature {
        name: "workflow_diff",
        params: &[Param::new("left", TY_DICT), Param::new("right", TY_DICT)],
        returns: TY_DICT,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    // workflow_execute(task, workflow, artifacts?, options?) — async;
    // runs the workflow and returns `{status, run, artifacts,
    // transcript, path}`.
    BuiltinSignature {
        name: "workflow_execute",
        params: &[
            Param::new("task", TY_STRING),
            Param::new("workflow", TY_DICT),
            Param::optional("artifacts", Ty::Union(&[TY_LIST, TY_NIL])),
            Param::optional("options", TY_DICT_OR_NIL),
        ],
        returns: TY_DICT,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    // workflow_graph(workflow?) -> normalized graph dict.
    BuiltinSignature {
        name: "workflow_graph",
        params: &[Param::optional("workflow", TY_DICT_OR_NIL)],
        returns: TY_DICT,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    // workflow_insert_node(workflow, node, edge?) -> updated graph dict.
    BuiltinSignature {
        name: "workflow_insert_node",
        params: &[
            Param::new("workflow", TY_DICT),
            Param::new("node", TY_DICT),
            Param::optional("edge", TY_DICT_OR_NIL),
        ],
        returns: TY_DICT,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    // workflow_inspect(workflow, ceiling?) -> `{graph, validation,
    // node_count, edge_count}` dict.
    BuiltinSignature {
        name: "workflow_inspect",
        params: &[
            Param::optional("workflow", TY_DICT_OR_NIL),
            Param::optional("ceiling", TY_DICT_OR_NIL),
        ],
        returns: TY_DICT,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    // workflow_policy_report(workflow, ceiling?) -> per-node policy
    // report dict.
    BuiltinSignature {
        name: "workflow_policy_report",
        params: &[
            Param::optional("workflow", TY_DICT_OR_NIL),
            Param::optional("ceiling", TY_DICT_OR_NIL),
        ],
        returns: TY_DICT,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    // workflow_replace_node(workflow, node_id, node) -> updated graph
    // dict.
    BuiltinSignature {
        name: "workflow_replace_node",
        params: &[
            Param::new("workflow", TY_DICT),
            Param::new("node_id", TY_STRING),
            Param::new("node", TY_DICT),
        ],
        returns: TY_DICT,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    // workflow_rewire(workflow, from, to, branch?) -> updated graph dict.
    BuiltinSignature {
        name: "workflow_rewire",
        params: &[
            Param::new("workflow", TY_DICT),
            Param::new("from", TY_STRING),
            Param::new("to", TY_STRING),
            Param::optional("branch", TY_STRING_OR_NIL),
        ],
        returns: TY_DICT,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    // workflow_set_auto_compact(workflow, node_id, policy) -> updated
    // graph dict.
    BuiltinSignature {
        name: "workflow_set_auto_compact",
        params: &[
            Param::new("workflow", TY_DICT),
            Param::new("node_id", TY_STRING),
            Param::new("policy", TY_DICT),
        ],
        returns: TY_DICT,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    // workflow_set_context_policy(workflow, node_id, policy) -> updated
    // graph dict.
    BuiltinSignature {
        name: "workflow_set_context_policy",
        params: &[
            Param::new("workflow", TY_DICT),
            Param::new("node_id", TY_STRING),
            Param::new("policy", TY_DICT),
        ],
        returns: TY_DICT,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    // workflow_set_model_policy(workflow, node_id, policy) -> updated
    // graph dict.
    BuiltinSignature {
        name: "workflow_set_model_policy",
        params: &[
            Param::new("workflow", TY_DICT),
            Param::new("node_id", TY_STRING),
            Param::new("policy", TY_DICT),
        ],
        returns: TY_DICT,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    // workflow_set_output_visibility(workflow, node_id, visibility) ->
    // updated graph dict. `visibility` is `string | nil`.
    BuiltinSignature {
        name: "workflow_set_output_visibility",
        params: &[
            Param::new("workflow", TY_DICT),
            Param::new("node_id", TY_STRING),
            Param::new("visibility", TY_STRING_OR_NIL),
        ],
        returns: TY_DICT,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    // workflow_validate(workflow?, ceiling?) -> validation report dict
    // (`{valid, errors, warnings, ...}`).
    BuiltinSignature {
        name: "workflow_validate",
        params: &[
            Param::optional("workflow", TY_DICT_OR_NIL),
            Param::optional("ceiling", TY_DICT_OR_NIL),
        ],
        returns: TY_DICT,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
];
