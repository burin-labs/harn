//! Lexical binding and capture analysis shared by compiler and typechecker.
//!
//! AST visitors answer structural questions. Capture analysis is different: an
//! identifier only captures a binding when it resolves outside the callable
//! that contains the reference. Keeping that resolution here avoids each
//! consumer inventing a slightly different notion of scope and shadowing.

use std::collections::{BTreeSet, HashMap, HashSet};

use harn_lexer::Span;

use crate::ast::{is_discard_name, BindingPattern, Node, SNode, TypedParam};

/// Stable identity for a source binding. Patterns do not carry individual
/// spans, so the declaration span plus the bound name is the narrowest source
/// identity available without changing the AST.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BindingId {
    pub name: String,
    pub declaration_start: usize,
    pub declaration_end: usize,
}

impl BindingId {
    pub fn from_declaration(name: impl Into<String>, span: Span) -> Self {
        Self {
            name: name.into(),
            declaration_start: span.start,
            declaration_end: span.end,
        }
    }
}

/// Return every name introduced by a destructuring pattern, in source order.
/// The projection is intentionally shared by the compiler and typechecker.
pub fn binding_pattern_names(pattern: &BindingPattern) -> Vec<String> {
    match pattern {
        BindingPattern::Identifier(name) => vec![name.clone()],
        BindingPattern::Pair(first, second) => vec![first.clone(), second.clone()],
        BindingPattern::Dict(fields) => fields
            .iter()
            .map(|field| field.alias.clone().unwrap_or_else(|| field.key.clone()))
            .collect(),
        BindingPattern::List(elements) => elements
            .iter()
            .map(|element| element.name.clone())
            .collect(),
    }
}

/// Return the source identities introduced by `pattern` at `declaration`.
pub fn binding_pattern_ids(pattern: &BindingPattern, declaration: Span) -> Vec<BindingId> {
    binding_pattern_names(pattern)
        .into_iter()
        .filter(|name| !is_discard_name(name))
        .map(|name| BindingId::from_declaration(name, declaration))
        .collect()
}

/// Bindings in the current compiled body referenced by a nested callable.
///
/// A result is declaration-identity based rather than name based. That keeps
/// `let pin` distinct from a later `{ pin -> ... }` parameter or an inner
/// block-local `let pin`, which is essential when selecting VM storage.
pub fn captured_bindings_in_nested_callables(body: &[SNode]) -> HashSet<BindingId> {
    let mut analysis = LexicalAnalysis::default();
    analysis.walk_body(body, Vec::new(), false, BindingOwner::Current);
    analysis.captured
}

/// Names reassigned by a nested callable that are free relative to the current
/// callable body. Type-flow narrowing uses this conservative summary: unknown
/// names remain included so parameter captures continue to invalidate their
/// narrowing at the caller-owned scope.
pub fn nested_callable_reassigned_names(body: &[SNode]) -> Vec<String> {
    let mut analysis = LexicalAnalysis::default();
    analysis.walk_body(body, Vec::new(), false, BindingOwner::Current);
    analysis.reassigned.into_iter().collect()
}

#[derive(Debug, Clone)]
enum BindingOwner {
    Current,
    Nested,
}

#[derive(Debug, Clone)]
enum ScopeBinding {
    Current(BindingId),
    Nested,
}

type Scope = HashMap<String, ScopeBinding>;

#[derive(Default)]
struct LexicalAnalysis {
    captured: HashSet<BindingId>,
    reassigned: BTreeSet<String>,
}

impl LexicalAnalysis {
    fn walk_body(
        &mut self,
        body: &[SNode],
        scopes: Vec<Scope>,
        inside_nested_callable: bool,
        owner: BindingOwner,
    ) {
        self.walk_body_with_bindings(body, scopes, inside_nested_callable, owner, Scope::new());
    }

    fn walk_body_with_bindings(
        &mut self,
        body: &[SNode],
        mut scopes: Vec<Scope>,
        inside_nested_callable: bool,
        owner: BindingOwner,
        extra_bindings: Scope,
    ) {
        // Named callables are late-bound and may recurse or mutually recurse.
        // Value bindings become visible only after their declaration executes.
        let mut scope = hoisted_callable_scope(body);
        scope.extend(extra_bindings);
        scopes.push(scope);
        for node in body {
            self.walk_node(node, &scopes, inside_nested_callable, &owner);
            extend_scope_with_value_declaration(
                scopes.last_mut().expect("body scope"),
                node,
                &owner,
            );
        }
    }

