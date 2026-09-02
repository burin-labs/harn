//! Typed effect records carried on `HandoffArtifact` envelopes.
//!
//! `EffectRecord` names one side-effect a spawned child may exercise. The set
//! sits on each handoff so the
//! dispatcher (E5.4) and the OpenTrustGraph receipt chain (E5.5) can prove
//! the child never escaped its parent's effect grant.
//! Computation at spawn time walks the child's entrypoint module via the
//! same capability analysis `harn graph --json` uses (issue HARN-#1758),
//! plus a conservative AST walker for harness calls embedded in inline spawn
//! configs. The two extraction paths feed one canonicalization step so
//! downstream consumers see a single deduped, deterministically ordered list.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, HashSet};

use serde::{Deserialize, Serialize};

use harn_ir::{CallClassification, Capability, LiteralValue, NodeSemantics};
use harn_parser::{Node, SNode};

use super::effect_call_cache::resolve_runtime_resources;
use super::CapabilityPolicy;
use crate::VmValue;

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
    /// Environment-variable access.
    Env,
    /// Wall or monotonic clock access.
    Clock,
    /// Nondeterministic random source access.
    Random,
    /// Child-process execution or observation.
    Process,
    /// Secret custody access.
    Secret,
    /// Logs, traces, metrics, and request-correlation state.
    Observability,
    /// Durable transcript channel access.
    Channel,
    /// Durable or execution-local state access.
    State,
    /// Other typed host-service access.
    Host,
    /// Opaque authority that public values cannot manufacture.
    Authority,
    /// LLM calls with known provider and model receipt metadata.
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
    pub resource: Option<crate::value::HarnStr>,
}

impl EffectRecord {
    pub fn new(kind: EffectKind, scope: EffectScope) -> Self {
        Self {
            kind,
            scope,
            resource: None,
        }
    }

    pub fn with_resource(mut self, resource: impl Into<crate::value::HarnStr>) -> Self {
        let resource = resource.into();
        self.resource = if resource.is_empty() {
            None
        } else {
            Some(resource)
        };
        self
    }
}

/// Execution-local, thread-safe-at-the-owner accumulator for runtime effects.
///
/// Effect evidence is a set. VM-local caches avoid repeated access to this
/// shared execution-tree owner while child VMs still converge on one receipt.
#[derive(Default)]
pub(crate) struct ExecutedEffectRecorder {
    effects: HashSet<EffectRecord>,
}

impl ExecutedEffectRecorder {
    pub(crate) fn record(&mut self, specs: &[harn_builtin_meta::EffectSpec], args: &[VmValue]) {
        self.effects
            .extend(runtime_effects_from_contract(specs, args));
    }

    pub(crate) fn snapshot(&self) -> Vec<EffectRecord> {
        let mut effects = self.effects.iter().cloned().collect::<Vec<_>>();
        effects.sort();
        effects
    }

    pub(crate) fn clear(&mut self) {
        self.effects.clear();
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
        walk_for_harness_effects(node, &mut CapabilityBindings::default(), &mut collected);
    }

    let mut effects: Vec<EffectRecord> = collected.into_iter().collect();
    if let Some(ceiling) = ceiling {
        effects.retain(|effect| effect_allowed_by_ceiling(effect, ceiling));
    }
    effects
}

fn effects_from_call(call: &harn_ir::CallSemantics) -> Vec<EffectRecord> {
    // `harn-ir` projects this classification directly from the builtin
    // contract manifest. Keeping name tables here would create a competing
    // semantic owner and let static ceilings drift from runtime receipts.
    if let CallClassification::Capabilities(capability_effects) = &call.classification {
        let contract = call
            .name
            .strip_prefix("harness.")
            .and_then(|path| path.split_once('.'))
            .and_then(|(field, method)| {
                let capability = harn_builtin_meta::CapabilityId::from_field_name(field)?;
                crate::stdlib::capability_method_manifest_entry(capability, method)
            })
            .or_else(|| crate::stdlib::builtin_manifest_entry(&call.name));
        if let Some(entry) = contract {
            return effect_specs_to_records(entry.contract.effects, &call.literal_args);
        }
        return capability_effects
            .iter()
            .filter_map(capability_effect_to_record)
            .collect();
    }
    Vec::new()
}

