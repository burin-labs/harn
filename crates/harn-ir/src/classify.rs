//! Classifying a call site and summarizing the values around it.
//!
//! Decides whether a call is a tool call, a host call, a model call, a worker
//! dispatch, or network access, and which capability that implies. The
//! remaining helpers reduce expressions to the literal values, paths, and
//! summaries the invariants can reason about.

use harn_parser::{Node, SNode};
use std::collections::{BTreeMap, HashSet, VecDeque};

use crate::types::*;
pub(crate) fn scoped_policy_call(name: &str) -> Option<PolicyScopeKind> {
    match name {
        "with_execution_policy" => Some(PolicyScopeKind::Execution),
        "with_approval_policy" => Some(PolicyScopeKind::ToolApproval),
        "with_command_policy" => Some(PolicyScopeKind::Command),
        "with_autonomy_policy" => Some(PolicyScopeKind::Autonomy),
        "with_dynamic_permissions" => Some(PolicyScopeKind::DynamicPermissions),
        _ => None,
    }
}

/// Collect the literal-arg vector for a synthesized harness CallSemantics
/// node. Mirrors the equivalent line in [`classify_call`] but avoids
/// re-running the classifier — the caller already supplies the ambient
/// name to dispatch through.
pub(crate) fn literal_args(args: &[SNode]) -> Vec<LiteralValue> {
    args.iter().map(literal_value).collect()
}

/// If `object` is the `harness.<sub_handle>` chain and `method` maps
/// to an ambient builtin via the per-sub-handle dispatch table, return
/// the sub-handle name and the ambient builtin to attribute the call
/// to. Returns `None` for arbitrary method calls so the existing
/// pass-through walk continues to handle them.
pub(crate) fn harness_sub_handle_for(
    object: &SNode,
    method: &str,
) -> Option<(&'static str, &'static str)> {
    let (sub_handle, root) = match &object.node {
        Node::PropertyAccess { object, property }
        | Node::OptionalPropertyAccess { object, property } => (property.as_str(), object.as_ref()),
        _ => return None,
    };
    let Node::Identifier(receiver) = &root.node else {
        return None;
    };
    if receiver != "harness" && receiver != "_harness" {
        return None;
    }
    crate::call_semantics::HARNESS_SUB_HANDLES
        .iter()
        .find(|slug| **slug == sub_handle)
        .and_then(|slug| {
            harn_parser::harness_methods::harness_sub_handle_ambient(slug, method)
                .map(|ambient| (*slug, ambient))
        })
}

pub(crate) fn classify_call(name: &str, args: &[SNode]) -> CallSemantics {
    let literal_args = args.iter().map(literal_value).collect::<Vec<_>>();
    let mut display_name = name.to_string();
    let classification = match name {
        "request_approval" => CallClassification::ApprovalGate,
        "llm_budget_remaining" | "llm_budget" => CallClassification::BudgetRead,
        "egress_policy" => CallClassification::PolicyGate(PolicyScopeKind::Egress),
        "command_policy_push" => CallClassification::PolicyPush(PolicyScopeKind::Command),
        "command_policy_pop" => CallClassification::PolicyPop(PolicyScopeKind::Command),
        _ if crate::call_semantics::is_workspace_mutation(name) => {
            let path = literal_args
                .first()
                .and_then(LiteralValue::as_str)
                .map(str::to_string);
            capability_classification(vec![CapabilityEffect::new(
                Capability::WorkspaceMutation,
                name,
                path,
            )])
        }
        "copy_file" => {
            let path = literal_args
                .get(1)
                .and_then(LiteralValue::as_str)
                .map(str::to_string);
            capability_classification(vec![CapabilityEffect::new(
                Capability::WorkspaceMutation,
                name,
                path,
            )])
        }
        "exec" | "exec_at" | "shell" | "shell_at" | "spawn_captured" => {
            capability_classification(vec![CapabilityEffect::new(
                Capability::CommandExecution,
                name,
                None,
            )])
        }
        "mcp_call" => {
            let tool_name = literal_args
                .get(1)
                .and_then(LiteralValue::as_str)
                .map(str::to_string);
            if let Some(tool_name) = tool_name {
                display_name = tool_name.clone();
                classify_tool_call(&tool_name, literal_args.get(2))
            } else {
                capability_classification(vec![CapabilityEffect::new(
                    Capability::ConnectorAccess,
                    name,
                    None,
                )])
            }
        }
        "host_tool_call" => {
            let tool_name = literal_args
                .first()
                .and_then(LiteralValue::as_str)
                .map(str::to_string);
            if let Some(tool_name) = tool_name {
                display_name = tool_name.clone();
                classify_tool_call(&tool_name, literal_args.get(1))
            } else {
                capability_classification(vec![CapabilityEffect::new(
                    Capability::ConnectorAccess,
                    name,
                    None,
                )])
            }
        }
        "host_call" => classify_host_call(literal_args.first()),
        _ if is_model_call(name) => capability_classification(vec![CapabilityEffect::new(
            Capability::ModelCall,
            name,
            None,
        )]),
        _ if is_worker_dispatch(name) => capability_classification(vec![CapabilityEffect::new(
            Capability::WorkerDispatch,
            name,
            None,
        )]),
        _ if is_network_call(name) => capability_classification(vec![CapabilityEffect::new(
            Capability::NetworkAccess,
            name,
            None,
        )]),
        _ if name.starts_with("mcp_") => capability_classification(vec![CapabilityEffect::new(
            Capability::ConnectorAccess,
            name,
            None,
        )]),
        _ => CallClassification::Other,
    };

    CallSemantics {
        name: name.to_string(),
        display_name,
        classification,
        literal_args,
    }
}