    fn walk_node(
        &mut self,
        node: &SNode,
        scopes: &[Scope],
        inside_nested_callable: bool,
        owner: &BindingOwner,
    ) {
        match &node.node {
            Node::Identifier(name) => self.record_reference(name, scopes, inside_nested_callable),
            Node::Assignment { target, .. } => {
                if inside_nested_callable {
                    if let Node::Identifier(name) = &target.node {
                        self.record_reassignment(name, scopes);
                    }
                }
                self.walk_children(node, scopes, inside_nested_callable, owner);
            }
            Node::Closure { params, body, .. }
            | Node::FnDecl { params, body, .. }
            | Node::ToolDecl { params, body, .. } => {
                // Defaults resolve before their parameter is bound. They still
                // belong to the nested callable and can capture an outer local.
                for param in params {
                    if let Some(default) = &param.default_value {
                        self.walk_node(default, scopes, true, owner);
                    }
                }
                self.walk_callable_body(body, params, scopes);
            }
            Node::Pipeline { params, body, .. } | Node::OverrideDecl { params, body, .. } => {
                let bindings = names_scope(params.iter().cloned());
                self.walk_body_with_bindings(
                    body,
                    scopes.to_vec(),
                    true,
                    BindingOwner::Nested,
                    bindings,
                );
            }
            Node::SpawnExpr { body } => {
                self.walk_body(body, scopes.to_vec(), true, BindingOwner::Nested);
            }
            Node::Parallel {
                expr,
                variable,
                body,
                options,
                ..
            } => {
                self.walk_node(expr, scopes, inside_nested_callable, owner);
                for (_, option) in options {
                    self.walk_node(option, scopes, inside_nested_callable, owner);
                }
                let bindings = variable.iter().cloned().collect::<Vec<_>>();
                self.walk_body_with_bindings(
                    body,
                    scopes.to_vec(),
                    true,
                    BindingOwner::Nested,
                    names_scope(bindings),
                );
            }
            Node::ForIn {
                pattern,
                iterable,
                body,
            } => {
                self.walk_pattern_defaults(pattern, scopes, inside_nested_callable, owner);
                self.walk_node(iterable, scopes, inside_nested_callable, owner);
                self.walk_body_with_bindings(
                    body,
                    scopes.to_vec(),
                    inside_nested_callable,
                    owner.clone(),
                    pattern_scope(pattern, node.span, owner),
                );
            }
            Node::IfElse {
                condition,
                then_body,
                else_body,
            } => {
                self.walk_node(condition, scopes, inside_nested_callable, owner);
                self.walk_body(
                    then_body,
                    scopes.to_vec(),
                    inside_nested_callable,
                    owner.clone(),
                );
                if let Some(else_body) = else_body {
                    self.walk_body(
                        else_body,
                        scopes.to_vec(),
                        inside_nested_callable,
                        owner.clone(),
                    );
                }
            }
            Node::MatchExpr { value, arms } => {
                self.walk_node(value, scopes, inside_nested_callable, owner);
                for arm in arms {
                    self.walk_node(&arm.pattern, scopes, inside_nested_callable, owner);
                    if let Some(guard) = &arm.guard {
                        self.walk_node(guard, scopes, inside_nested_callable, owner);
                    }
                    self.walk_body(
                        &arm.body,
                        scopes.to_vec(),
                        inside_nested_callable,
                        owner.clone(),
                    );
                }
            }
            Node::WhileLoop { condition, body } => {
                self.walk_node(condition, scopes, inside_nested_callable, owner);
                self.walk_body(body, scopes.to_vec(), inside_nested_callable, owner.clone());
            }
            Node::Retry { count, body } => {
                self.walk_node(count, scopes, inside_nested_callable, owner);
                self.walk_body(body, scopes.to_vec(), inside_nested_callable, owner.clone());
            }
            Node::CostRoute { options, body } => {
                for (_, option) in options {
                    self.walk_node(option, scopes, inside_nested_callable, owner);
                }
                self.walk_body(body, scopes.to_vec(), inside_nested_callable, owner.clone());
            }
            Node::TryCatch {
                body,
                error_var,
                catch_body,
                finally_body,
                ..
            } => {
                self.walk_body(body, scopes.to_vec(), inside_nested_callable, owner.clone());
                let catch_binding = names_scope(error_var.iter().cloned());
                self.walk_body_with_bindings(
                    catch_body,
                    scopes.to_vec(),
                    inside_nested_callable,
                    owner.clone(),
                    catch_binding,
                );
                if let Some(finally_body) = finally_body {
                    self.walk_body(
                        finally_body,
                        scopes.to_vec(),
                        inside_nested_callable,
                        owner.clone(),
                    );
                }
            }
            Node::TryExpr { body }
            | Node::ScopeBlock { body }
            | Node::DeferStmt { body }
            | Node::Block(body) => {
                self.walk_body(body, scopes.to_vec(), inside_nested_callable, owner.clone());
            }
            Node::GuardStmt {
                condition,
                else_body,
            } => {
                self.walk_node(condition, scopes, inside_nested_callable, owner);
                self.walk_body(
                    else_body,
                    scopes.to_vec(),
                    inside_nested_callable,
                    owner.clone(),
                );
            }
            Node::DeadlineBlock { duration, body } => {
                self.walk_node(duration, scopes, inside_nested_callable, owner);
                self.walk_body(body, scopes.to_vec(), inside_nested_callable, owner.clone());
            }
            Node::MutexBlock { key, body } => {
                if let Some(key) = key {
                    self.walk_node(key, scopes, inside_nested_callable, owner);
                }
                self.walk_body(body, scopes.to_vec(), inside_nested_callable, owner.clone());
            }
            Node::SelectExpr {
                cases,
                timeout,
                default_body,
            } => {
                for case in cases {
                    self.walk_node(&case.channel, scopes, inside_nested_callable, owner);
                    self.walk_body_with_bindings(
                        &case.body,
                        scopes.to_vec(),
                        inside_nested_callable,
                        owner.clone(),
                        names_scope([case.variable.clone()]),
                    );
                }
                if let Some((duration, body)) = timeout {
                    self.walk_node(duration, scopes, inside_nested_callable, owner);
                    self.walk_body(body, scopes.to_vec(), inside_nested_callable, owner.clone());
                }
                if let Some(body) = default_body {
                    self.walk_body(body, scopes.to_vec(), inside_nested_callable, owner.clone());
                }
            }
            Node::EvalPackDecl {
                fields,
                body,
                summarize,
                ..
            } => {
                for (_, value) in fields {
                    self.walk_node(value, scopes, inside_nested_callable, owner);
                }
                self.walk_body(body, scopes.to_vec(), inside_nested_callable, owner.clone());
                if let Some(summary) = summarize {
                    self.walk_body(
                        summary,
                        scopes.to_vec(),
                        inside_nested_callable,
                        owner.clone(),
                    );
                }
            }
            _ => self.walk_children(node, scopes, inside_nested_callable, owner),
        }
    }

