//! Top-level driver: declaration pre-passes and the program walk.
//!
//! `check_inner` is the canonical entry point — every public `check*`
//! method on `TypeChecker` funnels through here. The two `register_*`
//! helpers run before the main walk so forward references and
//! declaration order don't trip the strict cross-module undefined-name
//! check.

use crate::ast::*;

use super::super::scope::{
    EnumDeclInfo, FnSignature, ImplMethodSig, InterfaceDeclInfo, StructDeclInfo, TypeAliasInfo,
    TypeScope,
};
use super::super::{InlayHintInfo, TypeChecker, TypeDiagnostic};

impl TypeChecker {
    pub(in crate::typechecker) fn check_inner(
        mut self,
        program: &[SNode],
    ) -> (Vec<TypeDiagnostic>, Vec<InlayHintInfo>) {
        Self::register_declarations_into(&mut self.scope, &self.imported_type_decls);
        // First pass: collect declarations (type/enum/struct/interface) into scope
        // before type-checking bodies so forward references resolve.
        Self::register_declarations_into(&mut self.scope, program);
        for snode in program {
            if let Node::Pipeline { body, .. } = &snode.node {
                Self::register_declarations_into(&mut self.scope, body);
            }
        }
        // Pre-register every top-level `fn`/`pipeline`/`tool` name so a
        // caller earlier in the file can reference a callable defined
        // later without the strict cross-module check falsely flagging
        // it as undefined. Signatures populated here are overwritten
        // when the body is actually walked; this pass only needs the
        // name to exist so `check_call`'s resolvability check passes.
        Self::register_callable_placeholders(&mut self.scope, program);

        // Pre-pass: index `@deprecated` attributes on top-level fn decls so
        // `check_call` (and the standalone deprecation visitor below) can
        // flag callers anywhere in the program.
        for snode in program {
            if let Node::AttributedDecl { attributes, inner } = &snode.node {
                if let Node::FnDecl { name, .. } = &inner.node {
                    for attr in attributes {
                        if attr.name == "deprecated" {
                            let since = attr.string_arg("since");
                            let use_hint = attr.string_arg("use");
                            self.deprecated_fns.insert(name.clone(), (since, use_hint));
                        }
                    }
                }
            }
        }

        // Walk every node looking for FunctionCalls of deprecated names.
        // This catches calls in contexts (e.g. `let x = old_fn()`) where
        // `check_node`'s FunctionCall arm doesn't fire because the value
        // is inferred rather than checked.
        if !self.deprecated_fns.is_empty() {
            for snode in program {
                self.visit_for_deprecation(snode);
            }
        }

        for snode in program {
            // Transparently process attributed wrappers around top-level
            // declarations. Attribute-specific semantics (deprecation,
            // unknown-attribute warnings) are applied before unwrapping.
            if let Node::AttributedDecl { attributes, inner } = &snode.node {
                self.check_attributes(attributes, inner);
            }
            let inner_node = match &snode.node {
                Node::AttributedDecl { inner, .. } => inner.as_ref(),
                _ => snode,
            };
            match &inner_node.node {
                Node::Pipeline {
                    params,
                    return_type,
                    body,
                    ..
                } => {
                    let mut child = self.scope.child();
                    for p in params {
                        child.define_var(p, None);
                        child.clear_nil_widenable(p);
                    }
                    self.fn_depth += 1;
                    let ret_scope_base = return_type.as_ref().map(|_| child.child());
                    self.check_block(body, &mut child);
                    if let (Some(ret_type), Some(mut ret_scope)) =
                        (return_type.as_ref(), ret_scope_base)
                    {
                        for stmt in body {
                            self.check_return_type(stmt, ret_type, &mut ret_scope);
                        }
                    }
                    self.fn_depth -= 1;
                }
                Node::FnDecl {
                    name,
                    type_params,
                    params,
                    return_type,
                    where_clauses,
                    body,
                    is_stream,
                    ..
                } => {
                    let return_type = Self::callable_return_type(*is_stream, return_type, body);
                    let required_params =
                        params.iter().filter(|p| p.default_value.is_none()).count();
                    let sig = FnSignature {
                        params: params
                            .iter()
                            .map(|p| (p.name.clone(), p.type_expr.clone()))
                            .collect(),
                        return_type: return_type.clone(),
                        type_param_names: type_params.iter().map(|tp| tp.name.clone()).collect(),
                        required_params,
                        where_clauses: where_clauses
                            .iter()
                            .map(|wc| (wc.type_name.clone(), wc.bound.clone()))
                            .collect(),
                        has_rest: params.last().is_some_and(|p| p.rest),
                    };
                    self.scope.define_fn(name, sig);
                    self.check_fn_body(
                        type_params,
                        params,
                        &return_type,
                        body,
                        where_clauses,
                        *is_stream,
                    );
                }
                _ => {
                    let mut scope = self.scope.clone();
                    self.check_node(snode, &mut scope);
                    // Promote top-level definitions out of the temporary scope.
                    for (name, ty) in scope.vars {
                        self.scope.vars.entry(name).or_insert(ty);
                    }
                    for name in scope.mutable_vars {
                        self.scope.mutable_vars.insert(name);
                    }
                    for (name, enabled) in scope.nil_widenable_vars {
                        self.scope.nil_widenable_vars.insert(name, enabled);
                    }
                }
            }
        }

        (self.diagnostics, self.hints)
    }

