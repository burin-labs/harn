//! Typed effect records carried on `HandoffArtifact` envelopes.
//!
//! `EffectRecord` is the leaf payload that names a single side-effect a
//! spawned child agent may exercise (e.g. `Net write to https://api.example`,
//! `Fs read of /workspace/src`). The set sits on each handoff so the
//! dispatcher (E5.4) and the OpenTrustGraph receipt chain (E5.5) can prove
//! the child never escaped its parent's effect grant.
//!
//! Computation at spawn time walks the child's entrypoint module via the
//! same capability analysis `harn graph --json` uses (issue HARN-#1758),
//! plus a conservative AST walker for harness calls embedded in inline spawn
//! configs. The two extraction paths feed one canonicalization step so
//! downstream consumers see a single deduped, deterministically ordered list.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use harn_ir::{CallClassification, Capability, LiteralValue, NodeSemantics};
use harn_parser::{Node, SNode};

use super::CapabilityPolicy;

/// Discriminator for the kind of effect captured. Matches the
/// classification used by the OpenTrustGraph receipt format (E5.5).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Ord, PartialOrd, Hash)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EffectKind {
    /// Reads or writes against the host's stdio streams.
    Stdio,
    /// Filesystem access (read, write, list, delete, ...).
    Fs,
    /// Network access (HTTP, SSE, WebSocket).
    Net,
    /// LLM model calls — captures the provider and model when statically
    /// known so the receipt chain can name the inference dependency.
    Llm {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
    },
    /// Pipeline-declared tool dispatched through the agent loop.
    Tool { name: String },
    /// Bridged host capability call (`host_call(capability.operation, ...)`).
    Hostcall { name: String },
    /// Targeted delegation to a named persona / sub-agent identity.
    Persona { id: String },
    /// Spawn / sub-agent / worker dispatch primitives.
    Spawn,
}

/// What kind of interaction the effect represents. Mirrors the
/// `read | write | mutate | observe` taxonomy the receipt schema uses.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Ord, PartialOrd, Hash)]
#[serde(rename_all = "snake_case")]
pub enum EffectScope {
    /// Pure read: no observable state change for other actors.
    Read,
    /// Write that creates or replaces state owned by this effect.
    Write,
    /// Mutation of state that may already be observed by other actors.
    Mutate,
    /// Side-channel observation (stdio sink, telemetry emission, ...).
    Observe,
}

/// Single typed effect carried on a `HandoffArtifact.effects` entry.
///
/// `resource` is an opaque, statically-known target identifier (path,
/// URL, tool id, persona id). The dispatcher (E5.4) is free to enforce
/// ⊆ against the resource string; when no resource can be derived the
/// field stays `None`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Ord, PartialOrd, Hash)]
pub struct EffectRecord {
    pub kind: EffectKind,
    pub scope: EffectScope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource: Option<String>,
}

impl EffectRecord {
    pub fn new(kind: EffectKind, scope: EffectScope) -> Self {
        Self {
            kind,
            scope,
            resource: None,
        }
    }

    pub fn with_resource(mut self, resource: impl Into<String>) -> Self {
        let resource = resource.into();
        self.resource = if resource.is_empty() {
            None
        } else {
            Some(resource)
        };
        self
    }
}

/// Compute the effect set for a child agent's entrypoint module.
///
/// Parses `source`, walks the resulting AST via the same `harn_ir`
/// capability analyzer that backs `harn graph --json`, and supplements it with
/// a direct walk for harness calls in inline spawn configs. The result is
/// deterministically ordered and deduplicated.
///
/// When `ceiling` is provided, the result is clamped to it: an effect
/// is dropped if the ceiling's `capabilities` map is non-empty and does
/// not allow the matching capability/op, or if the effect's
/// `side_effect_level` exceeds the ceiling's `side_effect_level`. Empty
/// ceilings are treated as "no constraint" — the same convention the
/// rest of the policy machinery uses.
pub fn compute_handoff_effects(
    source: &str,
    ceiling: Option<&CapabilityPolicy>,
) -> Vec<EffectRecord> {
    let Ok(program) = harn_parser::parse_source(source) else {
        return Vec::new();
    };
    let mut collected: BTreeSet<EffectRecord> = BTreeSet::new();

    // Builtin / host-call effects via the existing IR analyzer — same
    // surface `harn graph --json` reads.
    let report = harn_ir::analyze_program(&program);
    for handler in &report.handlers {
        for node in &handler.nodes {
            let NodeSemantics::Call(call) = &node.semantics else {
                continue;
            };
            for effect in effects_from_call(call) {
                collected.insert(effect);
            }
        }
    }

    // Spawn preflight also wraps object-literal configs and inline closures
    // where the IR handler pass cannot always attribute harness calls. Keep
    // this broad direct pass so parent/child effect checks stay conservative.
    for node in &program {
        walk_for_harness_effects(node, &mut collected);
    }

    let mut effects: Vec<EffectRecord> = collected.into_iter().collect();
    if let Some(ceiling) = ceiling {
        effects.retain(|effect| effect_allowed_by_ceiling(effect, ceiling));
    }
    effects
}

