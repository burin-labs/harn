//! Imported callable capability signatures and conservative call repair.
//!
//! Imported calls are independent of the local invocation graph: their
//! signatures come from the module graph, while their arguments may be any
//! expression. This module only inserts a carrier when arity and syntax make
//! omission observable; ambiguous capability-producing expressions are left
//! untouched for the type checker or a future typed repair.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use harn_builtin_meta::CapabilityId;
use harn_lexer::FixEdit;
use harn_parser::{BindingPattern, Node, SNode, TypeExpr, TypedParam};

use super::edits::{add_call_arguments_at_index_edit, argument_for_kind};
use super::{capability_carrier_kind, Carrier, CarrierKind, ProgramCallable, ProgramFile};

#[derive(Debug, Clone)]
pub(super) struct Signature {
    pub(super) prefix: Vec<CarrierKind>,
    required_params: usize,
    total_params: usize,
    has_rest: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ArgumentEvidence {
    Carrier,
    Identifier,
    DefinitelyOrdinary,
    Ambiguous,
}

pub(super) fn signatures(
    file: &Path,
    module_graph: &harn_modules::ModuleGraph,
    type_aliases: &BTreeMap<String, TypeExpr>,
) -> BTreeMap<String, Signature> {
    module_graph
        .imported_callable_declarations_for_file(file)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|declaration| {
            let declaration = match &declaration.node {
                Node::AttributedDecl { inner, .. } => inner.as_ref(),
                _ => &declaration,
            };
            let (name, params) = match &declaration.node {
                Node::FnDecl { name, params, .. }
                | Node::Pipeline { name, params, .. }
                | Node::ToolDecl { name, params, .. } => (name, params),
                _ => return None,
            };
            let prefix = params
                .iter()
                .map_while(|param| {
                    capability_carrier_kind(
                        param.type_expr.as_ref()?,
                        type_aliases,
                        &mut BTreeSet::new(),
                    )
                })
                .collect::<Vec<_>>();
            if prefix.is_empty() {
                return None;
            }
            let required_params = params
                .iter()
                .position(|param| param.default_value.is_some())
                .unwrap_or_else(|| params.iter().filter(|param| !param.rest).count());
            Some((
                name.clone(),
                Signature {
                    prefix,
                    required_params,
                    total_params: params.len(),
                    has_rest: params.last().is_some_and(|param| param.rest),
                },
            ))
        })
        .collect()
}

pub(super) fn missing_prefix(
    source: &str,
    carriers: &[Carrier],
    ordinary_bindings: &BTreeSet<String>,
    call: &super::super::CallSite,
    signature: &Signature,
) -> Option<usize> {
    if !signature.has_rest && call.args.len() >= signature.total_params {
        return None;
    }

    let first_missing = signature
        .prefix
        .iter()
        .zip(&call.args)
        .take_while(|(expected, span)| {
            source
                .get(span.start..span.end)
                .map(str::trim)
                .is_some_and(|actual| argument_supplies(actual, carriers, expected))
        })
        .count();
    if first_missing == signature.prefix.len() {
        return None;
    }

    let missing_count = signature.prefix.len() - first_missing;
    let required_without_missing = signature.required_params.saturating_sub(missing_count);
    let total_without_missing = signature.total_params.saturating_sub(missing_count);
    let minimum_without_missing = if signature.has_rest {
        required_without_missing.min(total_without_missing.saturating_sub(1))
    } else {
        required_without_missing
    };
    if call.args.len() < minimum_without_missing {
        return None;
    }

    let Some(first_missing_arg) = call.args.get(first_missing) else {
        return Some(first_missing);
    };
    let actual = source
        .get(first_missing_arg.start..first_missing_arg.end)
        .map(str::trim)?;

    match classify_argument(actual, carriers, &signature.prefix[first_missing]) {
        ArgumentEvidence::Carrier | ArgumentEvidence::Ambiguous => None,
        ArgumentEvidence::DefinitelyOrdinary => Some(first_missing),
        ArgumentEvidence::Identifier => ordinary_bindings.contains(actual).then_some(first_missing),
    }
}