fn classify_tool_call(tool_name: &str, args: Option<&LiteralValue>) -> CallClassification {
    let normalized = tool_name.to_ascii_lowercase();
    let path = args.and_then(extract_path_from_tool_args);
    let mut effects = vec![CapabilityEffect::new(
        Capability::ConnectorAccess,
        tool_name,
        None,
    )];
    if matches!(
        normalized.as_str(),
        "write_file"
            | "copy_file"
            | "delete_file"
            | "mkdir"
            | "apply_edit"
            | "write"
            | "edit"
            | "delete"
            | "move"
            | "rename"
            | "patch"
    ) || normalized.contains("append")
        || normalized.contains("write")
        || normalized.contains("edit")
        || normalized.contains("delete")
        || normalized.contains("move")
        || normalized.contains("rename")
        || normalized.contains("patch")
    {
        effects.push(CapabilityEffect::new(
            Capability::WorkspaceMutation,
            tool_name,
            path,
        ));
    }
    if normalized.contains("exec")
        || normalized.contains("shell")
        || normalized.contains("run")
        || normalized.contains("push_pr")
        || normalized.contains("create_pr")
        || normalized.contains("deploy")
    {
        effects.push(CapabilityEffect::new(
            Capability::CommandExecution,
            tool_name,
            None,
        ));
    }
    capability_classification(effects)
}

fn classify_host_call(name: Option<&LiteralValue>) -> CallClassification {
    let Some(operation) = name.and_then(LiteralValue::as_str) else {
        return capability_classification(vec![CapabilityEffect::new(
            Capability::ConnectorAccess,
            "host_call",
            None,
        )]);
    };
    if operation == "process.exec" || operation.starts_with("process.") {
        return capability_classification(vec![CapabilityEffect::new(
            Capability::CommandExecution,
            operation,
            None,
        )]);
    }
    if operation.starts_with("workspace.")
        && (operation.contains("write")
            || operation.contains("edit")
            || operation.contains("delete")
            || operation.contains("move")
            || operation.contains("patch"))
    {
        return capability_classification(vec![CapabilityEffect::new(
            Capability::WorkspaceMutation,
            operation,
            None,
        )]);
    }
    capability_classification(vec![CapabilityEffect::new(
        Capability::ConnectorAccess,
        operation,
        None,
    )])
}

fn capability_classification(effects: Vec<CapabilityEffect>) -> CallClassification {
    if effects.is_empty() {
        CallClassification::Other
    } else {
        CallClassification::Capabilities(effects)
    }
}

fn is_model_call(name: &str) -> bool {
    matches!(
        name,
        "llm_call"
            | "llm_call_safe"
            | "llm_stream_call"
            | "llm_call_structured"
            | "llm_call_structured_safe"
            | "llm_call_structured_result"
            | "llm_completion"
            | "agent_llm_turn"
            | "agent_turn"
            | "agent_loop"
    )
}

fn is_worker_dispatch(name: &str) -> bool {
    matches!(
        name,
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
            | "__host_worker_close"
    )
}

fn is_network_call(name: &str) -> bool {
    matches!(
        name,
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
            | "websocket_accept"
            | "websocket_connect"
            | "websocket_send"
            | "websocket_receive"
            | "websocket_close"
            | "websocket_route"
            | "websocket_server"
            | "websocket_server_close"
            | "unix_socket_json_request"
            | "__net_unix_socket_json_request"
    )
}

fn extract_path_from_tool_args(value: &LiteralValue) -> Option<String> {
    for key in ["path", "dst", "destination", "target"] {
        if let Some(path) = value.dict_field(key).and_then(LiteralValue::as_str) {
            return Some(path.to_string());
        }
    }
    None
}