fn effects_from_call(call: &harn_ir::CallSemantics) -> Vec<EffectRecord> {
    // Primary extraction: name-based builtin recognition. This path
    // carries the richest information (resource from literal args,
    // provider/model on LLM calls) and is the authoritative shape.
    if call.name == "__files_upload" || call.name == "upload" {
        let mut effects = Vec::with_capacity(2);
        let mut fs = EffectRecord::new(EffectKind::Fs, EffectScope::Read);
        if let Some(path) = call.literal_args.first().and_then(literal_as_str) {
            fs = fs.with_resource(path);
        }
        effects.push(fs);
        let mut net = EffectRecord::new(EffectKind::Net, EffectScope::Write);
        if let Some(provider) = call.literal_args.get(1).and_then(literal_as_str) {
            net = net.with_resource(provider);
        }
        effects.push(net);
        return effects;
    }
    if let Some(effect) = builtin_effect(&call.name) {
        return vec![annotate_with_resource(effect, call)];
    }
    if call.name == "host_call" {
        if let Some(operation) = call.literal_args.first().and_then(literal_as_str) {
            return vec![EffectRecord::new(
                EffectKind::Hostcall {
                    name: operation.to_string(),
                },
                hostcall_scope(operation),
            )];
        }
    }
    // Fallback: pipeline-declared tools and other capabilities the
    // builtin recognizer doesn't know about (custom tool_call dispatch,
    // user-defined capability classifications). Only consulted when the
    // primary extraction returns nothing — same call shouldn't produce
    // two records with different `resource` fields just because two
    // extraction paths happen to match.
    if let CallClassification::Capabilities(capability_effects) = &call.classification {
        return capability_effects
            .iter()
            .filter_map(capability_effect_to_record)
            .collect();
    }
    Vec::new()
}

fn builtin_effect(name: &str) -> Option<EffectRecord> {
    match name {
        // stdio
        "print" | "println" | "eprint" | "eprintln" | "write_stdout" | "write_stderr"
        | "__io_print" | "__io_println" | "__io_eprint" | "__io_eprintln" | "__io_write_stdout"
        | "__io_write_stderr" => Some(EffectRecord::new(EffectKind::Stdio, EffectScope::Observe)),
        "read_line" | "read_stdin" | "prompt_user" | "__io_read_line" => {
            Some(EffectRecord::new(EffectKind::Stdio, EffectScope::Read))
        }

        // fs reads
        "read_file"
        | "read_file_bytes"
        | "read_file_result"
        | "render"
        | "render_prompt"
        | "render_with_provenance"
        | "find_text"
        | "read_lines"
        | "list_dir"
        | "walk_dir"
        | "glob"
        | "file_exists"
        | "stat" => Some(EffectRecord::new(EffectKind::Fs, EffectScope::Read)),

        // fs writes
        "write_file" | "write_file_bytes" | "append_file" | "mkdir" | "mkdtemp" | "copy_file"
        | "move_file" => Some(EffectRecord::new(EffectKind::Fs, EffectScope::Write)),
        "delete_file" => Some(EffectRecord::new(EffectKind::Fs, EffectScope::Mutate)),
        "apply_edit" => Some(EffectRecord::new(EffectKind::Fs, EffectScope::Mutate)),

        // network — mirrors `is_network_call` in harn-ir; the EffectKind
        // is identical for every transport because the dispatcher (E5.4)
        // enforces the ⊆ relation at the `Net` granularity, not per-verb.
        "http_get"
        | "http_post"
        | "http_put"
        | "http_patch"
        | "http_delete"
        | "http_request"
        | "http_download"
        | "http_session"
        | "http_session_request"
        | "http_session_close"
        | "http_stream_open"
        | "http_stream_read"
        | "http_stream_close"
        | "sse_connect"
        | "sse_receive"
        | "sse_close"
        | "sse_server_response"
        | "sse_server_send"
        | "sse_server_heartbeat"
        | "sse_server_flush"
        | "sse_server_close"
        | "sse_server_cancel"
        | "websocket_connect"
        | "websocket_accept"
        | "websocket_send"
        | "websocket_receive"
        | "websocket_close"
        | "websocket_route"
        | "websocket_server"
        | "websocket_server_close"
        | "unix_socket_json_request"
        | "__net_unix_socket_json_request" => {
            Some(EffectRecord::new(EffectKind::Net, EffectScope::Write))
        }

        // llm
        "llm_call"
        | "llm_call_safe"
        | "llm_stream_call"
        | "llm_call_structured"
        | "llm_call_structured_safe"
        | "llm_call_structured_result"
        | "llm_completion"
        | "agent_llm_turn"
        | "agent_turn"
        | "agent_loop" => Some(EffectRecord::new(
            EffectKind::Llm {
                provider: None,
                model: None,
            },
            EffectScope::Write,
        )),
        "llm_catalog" | "llm_provider_status" => Some(EffectRecord::new(
            EffectKind::Llm {
                provider: None,
                model: None,
            },
            EffectScope::Read,
        )),
        "llm_catalog_refresh" => Some(EffectRecord::new(
            EffectKind::Llm {
                provider: None,
                model: None,
            },
            EffectScope::Write,
        )),

        // spawn / worker dispatch
        "spawn_agent"
        | "send_input"
        | "resume_agent"
        | "wait_agent"
        | "close_agent"
        | "worker_trigger"
        | "__host_sub_agent_run"
        | "__host_worker_spawn"
        | "__host_worker_send_input"
        | "__host_worker_resume"
        | "__host_worker_trigger"
        | "__host_worker_wait"
        | "__host_worker_close" => Some(EffectRecord::new(EffectKind::Spawn, EffectScope::Write)),

        // pipeline-declared tools dispatched through tool_call
        "tool_call" | "host_tool_call" => Some(EffectRecord::new(
            EffectKind::Tool {
                name: String::new(),
            },
            EffectScope::Write,
        )),

        _ => None,
    }
}

fn annotate_with_resource(mut effect: EffectRecord, call: &harn_ir::CallSemantics) -> EffectRecord {
    // Effect resources are best-effort: first literal arg (path / url /
    // tool name) when statically derivable. Unknown literals stay
    // unannotated rather than guessed.
    match &mut effect.kind {
        EffectKind::Llm { provider, model } => {
            for arg in &call.literal_args {
                if let LiteralValue::Dict(entries) = arg {
                    if let Some(value) = entries.get("provider").and_then(literal_as_str) {
                        *provider = Some(value.to_string());
                    }
                    if let Some(value) = entries.get("model").and_then(literal_as_str) {
                        *model = Some(value.to_string());
                    }
                }
            }
        }
        EffectKind::Tool { name } => {
            if let Some(value) = call.literal_args.first().and_then(literal_as_str) {
                *name = value.to_string();
            }
        }
        _ => {
            if let Some(value) = call.literal_args.first().and_then(literal_as_str) {
                effect.resource = Some(value.to_string());
            }
        }
    }
    effect
}

