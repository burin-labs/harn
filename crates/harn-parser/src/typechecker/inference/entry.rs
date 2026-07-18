//! Top-level driver: declaration pre-passes and the program walk.
//!
//! `check_inner` is the canonical entry point — every public `check*`
//! method on `TypeChecker` funnels through here. The two `register_*`
//! helpers run before the main walk so forward references and
//! declaration order don't trip the strict cross-module undefined-name
//! check.

use std::rc::Rc;

use crate::ast::*;
use crate::diagnostic_codes::Code;

use super::super::scope::{
    EnumDeclInfo, ImplMethodSig, InterfaceDeclInfo, StructDeclInfo, TypeAliasInfo, TypeScope,
};
use super::super::{InlayHintInfo, TypeChecker, TypeDiagnostic};
use super::decls::CallableDeclarationContext;

impl TypeChecker {
    pub(in crate::typechecker) fn check_inner(
        mut self,
        program: &[SNode],
    ) -> (Vec<TypeDiagnostic>, Vec<InlayHintInfo>) {
        // Pre-pass mutations: nobody else holds an `Rc` to `self.scope`
        // yet, so `Rc::make_mut` resolves to a direct `&mut TypeScope`
        // without copying. Once body children start to share the root
        // (via `child_of(&self.scope)`), the refcount climbs above 1 and
        // any further `make_mut` would copy — but at that point we don't
        // mutate `self.scope` directly, only its children.
        let scope_mut = Rc::make_mut(&mut self.scope);
        Self::register_declarations_into(scope_mut, &self.imported_type_decls);
        Self::register_imported_callable_signatures_into(scope_mut, &self.imported_callable_decls);
        // First pass: collect declarations (type/enum/struct/interface) into scope
        // before type-checking bodies so forward references resolve.
        for nodes in crate::lexical::module_scope_node_slices(program) {
            Self::register_declarations_into(scope_mut, nodes);
        }
        // Pre-register every top-level `fn`/`pipeline`/`tool` name so a
        // caller earlier in the file can reference a callable defined
        // later without the strict cross-module check falsely flagging
        // it as undefined. Signatures populated here are overwritten
        // when the body is actually walked; this pass only needs the
        // name to exist so `check_call`'s resolvability check passes.
        Self::register_callable_placeholders(scope_mut, program);

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
                    throws,
                    body,
                    ..
                } => {
                    let root_scope = Rc::clone(&self.scope);
                    let mut child = TypeScope::child_of(&self.scope);
                    for p in params {
                        child.define_var(p, None);
                        child.clear_nil_widenable(p);
                    }
                    self.fn_depth += 1;
                    Self::mark_closure_mutated_captures(&mut child, body);
                    self.expected_return_types.push(return_type.clone());
                    self.check_block_with_expected_tail(body, return_type.as_ref(), &mut child);
                    self.expected_return_types.pop();
                    if let Some(declared) = throws {
                        self.check_declared_throws_untyped_params(
                            declared,
                            params,
                            body,
                            inner_node.span,
                            root_scope.as_ref(),
                        );
                    }
                    if let Some(ret_type) = return_type.as_ref() {
                        let mut ret_scope = child.clone();
                        ret_scope.restore_narrowed_vars();
                        for stmt in body {
                            self.check_return_type(stmt, ret_type, inner_node.span, &mut ret_scope);
                        }
                        if !Self::block_definitely_exits(body) {
                            let actual = self
                                .infer_block_type(body, &ret_scope)
                                .unwrap_or_else(|| TypeExpr::Named("nil".into()));
                            if !self.types_compatible(ret_type, &actual, &ret_scope) {
                                let value_span =
                                    body.last().map(|stmt| stmt.span).unwrap_or(inner_node.span);
                                self.type_mismatch_at(
                                    Code::ReturnTypeMismatch,
                                    "pipeline result",
                                    ret_type,
                                    &actual,
                                    value_span,
                                    (
                                        Some((
                                            inner_node.span,
                                            "pipeline return type declared here".to_string(),
                                        )),
                                        Some(value_span),
                                    ),
                                    &ret_scope,
                                );
                            }
                        }
                    }
                    self.fn_depth -= 1;
                }
                Node::FnDecl {
                    name,
                    type_params,
                    params,
                    return_type,
                    throws,
                    where_clauses,
                    body,
                    is_stream,
                    ..
                } => {
                    let inference_scope = Rc::clone(&self.scope);
                    // `declared` is the user-written/`gen`-implied return type
                    // (`None` when unannotated). The signature additionally
                    // carries an *inferred* return type so callers recover a
                    // precise type, but body-checking uses only `declared` —
                    // an inferred type must never drive the
                    // declared-return diagnostics (fall-through / mismatch),
                    // since it was derived from the body and so cannot
                    // contradict it.
                    let declared = Self::callable_return_type(*is_stream, return_type, body);
                    let sig = Self::fn_signature_from_decl(
                        inner_node,
                        Some(snode.span),
                        |params, body| {
                            self.infer_unannotated_fn_return(params, body, inference_scope.as_ref())
                        },
                    )
                    .expect("matched FnDecl");
                    Rc::make_mut(&mut self.scope).define_fn(name, sig);
                    let body_scope = Rc::clone(&self.scope);
                    if name == "main" {
                        self.check_main_signature(params, snode.span);
                    }
                    self.check_fn_body(
                        type_params,
                        params,
                        &declared,
                        body,
                        where_clauses,
                        *is_stream,
                        CallableDeclarationContext {
                            span: snode.span,
                            scope: body_scope.as_ref(),
                        },
                    );
                    if let Some(declared_throws) = throws {
                        self.check_declared_throws(
                            declared_throws,
                            params,
                            body,
                            snode.span,
                            body_scope.as_ref(),
                        );
                    }
                }
                _ => {
                    // Top-level statements that aren't fn/pipeline decls
                    // (e.g. bare `let` bindings). Type-check in a child
                    // scope, then promote newly defined names back onto
                    // the root so subsequent top-level nodes see them.
                    //
                    // Destructure to move the maps out and drop the
                    // child's `Rc` reference to the parent before calling
                    // `Rc::make_mut`; otherwise the live child clone would
                    // force `make_mut` to copy the root scope.
                    let mut scope = TypeScope::child_of(&self.scope);
                    self.check_node(snode, &mut scope);
                    let TypeScope {
                        vars,
                        mutable_vars,
                        nil_widenable_vars,
                        schema_bindings,
                        untyped_sources,
                        annotated_vars,
                        ..
                    } = scope;
                    let root = Rc::make_mut(&mut self.scope);
                    for (name, ty) in vars {
                        root.vars.insert(name, ty);
                    }
                    root.mutable_vars.extend(mutable_vars);
                    root.nil_widenable_vars.extend(nil_widenable_vars);
                    root.schema_bindings.extend(schema_bindings);
                    root.untyped_sources.extend(untyped_sources);
                    root.annotated_vars.extend(annotated_vars);
                }
            }
        }

        (self.diagnostics, self.hints)
    }

    /// Validate the entrypoint convention: a top-level `fn main` must take
    /// a single typed parameter `harness: Harness` (no defaults, no rest,
    /// no extras). Anything else fires `HARN-NAM-101`.
    pub(in crate::typechecker) fn check_main_signature(
        &mut self,
        params: &[TypedParam],
        span: harn_lexer::Span,
    ) {
        let signature_ok = matches!(
            params,
            [TypedParam {
                name,
                type_expr: Some(TypeExpr::Named(ty)),
                default_value: None,
                rest: false,
            }] if matches!(name.as_str(), "harness" | "_harness") && ty == "Harness"
        );
        if signature_ok {
            return;
        }
        let message = if params.is_empty() {
            "`main` must take a single `harness: Harness` parameter".to_string()
        } else if params.len() == 1 {
            let p = &params[0];
            match (&p.name, &p.type_expr) {
                (name, _) if !matches!(name.as_str(), "harness" | "_harness") => {
                    format!("`main` parameter is named `{name}`, expected `harness` or `_harness`")
                }
                (_, None) => {
                    "`main(harness: Harness)` requires an explicit `Harness` type annotation"
                        .to_string()
                }
                (_, Some(actual)) => format!(
                    "`main(harness: …)` parameter type must be `Harness`, found `{}`",
                    crate::typechecker::format_type(actual)
                ),
            }
        } else {
            format!(
                "`main` must take exactly one parameter (`harness: Harness`), found {}",
                params.len()
            )
        };
        self.error_at_with_help(
            Code::InvalidMainSignature,
            message,
            span,
            "rewrite as `fn main(harness: Harness) { ... }` — the runtime threads its \
             capability handle through this single parameter"
                .to_string(),
        );
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
                Node::FnDecl { name, body, .. } => {
                    let sig =
                        TypeChecker::fn_signature_from_decl(inner, Some(inner.span), |_, _| None)
                            .expect("matched FnDecl");
                    scope.define_fn(name, sig);
                    walk_all(scope, body);
                }
                Node::Pipeline { name, body, .. } => {
                    let sig = TypeChecker::empty_callable_signature(Some(inner.span));
                    scope.define_fn(name, sig);
                    walk_all(scope, body);
                }
                Node::ToolDecl { name, body, .. } => {
                    let sig = TypeChecker::empty_callable_signature(Some(inner.span));
                    scope.define_fn(name, sig);
                    walk_all(scope, body);
                }
                Node::SkillDecl { name, .. } => {
                    scope.define_var(name, None);
                    scope.clear_nil_widenable(name);
                }
                Node::EvalPackDecl {
                    binding_name,
                    body,
                    summarize,
                    ..
                } => {
                    scope.define_var(binding_name, Some(TypeExpr::Named("dict".into())));
                    scope.clear_nil_widenable(binding_name);
                    walk_all(scope, body);
                    if let Some(summary_body) = summarize {
                        walk_all(scope, summary_body);
                    }
                }
                Node::LetBinding { pattern, .. } | Node::ConstBinding { pattern, .. } => {
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

    fn register_imported_callable_signatures_into(scope: &mut TypeScope, nodes: &[SNode]) {
        for snode in nodes {
            let inner = match &snode.node {
                Node::AttributedDecl { inner, .. } => inner.as_ref(),
                _ => snode,
            };
            match &inner.node {
                Node::FnDecl { name, .. } => {
                    let sig = TypeChecker::fn_signature_from_decl(inner, None, |_, _| None)
                        .expect("matched FnDecl");
                    scope.define_fn(name, sig);
                }
                Node::Pipeline { name, .. } | Node::ToolDecl { name, .. } => {
                    let sig = TypeChecker::empty_callable_signature(None);
                    scope.define_fn(name, sig);
                }
                _ => {}
            }
        }
    }

    /// Register type, enum, interface, and struct declarations from AST nodes into a scope.
    fn register_declarations_into(scope: &mut TypeScope, nodes: &[SNode]) {
        for snode in nodes {
            let (_, decl) = peel_attributes(snode);
            match &decl.node {
                Node::TypeDecl {
                    name,
                    type_params,
                    type_expr,
                    ..
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
                            associated_types: AssociatedType::bindings(associated_types),
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
