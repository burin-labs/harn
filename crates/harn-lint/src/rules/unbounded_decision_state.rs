//! `unbounded-native-decision-state` rule: warn when an evaluation site hands
//! a native-decision route an input whose declared type has no finite size
//! bound.
//!
//! A structured-LLM route admits an oversized state by refusing it: the
//! ceiling compares the encoded state against the route's window and returns
//! `state_too_large` before any request is dispatched. A native-decision route
//! does not have that escape. Its profile bounds encoded input and question
//! count as an admission condition, so a route whose input has no bound at all
//! cannot establish one and stays unavailable.
//!
//! The declared type is where that bound either exists or does not. `int`,
//! `bool`, `float` and string-literal enums encode to a bounded number of
//! tokens. `string`, `list<T>`, `dict<K, V>`, `any` and an open record admit
//! arbitrarily many, so no window is large enough by construction and the
//! site's behavior depends on data the author never sees.
//!
//! The fix is to bound the input at the site: narrow the declared type, or
//! split the input into windows that each fit, which `evaluation_windows`
//! in `std/predicate` does with the ceiling's own estimator.
//!
//! The gradual spellings — `any`, `unknown`, a bare `list` or `dict`, an open
//! record — are already a check-time error for any backend, because an
//! evaluation input must be a closed serializable type. They stay in the walk
//! below so the definition of "bounded" is structural rather than a list of
//! the cases another rule happens to leave. What this rule adds on top is the
//! typed container: `list<T>`, `dict<string, V>` and `string` are perfectly
//! good evaluation inputs and still admit arbitrarily many tokens.
//!
//! Scope. The rule reads declared types in one file: `let`/`const`
//! annotations, function and pipeline parameters, and `type` aliases declared
//! there. A state whose type comes from an import, or from inference with no
//! annotation, is not reported. That under-reports rather than warning on
//! every site whose type this rule cannot see, which is the failure mode that
//! gets a rule switched off.

use harn_lexer::Span;
use harn_parser::visit;
use harn_parser::{
    BindingPattern, DiagnosticCode as Code, DictEntry, Node, SNode, ShapeField, TypeExpr,
};
use std::collections::HashMap;

use crate::diagnostic::{LintDiagnostic, LintSeverity};
use crate::linter::harness_facts::HarnessFacts;

const RULE_NAME: &str = "unbounded-native-decision-state";

/// The backend spelling that names a native decision route.
const NATIVE_DECISION_BACKEND: &str = "native_decision";

/// The evaluation sites, and which argument carries the input the route
/// encodes. Both spellings of each reach the same ceiling.
const EVALUATION_SITES: &[(&str, usize, usize, &str)] = &[
    // (method, input index, policy index, input parameter name)
    ("evaluate", 1, 3, "state"),
    ("evaluate_predicate", 2, 3, "input"),
];

/// A named type whose values encode to a bounded number of tokens.
const BOUNDED_SCALARS: &[&str] = &["int", "float", "bool", "nil"];

/// The unbounded named types, reported by name so the message can say which
/// one removed the bound.
const UNBOUNDED_SCALARS: &[&str] = &["string", "any", "unknown", "bytes"];

pub(crate) fn check_unbounded_native_decision_state(
    program: &[SNode],
    harness: &HarnessFacts,
    diagnostics: &mut Vec<LintDiagnostic>,
) {
    let types = DeclaredTypes::collect(program);
    visit::walk_program(program, &mut |node| {
        let Some((args, input_index, policy_index, parameter)) = evaluation_site(node, harness)
        else {
            return;
        };
        let (Some(input), Some(policy)) = (args.get(input_index), args.get(policy_index)) else {
            return;
        };
        if !types.names_native_decision(policy) {
            return;
        }
        let Node::Identifier(name) = &input.node else {
            return;
        };
        let Some(declared) = types.bindings.get(name) else {
            return;
        };
        if let Some(unbounded) = types.unbounded_reason(declared) {
            diagnostics.push(make_diagnostic(input.span, parameter, &unbounded));
        }
    });
}

/// The call's argument list, when `node` is an evaluation site in either the
/// ambient or the `harness.llm.<method>` spelling.
fn evaluation_site<'node>(
    node: &'node SNode,
    harness: &HarnessFacts,
) -> Option<(&'node [SNode], usize, usize, &'static str)> {
    let (object, method, args) = match &node.node {
        Node::MethodCall {
            object,
            method,
            args,
        }
        | Node::OptionalMethodCall {
            object,
            method,
            args,
        } => (object, method, args),
        _ => return None,
    };
    if harness.capability_of(object) != Some("llm") {
        return None;
    }
    EVALUATION_SITES
        .iter()
        .find(|(name, ..)| name == method)
        .map(|(_, input, policy, parameter)| (args.as_slice(), *input, *policy, *parameter))
}

/// Declared types visible in one file: bindings and parameters by name, the
/// aliases those annotations may refer to, and the record literals policy
/// bindings are built from.
struct DeclaredTypes {
    bindings: HashMap<String, TypeExpr>,
    aliases: HashMap<String, TypeExpr>,
    records: HashMap<String, Vec<DictEntry>>,
}