fn capability_effect_to_record(effect: &harn_ir::CapabilityEffect) -> Option<EffectRecord> {
    let (kind, scope) = match effect.capability {
        Capability::WorkspaceMutation => (EffectKind::Fs, EffectScope::Mutate),
        Capability::CommandExecution => (
            EffectKind::Hostcall {
                name: format!("process.{}", effect.operation),
            },
            EffectScope::Write,
        ),
        Capability::NetworkAccess => (EffectKind::Net, EffectScope::Write),
        Capability::ConnectorAccess => (
            EffectKind::Hostcall {
                name: if effect.operation.is_empty() {
                    "connector.call".to_string()
                } else {
                    format!("connector.{}", effect.operation)
                },
            },
            EffectScope::Write,
        ),
        Capability::ModelCall => (
            EffectKind::Llm {
                provider: None,
                model: None,
            },
            EffectScope::Write,
        ),
        Capability::WorkerDispatch => (EffectKind::Spawn, EffectScope::Write),
        Capability::HumanApproval => return None,
        Capability::AutonomyPolicy => return None,
    };
    let resource = effect.path.clone();
    Some(EffectRecord {
        kind,
        scope,
        resource,
    })
}

fn hostcall_scope(operation: &str) -> EffectScope {
    match operation {
        op if op.starts_with("workspace.read") || op.starts_with("workspace.list") => {
            EffectScope::Read
        }
        op if op.starts_with("workspace.write") || op == "workspace.apply_edit" => {
            EffectScope::Mutate
        }
        op if op.starts_with("process.") => EffectScope::Write,
        _ => EffectScope::Write,
    }
}

fn literal_as_str(value: &LiteralValue) -> Option<&str> {
    match value {
        LiteralValue::String(value) | LiteralValue::Identifier(value) => Some(value.as_str()),
        _ => None,
    }
}

fn walk_for_harness_effects(node: &SNode, out: &mut BTreeSet<EffectRecord>) {
    if let Some(effect) = harness_method_effect(node) {
        out.insert(effect);
    }
    for child in child_nodes(node) {
        walk_for_harness_effects(child, out);
    }
}

fn harness_method_effect(node: &SNode) -> Option<EffectRecord> {
    let (object, method) = match &node.node {
        Node::MethodCall { object, method, .. }
        | Node::OptionalMethodCall { object, method, .. } => (object, method),
        _ => return None,
    };
    let (sub_handle, root) = harness_sub_handle(object)?;
    if !is_harness_root(root) {
        return None;
    }
    let (kind, scope) = match (sub_handle.as_str(), method.as_str()) {
        ("stdio", "print" | "println" | "eprint" | "eprintln") => {
            (EffectKind::Stdio, EffectScope::Observe)
        }
        ("stdio", "read_line" | "prompt") => (EffectKind::Stdio, EffectScope::Read),
        ("term", "width" | "height" | "read_password") => (EffectKind::Stdio, EffectScope::Read),
        ("clock", _) => return None,
        ("env", "set" | "unset") => (
            EffectKind::Hostcall {
                name: "env.set".to_string(),
            },
            EffectScope::Mutate,
        ),
        ("env", _) => (
            EffectKind::Hostcall {
                name: "env.get".to_string(),
            },
            EffectScope::Read,
        ),
        ("random", _) => return None,
        ("fs", "read_file" | "read_text" | "read" | "exists" | "list_dir" | "stat") => {
            (EffectKind::Fs, EffectScope::Read)
        }
        ("fs", "write_file" | "write_text" | "append_file" | "mkdir" | "mkdtemp" | "copy_file") => {
            (EffectKind::Fs, EffectScope::Write)
        }
        ("fs", "delete_file" | "delete" | "remove") => (EffectKind::Fs, EffectScope::Mutate),
        ("fs", _) => (EffectKind::Fs, EffectScope::Read),
        ("net", _) => (EffectKind::Net, EffectScope::Write),
        ("process", "spawn_captured") => (
            EffectKind::Hostcall {
                name: "process.spawn_captured".to_string(),
            },
            EffectScope::Write,
        ),
        ("crypto", "sha256") => return None,
        // System-introspection methods (`cpu`, `memory`, `gpus`,
        // `temperature`, `platform`, `processes`) are pure host reads
        // — no state mutation, no resource consumed. They're gated by
        // the harness capability handle itself, so deny-by-default
        // policies still block them, but they don't produce a typed
        // effect record for child grant enforcement.
        ("system", _) => return None,
        ("llm", "catalog" | "providers") => (
            EffectKind::Llm {
                provider: None,
                model: None,
            },
            EffectScope::Read,
        ),
        ("llm", _) => return None,
        _ => return None,
    };
    Some(EffectRecord::new(kind, scope))
}

fn harness_sub_handle(node: &SNode) -> Option<(String, &SNode)> {
    match &node.node {
        Node::PropertyAccess { object, property }
        | Node::OptionalPropertyAccess { object, property } => {
            Some((property.clone(), object.as_ref()))
        }
        _ => None,
    }
}

fn is_harness_root(node: &SNode) -> bool {
    matches!(&node.node, Node::Identifier(name) if name == "harness")
}