pub(super) fn argument_edits(
    file: &ProgramFile,
    callable: &ProgramCallable,
    desired: &CarrierKind,
    additions: &BTreeMap<CapabilityId, String>,
) -> Vec<FixEdit> {
    callable
        .info
        .calls
        .iter()
        .filter_map(|call| {
            let signature = file.imported_capability_signatures.get(&call.callee)?;
            let first_missing = missing_prefix(
                &file.source,
                &callable.carriers,
                &callable.ordinary_bindings,
                call,
                signature,
            )?;
            let arguments = signature
                .prefix
                .iter()
                .skip(first_missing)
                .map(|kind| argument_for_kind(callable, desired, additions, kind))
                .collect::<Result<Vec<_>, _>>()
                .ok()?;
            add_call_arguments_at_index_edit(&file.source, call, first_missing, &arguments)
        })
        .collect()
}

pub(super) fn ordinary_bindings(
    params: &[TypedParam],
    body: &[SNode],
    type_aliases: &BTreeMap<String, TypeExpr>,
) -> BTreeSet<String> {
    let mut bindings = params
        .iter()
        .filter(|param| {
            param.type_expr.as_ref().is_some_and(|type_expr| {
                definitely_ordinary_type(type_expr, type_aliases, &mut BTreeSet::new())
            })
        })
        .map(|param| param.name.clone())
        .collect::<BTreeSet<_>>();
    for node in body {
        let (Node::LetBinding { pattern, value, .. } | Node::ConstBinding { pattern, value, .. }) =
            &node.node
        else {
            continue;
        };
        if definitely_ordinary(&value.node) {
            if let BindingPattern::Identifier(name) = pattern {
                bindings.insert(name.clone());
            }
        }
    }
    bindings
}

fn argument_supplies(actual: &str, carriers: &[Carrier], expected: &CarrierKind) -> bool {
    if actual == "harness" && matches!(expected, CarrierKind::Root) {
        return true;
    }
    carriers.iter().any(|carrier| {
        if actual == carrier.name {
            return &carrier.kind == expected;
        }
        let CarrierKind::Narrow(required) = expected else {
            return false;
        };
        let expected_projection = format!("{}.{}", carrier.name, required.field_name());
        actual == expected_projection
            && match &carrier.kind {
                CarrierKind::Root => true,
                CarrierKind::Bundle(capabilities) => capabilities.contains(required),
                CarrierKind::Narrow(_) => false,
            }
    })
}

fn classify_argument(
    actual: &str,
    carriers: &[Carrier],
    expected: &CarrierKind,
) -> ArgumentEvidence {
    if argument_supplies(actual, carriers, expected) {
        return ArgumentEvidence::Carrier;
    }

    let Some(expression) = harn_parser::interpolation::parse_expression(None, actual, 1, 1) else {
        return ArgumentEvidence::Ambiguous;
    };
    match expression.node {
        Node::Identifier(_) => ArgumentEvidence::Identifier,
        node if definitely_ordinary(&node) => ArgumentEvidence::DefinitelyOrdinary,
        _ => ArgumentEvidence::Ambiguous,
    }
}

fn definitely_ordinary_type(
    type_expr: &TypeExpr,
    type_aliases: &BTreeMap<String, TypeExpr>,
    resolving: &mut BTreeSet<String>,
) -> bool {
    match type_expr {
        TypeExpr::Named(name)
            if matches!(name.as_str(), "string" | "int" | "float" | "bool" | "nil") =>
        {
            true
        }
        TypeExpr::Named(name) => {
            let Some(alias) = type_aliases.get(name) else {
                return false;
            };
            if !resolving.insert(name.clone()) {
                return false;
            }
            let ordinary = definitely_ordinary_type(alias, type_aliases, resolving);
            resolving.remove(name);
            ordinary
        }
        TypeExpr::Union(types) | TypeExpr::Tuple(types) => types
            .iter()
            .all(|item| definitely_ordinary_type(item, type_aliases, resolving)),
        TypeExpr::List(_) | TypeExpr::FnType { .. } | TypeExpr::Never => true,
        TypeExpr::LitString(_) | TypeExpr::LitInt(_) => true,
        _ => false,
    }
}

fn definitely_ordinary(node: &Node) -> bool {
    matches!(
        node,
        Node::InterpolatedString(_)
            | Node::StringLiteral(_)
            | Node::RawStringLiteral(_)
            | Node::IntLiteral(_)
            | Node::FloatLiteral(_)
            | Node::BoolLiteral(_)
            | Node::NilLiteral
            | Node::DurationLiteral(_)
            | Node::ListLiteral(_)
            | Node::Closure { .. }
    )
}