impl DeclaredTypes {
    fn collect(program: &[SNode]) -> Self {
        let mut types = DeclaredTypes {
            bindings: HashMap::new(),
            aliases: HashMap::new(),
            records: HashMap::new(),
        };
        visit::walk_program(program, &mut |node| match &node.node {
            Node::LetBinding {
                pattern,
                type_ann,
                value,
                ..
            }
            | Node::ConstBinding {
                pattern,
                type_ann,
                value,
                ..
            } => {
                let BindingPattern::Identifier(name) = pattern else {
                    return;
                };
                if let Some(type_ann) = type_ann {
                    types.bindings.insert(name.clone(), type_ann.clone());
                }
                if let Node::DictLiteral(entries) = &value.node {
                    types.records.insert(name.clone(), entries.clone());
                }
            }
            Node::TypeDecl {
                name, type_expr, ..
            } => {
                types.aliases.insert(name.clone(), type_expr.clone());
            }
            Node::FnDecl { params, .. }
            | Node::Pipeline { params, .. }
            | Node::ToolDecl { params, .. } => {
                for param in params {
                    if let Some(type_expr) = &param.type_expr {
                        types.bindings.insert(param.name.clone(), type_expr.clone());
                    }
                }
            }
            _ => {}
        });
        types
    }

    /// Whether `policy` names the native decision route.
    ///
    /// A policy is normally a binding rather than an inline literal, so an
    /// identifier resolves through the record it was built from. A policy this
    /// rule cannot read is not a native route as far as it knows, and stays
    /// silent rather than guessing.
    fn names_native_decision(&self, policy: &SNode) -> bool {
        let entries = match &policy.node {
            Node::DictLiteral(entries) => entries.as_slice(),
            Node::Identifier(name) => match self.records.get(name) {
                Some(entries) => entries.as_slice(),
                None => return false,
            },
            _ => return false,
        };
        entry_for_key(entries, "backend").is_some_and(|entry| {
            matches!(
                &entry.value.node,
                Node::StringLiteral(value) | Node::RawStringLiteral(value)
                    if value == NATIVE_DECISION_BACKEND
            )
        })
    }

    /// The name of the first construct in `declared` that removes its finite
    /// size bound, or `None` when every part of the type is bounded.
    fn unbounded_reason(&self, declared: &TypeExpr) -> Option<String> {
        self.reason(declared, 0)
    }

    fn reason(&self, declared: &TypeExpr, depth: usize) -> Option<String> {
        // An alias chain this deep is either cyclic or beyond what a check-time
        // reading should follow. Stay silent rather than report from a
        // truncated walk.
        if depth > 16 {
            return None;
        }
        match declared {
            TypeExpr::Named(name) => {
                if BOUNDED_SCALARS.contains(&name.as_str()) {
                    return None;
                }
                if UNBOUNDED_SCALARS.contains(&name.as_str()) {
                    return Some(name.clone());
                }
                // A bare `list` or `dict` with no argument is the gradual
                // spelling of the same unbounded container.
                if name == "list" || name == "dict" {
                    return Some(name.clone());
                }
                let alias = self.aliases.get(name)?;
                self.reason(alias, depth + 1)
            }
            TypeExpr::List(inner) | TypeExpr::Iter(inner) | TypeExpr::Stream(inner) => {
                let _ = inner;
                Some(type_name(declared).to_string())
            }
            TypeExpr::DictType(..) => Some("dict".to_string()),
            TypeExpr::Generator(_) => Some("Generator".to_string()),
            TypeExpr::OpenShape { .. } => Some("open record".to_string()),
            TypeExpr::Shape(fields) => fields
                .iter()
                .find_map(|field: &ShapeField| self.reason(&field.type_expr, depth + 1)),
            TypeExpr::Tuple(items) => items.iter().find_map(|item| self.reason(item, depth + 1)),
            TypeExpr::Union(parts) | TypeExpr::Intersection(parts) => {
                parts.iter().find_map(|part| self.reason(part, depth + 1))
            }
            TypeExpr::Applied { name, args } => {
                if name == "list" || name == "dict" {
                    return Some(name.clone());
                }
                args.iter().find_map(|arg| self.reason(arg, depth + 1))
            }
            // Literal types, `Never`, function types and owned handles carry
            // no unbounded payload of their own.
            _ => None,
        }
    }
}

fn type_name(declared: &TypeExpr) -> &'static str {
    match declared {
        TypeExpr::List(_) => "list",
        TypeExpr::Iter(_) => "iter",
        TypeExpr::Stream(_) => "Stream",
        _ => "type",
    }
}

fn entry_for_key<'a>(entries: &'a [DictEntry], key: &str) -> Option<&'a DictEntry> {
    entries
        .iter()
        .find(|entry| key_name(&entry.key).as_deref() == Some(key))
}

fn key_name(node: &SNode) -> Option<String> {
    match &node.node {
        Node::StringLiteral(value) | Node::RawStringLiteral(value) | Node::Identifier(value) => {
            Some(value.clone())
        }
        _ => None,
    }
}

fn make_diagnostic(span: Span, parameter: &str, unbounded: &str) -> LintDiagnostic {
    LintDiagnostic {
        code: Code::LintUnboundedNativeDecisionState,
        rule: RULE_NAME.into(),
        message: format!(
            "this policy names the `{NATIVE_DECISION_BACKEND}` route, whose admission bounds \
             encoded input, but the declared type of `{parameter}` contains `{unbounded}` and so \
             has no finite size bound."
        ),
        span,
        severity: LintSeverity::Warning,
        suggestion: Some(
            "narrow the declared type so its encoded size is bounded, or split the input with \
             `evaluation_windows` from `std/predicate`, which sizes each window with the \
             evaluator's own estimator."
                .to_string(),
        ),
        fix: None,
    }
}