pub(crate) fn literal_value(node: &SNode) -> LiteralValue {
    match &node.node {
        Node::StringLiteral(value) | Node::RawStringLiteral(value) => {
            LiteralValue::String(value.clone())
        }
        Node::Identifier(value) => LiteralValue::Identifier(value.clone()),
        Node::IntLiteral(value) => LiteralValue::Number(value.to_string()),
        Node::FloatLiteral(value) => LiteralValue::Number(value.to_string()),
        Node::BoolLiteral(value) => LiteralValue::Bool(*value),
        Node::NilLiteral => LiteralValue::Nil,
        Node::DictLiteral(entries)
        | Node::StructConstruct {
            fields: entries, ..
        } => {
            let mut map = BTreeMap::new();
            for entry in entries {
                if let Some(key) = literal_key(&entry.key) {
                    map.insert(key, literal_value(&entry.value));
                }
            }
            LiteralValue::Dict(map)
        }
        Node::ListLiteral(items) => LiteralValue::List(items.iter().map(literal_value).collect()),
        _ => LiteralValue::Unknown,
    }
}

fn literal_key(node: &SNode) -> Option<String> {
    match &node.node {
        Node::StringLiteral(value) | Node::RawStringLiteral(value) | Node::Identifier(value) => {
            Some(value.clone())
        }
        _ => None,
    }
}

pub(crate) fn expr_summary(node: &SNode) -> ExprSummary {
    match &node.node {
        Node::Identifier(name) => ExprSummary::Reference(name.clone()),
        Node::PropertyAccess { .. } | Node::OptionalPropertyAccess { .. } => target_path(node)
            .map(ExprSummary::Reference)
            .unwrap_or(ExprSummary::Unknown),
        Node::FunctionCall { name, .. } => ExprSummary::Call(name.clone()),
        Node::BinaryOp { op, left, right } => ExprSummary::Binary {
            op: op.clone(),
            left: Box::new(expr_summary(left)),
            right: Box::new(expr_summary(right)),
        },
        Node::IntLiteral(_)
        | Node::FloatLiteral(_)
        | Node::StringLiteral(_)
        | Node::RawStringLiteral(_)
        | Node::BoolLiteral(_)
        | Node::NilLiteral => ExprSummary::Literal,
        _ => ExprSummary::Unknown,
    }
}

pub(crate) fn assignment_is_non_increasing(assignment: &AssignmentSemantics, target: &str) -> bool {
    match assignment.op.as_deref() {
        Some("-") => true,
        Some("+") | Some("*") | Some("/") | Some("%") => false,
        Some(_) => false,
        None => match &assignment.value {
            ExprSummary::Reference(value) => value == target,
            ExprSummary::Call(name) => name == "llm_budget_remaining",
            ExprSummary::Binary { op, left, .. } if op == "-" => {
                matches!(left.as_ref(), ExprSummary::Reference(value) if value == target)
            }
            _ => false,
        },
    }
}

pub(crate) fn path_to_node(ir: &HandlerIr, target: NodeId) -> Vec<PathStep> {
    let mut queue = VecDeque::new();
    let mut seen = HashSet::new();
    queue.push_back((ir.entry, vec![ir.entry]));

    while let Some((node, path)) = queue.pop_front() {
        if node == target {
            return path
                .into_iter()
                .map(|id| {
                    let node = ir.node(id);
                    PathStep {
                        span: node.span,
                        label: node.label.clone(),
                    }
                })
                .collect();
        }
        if !seen.insert(node) {
            continue;
        }
        for succ in ir.successors(node) {
            let mut next_path = path.clone();
            next_path.push(succ);
            queue.push_back((succ, next_path));
        }
    }

    Vec::new()
}

pub(crate) fn target_path(node: &SNode) -> Option<String> {
    match &node.node {
        Node::Identifier(name) => Some(name.clone()),
        Node::PropertyAccess { object, property }
        | Node::OptionalPropertyAccess { object, property } => {
            let base = target_path(object)?;
            Some(format!("{base}.{property}"))
        }
        _ => None,
    }
}

pub(crate) fn pattern_label(node: &SNode) -> String {
    match &node.node {
        Node::StringLiteral(value) | Node::RawStringLiteral(value) => format!("{value:?}"),
        Node::Identifier(value) => value.clone(),
        Node::IntLiteral(value) => value.to_string(),
        Node::BoolLiteral(value) => value.to_string(),
        Node::NilLiteral => "nil".to_string(),
        Node::OrPattern(_) => "or-pattern".to_string(),
        _ => "pattern".to_string(),
    }
}