pub(crate) fn runtime_effects_from_contract(
    specs: &[harn_builtin_meta::EffectSpec],
    args: &[VmValue],
) -> Vec<EffectRecord> {
    let mut records = Vec::new();
    let llm_specs = specs
        .iter()
        .filter(|spec| spec.kind == harn_builtin_meta::EffectKind::Llm)
        .collect::<Vec<_>>();
    if let Some(first) = llm_specs.first() {
        let mut provider = None;
        let mut model = None;
        for spec in &llm_specs {
            for selector in spec.resources {
                let harn_builtin_meta::ResourceSelector::Field { path, .. } = selector else {
                    continue;
                };
                let value = resolve_runtime_resources(*selector, args)
                    .into_iter()
                    .next();
                match path.last().copied() {
                    Some("provider") => provider = value.map(|value| value.to_string()),
                    Some("model") => model = value.map(|value| value.to_string()),
                    _ => {}
                }
            }
        }
        records.push(EffectRecord::new(
            EffectKind::Llm { provider, model },
            effect_scope_from_contract(first.access),
        ));
    }
    for spec in specs {
        if spec.kind == harn_builtin_meta::EffectKind::Llm {
            continue;
        }
        let kind = effect_kind_from_contract(spec.kind);
        let scope = effect_scope_from_contract(spec.access);
        let resources = spec
            .resources
            .iter()
            .flat_map(|selector| resolve_runtime_resources(*selector, args))
            .collect::<Vec<_>>();
        if resources.is_empty() {
            records.push(EffectRecord::new(kind, scope));
        } else {
            records.extend(
                resources
                    .into_iter()
                    .map(|resource| EffectRecord::new(kind.clone(), scope).with_resource(resource)),
            );
        }
    }
    records
}

fn effect_kind_from_contract(kind: harn_builtin_meta::EffectKind) -> EffectKind {
    use harn_builtin_meta::EffectKind as ContractKind;
    match kind {
        ContractKind::Stdio => EffectKind::Stdio,
        ContractKind::Fs => EffectKind::Fs,
        ContractKind::Network => EffectKind::Net,
        ContractKind::Llm => EffectKind::Llm {
            provider: None,
            model: None,
        },
        ContractKind::Tool => EffectKind::Tool {
            name: String::new(),
        },
        ContractKind::Mcp => EffectKind::Tool {
            name: "mcp".to_string(),
        },
        ContractKind::Worker => EffectKind::Spawn,
        ContractKind::Process => EffectKind::Process,
        ContractKind::Env => EffectKind::Env,
        ContractKind::Clock => EffectKind::Clock,
        ContractKind::Random => EffectKind::Random,
        ContractKind::Host => EffectKind::Host,
        ContractKind::Authority => EffectKind::Authority,
        ContractKind::Secret => EffectKind::Secret,
        ContractKind::Observability => EffectKind::Observability,
        ContractKind::Channel => EffectKind::Channel,
        ContractKind::State => EffectKind::State,
    }
}

fn effect_scope_from_contract(access: harn_builtin_meta::EffectAccess) -> EffectScope {
    match access {
        harn_builtin_meta::EffectAccess::Read => EffectScope::Read,
        harn_builtin_meta::EffectAccess::Write => EffectScope::Write,
        harn_builtin_meta::EffectAccess::Mutate => EffectScope::Mutate,
        harn_builtin_meta::EffectAccess::Observe => EffectScope::Observe,
    }
}