    fn walk_callable_body(&mut self, body: &[SNode], params: &[TypedParam], scopes: &[Scope]) {
        self.walk_body_with_bindings(
            body,
            scopes.to_vec(),
            true,
            BindingOwner::Nested,
            names_scope(params.iter().map(|param| param.name.clone())),
        );
    }

    fn walk_pattern_defaults(
        &mut self,
        pattern: &BindingPattern,
        scopes: &[Scope],
        inside_nested_callable: bool,
        owner: &BindingOwner,
    ) {
        match pattern {
            BindingPattern::Dict(fields) => {
                for field in fields {
                    if let Some(default) = &field.default_value {
                        self.walk_node(default, scopes, inside_nested_callable, owner);
                    }
                }
            }
            BindingPattern::List(elements) => {
                for element in elements {
                    if let Some(default) = &element.default_value {
                        self.walk_node(default, scopes, inside_nested_callable, owner);
                    }
                }
            }
            BindingPattern::Identifier(_) | BindingPattern::Pair(_, _) => {}
        }
    }

    fn walk_children(
        &mut self,
        node: &SNode,
        scopes: &[Scope],
        inside_nested_callable: bool,
        owner: &BindingOwner,
    ) {
        for child in crate::visit::immediate_children(node) {
            self.walk_node(child, scopes, inside_nested_callable, owner);
        }
    }

