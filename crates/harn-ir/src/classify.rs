//! Classifying a call site and summarizing the values around it.
//!
//! Decides whether a call is a tool call, a host call, a model call, a worker
//! dispatch, or network access, and which capability that implies. The
//! remaining helpers reduce expressions to the literal values, paths, and
//! summaries the invariants can reason about.

use harn_builtin_meta::{BuiltinContract, EffectAccess, EffectKind, ResourceSelector};
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

pub(crate) fn literal_args(args: &[SNode]) -> Vec<LiteralValue> {
    args.iter().map(literal_value).collect()
}

/// Resolve a root Harness method to its manifest entry. No legacy builtin
/// name is reconstructed: the typed contract is the semantic owner.
pub(crate) fn capability_method_for(
    object: &SNode,
    method: &str,
    capability_handles: &BTreeMap<String, harn_builtin_meta::CapabilityId>,
) -> Option<(
    &'static str,
    &'static harn_builtin_registry::BuiltinManifestEntry,
)> {
    let capability = match &object.node {
        Node::PropertyAccess { object, property }
        | Node::OptionalPropertyAccess { object, property } => {
            let Node::Identifier(receiver) = &object.node else {
                return None;
            };
            if receiver != "harness" && receiver != "_harness" {
                return None;
            }
            harn_builtin_meta::CapabilityId::from_field_name(property)?
        }
        Node::Identifier(receiver) => *capability_handles.get(receiver)?,
        _ => return None,
    };
    let entry =
        harn_parser::builtin_signatures::capability_method_entry(capability.field_name(), method)?;
    let harn_builtin_meta::BuiltinExposure::HarnessMethod { capability, .. } =
        entry.contract.exposure
    else {
        return None;
    };
    Some((capability.field_name(), entry))
}

pub(crate) fn classify_call(name: &str, args: &[SNode]) -> CallSemantics {
    let literal_args = args.iter().map(literal_value).collect::<Vec<_>>();
    let display_name = name.to_string();
    let classification = match name {
        "request_approval" => CallClassification::ApprovalGate,
        "llm_budget_remaining" | "llm_budget" => CallClassification::BudgetRead,
        "egress_policy" => CallClassification::PolicyGate(PolicyScopeKind::Egress),
        "command_policy_push" => CallClassification::PolicyPush(PolicyScopeKind::Command),
        "command_policy_pop" => CallClassification::PolicyPop(PolicyScopeKind::Command),
        _ => harn_builtin_registry::builtin_contract(name)
            .map(|contract| classify_contract(name, contract, &literal_args))
            .unwrap_or(CallClassification::Other),
    };

    CallSemantics {
        name: name.to_string(),
        display_name,
        classification,
        literal_args,
    }
}

pub(crate) fn classify_contract(
    operation: &str,
    contract: &BuiltinContract,
    args: &[LiteralValue],
) -> CallClassification {
    let effects = contract
        .effects
        .iter()
        .map(|spec| {
            let capability = capability_for_effect(spec.kind, spec.access);
            let path = spec
                .resources
                .iter()
                .find_map(|selector| resolve_resource(selector, args));
            CapabilityEffect::from_contract(capability, operation, path, spec.access)
        })
        .collect();
    capability_classification(effects)
}

fn capability_for_effect(kind: EffectKind, access: EffectAccess) -> Capability {
    match kind {
        EffectKind::Fs if access == EffectAccess::Read => Capability::FilesystemRead,
        EffectKind::Fs => Capability::WorkspaceMutation,
        EffectKind::Process => Capability::CommandExecution,
        EffectKind::Network => Capability::NetworkAccess,
        EffectKind::Llm => Capability::ModelCall,
        EffectKind::Tool | EffectKind::Mcp | EffectKind::Host => Capability::ConnectorAccess,
        EffectKind::Authority => Capability::Authority,
        EffectKind::Worker => Capability::WorkerDispatch,
        EffectKind::Stdio => Capability::Stdio,
        EffectKind::Env => Capability::Environment,
        EffectKind::Clock => Capability::Clock,
        EffectKind::Random => Capability::Random,
        EffectKind::Secret => Capability::Secret,
        EffectKind::Observability => Capability::Observability,
        EffectKind::Channel => Capability::Channel,
        EffectKind::State => Capability::State,
    }
}

fn resolve_resource(selector: &ResourceSelector, args: &[LiteralValue]) -> Option<String> {
    match selector {
        ResourceSelector::Argument(index) => args
            .get(*index as usize)
            .and_then(LiteralValue::as_str)
            .map(str::to_string),
        ResourceSelector::Field { argument, path } => {
            let mut value = args.get(*argument as usize)?;
            for field in *path {
                value = value.dict_field(field)?;
            }
            value.as_str().map(str::to_string)
        }
        ResourceSelector::EachArgument(index) => args
            .get(*index as usize)
            .and_then(LiteralValue::list_items)
            .and_then(|items| items.first())
            .and_then(LiteralValue::as_str)
            .map(str::to_string),
        ResourceSelector::Constant(value) => Some((*value).to_string()),
        ResourceSelector::Dynamic => None,
    }
}

fn capability_classification(effects: Vec<CapabilityEffect>) -> CallClassification {
    if effects.is_empty() {
        CallClassification::Other
    } else {
        CallClassification::Capabilities(effects)
    }
}

pub fn literal_value(node: &SNode) -> LiteralValue {
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