fn resolve_contract_resources(
    selector: harn_builtin_meta::ResourceSelector,
    args: &[LiteralValue],
) -> Vec<String> {
    use harn_builtin_meta::ResourceSelector;
    match selector {
        ResourceSelector::Argument(index) => args
            .get(index as usize)
            .and_then(LiteralValue::as_str)
            .map(|value| vec![value.to_string()])
            .unwrap_or_default(),
        ResourceSelector::Field { argument, path } => {
            let mut value = args.get(argument as usize);
            for field in path {
                value = value.and_then(|value| value.dict_field(field));
            }
            value
                .and_then(LiteralValue::as_str)
                .map(|value| vec![value.to_string()])
                .unwrap_or_default()
        }
        ResourceSelector::EachArgument(index) => args
            .get(index as usize)
            .and_then(LiteralValue::list_items)
            .into_iter()
            .flatten()
            .filter_map(LiteralValue::as_str)
            .map(str::to_string)
            .collect(),
        ResourceSelector::Constant(value) => vec![value.to_string()],
        ResourceSelector::Dynamic => Vec::new(),
    }
}

fn effect_specs_to_records(
    specs: &[harn_builtin_meta::EffectSpec],
    args: &[LiteralValue],
) -> Vec<EffectRecord> {
    let mut records = Vec::new();
    let llm_specs = specs
        .iter()
        .filter(|spec| spec.kind == harn_builtin_meta::EffectKind::Llm)
        .collect::<Vec<_>>();
    if let Some(first) = llm_specs.first() {
        let mut provider = None;
        let mut model = None;
        for spec in &llm_specs {
            for selector in spec.resources {
                let harn_builtin_meta::ResourceSelector::Field { path, .. } = selector else {
                    continue;
                };
                let value = resolve_contract_resources(*selector, args)
                    .into_iter()
                    .next();
                match path.last().copied() {
                    Some("provider") => provider = value,
                    Some("model") => model = value,
                    _ => {}
                }
            }
        }
        records.push(EffectRecord::new(
            EffectKind::Llm { provider, model },
            effect_scope_from_contract(first.access),
        ));
    }
    for spec in specs {
        if spec.kind == harn_builtin_meta::EffectKind::Llm {
            continue;
        }
        let kind = effect_kind_from_contract(spec.kind);
        let scope = effect_scope_from_contract(spec.access);
        let resources = spec
            .resources
            .iter()
            .flat_map(|selector| resolve_contract_resources(*selector, args))
            .collect::<Vec<_>>();
        if resources.is_empty() {
            records.push(EffectRecord::new(kind, scope));
        } else {
            records.extend(
                resources
                    .into_iter()
                    .map(|resource| EffectRecord::new(kind.clone(), scope).with_resource(resource)),
            );
        }
    }
    records
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
        | "package_snapshot_open"
        | "render"
        | "render_prompt"
        | "render_with_provenance"
        | "find_text"
        | "find_evidence"
        | "read_lines"
        | "list_dir"
        | "walk_dir"
        | "glob"
        | "file_exists"
        | "path_status"
        | "stat" => Some(EffectRecord::new(EffectKind::Fs, EffectScope::Read)),

        // fs writes
        "write_file"
        | "write_file_bytes"
        | "replace_file"
        | "replace_file_result"
        | "replace_file_bytes"
        | "replace_file_bytes_result"
        | "append_file"
        | "append_file_locked"
        | "mkdir"
        | "mkdtemp"
        | "mkdtemp_in_workspace"
        | "copy_file"
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
        | "http_stream_info"
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

pub(super) fn builtin_has_network_effect(name: &str) -> bool {
    if matches!(name, "__files_upload" | "upload") {
        return true;
    }
    builtin_effect(name).is_some_and(|effect| matches!(effect.kind, EffectKind::Net))
}

fn capability_effect_to_record(effect: &harn_ir::CapabilityEffect) -> Option<EffectRecord> {
    let contract_scope = match effect.access {
        harn_builtin_meta::EffectAccess::Read => EffectScope::Read,
        harn_builtin_meta::EffectAccess::Write => EffectScope::Write,
        harn_builtin_meta::EffectAccess::Mutate => EffectScope::Mutate,
        harn_builtin_meta::EffectAccess::Observe => EffectScope::Observe,
    };
    let (kind, scope) = match effect.capability {
        Capability::FilesystemRead => (EffectKind::Fs, contract_scope),
        Capability::WorkspaceMutation => (EffectKind::Fs, EffectScope::Mutate),
        Capability::CommandExecution => (
            EffectKind::Hostcall {
                name: format!("process.{}", effect.operation),
            },
            EffectScope::Write,
        ),
        Capability::NetworkAccess => (EffectKind::Net, contract_scope),
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
        Capability::Authority => (EffectKind::Authority, contract_scope),
        Capability::ModelCall => (
            EffectKind::Llm {
                provider: None,
                model: None,
            },
            contract_scope,
        ),
        Capability::WorkerDispatch => (EffectKind::Spawn, EffectScope::Write),
        Capability::Stdio => (EffectKind::Stdio, contract_scope),
        Capability::Environment => (EffectKind::Env, contract_scope),
        Capability::Clock => (EffectKind::Clock, contract_scope),
        Capability::Random => (EffectKind::Random, contract_scope),
        Capability::Secret => (EffectKind::Secret, contract_scope),
        Capability::Observability => (EffectKind::Observability, contract_scope),
        Capability::Channel => (EffectKind::Channel, contract_scope),
        Capability::State => (EffectKind::State, contract_scope),
        Capability::HumanApproval => return None,
        Capability::AutonomyPolicy => return None,
    };
    let resource = effect.path.as_deref().map(crate::value::HarnStr::from);
    Some(EffectRecord {
        kind,
        scope,
        resource,
    })
}

#[derive(Clone, Default)]
struct CapabilityBindings {
    roots: BTreeSet<String>,
    handles: BTreeMap<String, harn_builtin_meta::CapabilityId>,
}

fn walk_for_harness_effects(
    node: &SNode,
    bindings: &mut CapabilityBindings,
    out: &mut BTreeSet<EffectRecord>,
) {
    match &node.node {
        Node::FnDecl { params, body, .. }
        | Node::ToolDecl { params, body, .. }
        | Node::Pipeline { params, body, .. } => {
            let mut callable_bindings = bindings.clone();
            for param in params {
                let Some(harn_parser::TypeExpr::Named(type_name)) = &param.type_expr else {
                    continue;
                };
                if type_name == "Harness" {
                    callable_bindings.roots.insert(param.name.clone());
                } else if let Some(capability) =
                    harn_builtin_meta::CapabilityId::from_type_name(type_name)
                {
                    callable_bindings
                        .handles
                        .insert(param.name.clone(), capability);
                }
            }
            for statement in body {
                walk_for_harness_effects(statement, &mut callable_bindings, out);
            }
            return;
        }
        Node::LetBinding { pattern, value, .. } | Node::ConstBinding { pattern, value, .. } => {
            if let harn_parser::BindingPattern::Identifier(name) = pattern {
                if let Some(capability) = capability_value(value, bindings) {
                    bindings.handles.insert(name.clone(), capability);
                }
            }
        }
        _ => {}
    }
    out.extend(harness_method_effects(node, bindings));
    for child in child_nodes(node) {
        walk_for_harness_effects(child, bindings, out);
    }
}

fn capability_value(
    node: &SNode,
    bindings: &CapabilityBindings,
) -> Option<harn_builtin_meta::CapabilityId> {
    match &node.node {
        Node::Identifier(name) => bindings.handles.get(name).copied(),
        Node::PropertyAccess { object, property }
        | Node::OptionalPropertyAccess { object, property }
            if matches!(&object.node, Node::Identifier(root) if bindings.roots.contains(root)) =>
        {
            harn_builtin_meta::CapabilityId::from_field_name(property)
        }
        _ => None,
    }
}

fn harness_method_effects(node: &SNode, bindings: &CapabilityBindings) -> Vec<EffectRecord> {
    let (object, method, args) = match &node.node {
        Node::MethodCall {
            object,
            method,
            args,
            ..
        }
        | Node::OptionalMethodCall {
            object,
            method,
            args,
            ..
        } => (object, method, args),
        _ => return Vec::new(),
    };
    let capability = capability_value(object, bindings).or_else(|| {
        let (sub_handle, root) = harness_sub_handle(object)?;
        matches!(&root.node, Node::Identifier(name) if bindings.roots.contains(name))
            .then(|| harn_builtin_meta::CapabilityId::from_field_name(&sub_handle))
            .flatten()
    });
    let Some(capability) = capability else {
        return Vec::new();
    };
    let Some(entry) = crate::stdlib::capability_method_manifest_entry(capability, method) else {
        return Vec::new();
    };
    let literal_args = args.iter().map(harn_ir::literal_value).collect::<Vec<_>>();
    effect_specs_to_records(entry.contract.effects, &literal_args)
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

fn child_nodes(node: &SNode) -> Vec<&SNode> {
    harn_parser::visit::immediate_children(node)
}

pub(crate) fn effect_allowed_by_ceiling(effect: &EffectRecord, ceiling: &CapabilityPolicy) -> bool {
    effect_allowed_by_ceiling_with_authorization(effect, ceiling, false)
}

pub(crate) fn contract_effect_allowed_by_ceiling(
    effect: &EffectRecord,
    contract: harn_builtin_meta::BuiltinContract,
    ceiling: &CapabilityPolicy,
) -> bool {
    let explicitly_authorized = contract.effects_authorized_by.is_some_and(|authority| {
        super::policy_allows_capability(
            ceiling,
            authority.capability.field_name(),
            authority.operation,
        )
    });
    if !effect_capability_allowed_by_ceiling(effect, ceiling, explicitly_authorized) {
        return false;
    }
    // The side-effect ladder ranks mutations of the user's workspace and
    // external systems. A declared runtime-control-plane operation mutates
    // Harn-owned session state instead, so that orthogonal ladder does not
    // rank it.
    //
    // Without this, `harness.agent.open` — `state:mutate (agent-sessions)`,
    // which the ladder classifies `workspace_write` — was rejected under the
    // `read_only` ceiling every non-`code` ACP session mode installs, and every
    // served turn died before its first model call. `harn run` never saw it
    // because it installs no ceiling at all.
    //
    // The capability gate above still applies in full: a ceiling that
    // restricts `state`/`write` still denies these, and the effects stay in
    // the record for receipts and lineage. The marker classifies the target
    // domain and does not establish caller identity.
    if contract.is_runtime_control_plane() {
        return true;
    }
    effect_within_side_effect_ceiling(effect, ceiling)
}

fn effect_capability_allowed_by_ceiling(
    effect: &EffectRecord,
    ceiling: &CapabilityPolicy,
    explicitly_authorized: bool,
) -> bool {
    if ceiling.capabilities_are_restricted() {
        let (capability, op) = effect_capability_op(effect);
        let allowed = super::policy_allows_capability(ceiling, capability, op.as_ref());
        if !allowed && !explicitly_authorized {
            return false;
        }
    }
    true
}

fn effect_within_side_effect_ceiling(effect: &EffectRecord, ceiling: &CapabilityPolicy) -> bool {
    let Some(ceiling_level) = ceiling.side_effect_level.as_deref() else {
        return true;
    };
    !requested_exceeds_ceiling(side_effect_level_for(effect), ceiling_level)
}

fn effect_allowed_by_ceiling_with_authorization(
    effect: &EffectRecord,
    ceiling: &CapabilityPolicy,
    explicitly_authorized: bool,
) -> bool {
    effect_capability_allowed_by_ceiling(effect, ceiling, explicitly_authorized)
        && effect_within_side_effect_ceiling(effect, ceiling)
}

fn effect_capability_op(effect: &EffectRecord) -> (&'static str, Cow<'static, str>) {
    let fixed = |capability, operation| (capability, Cow::Borrowed(operation));
    match (&effect.kind, effect.scope) {
        (EffectKind::Stdio, EffectScope::Read) => fixed("stdio", "read"),
        (EffectKind::Stdio, _) => fixed("stdio", "write"),
        (EffectKind::Fs, EffectScope::Read) => fixed("workspace", "read_text"),
        (EffectKind::Fs, EffectScope::Write) => fixed("workspace", "write_text"),
        (EffectKind::Fs, EffectScope::Mutate) => fixed("workspace", "apply_edit"),
        (EffectKind::Fs, EffectScope::Observe) => fixed("workspace", "exists"),
        (EffectKind::Net, _) => fixed("network", "http"),
        (EffectKind::Env, EffectScope::Read | EffectScope::Observe) => fixed("environment", "read"),
        (EffectKind::Env, _) => fixed("environment", "write"),
        (EffectKind::Clock, _) => fixed("clock", "now"),
        (EffectKind::Random, _) => fixed("random", "bytes"),
        (EffectKind::Process, EffectScope::Read | EffectScope::Observe) => {
            fixed("process", "inspect")
        }
        (EffectKind::Process, _) => fixed("process", "run"),
        (EffectKind::Secret, EffectScope::Read | EffectScope::Observe) => fixed("secrets", "read"),
        (EffectKind::Secret, _) => fixed("secrets", "write"),
        (EffectKind::Observability, _) => fixed("observability", "emit"),
        (EffectKind::Channel, EffectScope::Read | EffectScope::Observe) => fixed("channel", "read"),
        (EffectKind::Channel, _) => fixed("channel", "write"),
        (EffectKind::State, EffectScope::Read | EffectScope::Observe) => fixed("state", "read"),
        (EffectKind::State, _) => fixed("state", "write"),
        (EffectKind::Host, _) => fixed("connector", "call"),
        (EffectKind::Authority, scope) => {
            let access = match scope {
                EffectScope::Read => harn_builtin_meta::EffectAccess::Read,
                EffectScope::Write => harn_builtin_meta::EffectAccess::Write,
                EffectScope::Mutate => harn_builtin_meta::EffectAccess::Mutate,
                EffectScope::Observe => harn_builtin_meta::EffectAccess::Observe,
            };
            let operation = Cow::Owned(harn_ir::authority_effect_policy_operation(
                access,
                effect.resource.as_deref(),
            ));
            ("authority", operation)
        }
        (EffectKind::Llm { .. }, EffectScope::Read) => fixed("llm", "catalog"),
        (EffectKind::Llm { .. }, _) => fixed("llm", "call"),
        (EffectKind::Tool { .. }, _) => fixed("host", "tool_call"),
        (EffectKind::Hostcall { .. }, _) => fixed("connector", "call"),
        (EffectKind::Persona { .. }, _) => fixed("worker", "dispatch"),
        (EffectKind::Spawn, _) => fixed("worker", "dispatch"),
    }
}

fn side_effect_level_for(effect: &EffectRecord) -> &'static str {
    match (&effect.kind, effect.scope) {
        (EffectKind::Stdio, _) => "read_only",
        (EffectKind::Fs, EffectScope::Read | EffectScope::Observe) => "read_only",
        (EffectKind::Fs, _) => "workspace_write",
        (EffectKind::Net, _) => "network",
        (EffectKind::Env, EffectScope::Read | EffectScope::Observe) => "read_only",
        (EffectKind::Env, _) => "workspace_write",
        (EffectKind::Clock, _) => "read_only",
        (EffectKind::Random, _) => "read_only",
        (EffectKind::Process, EffectScope::Read | EffectScope::Observe) => "read_only",
        (EffectKind::Process, _) => "process_exec",
        (EffectKind::Secret, EffectScope::Read | EffectScope::Observe) => "read_only",
        (EffectKind::Secret, _) => "workspace_write",
        (EffectKind::Observability, _) => "read_only",
        (EffectKind::Channel, EffectScope::Read | EffectScope::Observe) => "read_only",
        (EffectKind::Channel, _) => "workspace_write",
        (EffectKind::State, EffectScope::Read | EffectScope::Observe) => "read_only",
        (EffectKind::State, _) => "workspace_write",
        (EffectKind::Host, EffectScope::Read | EffectScope::Observe) => "read_only",
        (EffectKind::Host, _) => "workspace_write",
        (EffectKind::Authority, EffectScope::Read | EffectScope::Observe) => "read_only",
        (EffectKind::Authority, _) => "workspace_write",
        // Model inference consumes an explicitly granted `llm.call`
        // capability but does not mutate the user's workspace or an external
        // system. Keep its rich read/write effect scope for lineage and
        // attenuation while classifying every LLM effect as read-only for the
        // orthogonal tool side-effect ceiling.
        (EffectKind::Llm { .. }, _) => "read_only",
        (EffectKind::Tool { .. }, _) => "workspace_write",
        (EffectKind::Hostcall { name }, _) if name.starts_with("process.") => "process_exec",
        (EffectKind::Hostcall { .. }, _) => "read_only",
        (EffectKind::Persona { .. }, _) => "workspace_write",
        (EffectKind::Spawn, _) => "workspace_write",
    }
}

fn requested_exceeds_ceiling(requested: &str, ceiling: &str) -> bool {
    fn rank(value: &str) -> usize {
        crate::tool_annotations::SideEffectLevel::rank_str(value)
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
        | (EffectKind::Env, EffectKind::Env)
        | (EffectKind::Clock, EffectKind::Clock)
        | (EffectKind::Random, EffectKind::Random)
        | (EffectKind::Process, EffectKind::Process)
        | (EffectKind::Secret, EffectKind::Secret)
        | (EffectKind::Observability, EffectKind::Observability)
        | (EffectKind::Channel, EffectKind::Channel)
        | (EffectKind::State, EffectKind::State)
        | (EffectKind::Host, EffectKind::Host)
        | (EffectKind::Authority, EffectKind::Authority)
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
        EffectKind::Env => "env".to_string(),
        EffectKind::Clock => "clock".to_string(),
        EffectKind::Random => "random".to_string(),
        EffectKind::Process => "process".to_string(),
        EffectKind::Secret => "secret".to_string(),
        EffectKind::Observability => "observability".to_string(),
        EffectKind::Channel => "channel".to_string(),
        EffectKind::State => "state".to_string(),
        EffectKind::Host => "host".to_string(),
        EffectKind::Authority => "authority".to_string(),
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
#[path = "effects_authority_tests.rs"]
mod authority_tests;

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
                    && effect.scope == EffectScope::Read
                    && effect.resource.as_deref() == Some("https://example.test")),
            "expected Net read effect, got {effects:?}"
        );
    }

    #[test]
    fn harness_process_run_yields_process_hostcall_effect() {
        let source = r#"fn main(harness: Harness) {
            harness.process.run({program: "printf", args: ["hello"]})
        }"#;
        let effects = compute_handoff_effects(source, None);
        assert!(
            effects.iter().any(|effect| {
                matches!(&effect.kind, EffectKind::Process)
                    && effect.scope == EffectScope::Write
                    && effect.resource.as_deref() == Some("printf")
            }),
            "expected process hostcall write effect, got {effects:?}"
        );
    }

    #[test]
    fn http_get_builtin_yields_net_effect_with_resource() {
        let source = r#"fn main(harness: Harness) { harness.net.get("https://example.test/api") }"#;
        let effects = compute_handoff_effects(source, None);
        let net = effects
            .iter()
            .find(|effect| matches!(effect.kind, EffectKind::Net))
            .expect("net effect");
        assert_eq!(net.scope, EffectScope::Read);
        assert_eq!(net.resource.as_deref(), Some("https://example.test/api"));
    }

    #[test]
    fn unix_socket_json_request_yields_net_effect_with_resource() {
        let source = r#"fn main(harness: Harness) {
            harness.net.unix_socket_json_request("/tmp/harn.sock", {})
        }"#;
        let effects = compute_handoff_effects(source, None);
        let net = effects
            .iter()
            .find(|effect| matches!(effect.kind, EffectKind::Net))
            .expect("net effect");
        assert_eq!(net.scope, EffectScope::Mutate);
        assert_eq!(net.resource.as_deref(), Some("/tmp/harn.sock"));
    }

    #[test]
    fn files_upload_yields_fs_read_and_net_write_effects() {
        let source = r#"fn main(harness: Harness) {
            harness.llm.upload_file("/tmp/input.pdf", "gemini")
        }"#;
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
        let source = r#"fn main(harness: Harness) { harness.fs.write_text("/tmp/out", "hi") }"#;
        let effects = compute_handoff_effects(source, None);
        assert!(
            effects
                .iter()
                .any(|effect| matches!(effect.kind, EffectKind::Fs)
                    && effect.scope == EffectScope::Write
                    && effect.resource.as_deref() == Some("/tmp/out")),
            "expected Fs write effect, got {effects:?}"
        );
    }

    #[test]
    fn granular_capability_parameter_preserves_effect_contract() {
        let source = r#"
fn write_output(fs: HarnessFs) {
    fs.write_text("/tmp/out", "hi")
}

fn main(harness: Harness) {
    write_output(harness.fs)
}
"#;
        let effects = compute_handoff_effects(source, None);
        assert!(
            effects.iter().any(|effect| {
                matches!(effect.kind, EffectKind::Fs)
                    && effect.scope == EffectScope::Write
                    && effect.resource.as_deref() == Some("/tmp/out")
            }),
            "expected granular HarnessFs effect, got {effects:?}"
        );
    }

    #[test]
    fn capability_alias_preserves_effect_contract() {
        let source = r#"
fn main(harness: Harness) {
    const fs = harness.fs
    fs.write_text("/tmp/out", "hi")
}
"#;
        let effects = compute_handoff_effects(source, None);
        assert!(
            effects.iter().any(|effect| {
                matches!(effect.kind, EffectKind::Fs)
                    && effect.scope == EffectScope::Write
                    && effect.resource.as_deref() == Some("/tmp/out")
            }),
            "expected aliased HarnessFs effect, got {effects:?}"
        );
    }

    #[test]
    fn capability_method_can_declare_multiple_effects() {
        let source = r#"fn main(harness: Harness) {
            harness.net.download("https://example.test/data", "/tmp/data")
        }"#;
        let effects = compute_handoff_effects(source, None);
        assert!(
            effects.iter().any(|effect| {
                matches!(effect.kind, EffectKind::Net)
                    && effect.scope == EffectScope::Read
                    && effect.resource.as_deref() == Some("https://example.test/data")
            }),
            "expected download network effect, got {effects:?}"
        );
        assert!(
            effects.iter().any(|effect| {
                matches!(effect.kind, EffectKind::Fs)
                    && effect.scope == EffectScope::Write
                    && effect.resource.as_deref() == Some("/tmp/data")
            }),
            "expected download filesystem effect, got {effects:?}"
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
        let source = r#"fn main(harness: Harness) { sha256_hex("hello") }"#;
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
        let source = r#"fn main(harness: Harness) {
            harness.llm.call(
                "summarize",
                nil,
                { provider: "anthropic", model: "claude-3-5-sonnet" },
            )
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
    fn runtime_llm_contract_combines_provider_and_model_resources() {
        let entry = crate::stdlib::builtin_manifest_entry("__cap_llm_call")
            .expect("LLM capability manifest entry");
        let options = VmValue::dict(crate::value::DictMap::from_iter([
            ("provider", VmValue::String("anthropic".into())),
            ("model", VmValue::String("claude-sonnet-4".into())),
        ]));
        let effects = runtime_effects_from_contract(
            entry.contract.effects,
            &[VmValue::Nil, VmValue::Nil, options],
        );
        assert_eq!(effects.len(), 1);
        assert!(matches!(
            &effects[0].kind,
            EffectKind::Llm { provider: Some(provider), model: Some(model) }
                if provider == "anthropic" && model == "claude-sonnet-4"
        ));
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
            harness.fs.read_text("/tmp/in")
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
            harness.stdio.println("hi")
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
        let source = r#"fn main(harness: Harness) {
            harness.net.get("https://example.test")
            harness.net.get("https://example.test")
            harness.net.get("https://example.test")
        }"#;
        let effects = compute_handoff_effects(source, None);
        let net_count = effects
            .iter()
            .filter(|effect| matches!(effect.kind, EffectKind::Net))
            .count();
        assert_eq!(net_count, 1, "expected dedup, got {effects:?}");
    }
}