fn child_nodes(node: &SNode) -> Vec<&SNode> {
    let mut children: Vec<&SNode> = Vec::new();
    match &node.node {
        Node::AttributedDecl { inner, .. } => children.push(inner.as_ref()),
        Node::Pipeline { body, .. }
        | Node::FnDecl { body, .. }
        | Node::ToolDecl { body, .. }
        | Node::SpawnExpr { body }
        | Node::Retry { body, .. }
        | Node::TryExpr { body }
        | Node::DeferStmt { body }
        | Node::Block(body)
        | Node::OverrideDecl { body, .. } => children.extend(body.iter()),
        Node::MutexBlock { key, body } => {
            children.extend(key.as_deref());
            children.extend(body.iter());
        }
        Node::ImplBlock { methods, .. } => children.extend(methods.iter()),
        Node::IfElse {
            condition,
            then_body,
            else_body,
        } => {
            children.push(condition.as_ref());
            children.extend(then_body.iter());
            if let Some(else_body) = else_body.as_ref() {
                children.extend(else_body.iter());
            }
        }
        Node::ForIn { iterable, body, .. } => {
            children.push(iterable.as_ref());
            children.extend(body.iter());
        }
        Node::WhileLoop { condition, body } => {
            children.push(condition.as_ref());
            children.extend(body.iter());
        }
        Node::MatchExpr { value, arms } => {
            children.push(value.as_ref());
            for arm in arms {
                if let Some(guard) = arm.guard.as_ref() {
                    children.push(guard.as_ref());
                }
                children.extend(arm.body.iter());
            }
        }
        Node::CostRoute { options, body } => {
            for (_key, value) in options {
                children.push(value);
            }
            children.extend(body.iter());
        }
        Node::ReturnStmt { value } => {
            if let Some(value) = value.as_ref() {
                children.push(value.as_ref());
            }
        }
        Node::ThrowStmt { value } => children.push(value.as_ref()),
        Node::TryCatch {
            body,
            catch_body,
            finally_body,
            ..
        } => {
            children.extend(body.iter());
            children.extend(catch_body.iter());
            if let Some(finally_body) = finally_body.as_ref() {
                children.extend(finally_body.iter());
            }
        }
        Node::SkillDecl { fields, .. } => {
            for (_name, value) in fields {
                children.push(value);
            }
        }
        Node::EvalPackDecl {
            fields,
            body,
            summarize,
            ..
        } => {
            for (_name, value) in fields {
                children.push(value);
            }
            children.extend(body.iter());
            if let Some(summarize) = summarize.as_ref() {
                children.extend(summarize.iter());
            }
        }
        Node::LetBinding { value, .. } | Node::VarBinding { value, .. } => {
            children.push(value.as_ref());
        }
        Node::ConstBinding { value, .. } => {
            children.push(value.as_ref());
        }
        Node::DeadlineBlock { duration, body } => {
            children.push(duration.as_ref());
            children.extend(body.iter());
        }
        Node::YieldExpr { value } => {
            if let Some(value) = value.as_ref() {
                children.push(value.as_ref());
            }
        }
        Node::EmitExpr { value } => children.push(value.as_ref()),
        Node::GuardStmt {
            condition,
            else_body,
        } => {
            children.push(condition.as_ref());
            children.extend(else_body.iter());
        }
        Node::RequireStmt { condition, message } => {
            children.push(condition.as_ref());
            if let Some(message) = message.as_ref() {
                children.push(message.as_ref());
            }
        }
        Node::HitlExpr { args, .. } => {
            for arg in args {
                children.push(&arg.value);
            }
        }
        Node::Parallel {
            expr,
            body,
            options,
            ..
        } => {
            children.push(expr.as_ref());
            children.extend(body.iter());
            for (_key, value) in options {
                children.push(value);
            }
        }
        Node::SelectExpr {
            cases,
            timeout,
            default_body,
        } => {
            for case in cases {
                children.push(case.channel.as_ref());
                children.extend(case.body.iter());
            }
            if let Some((duration, body)) = timeout.as_ref() {
                children.push(duration.as_ref());
                children.extend(body.iter());
            }
            if let Some(body) = default_body.as_ref() {
                children.extend(body.iter());
            }
        }
        Node::FunctionCall { args, .. } => children.extend(args.iter()),
        Node::MethodCall { object, args, .. } | Node::OptionalMethodCall { object, args, .. } => {
            children.push(object.as_ref());
            children.extend(args.iter());
        }
        Node::PropertyAccess { object, .. } | Node::OptionalPropertyAccess { object, .. } => {
            children.push(object.as_ref());
        }
        Node::SubscriptAccess { object, index }
        | Node::OptionalSubscriptAccess { object, index } => {
            children.push(object.as_ref());
            children.push(index.as_ref());
        }
        Node::SliceAccess { object, start, end } => {
            children.push(object.as_ref());
            if let Some(start) = start.as_ref() {
                children.push(start.as_ref());
            }
            if let Some(end) = end.as_ref() {
                children.push(end.as_ref());
            }
        }
        Node::BinaryOp { left, right, .. } => {
            children.push(left.as_ref());
            children.push(right.as_ref());
        }
        Node::UnaryOp { operand, .. } => children.push(operand.as_ref()),
        Node::Ternary {
            condition,
            true_expr,
            false_expr,
        } => {
            children.push(condition.as_ref());
            children.push(true_expr.as_ref());
            children.push(false_expr.as_ref());
        }
        Node::Assignment { target, value, .. } => {
            children.push(target.as_ref());
            children.push(value.as_ref());
        }
        Node::EnumConstruct { args, .. } => children.extend(args.iter()),
        Node::StructConstruct { fields, .. } => {
            for entry in fields {
                children.push(&entry.key);
                children.push(&entry.value);
            }
        }
        Node::ListLiteral(items) => children.extend(items.iter()),
        Node::DictLiteral(entries) => {
            for entry in entries {
                children.push(&entry.key);
                children.push(&entry.value);
            }
        }
        Node::Spread(inner) => children.push(inner.as_ref()),
        Node::TryOperator { operand } | Node::TryStar { operand } => {
            children.push(operand.as_ref());
        }
        Node::OrPattern(items) => children.extend(items.iter()),
        Node::Closure { body, .. } => children.extend(body.iter()),
        Node::RangeExpr { start, end, .. } => {
            children.push(start.as_ref());
            children.push(end.as_ref());
        }
        _ => {}
    }
    children
}