    fn record_reference(&mut self, name: &str, scopes: &[Scope], inside_nested_callable: bool) {
        if !inside_nested_callable {
            return;
        }
        if let Some(ScopeBinding::Current(binding)) = resolve(scopes, name) {
            self.captured.insert(binding.clone());
        }
    }

    fn record_reassignment(&mut self, name: &str, scopes: &[Scope]) {
        match resolve(scopes, name) {
            Some(ScopeBinding::Nested) => {}
            Some(ScopeBinding::Current(binding)) => {
                self.reassigned.insert(binding.name.clone());
            }
            None => {
                self.reassigned.insert(name.to_string());
            }
        }
    }
}

fn hoisted_callable_scope(body: &[SNode]) -> Scope {
    let mut scope = Scope::new();
    for node in body {
        match &node.node {
            Node::FnDecl { name, .. }
            | Node::ToolDecl { name, .. }
            | Node::Pipeline { name, .. }
            | Node::OverrideDecl { name, .. } => {
                scope.insert(name.clone(), ScopeBinding::Nested);
            }
            Node::AttributedDecl { inner, .. } => {
                if let Node::FnDecl { name, .. }
                | Node::ToolDecl { name, .. }
                | Node::Pipeline { name, .. }
                | Node::OverrideDecl { name, .. } = &inner.node
                {
                    scope.insert(name.clone(), ScopeBinding::Nested);
                }
            }
            _ => {}
        }
    }
    scope
}

fn extend_scope_with_value_declaration(scope: &mut Scope, node: &SNode, owner: &BindingOwner) {
    let (Node::LetBinding { pattern, .. } | Node::ConstBinding { pattern, .. }) = &node.node else {
        return;
    };
    for binding in binding_pattern_ids(pattern, node.span) {
        let name = binding.name.clone();
        let entry = match owner {
            BindingOwner::Current => ScopeBinding::Current(binding),
            BindingOwner::Nested => ScopeBinding::Nested,
        };
        scope.insert(name, entry);
    }
}

fn pattern_scope(pattern: &BindingPattern, declaration: Span, owner: &BindingOwner) -> Scope {
    let mut scope = Scope::new();
    for binding in binding_pattern_ids(pattern, declaration) {
        let name = binding.name.clone();
        let entry = match owner {
            BindingOwner::Current => ScopeBinding::Current(binding),
            BindingOwner::Nested => ScopeBinding::Nested,
        };
        scope.insert(name, entry);
    }
    scope
}

fn names_scope(names: impl IntoIterator<Item = String>) -> Scope {
    names
        .into_iter()
        .filter(|name| !is_discard_name(name))
        .map(|name| (name, ScopeBinding::Nested))
        .collect()
}

fn resolve<'a>(scopes: &'a [Scope], name: &str) -> Option<&'a ScopeBinding> {
    scopes.iter().rev().find_map(|scope| scope.get(name))
}

#[cfg(test)]
mod tests {
    use harn_lexer::Span;

    use crate::ast::SelectCase;

    use super::*;

    fn node(offset: usize, node: Node) -> SNode {
        SNode::new(node, Span::with_offsets(offset, offset + 1, 1, offset + 1))
    }

    fn identifier(offset: usize, name: &str) -> SNode {
        node(offset, Node::Identifier(name.to_string()))
    }

    fn let_binding(offset: usize, name: &str) -> SNode {
        node(
            offset,
            Node::LetBinding {
                pattern: BindingPattern::Identifier(name.to_string()),
                type_ann: None,
                value: Box::new(identifier(offset + 100, "value")),
                is_pub: false,
            },
        )
    }

    fn closure(offset: usize, params: Vec<TypedParam>, body: Vec<SNode>) -> SNode {
        node(
            offset,
            Node::Closure {
                params,
                return_type: None,
                throws: None,
                body,
                fn_syntax: false,
            },
        )
    }