    /// Pre-populate placeholder signatures for every
    /// `fn`/`pipeline`/`tool`/`let`/`var` name reachable from the
    /// program (including names defined inside pipeline or fn bodies)
    /// so the strict cross-module undefined-call check can resolve
    /// forward references and recursive calls whose own scope does not
    /// inherit from the enclosing block.
    ///
    /// Rust's lexical scoping guarantees the runtime lookup will still
    /// respect shadowing at execution time; the placeholders only
    /// satisfy the *static* "does this name exist somewhere" check.
    fn register_callable_placeholders(scope: &mut TypeScope, nodes: &[SNode]) {
        fn walk(scope: &mut TypeScope, node: &SNode) {
            let inner = match &node.node {
                Node::AttributedDecl { inner, .. } => inner.as_ref(),
                _ => node,
            };
            match &inner.node {
                Node::FnDecl {
                    name,
                    params,
                    return_type,
                    type_params,
                    where_clauses,
                    body,
                    is_stream,
                    ..
                } => {
                    let return_type =
                        TypeChecker::callable_return_type(*is_stream, return_type, body);
                    let sig = FnSignature {
                        params: params
                            .iter()
                            .map(|p| (p.name.clone(), p.type_expr.clone()))
                            .collect(),
                        return_type,
                        type_param_names: type_params.iter().map(|tp| tp.name.clone()).collect(),
                        required_params: params
                            .iter()
                            .filter(|p| p.default_value.is_none())
                            .count(),
                        where_clauses: where_clauses
                            .iter()
                            .map(|wc| (wc.type_name.clone(), wc.bound.clone()))
                            .collect(),
                        has_rest: params.last().is_some_and(|p| p.rest),
                    };
                    scope.define_fn(name, sig);
                    walk_all(scope, body);
                }
                Node::Pipeline { name, body, .. } => {
                    let sig = FnSignature {
                        params: Vec::new(),
                        return_type: None,
                        type_param_names: Vec::new(),
                        required_params: 0,
                        where_clauses: Vec::new(),
                        has_rest: false,
                    };
                    scope.define_fn(name, sig);
                    walk_all(scope, body);
                }
                Node::ToolDecl { name, body, .. } => {
                    let sig = FnSignature {
                        params: Vec::new(),
                        return_type: None,
                        type_param_names: Vec::new(),
                        required_params: 0,
                        where_clauses: Vec::new(),
                        has_rest: false,
                    };
                    scope.define_fn(name, sig);
                    walk_all(scope, body);
                }
                Node::SkillDecl { name, .. } => {
                    scope.define_var(name, None);
                    scope.clear_nil_widenable(name);
                }
                Node::LetBinding { pattern, .. } | Node::VarBinding { pattern, .. } => {
                    // Only bare-identifier patterns at module scope
                    // need forward-ref placeholders; destructuring
                    // patterns are checked as statements and define
                    // their vars as they are walked.
                    if let BindingPattern::Identifier(name) = pattern {
                        if !crate::ast::is_discard_name(name) {
                            scope.define_var(name, None);
                            scope.clear_nil_widenable(name);
                        }
                    }
                }
                _ => {}
            }
        }
        fn walk_all(scope: &mut TypeScope, nodes: &[SNode]) {
            for node in nodes {
                walk(scope, node);
            }
        }
        walk_all(scope, nodes);
    }

    /// Register type, enum, interface, and struct declarations from AST nodes into a scope.
    fn register_declarations_into(scope: &mut TypeScope, nodes: &[SNode]) {
        for snode in nodes {
            match &snode.node {
                Node::TypeDecl {
                    name,
                    type_params,
                    type_expr,
                } => {
                    scope.type_aliases.insert(
                        name.clone(),
                        TypeAliasInfo {
                            type_params: type_params.clone(),
                            body: type_expr.clone(),
                        },
                    );
                }
                Node::EnumDecl {
                    name,
                    type_params,
                    variants,
                    ..
                } => {
                    scope.enums.insert(
                        name.clone(),
                        EnumDeclInfo {
                            type_params: type_params.clone(),
                            variants: variants.clone(),
                        },
                    );
                }
                Node::InterfaceDecl {
                    name,
                    type_params,
                    associated_types,
                    methods,
                } => {
                    scope.interfaces.insert(
                        name.clone(),
                        InterfaceDeclInfo {
                            type_params: type_params.clone(),
                            associated_types: associated_types.clone(),
                            methods: methods.clone(),
                        },
                    );
                }
                Node::StructDecl {
                    name,
                    type_params,
                    fields,
                    ..
                } => {
                    scope.structs.insert(
                        name.clone(),
                        StructDeclInfo {
                            type_params: type_params.clone(),
                            fields: fields.clone(),
                        },
                    );
                }
                Node::ImplBlock {
                    type_name, methods, ..
                } => {
                    let sigs: Vec<ImplMethodSig> = methods
                        .iter()
                        .filter_map(|m| {
                            if let Node::FnDecl {
                                name,
                                params,
                                return_type,
                                ..
                            } = &m.node
                            {
                                let non_self: Vec<_> =
                                    params.iter().filter(|p| p.name != "self").collect();
                                let param_count = non_self.len();
                                let param_types: Vec<Option<TypeExpr>> =
                                    non_self.iter().map(|p| p.type_expr.clone()).collect();
                                Some(ImplMethodSig {
                                    name: name.clone(),
                                    param_count,
                                    param_types,
                                    return_type: return_type.clone(),
                                })
                            } else {
                                None
                            }
                        })
                        .collect();
                    scope.impl_methods.insert(type_name.clone(), sigs);
                }
                _ => {}
            }
        }
    }
}