fn effect_allowed_by_ceiling(effect: &EffectRecord, ceiling: &CapabilityPolicy) -> bool {
    if !ceiling.capabilities.is_empty() {
        let (capability, op) = effect_capability_op(effect);
        let allowed = ceiling
            .capabilities
            .get(capability)
            .is_some_and(|ops| ops.is_empty() || ops.iter().any(|allowed| allowed == op));
        if !allowed {
            return false;
        }
    }
    if let Some(ceiling_level) = ceiling.side_effect_level.as_deref() {
        let requested = side_effect_level_for(effect);
        if requested_exceeds_ceiling(requested, ceiling_level) {
            return false;
        }
    }
    true
}

fn effect_capability_op(effect: &EffectRecord) -> (&'static str, &'static str) {
    match (&effect.kind, effect.scope) {
        (EffectKind::Stdio, EffectScope::Read) => ("stdio", "read"),
        (EffectKind::Stdio, _) => ("stdio", "write"),
        (EffectKind::Fs, EffectScope::Read) => ("workspace", "read_text"),
        (EffectKind::Fs, EffectScope::Write) => ("workspace", "write_text"),
        (EffectKind::Fs, EffectScope::Mutate) => ("workspace", "apply_edit"),
        (EffectKind::Fs, EffectScope::Observe) => ("workspace", "exists"),
        (EffectKind::Net, _) => ("network", "http"),
        (EffectKind::Llm { .. }, EffectScope::Read) => ("llm", "catalog"),
        (EffectKind::Llm { .. }, _) => ("llm", "call"),
        (EffectKind::Tool { .. }, _) => ("host", "tool_call"),
        (EffectKind::Hostcall { .. }, _) => ("connector", "call"),
        (EffectKind::Persona { .. }, _) => ("worker", "dispatch"),
        (EffectKind::Spawn, _) => ("worker", "dispatch"),
    }
}

fn side_effect_level_for(effect: &EffectRecord) -> &'static str {
    match (&effect.kind, effect.scope) {
        (EffectKind::Stdio, _) => "read_only",
        (EffectKind::Fs, EffectScope::Read | EffectScope::Observe) => "read_only",
        (EffectKind::Fs, _) => "workspace_write",
        (EffectKind::Net, _) => "network",
        (EffectKind::Llm { .. }, EffectScope::Read) => "read_only",
        (EffectKind::Llm { .. }, _) => "network",
        (EffectKind::Tool { .. }, _) => "workspace_write",
        (EffectKind::Hostcall { name }, _) if name.starts_with("process.") => "process_exec",
        (EffectKind::Hostcall { .. }, _) => "read_only",
        (EffectKind::Persona { .. }, _) => "workspace_write",
        (EffectKind::Spawn, _) => "workspace_write",
    }
}

fn requested_exceeds_ceiling(requested: &str, ceiling: &str) -> bool {
    fn rank(value: &str) -> usize {
        match value {
            "none" => 0,
            "read_only" => 1,
            "workspace_write" => 2,
            "process_exec" => 3,
            "network" => 4,
            _ => 5,
        }
    }
    rank(requested) > rank(ceiling)
}

/// Round-trip a typed effect list through the `metadata` map a child
/// spawn-config carries. Pipelines that pre-compute effects can stash
/// them under `effects` and the spawn shim lifts them onto the handoff.
pub fn effects_from_metadata(metadata: &BTreeMap<String, serde_json::Value>) -> Vec<EffectRecord> {
    metadata
        .get("effects")
        .and_then(|value| serde_json::from_value::<Vec<EffectRecord>>(value.clone()).ok())
        .unwrap_or_default()
}

/// Decide whether `child` is covered by `parent`. An effect is covered
/// when the parent declares another record with the same kind family
/// and a scope that is at least as permissive. `resource` is treated
/// best-effort: when the parent carries a non-empty resource it must
/// match the child's resource exactly (and the child's resource must be
/// known); when the parent has no resource it covers any resource the
/// child names. This is the core of E5.4's `HARN-CAP-301` enforcement —
/// the dispatcher and the static analyzer share one implementation so
/// preflight and runtime never disagree.
fn parent_covers_child(parent: &EffectRecord, child: &EffectRecord) -> bool {
    if !effect_kind_family_matches(&parent.kind, &child.kind) {
        return false;
    }
    if !effect_scope_covers(parent.scope, child.scope) {
        return false;
    }
    match (parent.resource.as_deref(), child.resource.as_deref()) {
        (Some(""), _) => true,
        (Some(parent_resource), Some(child_resource)) => parent_resource == child_resource,
        (Some(_), None) => false,
        (None, _) => true,
    }
}

fn effect_kind_family_matches(parent: &EffectKind, child: &EffectKind) -> bool {
    match (parent, child) {
        (EffectKind::Stdio, EffectKind::Stdio)
        | (EffectKind::Fs, EffectKind::Fs)
        | (EffectKind::Net, EffectKind::Net)
        | (EffectKind::Spawn, EffectKind::Spawn) => true,
        (EffectKind::Llm { .. }, EffectKind::Llm { .. }) => true,
        (
            EffectKind::Tool {
                name: parent_name, ..
            },
            EffectKind::Tool {
                name: child_name, ..
            },
        ) => parent_name.is_empty() || parent_name == child_name,
        (
            EffectKind::Hostcall {
                name: parent_name, ..
            },
            EffectKind::Hostcall {
                name: child_name, ..
            },
        ) => parent_name.is_empty() || parent_name == child_name,
        (EffectKind::Persona { id: parent_id }, EffectKind::Persona { id: child_id }) => {
            parent_id.is_empty() || parent_id == child_id
        }
        _ => false,
    }
}