    #[test]
    fn parameter_shadow_does_not_capture_outer_binding() {
        let outer = let_binding(10, "pin");
        let body = vec![
            outer.clone(),
            closure(
                20,
                vec![TypedParam::untyped("pin")],
                vec![identifier(21, "pin")],
            ),
        ];

        assert!(!captured_bindings_in_nested_callables(&body)
            .contains(&BindingId::from_declaration("pin", outer.span)));
    }

    #[test]
    fn block_shadow_captures_exact_inner_binding() {
        let outer = let_binding(10, "pin");
        let inner = let_binding(20, "pin");
        let body = vec![
            outer.clone(),
            node(
                19,
                Node::Block(vec![
                    inner.clone(),
                    closure(30, Vec::new(), vec![identifier(31, "pin")]),
                ]),
            ),
        ];

        let captured = captured_bindings_in_nested_callables(&body);
        assert!(captured.contains(&BindingId::from_declaration("pin", inner.span)));
        assert!(!captured.contains(&BindingId::from_declaration("pin", outer.span)));
    }

    #[test]
    fn later_block_binding_does_not_shadow_an_earlier_reference() {
        let outer = let_binding(10, "pin");
        let inner = let_binding(30, "pin");
        let body = vec![
            outer.clone(),
            node(
                19,
                Node::Block(vec![
                    closure(20, Vec::new(), vec![identifier(21, "pin")]),
                    inner.clone(),
                ]),
            ),
        ];

        let captured = captured_bindings_in_nested_callables(&body);
        assert!(captured.contains(&BindingId::from_declaration("pin", outer.span)));
        assert!(!captured.contains(&BindingId::from_declaration("pin", inner.span)));
    }

    #[test]
    fn loop_binding_shadows_outer_capture() {
        let outer = let_binding(10, "pin");
        let loop_node = node(
            20,
            Node::ForIn {
                pattern: BindingPattern::Identifier("pin".to_string()),
                iterable: Box::new(identifier(21, "pins")),
                body: vec![closure(22, Vec::new(), vec![identifier(23, "pin")])],
            },
        );
        let captured = captured_bindings_in_nested_callables(&[outer.clone(), loop_node.clone()]);

        assert!(captured.contains(&BindingId::from_declaration("pin", loop_node.span)));
        assert!(!captured.contains(&BindingId::from_declaration("pin", outer.span)));
    }

    #[test]
    fn catch_and_select_bindings_shadow_outer_capture() {
        let outer = let_binding(10, "pin");
        let try_catch = node(
            20,
            Node::TryCatch {
                body: Vec::new(),
                has_catch: true,
                error_var: Some("pin".to_string()),
                error_type: None,
                catch_body: vec![closure(21, Vec::new(), vec![identifier(22, "pin")])],
                finally_body: None,
            },
        );
        let select = node(
            30,
            Node::SelectExpr {
                cases: vec![SelectCase {
                    variable: "pin".to_string(),
                    channel: Box::new(identifier(31, "channel")),
                    body: vec![closure(32, Vec::new(), vec![identifier(33, "pin")])],
                }],
                timeout: None,
                default_body: None,
            },
        );

        let captured = captured_bindings_in_nested_callables(&[outer.clone(), try_catch, select]);
        assert!(!captured.contains(&BindingId::from_declaration("pin", outer.span)));
    }

    #[test]
    fn nested_callable_capture_is_transitive() {
        let outer = let_binding(10, "pin");
        let nested = closure(
            20,
            Vec::new(),
            vec![closure(30, Vec::new(), vec![identifier(31, "pin")])],
        );
        let captured = captured_bindings_in_nested_callables(&[outer.clone(), nested]);

        assert!(captured.contains(&BindingId::from_declaration("pin", outer.span)));
    }

    #[test]
    fn nested_reassignment_ignores_shadowed_parameter() {
        let body = vec![closure(
            10,
            vec![TypedParam::untyped("pin")],
            vec![node(
                11,
                Node::Assignment {
                    target: Box::new(identifier(12, "pin")),
                    value: Box::new(identifier(13, "next")),
                    op: None,
                },
            )],
        )];

        assert!(nested_callable_reassigned_names(&body).is_empty());
    }
}
