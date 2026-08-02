//! Imported callable capability signatures and conservative call repair.
//!
//! Imported calls are independent of the local invocation graph: their
//! signatures come from the module graph, while their arguments may be any
//! expression. This module only inserts a carrier when arity and syntax make
//! omission observable; ambiguous capability-producing expressions are left
//! untouched for the type checker or a future typed repair.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;

use harn_builtin_meta::CapabilityId;
use harn_lexer::FixEdit;
use harn_parser::{Node, SNode, TypeExpr, TypedParam};

use super::edits::{add_call_arguments_at_index_edit, argument_for_kind};
use super::{capability_carrier_kind, Carrier, CarrierKind, ProgramCallable, ProgramFile};

#[derive(Debug, Clone)]
pub(super) struct Signature {
    pub(super) prefix: Vec<CarrierKind>,
    required_params: usize,
    total_params: usize,
    has_rest: bool,
}

#[derive(Debug, Clone)]
pub(super) struct PrefixRepair {
    insertions: Vec<PrefixInsertion>,
}

#[derive(Debug, Clone)]
struct PrefixInsertion {
    argument_index: usize,
    prefix_start: usize,
    prefix_end: usize,
}

impl PrefixRepair {
    pub(super) fn missing_kinds<'a>(
        &'a self,
        signature: &'a Signature,
    ) -> impl Iterator<Item = &'a CarrierKind> {
        self.insertions.iter().flat_map(|insertion| {
            signature.prefix[insertion.prefix_start..insertion.prefix_end].iter()
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ArgumentEvidence {
    Carrier,
    Identifier,
    DefinitelyOrdinary,
    Ambiguous,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum BindingEvidence {
    Capability(CarrierKind),
    Ordinary,
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

pub(super) fn prefix_repair(
    source: &str,
    carriers: &[Carrier],
    bindings: &BTreeMap<harn_parser::lexical::BindingId, BindingEvidence>,
    resolutions: &HashMap<(usize, usize), harn_parser::lexical::BindingId>,
    call: &super::super::CallSite,
    signature: &Signature,
) -> Option<PrefixRepair> {
    if !signature.has_rest && call.args.len() >= signature.total_params {
        return None;
    }

    let mut expected_index = 0;
    let mut argument_index = 0;
    let mut insertions = Vec::new();
    while expected_index < signature.prefix.len() {
        let Some(argument_span) = call.args.get(argument_index) else {
            insertions.push(PrefixInsertion {
                argument_index,
                prefix_start: expected_index,
                prefix_end: signature.prefix.len(),
            });
            break;
        };
        let actual = source
            .get(argument_span.start..argument_span.end)
            .map(str::trim)?;
        let evidence = classify_argument(
            actual,
            carriers,
            bindings,
            resolutions,
            argument_span,
            &signature.prefix[expected_index],
        );
        if evidence == ArgumentEvidence::Carrier {
            expected_index += 1;
            argument_index += 1;
            continue;
        }
        if let Some(later_index) = ((expected_index + 1)..signature.prefix.len()).find(|index| {
            classify_argument(
                actual,
                carriers,
                bindings,
                resolutions,
                argument_span,
                &signature.prefix[*index],
            ) == ArgumentEvidence::Carrier
        }) {
            insertions.push(PrefixInsertion {
                argument_index,
                prefix_start: expected_index,
                prefix_end: later_index,
            });
            expected_index = later_index;
            continue;
        }
        match evidence {
            ArgumentEvidence::DefinitelyOrdinary => {
                insertions.push(PrefixInsertion {
                    argument_index,
                    prefix_start: expected_index,
                    prefix_end: signature.prefix.len(),
                });
                break;
            }
            ArgumentEvidence::Identifier | ArgumentEvidence::Ambiguous => return None,
            ArgumentEvidence::Carrier => unreachable!(),
        }
    }
    if insertions.is_empty() {
        return None;
    }

    let missing_count = insertions
        .iter()
        .map(|insertion| insertion.prefix_end - insertion.prefix_start)
        .sum::<usize>();
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
    Some(PrefixRepair { insertions })
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
            let repair = prefix_repair(
                &file.source,
                &callable.carriers,
                &callable.imported_binding_evidence,
                &callable.resolved_imported_bindings,
                call,
                signature,
            )?;
            Some(repair.insertions.into_iter().filter_map(|insertion| {
                let arguments = signature.prefix[insertion.prefix_start..insertion.prefix_end]
                    .iter()
                    .map(|kind| argument_for_kind(callable, desired, additions, kind))
                    .collect::<Result<Vec<_>, _>>()
                    .ok()?;
                add_call_arguments_at_index_edit(
                    &file.source,
                    call,
                    insertion.argument_index,
                    &arguments,
                )
            }))
        })
        .flatten()
        .collect()
}

pub(super) fn binding_evidence(
    params: &[TypedParam],
    body: &[SNode],
    type_aliases: &BTreeMap<String, TypeExpr>,
    binding_types: &[harn_parser::BindingTypeInfo],
) -> BTreeMap<harn_parser::lexical::BindingId, BindingEvidence> {
    let mut bindings = params
        .iter()
        .filter_map(|param| {
            let type_expr = param.type_expr.as_ref()?;
            evidence_for_type(type_expr, type_aliases).map(|evidence| {
                (
                    harn_parser::lexical::BindingId::from_declaration(
                        param.name.clone(),
                        param.span,
                    ),
                    evidence,
                )
            })
        })
        .collect::<BTreeMap<_, _>>();
    for binding in binding_types.iter().filter(|binding| {
        body.iter()
            .any(|node| node.span.start <= binding.span.start && binding.span.end <= node.span.end)
    }) {
        if let Some(evidence) = evidence_for_type(&binding.type_expr, type_aliases) {
            bindings.insert(
                harn_parser::lexical::BindingId::from_declaration(
                    binding.name.clone(),
                    binding.span,
                ),
                evidence,
            );
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
    bindings: &BTreeMap<harn_parser::lexical::BindingId, BindingEvidence>,
    resolutions: &HashMap<(usize, usize), harn_parser::lexical::BindingId>,
    argument_span: &harn_lexer::Span,
    expected: &CarrierKind,
) -> ArgumentEvidence {
    if argument_supplies(actual, carriers, expected) {
        return ArgumentEvidence::Carrier;
    }

    let Some(expression) = harn_parser::interpolation::parse_expression(None, actual, 1, 1) else {
        return ArgumentEvidence::Ambiguous;
    };
    match expression.node {
        Node::Identifier(_) => match resolutions
            .get(&(argument_span.start, argument_span.end))
            .and_then(|binding| bindings.get(binding))
        {
            Some(BindingEvidence::Capability(actual)) if actual == expected => {
                ArgumentEvidence::Carrier
            }
            Some(BindingEvidence::Capability(_) | BindingEvidence::Ordinary) => {
                ArgumentEvidence::DefinitelyOrdinary
            }
            None => ArgumentEvidence::Identifier,
        },
        node if definitely_ordinary(&node) => ArgumentEvidence::DefinitelyOrdinary,
        _ => ArgumentEvidence::Ambiguous,
    }
}

fn evidence_for_type(
    type_expr: &TypeExpr,
    type_aliases: &BTreeMap<String, TypeExpr>,
) -> Option<BindingEvidence> {
    capability_carrier_kind(type_expr, type_aliases, &mut BTreeSet::new())
        .map(BindingEvidence::Capability)
        .or_else(|| {
            definitely_ordinary_type(type_expr, type_aliases, &mut BTreeSet::new())
                .then_some(BindingEvidence::Ordinary)
        })
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