fn effect_scope_covers(parent: EffectScope, child: EffectScope) -> bool {
    fn rank(scope: EffectScope) -> u8 {
        match scope {
            EffectScope::Read => 1,
            EffectScope::Observe => 1,
            EffectScope::Write => 2,
            EffectScope::Mutate => 3,
        }
    }
    rank(parent) >= rank(child)
}

/// Compute the subset of `child` effects that are not covered by any
/// record in `parent`. An empty parent set is treated as "no declared
/// effects" — under E5.4 the dispatcher takes that to mean every child
/// effect is a violation, because a child can never out-grant an
/// undeclared parent. When `parent` is `None` enforcement is skipped
/// entirely (the caller has decided no static ceiling applies).
pub fn effect_subset_violations(
    parent: Option<&[EffectRecord]>,
    child: &[EffectRecord],
) -> Vec<EffectRecord> {
    let Some(parent) = parent else {
        return Vec::new();
    };
    child
        .iter()
        .filter(|effect| {
            !parent
                .iter()
                .any(|allowed| parent_covers_child(allowed, effect))
        })
        .cloned()
        .collect()
}

/// Short human-readable label for `effect.kind` used in
/// `EffectInheritanceViolation` messages and `HARN-CAP-301` diagnostics.
pub fn effect_kind_label(kind: &EffectKind) -> String {
    match kind {
        EffectKind::Stdio => "stdio".to_string(),
        EffectKind::Fs => "fs".to_string(),
        EffectKind::Net => "net".to_string(),
        EffectKind::Llm { provider, model } => match (provider.as_deref(), model.as_deref()) {
            (Some(provider), Some(model)) => format!("llm:{provider}/{model}"),
            (Some(provider), None) => format!("llm:{provider}"),
            (None, Some(model)) => format!("llm:{model}"),
            (None, None) => "llm".to_string(),
        },
        EffectKind::Tool { name } if !name.is_empty() => format!("tool:{name}"),
        EffectKind::Tool { .. } => "tool".to_string(),
        EffectKind::Hostcall { name } if !name.is_empty() => format!("hostcall:{name}"),
        EffectKind::Hostcall { .. } => "hostcall".to_string(),
        EffectKind::Persona { id } if !id.is_empty() => format!("persona:{id}"),
        EffectKind::Persona { .. } => "persona".to_string(),
        EffectKind::Spawn => "spawn".to_string(),
    }
}

/// One-line summary suitable for diagnostic messages and deny events.
pub fn effect_record_summary(effect: &EffectRecord) -> String {
    let scope = match effect.scope {
        EffectScope::Read => "read",
        EffectScope::Write => "write",
        EffectScope::Mutate => "mutate",
        EffectScope::Observe => "observe",
    };
    match effect.resource.as_deref() {
        Some(resource) if !resource.is_empty() => {
            format!(
                "{}:{} ({})",
                effect_kind_label(&effect.kind),
                scope,
                resource
            )
        }
        _ => format!("{}:{}", effect_kind_label(&effect.kind), scope),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn harness_net_call_yields_net_effect() {
        let source = r#"fn main(harness: Harness) { harness.net.get("https://example.test") }"#;
        let effects = compute_handoff_effects(source, None);
        assert!(
            effects
                .iter()
                .any(|effect| matches!(effect.kind, EffectKind::Net)
                    && effect.scope == EffectScope::Write),
            "expected Net write effect, got {effects:?}"
        );
    }

    #[test]
    fn harness_process_spawn_captured_yields_process_hostcall_effect() {
        let source =
            r#"fn main(harness: Harness) { harness.process.spawn_captured({cmd: "printf"}) }"#;
        let effects = compute_handoff_effects(source, None);
        assert!(
            effects.iter().any(|effect| {
                matches!(
                    &effect.kind,
                    EffectKind::Hostcall { name } if name == "process.spawn_captured"
                ) && effect.scope == EffectScope::Write
            }),
            "expected process hostcall write effect, got {effects:?}"
        );
    }

    #[test]
    fn http_get_builtin_yields_net_effect_with_resource() {
        let source = r#"fn main() { http_get("https://example.test/api") }"#;
        let effects = compute_handoff_effects(source, None);
        let net = effects
            .iter()
            .find(|effect| matches!(effect.kind, EffectKind::Net))
            .expect("net effect");
        assert_eq!(net.scope, EffectScope::Write);
        assert_eq!(net.resource.as_deref(), Some("https://example.test/api"));
    }

    #[test]
    fn unix_socket_json_request_yields_net_effect_with_resource() {
        let source = r#"fn main() { __net_unix_socket_json_request("/tmp/harn.sock", {}) }"#;
        let effects = compute_handoff_effects(source, None);
        let net = effects
            .iter()
            .find(|effect| matches!(effect.kind, EffectKind::Net))
            .expect("net effect");
        assert_eq!(net.scope, EffectScope::Write);
        assert_eq!(net.resource.as_deref(), Some("/tmp/harn.sock"));
    }

    #[test]
    fn files_upload_yields_fs_read_and_net_write_effects() {
        let source = r#"fn main() { __files_upload("/tmp/input.pdf", "gemini") }"#;
        let effects = compute_handoff_effects(source, None);
        assert!(
            effects.iter().any(|effect| {
                matches!(effect.kind, EffectKind::Fs)
                    && effect.scope == EffectScope::Read
                    && effect.resource.as_deref() == Some("/tmp/input.pdf")
            }),
            "expected Fs read effect, got {effects:?}"
        );
        assert!(
            effects.iter().any(|effect| {
                matches!(effect.kind, EffectKind::Net)
                    && effect.scope == EffectScope::Write
                    && effect.resource.as_deref() == Some("gemini")
            }),
            "expected Net write effect, got {effects:?}"
        );
    }

    #[test]
    fn std_files_upload_wrapper_yields_fs_read_and_net_write_effects() {
        let source = r#"
import { upload } from "std/files"
fn main() { upload("/tmp/input.pdf", "gemini") }
"#;
        let effects = compute_handoff_effects(source, None);
        assert!(
            effects.iter().any(|effect| {
                matches!(effect.kind, EffectKind::Fs)
                    && effect.scope == EffectScope::Read
                    && effect.resource.as_deref() == Some("/tmp/input.pdf")
            }),
            "expected Fs read effect, got {effects:?}"
        );
        assert!(
            effects.iter().any(|effect| {
                matches!(effect.kind, EffectKind::Net)
                    && effect.scope == EffectScope::Write
                    && effect.resource.as_deref() == Some("gemini")
            }),
            "expected Net write effect, got {effects:?}"
        );
    }

    #[test]
    fn harness_fs_write_yields_fs_write_effect() {
        let source = r#"fn main(harness: Harness) { harness.fs.write_file("/tmp/out", "hi") }"#;
        let effects = compute_handoff_effects(source, None);
        assert!(
            effects
                .iter()
                .any(|effect| matches!(effect.kind, EffectKind::Fs)
                    && effect.scope == EffectScope::Write),
            "expected Fs write effect, got {effects:?}"
        );
    }

    #[test]
    fn harness_term_read_password_yields_stdio_read_effect() {
        let source = r#"fn main(harness: Harness) { harness.term.read_password("password: ") }"#;
        let effects = compute_handoff_effects(source, None);
        assert!(
            effects
                .iter()
                .any(|effect| matches!(effect.kind, EffectKind::Stdio)
                    && effect.scope == EffectScope::Read),
            "expected Stdio read effect, got {effects:?}"
        );
    }

    #[test]
    fn harness_fs_mkdtemp_yields_fs_write_effect() {
        let source = r#"fn main(harness: Harness) { harness.fs.mkdtemp("harn-") }"#;
        let effects = compute_handoff_effects(source, None);
        assert!(
            effects
                .iter()
                .any(|effect| matches!(effect.kind, EffectKind::Fs)
                    && effect.scope == EffectScope::Write),
            "expected Fs write effect, got {effects:?}"
        );
    }

    #[test]
    fn harness_crypto_sha256_is_pure_for_handoff_effects() {
        let source = r#"fn main(harness: Harness) { harness.crypto.sha256("hello") }"#;
        let effects = compute_handoff_effects(source, None);
        assert!(effects.is_empty(), "expected no effects, got {effects:?}");
    }

    #[test]
    fn harness_stdio_read_line_yields_stdio_read_effect() {
        let source = r"fn main(harness: Harness) { harness.stdio.read_line() }";
        let effects = compute_handoff_effects(source, None);
        assert!(
            effects
                .iter()
                .any(|effect| matches!(effect.kind, EffectKind::Stdio)
                    && effect.scope == EffectScope::Read),
            "expected Stdio read effect, got {effects:?}"
        );
    }

    #[test]
    fn llm_call_emits_llm_effect_with_provider_and_model() {
        let source = r#"fn main() {
            llm_call("summarize", { provider: "anthropic", model: "claude-3-5-sonnet" })
        }"#;
        let effects = compute_handoff_effects(source, None);
        let llm = effects
            .iter()
            .find(|effect| matches!(effect.kind, EffectKind::Llm { .. }))
            .expect("llm effect");
        let EffectKind::Llm { provider, model } = &llm.kind else {
            panic!("expected llm kind, got {:?}", llm.kind);
        };
        assert_eq!(provider.as_deref(), Some("anthropic"));
        assert_eq!(model.as_deref(), Some("claude-3-5-sonnet"));
    }

    #[test]
    fn harness_llm_catalog_yields_read_effect() {
        let source = r"fn main(harness: Harness) {
            harness.llm.catalog()
            harness.llm.providers()
        }";
        let effects = compute_handoff_effects(source, None);
        assert!(
            effects
                .iter()
                .any(|effect| matches!(effect.kind, EffectKind::Llm { .. })
                    && effect.scope == EffectScope::Read),
            "expected LLM read effect, got {effects:?}"
        );
    }

    #[test]
    fn ceiling_drops_disallowed_capabilities() {
        let source = r#"fn main(harness: Harness) {
            harness.net.get("https://example.test")
            harness.fs.read_file("/tmp/in")
        }"#;
        let mut ceiling = CapabilityPolicy::default();
        ceiling
            .capabilities
            .insert("workspace".to_string(), vec!["read_text".to_string()]);
        let effects = compute_handoff_effects(source, Some(&ceiling));
        assert!(
            effects
                .iter()
                .all(|effect| !matches!(effect.kind, EffectKind::Net)),
            "ceiling without `network` should drop Net effect, got {effects:?}"
        );
        assert!(
            effects
                .iter()
                .any(|effect| matches!(effect.kind, EffectKind::Fs)),
            "ceiling with workspace.read_text should keep Fs read, got {effects:?}"
        );
    }

    #[test]
    fn ceiling_side_effect_level_clamps_writes() {
        let source = r#"fn main(harness: Harness) {
            harness.net.get("https://example.test")
            __io_println("hi")
        }"#;
        let ceiling = CapabilityPolicy {
            side_effect_level: Some("read_only".to_string()),
            ..Default::default()
        };
        let effects = compute_handoff_effects(source, Some(&ceiling));
        assert!(
            effects
                .iter()
                .all(|effect| !matches!(effect.kind, EffectKind::Net)),
            "read_only ceiling must drop Net write, got {effects:?}"
        );
        assert!(
            effects
                .iter()
                .any(|effect| matches!(effect.kind, EffectKind::Stdio)),
            "stdio observe should pass read_only ceiling, got {effects:?}"
        );
    }

    #[test]
    fn effect_record_round_trips_through_serde() {
        let effects = vec![
            EffectRecord::new(EffectKind::Net, EffectScope::Write)
                .with_resource("https://api.example/v1"),
            EffectRecord::new(EffectKind::Fs, EffectScope::Read).with_resource("/workspace/src"),
            EffectRecord::new(
                EffectKind::Llm {
                    provider: Some("anthropic".to_string()),
                    model: Some("claude-3-7-sonnet".to_string()),
                },
                EffectScope::Write,
            ),
            EffectRecord::new(
                EffectKind::Tool {
                    name: "search".to_string(),
                },
                EffectScope::Read,
            ),
        ];
        let encoded = serde_json::to_string(&effects).expect("encode");
        let decoded: Vec<EffectRecord> = serde_json::from_str(&encoded).expect("decode");
        assert_eq!(decoded, effects);
    }

    #[test]
    fn empty_source_returns_no_effects() {
        let effects = compute_handoff_effects("fn main() {}", None);
        assert!(effects.is_empty(), "got {effects:?}");
    }

    #[test]
    fn effects_from_metadata_round_trips_typed_payload() {
        let effects = vec![EffectRecord::new(EffectKind::Net, EffectScope::Write)
            .with_resource("https://api.example")];
        let mut metadata: BTreeMap<String, serde_json::Value> = BTreeMap::new();
        metadata.insert(
            "effects".to_string(),
            serde_json::to_value(&effects).expect("encode"),
        );
        assert_eq!(effects_from_metadata(&metadata), effects);
    }

    #[test]
    fn subset_violations_returns_empty_when_child_covered() {
        let parent = vec![
            EffectRecord::new(EffectKind::Net, EffectScope::Write),
            EffectRecord::new(EffectKind::Fs, EffectScope::Read).with_resource("/workspace"),
        ];
        let child = vec![
            EffectRecord::new(EffectKind::Net, EffectScope::Write)
                .with_resource("https://example.test"),
            EffectRecord::new(EffectKind::Fs, EffectScope::Read).with_resource("/workspace"),
        ];
        assert!(effect_subset_violations(Some(&parent), &child).is_empty());
    }

    #[test]
    fn subset_violations_flags_unmatched_kinds() {
        let parent = vec![EffectRecord::new(EffectKind::Fs, EffectScope::Read)];
        let child = vec![EffectRecord::new(EffectKind::Net, EffectScope::Write)
            .with_resource("https://example.test")];
        let violations = effect_subset_violations(Some(&parent), &child);
        assert_eq!(violations.len(), 1);
        assert!(matches!(violations[0].kind, EffectKind::Net));
    }

    #[test]
    fn subset_violations_flags_scope_escalations() {
        let parent = vec![EffectRecord::new(EffectKind::Fs, EffectScope::Read)];
        let child = vec![EffectRecord::new(EffectKind::Fs, EffectScope::Mutate)];
        let violations = effect_subset_violations(Some(&parent), &child);
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].scope, EffectScope::Mutate);
    }

    #[test]
    fn subset_violations_treats_missing_parent_resource_as_wildcard() {
        let parent = vec![EffectRecord::new(EffectKind::Net, EffectScope::Write)];
        let child = vec![EffectRecord::new(EffectKind::Net, EffectScope::Write)
            .with_resource("https://api.example/v1")];
        assert!(effect_subset_violations(Some(&parent), &child).is_empty());
    }

    #[test]
    fn subset_violations_requires_resource_match_when_parent_declares_one() {
        let parent = vec![EffectRecord::new(EffectKind::Net, EffectScope::Write)
            .with_resource("https://allowed.test")];
        let child = vec![EffectRecord::new(EffectKind::Net, EffectScope::Write)
            .with_resource("https://disallowed.test")];
        let violations = effect_subset_violations(Some(&parent), &child);
        assert_eq!(violations.len(), 1);
    }

    #[test]
    fn subset_violations_skip_when_parent_is_none() {
        let child = vec![EffectRecord::new(EffectKind::Net, EffectScope::Write)];
        assert!(effect_subset_violations(None, &child).is_empty());
    }

    #[test]
    fn subset_violations_empty_parent_flags_every_child_effect() {
        let parent: Vec<EffectRecord> = Vec::new();
        let child = vec![
            EffectRecord::new(EffectKind::Net, EffectScope::Write),
            EffectRecord::new(EffectKind::Fs, EffectScope::Read),
        ];
        let violations = effect_subset_violations(Some(&parent), &child);
        assert_eq!(violations.len(), 2);
    }

    #[test]
    fn subset_violations_empty_child_is_always_allowed() {
        let parent = vec![EffectRecord::new(EffectKind::Net, EffectScope::Write)];
        assert!(effect_subset_violations(Some(&parent), &[]).is_empty());
    }

    #[test]
    fn effect_kind_label_shape() {
        assert_eq!(effect_kind_label(&EffectKind::Net), "net");
        assert_eq!(
            effect_kind_label(&EffectKind::Llm {
                provider: Some("anthropic".to_string()),
                model: Some("claude-3-7-sonnet".to_string()),
            }),
            "llm:anthropic/claude-3-7-sonnet"
        );
        assert_eq!(
            effect_kind_label(&EffectKind::Tool {
                name: "search".to_string()
            }),
            "tool:search"
        );
    }

    #[test]
    fn effect_record_summary_includes_resource() {
        let effect = EffectRecord::new(EffectKind::Net, EffectScope::Write)
            .with_resource("https://example.test/api");
        assert_eq!(
            effect_record_summary(&effect),
            "net:write (https://example.test/api)"
        );
    }

    #[test]
    fn deduplicates_repeated_effects() {
        let source = r#"fn main() {
            http_get("https://example.test")
            http_get("https://example.test")
            http_get("https://example.test")
        }"#;
        let effects = compute_handoff_effects(source, None);
        let net_count = effects
            .iter()
            .filter(|effect| matches!(effect.kind, EffectKind::Net))
            .count();
        assert_eq!(net_count, 1, "expected dedup, got {effects:?}");
    }
}
