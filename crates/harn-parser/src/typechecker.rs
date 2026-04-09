use std::collections::BTreeMap;

use crate::ast::*;
use crate::builtin_signatures;
use harn_lexer::{FixEdit, Span};

/// An inlay hint produced during type checking.
#[derive(Debug, Clone)]
pub struct InlayHintInfo {
    /// Position (line, column) where the hint should be displayed (after the variable name).
    pub line: usize,
    pub column: usize,
    /// The type label to display (e.g. ": string").
    pub label: String,
}

/// A diagnostic produced by the type checker.
#[derive(Debug, Clone)]
pub struct TypeDiagnostic {
    pub message: String,
    pub severity: DiagnosticSeverity,
    pub span: Option<Span>,
    pub help: Option<String>,
    /// Machine-applicable fix edits.
    pub fix: Option<Vec<FixEdit>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticSeverity {
    Error,
    Warning,
}

/// Inferred type of an expression. None means unknown/untyped (gradual typing).
type InferredType = Option<TypeExpr>;

/// Scope for tracking variable types.
#[derive(Debug, Clone)]
struct TypeScope {
    /// Variable name → inferred type.
    vars: BTreeMap<String, InferredType>,
    /// Function name → (param types, return type).
    functions: BTreeMap<String, FnSignature>,
    /// Named type aliases.
    type_aliases: BTreeMap<String, TypeExpr>,
    /// Enum declarations: name → variant names.
    enums: BTreeMap<String, Vec<String>>,
    /// Interface declarations: name → method signatures.
    interfaces: BTreeMap<String, Vec<InterfaceMethod>>,
    /// Struct declarations: name → field types.
    structs: BTreeMap<String, Vec<(String, InferredType)>>,
    /// Impl block methods: type_name → method signatures.
    impl_methods: BTreeMap<String, Vec<ImplMethodSig>>,
    /// Generic type parameter names in scope (treated as compatible with any type).
    generic_type_params: std::collections::BTreeSet<String>,
    /// Where-clause constraints: type_param → interface_bound.
    /// Used for definition-site checking of generic function bodies.
    where_constraints: BTreeMap<String, String>,
    /// Variables declared with `var` (mutable). Variables not in this set
    /// are immutable (`let`, function params, loop vars, etc.).
    mutable_vars: std::collections::BTreeSet<String>,
    /// Variables that have been narrowed by flow-sensitive refinement.
    /// Maps var name → pre-narrowing type (used to restore on reassignment).
    narrowed_vars: BTreeMap<String, InferredType>,
    parent: Option<Box<TypeScope>>,
}

/// Method signature extracted from an impl block (for interface checking).
#[derive(Debug, Clone)]
struct ImplMethodSig {
    name: String,
    /// Number of parameters excluding `self`.
    param_count: usize,
    /// Parameter types (excluding `self`), None means untyped.
    param_types: Vec<Option<TypeExpr>>,
    /// Return type, None means untyped.
    return_type: Option<TypeExpr>,
}

#[derive(Debug, Clone)]
struct FnSignature {
    params: Vec<(String, InferredType)>,
    return_type: InferredType,
    /// Generic type parameter names declared on the function.
    type_param_names: Vec<String>,
    /// Number of required parameters (those without defaults).
    required_params: usize,
    /// Where-clause constraints: (type_param_name, interface_bound).
    where_clauses: Vec<(String, String)>,
    /// True if the last parameter is a rest parameter.
    has_rest: bool,
}

impl TypeScope {
    fn new() -> Self {
        Self {
            vars: BTreeMap::new(),
            functions: BTreeMap::new(),
            type_aliases: BTreeMap::new(),
            enums: BTreeMap::new(),
            interfaces: BTreeMap::new(),
            structs: BTreeMap::new(),
            impl_methods: BTreeMap::new(),
            generic_type_params: std::collections::BTreeSet::new(),
            where_constraints: BTreeMap::new(),
            mutable_vars: std::collections::BTreeSet::new(),
            narrowed_vars: BTreeMap::new(),
            parent: None,
        }
    }

    fn child(&self) -> Self {
        Self {
            vars: BTreeMap::new(),
            functions: BTreeMap::new(),
            type_aliases: BTreeMap::new(),
            enums: BTreeMap::new(),
            interfaces: BTreeMap::new(),
            structs: BTreeMap::new(),
            impl_methods: BTreeMap::new(),
            generic_type_params: std::collections::BTreeSet::new(),
            where_constraints: BTreeMap::new(),
            mutable_vars: std::collections::BTreeSet::new(),
            narrowed_vars: BTreeMap::new(),
            parent: Some(Box::new(self.clone())),
        }
    }

    fn get_var(&self, name: &str) -> Option<&InferredType> {
        self.vars
            .get(name)
            .or_else(|| self.parent.as_ref()?.get_var(name))
    }

    fn get_fn(&self, name: &str) -> Option<&FnSignature> {
        self.functions
            .get(name)
            .or_else(|| self.parent.as_ref()?.get_fn(name))
    }

    fn resolve_type(&self, name: &str) -> Option<&TypeExpr> {
        self.type_aliases
            .get(name)
            .or_else(|| self.parent.as_ref()?.resolve_type(name))
    }

    fn is_generic_type_param(&self, name: &str) -> bool {
        self.generic_type_params.contains(name)
            || self
                .parent
                .as_ref()
                .is_some_and(|p| p.is_generic_type_param(name))
    }

    fn get_where_constraint(&self, type_param: &str) -> Option<&str> {
        self.where_constraints
            .get(type_param)
            .map(|s| s.as_str())
            .or_else(|| {
                self.parent
                    .as_ref()
                    .and_then(|p| p.get_where_constraint(type_param))
            })
    }

    fn get_enum(&self, name: &str) -> Option<&Vec<String>> {
        self.enums
            .get(name)
            .or_else(|| self.parent.as_ref()?.get_enum(name))
    }

    fn get_interface(&self, name: &str) -> Option<&Vec<InterfaceMethod>> {
        self.interfaces
            .get(name)
            .or_else(|| self.parent.as_ref()?.get_interface(name))
    }

    fn get_struct(&self, name: &str) -> Option<&Vec<(String, InferredType)>> {
        self.structs
            .get(name)
            .or_else(|| self.parent.as_ref()?.get_struct(name))
    }

    fn get_impl_methods(&self, name: &str) -> Option<&Vec<ImplMethodSig>> {
        self.impl_methods
            .get(name)
            .or_else(|| self.parent.as_ref()?.get_impl_methods(name))
    }

    fn define_var(&mut self, name: &str, ty: InferredType) {
        self.vars.insert(name.to_string(), ty);
    }

    fn define_var_mutable(&mut self, name: &str, ty: InferredType) {
        self.vars.insert(name.to_string(), ty);
        self.mutable_vars.insert(name.to_string());
    }

    /// Check if a variable is mutable (declared with `var`).
    fn is_mutable(&self, name: &str) -> bool {
        self.mutable_vars.contains(name) || self.parent.as_ref().is_some_and(|p| p.is_mutable(name))
    }

    fn define_fn(&mut self, name: &str, sig: FnSignature) {
        self.functions.insert(name.to_string(), sig);
    }
}

/// Bidirectional type refinements extracted from a condition.
/// Each path contains a list of (variable_name, narrowed_type) pairs.
#[derive(Debug, Clone, Default)]
struct Refinements {
    /// Narrowings when the condition evaluates to true/truthy.
    truthy: Vec<(String, InferredType)>,
    /// Narrowings when the condition evaluates to false/falsy.
    falsy: Vec<(String, InferredType)>,
}

impl Refinements {
    fn empty() -> Self {
        Self::default()
    }

    /// Swap truthy and falsy (used for negation).
    fn inverted(self) -> Self {
        Self {
            truthy: self.falsy,
            falsy: self.truthy,
        }
    }
}

/// Known return types for builtin functions. Delegates to the shared
/// [`builtin_signatures`] registry — see that module for the full table.
fn builtin_return_type(name: &str) -> InferredType {
    builtin_signatures::builtin_return_type(name)
}

/// Check if a name is a known builtin. Delegates to the shared
/// [`builtin_signatures`] registry.
fn is_builtin(name: &str) -> bool {
    builtin_signatures::is_builtin(name)
}

/// The static type checker.
pub struct TypeChecker {
    diagnostics: Vec<TypeDiagnostic>,
    scope: TypeScope,
    source: Option<String>,
    hints: Vec<InlayHintInfo>,
}

impl TypeChecker {
    pub fn new() -> Self {
        Self {
            diagnostics: Vec::new(),
            scope: TypeScope::new(),
            source: None,
            hints: Vec::new(),
        }
    }

    /// Check a program with source text for autofix generation.
    pub fn check_with_source(mut self, program: &[SNode], source: &str) -> Vec<TypeDiagnostic> {
        self.source = Some(source.to_string());
        self.check_inner(program).0
    }

    /// Check a program and return diagnostics.
    pub fn check(self, program: &[SNode]) -> Vec<TypeDiagnostic> {
        self.check_inner(program).0
    }

    /// Check a program and return both diagnostics and inlay hints.
    pub fn check_with_hints(
        mut self,
        program: &[SNode],
        source: &str,
    ) -> (Vec<TypeDiagnostic>, Vec<InlayHintInfo>) {
        self.source = Some(source.to_string());
        self.check_inner(program)
    }

    fn check_inner(mut self, program: &[SNode]) -> (Vec<TypeDiagnostic>, Vec<InlayHintInfo>) {
        // First pass: register type and enum declarations into root scope
        Self::register_declarations_into(&mut self.scope, program);

        // Also scan pipeline bodies for declarations
        for snode in program {
            if let Node::Pipeline { body, .. } = &snode.node {
                Self::register_declarations_into(&mut self.scope, body);
            }
        }

        // Check each top-level node
        for snode in program {
            match &snode.node {
                Node::Pipeline { params, body, .. } => {
                    let mut child = self.scope.child();
                    for p in params {
                        child.define_var(p, None);
                    }
                    self.check_block(body, &mut child);
                }
                Node::FnDecl {
                    name,
                    type_params,
                    params,
                    return_type,
                    where_clauses,
                    body,
                    ..
                } => {
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
                    self.check_fn_body(type_params, params, return_type, body, where_clauses);
                }
                _ => {
                    let mut scope = self.scope.clone();
                    self.check_node(snode, &mut scope);
                    // Merge any new definitions back into the top-level scope
                    for (name, ty) in scope.vars {
                        self.scope.vars.entry(name).or_insert(ty);
                    }
                    for name in scope.mutable_vars {
                        self.scope.mutable_vars.insert(name);
                    }
                }
            }
        }

        (self.diagnostics, self.hints)
    }

    /// Register type, enum, interface, and struct declarations from AST nodes into a scope.
    fn register_declarations_into(scope: &mut TypeScope, nodes: &[SNode]) {
        for snode in nodes {
            match &snode.node {
                Node::TypeDecl { name, type_expr } => {
                    scope.type_aliases.insert(name.clone(), type_expr.clone());
                }
                Node::EnumDecl { name, variants, .. } => {
                    let variant_names: Vec<String> =
                        variants.iter().map(|v| v.name.clone()).collect();
                    scope.enums.insert(name.clone(), variant_names);
                }
                Node::InterfaceDecl { name, methods, .. } => {
                    scope.interfaces.insert(name.clone(), methods.clone());
                }
                Node::StructDecl { name, fields, .. } => {
                    let field_types: Vec<(String, InferredType)> = fields
                        .iter()
                        .map(|f| (f.name.clone(), f.type_expr.clone()))
                        .collect();
                    scope.structs.insert(name.clone(), field_types);
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

    fn check_block(&mut self, stmts: &[SNode], scope: &mut TypeScope) {
        for stmt in stmts {
            self.check_node(stmt, scope);
        }
    }

    /// Define variables from a destructuring pattern in the given scope (as unknown type).
    fn define_pattern_vars(pattern: &BindingPattern, scope: &mut TypeScope, mutable: bool) {
        let define = |scope: &mut TypeScope, name: &str| {
            if mutable {
                scope.define_var_mutable(name, None);
            } else {
                scope.define_var(name, None);
            }
        };
        match pattern {
            BindingPattern::Identifier(name) => {
                define(scope, name);
            }
            BindingPattern::Dict(fields) => {
                for field in fields {
                    let name = field.alias.as_deref().unwrap_or(&field.key);
                    define(scope, name);
                }
            }
            BindingPattern::List(elements) => {
                for elem in elements {
                    define(scope, &elem.name);
                }
            }
        }
    }

    /// Type-check default value expressions within a destructuring pattern.
    fn check_pattern_defaults(&mut self, pattern: &BindingPattern, scope: &mut TypeScope) {
        match pattern {
            BindingPattern::Identifier(_) => {}
            BindingPattern::Dict(fields) => {
                for field in fields {
                    if let Some(default) = &field.default_value {
                        self.check_binops(default, scope);
                    }
                }
            }
            BindingPattern::List(elements) => {
                for elem in elements {
                    if let Some(default) = &elem.default_value {
                        self.check_binops(default, scope);
                    }
                }
            }
        }
    }

    fn check_node(&mut self, snode: &SNode, scope: &mut TypeScope) {
        let span = snode.span;
        match &snode.node {
            Node::LetBinding {
                pattern,
                type_ann,
                value,
            } => {
                self.check_binops(value, scope);
                let inferred = self.infer_type(value, scope);
                if let BindingPattern::Identifier(name) = pattern {
                    if let Some(expected) = type_ann {
                        if let Some(actual) = &inferred {
                            if !self.types_compatible(expected, actual, scope) {
                                let mut msg = format!(
                                    "Type mismatch: '{}' declared as {}, but assigned {}",
                                    name,
                                    format_type(expected),
                                    format_type(actual)
                                );
                                if let Some(detail) = shape_mismatch_detail(expected, actual) {
                                    msg.push_str(&format!(" ({})", detail));
                                }
                                self.error_at(msg, span);
                            }
                        }
                    }
                    // Collect inlay hint when type is inferred (no annotation)
                    if type_ann.is_none() {
                        if let Some(ref ty) = inferred {
                            if !is_obvious_type(value, ty) {
                                self.hints.push(InlayHintInfo {
                                    line: span.line,
                                    column: span.column + "let ".len() + name.len(),
                                    label: format!(": {}", format_type(ty)),
                                });
                            }
                        }
                    }
                    let ty = type_ann.clone().or(inferred);
                    scope.define_var(name, ty);
                } else {
                    self.check_pattern_defaults(pattern, scope);
                    Self::define_pattern_vars(pattern, scope, false);
                }
            }

            Node::VarBinding {
                pattern,
                type_ann,
                value,
            } => {
                self.check_binops(value, scope);
                let inferred = self.infer_type(value, scope);
                if let BindingPattern::Identifier(name) = pattern {
                    if let Some(expected) = type_ann {
                        if let Some(actual) = &inferred {
                            if !self.types_compatible(expected, actual, scope) {
                                let mut msg = format!(
                                    "Type mismatch: '{}' declared as {}, but assigned {}",
                                    name,
                                    format_type(expected),
                                    format_type(actual)
                                );
                                if let Some(detail) = shape_mismatch_detail(expected, actual) {
                                    msg.push_str(&format!(" ({})", detail));
                                }
                                self.error_at(msg, span);
                            }
                        }
                    }
                    if type_ann.is_none() {
                        if let Some(ref ty) = inferred {
                            if !is_obvious_type(value, ty) {
                                self.hints.push(InlayHintInfo {
                                    line: span.line,
                                    column: span.column + "var ".len() + name.len(),
                                    label: format!(": {}", format_type(ty)),
                                });
                            }
                        }
                    }
                    let ty = type_ann.clone().or(inferred);
                    scope.define_var_mutable(name, ty);
                } else {
                    self.check_pattern_defaults(pattern, scope);
                    Self::define_pattern_vars(pattern, scope, true);
                }
            }

            Node::FnDecl {
                name,
                type_params,
                params,
                return_type,
                where_clauses,
                body,
                ..
            } => {
                let required_params = params.iter().filter(|p| p.default_value.is_none()).count();
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
                scope.define_fn(name, sig.clone());
                scope.define_var(name, None);
                self.check_fn_body(type_params, params, return_type, body, where_clauses);
            }

            Node::ToolDecl {
                name,
                params,
                return_type,
                body,
                ..
            } => {
                // Register the tool like a function for type checking purposes
                let required_params = params.iter().filter(|p| p.default_value.is_none()).count();
                let sig = FnSignature {
                    params: params
                        .iter()
                        .map(|p| (p.name.clone(), p.type_expr.clone()))
                        .collect(),
                    return_type: return_type.clone(),
                    type_param_names: Vec::new(),
                    required_params,
                    where_clauses: Vec::new(),
                    has_rest: params.last().is_some_and(|p| p.rest),
                };
                scope.define_fn(name, sig);
                scope.define_var(name, None);
                self.check_fn_body(&[], params, return_type, body, &[]);
            }

            Node::FunctionCall { name, args } => {
                self.check_call(name, args, scope, span);
            }

            Node::IfElse {
                condition,
                then_body,
                else_body,
            } => {
                self.check_node(condition, scope);
                let refs = Self::extract_refinements(condition, scope);

                let mut then_scope = scope.child();
                apply_refinements(&mut then_scope, &refs.truthy);
                self.check_block(then_body, &mut then_scope);

                if let Some(else_body) = else_body {
                    let mut else_scope = scope.child();
                    apply_refinements(&mut else_scope, &refs.falsy);
                    self.check_block(else_body, &mut else_scope);

                    // Post-branch narrowing: if one branch definitely exits,
                    // apply the other branch's refinements to the outer scope
                    if Self::block_definitely_exits(then_body)
                        && !Self::block_definitely_exits(else_body)
                    {
                        apply_refinements(scope, &refs.falsy);
                    } else if Self::block_definitely_exits(else_body)
                        && !Self::block_definitely_exits(then_body)
                    {
                        apply_refinements(scope, &refs.truthy);
                    }
                } else {
                    // No else: if then-body always exits, apply falsy after
                    if Self::block_definitely_exits(then_body) {
                        apply_refinements(scope, &refs.falsy);
                    }
                }
            }

            Node::ForIn {
                pattern,
                iterable,
                body,
            } => {
                self.check_node(iterable, scope);
                let mut loop_scope = scope.child();
                if let BindingPattern::Identifier(variable) = pattern {
                    // Infer loop variable type from iterable
                    let elem_type = match self.infer_type(iterable, scope) {
                        Some(TypeExpr::List(inner)) => Some(*inner),
                        Some(TypeExpr::Named(n)) if n == "string" => {
                            Some(TypeExpr::Named("string".into()))
                        }
                        _ => None,
                    };
                    loop_scope.define_var(variable, elem_type);
                } else {
                    self.check_pattern_defaults(pattern, &mut loop_scope);
                    Self::define_pattern_vars(pattern, &mut loop_scope, false);
                }
                self.check_block(body, &mut loop_scope);
            }

            Node::WhileLoop { condition, body } => {
                self.check_node(condition, scope);
                let refs = Self::extract_refinements(condition, scope);
                let mut loop_scope = scope.child();
                apply_refinements(&mut loop_scope, &refs.truthy);
                self.check_block(body, &mut loop_scope);
            }

            Node::RequireStmt { condition, message } => {
                self.check_node(condition, scope);
                if let Some(message) = message {
                    self.check_node(message, scope);
                }
            }

            Node::TryCatch {
                body,
                error_var,
                catch_body,
                finally_body,
                ..
            } => {
                let mut try_scope = scope.child();
                self.check_block(body, &mut try_scope);
                let mut catch_scope = scope.child();
                if let Some(var) = error_var {
                    catch_scope.define_var(var, None);
                }
                self.check_block(catch_body, &mut catch_scope);
                if let Some(fb) = finally_body {
                    let mut finally_scope = scope.child();
                    self.check_block(fb, &mut finally_scope);
                }
            }

            Node::TryExpr { body } => {
                let mut try_scope = scope.child();
                self.check_block(body, &mut try_scope);
            }

            Node::ReturnStmt {
                value: Some(val), ..
            } => {
                self.check_node(val, scope);
            }

            Node::Assignment {
                target, value, op, ..
            } => {
                self.check_node(value, scope);
                if let Node::Identifier(name) = &target.node {
                    // Compile-time immutability check
                    if scope.get_var(name).is_some() && !scope.is_mutable(name) {
                        self.warning_at(
                            format!(
                                "Cannot assign to '{}': variable is immutable (declared with 'let')",
                                name
                            ),
                            span,
                        );
                    }

                    if let Some(Some(var_type)) = scope.get_var(name) {
                        let value_type = self.infer_type(value, scope);
                        let assigned = if let Some(op) = op {
                            let var_inferred = scope.get_var(name).cloned().flatten();
                            infer_binary_op_type(op, &var_inferred, &value_type)
                        } else {
                            value_type
                        };
                        if let Some(actual) = &assigned {
                            // Check against the original (pre-narrowing) type if narrowed
                            let check_type = scope
                                .narrowed_vars
                                .get(name)
                                .and_then(|t| t.as_ref())
                                .unwrap_or(var_type);
                            if !self.types_compatible(check_type, actual, scope) {
                                self.error_at(
                                    format!(
                                        "Type mismatch: cannot assign {} to '{}' (declared as {})",
                                        format_type(actual),
                                        name,
                                        format_type(check_type)
                                    ),
                                    span,
                                );
                            }
                        }
                    }

                    // Invalidate narrowing on reassignment: restore original type
                    if let Some(original) = scope.narrowed_vars.remove(name) {
                        scope.define_var(name, original);
                    }
                }
            }

            Node::TypeDecl { name, type_expr } => {
                scope.type_aliases.insert(name.clone(), type_expr.clone());
            }

            Node::EnumDecl { name, variants, .. } => {
                let variant_names: Vec<String> = variants.iter().map(|v| v.name.clone()).collect();
                scope.enums.insert(name.clone(), variant_names);
            }

            Node::StructDecl { name, fields, .. } => {
                let field_types: Vec<(String, InferredType)> = fields
                    .iter()
                    .map(|f| (f.name.clone(), f.type_expr.clone()))
                    .collect();
                scope.structs.insert(name.clone(), field_types);
            }

            Node::InterfaceDecl { name, methods, .. } => {
                scope.interfaces.insert(name.clone(), methods.clone());
            }

            Node::ImplBlock {
                type_name, methods, ..
            } => {
                // Register impl methods for interface satisfaction checking
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
                for method_sn in methods {
                    self.check_node(method_sn, scope);
                }
            }

            Node::TryOperator { operand } => {
                self.check_node(operand, scope);
            }

            Node::MatchExpr { value, arms } => {
                self.check_node(value, scope);
                let value_type = self.infer_type(value, scope);
                for arm in arms {
                    self.check_node(&arm.pattern, scope);
                    // Check for incompatible literal pattern types
                    if let Some(ref vt) = value_type {
                        let value_type_name = format_type(vt);
                        let mismatch = match &arm.pattern.node {
                            Node::StringLiteral(_) => {
                                !self.types_compatible(vt, &TypeExpr::Named("string".into()), scope)
                            }
                            Node::IntLiteral(_) => {
                                !self.types_compatible(vt, &TypeExpr::Named("int".into()), scope)
                                    && !self.types_compatible(
                                        vt,
                                        &TypeExpr::Named("float".into()),
                                        scope,
                                    )
                            }
                            Node::FloatLiteral(_) => {
                                !self.types_compatible(vt, &TypeExpr::Named("float".into()), scope)
                                    && !self.types_compatible(
                                        vt,
                                        &TypeExpr::Named("int".into()),
                                        scope,
                                    )
                            }
                            Node::BoolLiteral(_) => {
                                !self.types_compatible(vt, &TypeExpr::Named("bool".into()), scope)
                            }
                            _ => false,
                        };
                        if mismatch {
                            let pattern_type = match &arm.pattern.node {
                                Node::StringLiteral(_) => "string",
                                Node::IntLiteral(_) => "int",
                                Node::FloatLiteral(_) => "float",
                                Node::BoolLiteral(_) => "bool",
                                _ => unreachable!(),
                            };
                            self.warning_at(
                                format!(
                                    "Match pattern type mismatch: matching {} against {} literal",
                                    value_type_name, pattern_type
                                ),
                                arm.pattern.span,
                            );
                        }
                    }
                    let mut arm_scope = scope.child();
                    // Narrow the matched value's type in each arm
                    if let Node::Identifier(var_name) = &value.node {
                        if let Some(Some(TypeExpr::Union(members))) = scope.get_var(var_name) {
                            let narrowed = match &arm.pattern.node {
                                Node::NilLiteral => narrow_to_single(members, "nil"),
                                Node::StringLiteral(_) => narrow_to_single(members, "string"),
                                Node::IntLiteral(_) => narrow_to_single(members, "int"),
                                Node::FloatLiteral(_) => narrow_to_single(members, "float"),
                                Node::BoolLiteral(_) => narrow_to_single(members, "bool"),
                                _ => None,
                            };
                            if let Some(narrowed_type) = narrowed {
                                arm_scope.define_var(var_name, Some(narrowed_type));
                            }
                        }
                    }
                    self.check_block(&arm.body, &mut arm_scope);
                }
                self.check_match_exhaustiveness(value, arms, scope, span);
            }

            // Recurse into nested expressions + validate binary op types
            Node::BinaryOp { op, left, right } => {
                self.check_node(left, scope);
                self.check_node(right, scope);
                // Validate operator/type compatibility
                let lt = self.infer_type(left, scope);
                let rt = self.infer_type(right, scope);
                if let (Some(TypeExpr::Named(l)), Some(TypeExpr::Named(r))) = (&lt, &rt) {
                    match op.as_str() {
                        "-" | "/" | "%" => {
                            let numeric = ["int", "float"];
                            if !numeric.contains(&l.as_str()) || !numeric.contains(&r.as_str()) {
                                self.error_at(
                                    format!(
                                        "Operator '{}' requires numeric operands, got {} and {}",
                                        op, l, r
                                    ),
                                    span,
                                );
                            }
                        }
                        "*" => {
                            let numeric = ["int", "float"];
                            let is_numeric =
                                numeric.contains(&l.as_str()) && numeric.contains(&r.as_str());
                            let is_string_repeat =
                                (l == "string" && r == "int") || (l == "int" && r == "string");
                            if !is_numeric && !is_string_repeat {
                                self.error_at(
                                    format!(
                                        "Operator '*' requires numeric operands or string * int, got {} and {}",
                                        l, r
                                    ),
                                    span,
                                );
                            }
                        }
                        "+" => {
                            let valid = matches!(
                                (l.as_str(), r.as_str()),
                                ("int" | "float", "int" | "float")
                                    | ("string", "string")
                                    | ("list", "list")
                                    | ("dict", "dict")
                            );
                            if !valid {
                                let msg =
                                    format!("Operator '+' is not valid for types {} and {}", l, r);
                                // Offer interpolation fix when one side is string
                                let fix = if l == "string" || r == "string" {
                                    self.build_interpolation_fix(left, right, l == "string", span)
                                } else {
                                    None
                                };
                                if let Some(fix) = fix {
                                    self.error_at_with_fix(msg, span, fix);
                                } else {
                                    self.error_at(msg, span);
                                }
                            }
                        }
                        "<" | ">" | "<=" | ">=" => {
                            let comparable = ["int", "float", "string"];
                            if !comparable.contains(&l.as_str())
                                || !comparable.contains(&r.as_str())
                            {
                                self.warning_at(
                                    format!(
                                        "Comparison '{}' may not be meaningful for types {} and {}",
                                        op, l, r
                                    ),
                                    span,
                                );
                            } else if (l == "string") != (r == "string") {
                                self.warning_at(
                                    format!(
                                        "Comparing {} with {} using '{}' may give unexpected results",
                                        l, r, op
                                    ),
                                    span,
                                );
                            }
                        }
                        _ => {}
                    }
                }
            }
            Node::UnaryOp { operand, .. } => {
                self.check_node(operand, scope);
            }
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
            } => {
                self.check_node(object, scope);
                for arg in args {
                    self.check_node(arg, scope);
                }
                // Definition-site generic checking: if the object's type is a
                // constrained generic param (where T: Interface), verify the
                // method exists in the bound interface.
                if let Some(TypeExpr::Named(type_name)) = self.infer_type(object, scope) {
                    if scope.is_generic_type_param(&type_name) {
                        if let Some(iface_name) = scope.get_where_constraint(&type_name) {
                            if let Some(iface_methods) = scope.get_interface(iface_name) {
                                let has_method = iface_methods.iter().any(|m| m.name == *method);
                                if !has_method {
                                    self.warning_at(
                                        format!(
                                            "Method '{}' not found in interface '{}' (constraint on '{}')",
                                            method, iface_name, type_name
                                        ),
                                        span,
                                    );
                                }
                            }
                        }
                    }
                }
            }
            Node::PropertyAccess { object, .. } | Node::OptionalPropertyAccess { object, .. } => {
                self.check_node(object, scope);
            }
            Node::SubscriptAccess { object, index } => {
                self.check_node(object, scope);
                self.check_node(index, scope);
            }
            Node::SliceAccess { object, start, end } => {
                self.check_node(object, scope);
                if let Some(s) = start {
                    self.check_node(s, scope);
                }
                if let Some(e) = end {
                    self.check_node(e, scope);
                }
            }

            // --- Compound nodes: recurse into children ---
            Node::Ternary {
                condition,
                true_expr,
                false_expr,
            } => {
                self.check_node(condition, scope);
                let refs = Self::extract_refinements(condition, scope);

                let mut true_scope = scope.child();
                apply_refinements(&mut true_scope, &refs.truthy);
                self.check_node(true_expr, &mut true_scope);

                let mut false_scope = scope.child();
                apply_refinements(&mut false_scope, &refs.falsy);
                self.check_node(false_expr, &mut false_scope);
            }

            Node::ThrowStmt { value } => {
                self.check_node(value, scope);
            }

            Node::GuardStmt {
                condition,
                else_body,
            } => {
                self.check_node(condition, scope);
                let refs = Self::extract_refinements(condition, scope);

                let mut else_scope = scope.child();
                apply_refinements(&mut else_scope, &refs.falsy);
                self.check_block(else_body, &mut else_scope);

                // After guard, condition is true — apply truthy refinements
                // to the OUTER scope (guard's else-body must exit)
                apply_refinements(scope, &refs.truthy);
            }

            Node::SpawnExpr { body } => {
                let mut spawn_scope = scope.child();
                self.check_block(body, &mut spawn_scope);
            }

            Node::Parallel {
                count,
                variable,
                body,
            } => {
                self.check_node(count, scope);
                let mut par_scope = scope.child();
                if let Some(var) = variable {
                    par_scope.define_var(var, Some(TypeExpr::Named("int".into())));
                }
                self.check_block(body, &mut par_scope);
            }

            Node::ParallelMap {
                list,
                variable,
                body,
            }
            | Node::ParallelSettle {
                list,
                variable,
                body,
            } => {
                self.check_node(list, scope);
                let mut par_scope = scope.child();
                let elem_type = match self.infer_type(list, scope) {
                    Some(TypeExpr::List(inner)) => Some(*inner),
                    _ => None,
                };
                par_scope.define_var(variable, elem_type);
                self.check_block(body, &mut par_scope);
            }

            Node::SelectExpr {
                cases,
                timeout,
                default_body,
            } => {
                for case in cases {
                    self.check_node(&case.channel, scope);
                    let mut case_scope = scope.child();
                    case_scope.define_var(&case.variable, None);
                    self.check_block(&case.body, &mut case_scope);
                }
                if let Some((dur, body)) = timeout {
                    self.check_node(dur, scope);
                    let mut timeout_scope = scope.child();
                    self.check_block(body, &mut timeout_scope);
                }
                if let Some(body) = default_body {
                    let mut default_scope = scope.child();
                    self.check_block(body, &mut default_scope);
                }
            }

            Node::DeadlineBlock { duration, body } => {
                self.check_node(duration, scope);
                let mut block_scope = scope.child();
                self.check_block(body, &mut block_scope);
            }

            Node::MutexBlock { body } => {
                let mut block_scope = scope.child();
                self.check_block(body, &mut block_scope);
            }

            Node::Retry { count, body } => {
                self.check_node(count, scope);
                let mut retry_scope = scope.child();
                self.check_block(body, &mut retry_scope);
            }

            Node::Closure { params, body, .. } => {
                let mut closure_scope = scope.child();
                for p in params {
                    closure_scope.define_var(&p.name, p.type_expr.clone());
                }
                self.check_block(body, &mut closure_scope);
            }

            Node::ListLiteral(elements) => {
                for elem in elements {
                    self.check_node(elem, scope);
                }
            }

            Node::DictLiteral(entries) | Node::AskExpr { fields: entries } => {
                for entry in entries {
                    self.check_node(&entry.key, scope);
                    self.check_node(&entry.value, scope);
                }
            }

            Node::RangeExpr { start, end, .. } => {
                self.check_node(start, scope);
                self.check_node(end, scope);
            }

            Node::Spread(inner) => {
                self.check_node(inner, scope);
            }

            Node::Block(stmts) => {
                let mut block_scope = scope.child();
                self.check_block(stmts, &mut block_scope);
            }

            Node::YieldExpr { value } => {
                if let Some(v) = value {
                    self.check_node(v, scope);
                }
            }

            // --- Struct construction: validate fields against declaration ---
            Node::StructConstruct {
                struct_name,
                fields,
            } => {
                for entry in fields {
                    self.check_node(&entry.key, scope);
                    self.check_node(&entry.value, scope);
                }
                if let Some(declared_fields) = scope.get_struct(struct_name).cloned() {
                    // Warn on unknown fields
                    for entry in fields {
                        if let Node::StringLiteral(key) | Node::Identifier(key) = &entry.key.node {
                            if !declared_fields.iter().any(|(name, _)| name == key) {
                                self.warning_at(
                                    format!("Unknown field '{}' in struct '{}'", key, struct_name),
                                    entry.key.span,
                                );
                            }
                        }
                    }
                    // Warn on missing required fields
                    let provided: Vec<String> = fields
                        .iter()
                        .filter_map(|e| match &e.key.node {
                            Node::StringLiteral(k) | Node::Identifier(k) => Some(k.clone()),
                            _ => None,
                        })
                        .collect();
                    for (name, _) in &declared_fields {
                        if !provided.contains(name) {
                            self.warning_at(
                                format!(
                                    "Missing field '{}' in struct '{}' construction",
                                    name, struct_name
                                ),
                                span,
                            );
                        }
                    }
                }
            }

            // --- Enum construction: validate variant exists ---
            Node::EnumConstruct {
                enum_name,
                variant,
                args,
            } => {
                for arg in args {
                    self.check_node(arg, scope);
                }
                if let Some(variants) = scope.get_enum(enum_name) {
                    if !variants.contains(variant) {
                        self.warning_at(
                            format!("Unknown variant '{}' in enum '{}'", variant, enum_name),
                            span,
                        );
                    }
                }
            }

            // --- InterpolatedString: segments are lexer-level, no SNode children ---
            Node::InterpolatedString(_) => {}

            // --- Terminals: no children to check ---
            Node::StringLiteral(_)
            | Node::RawStringLiteral(_)
            | Node::IntLiteral(_)
            | Node::FloatLiteral(_)
            | Node::BoolLiteral(_)
            | Node::NilLiteral
            | Node::Identifier(_)
            | Node::DurationLiteral(_)
            | Node::BreakStmt
            | Node::ContinueStmt
            | Node::ReturnStmt { value: None }
            | Node::ImportDecl { .. }
            | Node::SelectiveImport { .. } => {}

            // Declarations already handled above; catch remaining variants
            // that have no meaningful type-check behavior.
            Node::Pipeline { body, .. } | Node::OverrideDecl { body, .. } => {
                let mut decl_scope = scope.child();
                self.check_block(body, &mut decl_scope);
            }
        }
    }

    fn check_fn_body(
        &mut self,
        type_params: &[TypeParam],
        params: &[TypedParam],
        return_type: &Option<TypeExpr>,
        body: &[SNode],
        where_clauses: &[WhereClause],
    ) {
        let mut fn_scope = self.scope.child();
        // Register generic type parameters so they are treated as compatible
        // with any concrete type during type checking.
        for tp in type_params {
            fn_scope.generic_type_params.insert(tp.name.clone());
        }
        // Store where-clause constraints for definition-site checking
        for wc in where_clauses {
            fn_scope
                .where_constraints
                .insert(wc.type_name.clone(), wc.bound.clone());
        }
        for param in params {
            fn_scope.define_var(&param.name, param.type_expr.clone());
            if let Some(default) = &param.default_value {
                self.check_node(default, &mut fn_scope);
            }
        }
        // Snapshot scope before main pass (which may mutate it with narrowing)
        // so that return-type checking starts from the original parameter types.
        let ret_scope_base = if return_type.is_some() {
            Some(fn_scope.child())
        } else {
            None
        };

        self.check_block(body, &mut fn_scope);

        // Check return statements against declared return type
        if let Some(ret_type) = return_type {
            let mut ret_scope = ret_scope_base.unwrap();
            for stmt in body {
                self.check_return_type(stmt, ret_type, &mut ret_scope);
            }
        }
    }

    fn check_return_type(&mut self, snode: &SNode, expected: &TypeExpr, scope: &mut TypeScope) {
        let span = snode.span;
        match &snode.node {
            Node::ReturnStmt { value: Some(val) } => {
                let inferred = self.infer_type(val, scope);
                if let Some(actual) = &inferred {
                    if !self.types_compatible(expected, actual, scope) {
                        self.error_at(
                            format!(
                                "Return type mismatch: expected {}, got {}",
                                format_type(expected),
                                format_type(actual)
                            ),
                            span,
                        );
                    }
                }
            }
            Node::IfElse {
                condition,
                then_body,
                else_body,
            } => {
                let refs = Self::extract_refinements(condition, scope);
                let mut then_scope = scope.child();
                apply_refinements(&mut then_scope, &refs.truthy);
                for stmt in then_body {
                    self.check_return_type(stmt, expected, &mut then_scope);
                }
                if let Some(else_body) = else_body {
                    let mut else_scope = scope.child();
                    apply_refinements(&mut else_scope, &refs.falsy);
                    for stmt in else_body {
                        self.check_return_type(stmt, expected, &mut else_scope);
                    }
                    // Post-branch narrowing for return type checking
                    if Self::block_definitely_exits(then_body)
                        && !Self::block_definitely_exits(else_body)
                    {
                        apply_refinements(scope, &refs.falsy);
                    } else if Self::block_definitely_exits(else_body)
                        && !Self::block_definitely_exits(then_body)
                    {
                        apply_refinements(scope, &refs.truthy);
                    }
                } else {
                    // No else: if then-body always exits, apply falsy after
                    if Self::block_definitely_exits(then_body) {
                        apply_refinements(scope, &refs.falsy);
                    }
                }
            }
            _ => {}
        }
    }

    /// Check if a match expression on an enum's `.variant` property covers all variants.
    /// Extract narrowing info from nil-check conditions like `x != nil`.
    /// Returns (var_name, narrowed_type) where narrowed_type removes nil from a union.
    /// Check if a type satisfies an interface (Go-style implicit satisfaction).
    /// A type satisfies an interface if its impl block has all the required methods.
    fn satisfies_interface(
        &self,
        type_name: &str,
        interface_name: &str,
        scope: &TypeScope,
    ) -> bool {
        self.interface_mismatch_reason(type_name, interface_name, scope)
            .is_none()
    }

    /// Return a detailed reason why a type does not satisfy an interface, or None
    /// if it does satisfy it.  Used for producing actionable warning messages.
    fn interface_mismatch_reason(
        &self,
        type_name: &str,
        interface_name: &str,
        scope: &TypeScope,
    ) -> Option<String> {
        let interface_methods = match scope.get_interface(interface_name) {
            Some(methods) => methods,
            None => return Some(format!("interface '{}' not found", interface_name)),
        };
        let impl_methods = match scope.get_impl_methods(type_name) {
            Some(methods) => methods,
            None => {
                if interface_methods.is_empty() {
                    return None;
                }
                let names: Vec<_> = interface_methods.iter().map(|m| m.name.as_str()).collect();
                return Some(format!("missing method(s): {}", names.join(", ")));
            }
        };
        for iface_method in interface_methods {
            let iface_params: Vec<_> = iface_method
                .params
                .iter()
                .filter(|p| p.name != "self")
                .collect();
            let iface_param_count = iface_params.len();
            let matching_impl = impl_methods.iter().find(|im| im.name == iface_method.name);
            let impl_method = match matching_impl {
                Some(m) => m,
                None => {
                    return Some(format!("missing method '{}'", iface_method.name));
                }
            };
            if impl_method.param_count != iface_param_count {
                return Some(format!(
                    "method '{}' has {} parameter(s), expected {}",
                    iface_method.name, impl_method.param_count, iface_param_count
                ));
            }
            // Check parameter types where both sides specify them
            for (i, iface_param) in iface_params.iter().enumerate() {
                if let (Some(expected), Some(actual)) = (
                    &iface_param.type_expr,
                    impl_method.param_types.get(i).and_then(|t| t.as_ref()),
                ) {
                    if !self.types_compatible(expected, actual, scope) {
                        return Some(format!(
                            "method '{}' parameter {} has type '{}', expected '{}'",
                            iface_method.name,
                            i + 1,
                            format_type(actual),
                            format_type(expected),
                        ));
                    }
                }
            }
            // Check return type where both sides specify it
            if let (Some(expected_ret), Some(actual_ret)) =
                (&iface_method.return_type, &impl_method.return_type)
            {
                if !self.types_compatible(expected_ret, actual_ret, scope) {
                    return Some(format!(
                        "method '{}' returns '{}', expected '{}'",
                        iface_method.name,
                        format_type(actual_ret),
                        format_type(expected_ret),
                    ));
                }
            }
        }
        None
    }

    fn bind_type_param(
        param_name: &str,
        concrete: &TypeExpr,
        bindings: &mut BTreeMap<String, TypeExpr>,
    ) -> Result<(), String> {
        if let Some(existing) = bindings.get(param_name) {
            if existing != concrete {
                return Err(format!(
                    "type parameter '{}' was inferred as both {} and {}",
                    param_name,
                    format_type(existing),
                    format_type(concrete)
                ));
            }
            return Ok(());
        }
        bindings.insert(param_name.to_string(), concrete.clone());
        Ok(())
    }

    /// Recursively extract type parameter bindings from matching param/arg types.
    /// E.g., param_type=list<T> + arg_type=list<Dog> → binds T=Dog.
    fn extract_type_bindings(
        param_type: &TypeExpr,
        arg_type: &TypeExpr,
        type_params: &std::collections::BTreeSet<String>,
        bindings: &mut BTreeMap<String, TypeExpr>,
    ) -> Result<(), String> {
        match (param_type, arg_type) {
            (TypeExpr::Named(param_name), concrete) if type_params.contains(param_name) => {
                Self::bind_type_param(param_name, concrete, bindings)
            }
            (TypeExpr::List(p_inner), TypeExpr::List(a_inner)) => {
                Self::extract_type_bindings(p_inner, a_inner, type_params, bindings)
            }
            (TypeExpr::DictType(pk, pv), TypeExpr::DictType(ak, av)) => {
                Self::extract_type_bindings(pk, ak, type_params, bindings)?;
                Self::extract_type_bindings(pv, av, type_params, bindings)
            }
            (TypeExpr::Shape(param_fields), TypeExpr::Shape(arg_fields)) => {
                for param_field in param_fields {
                    if let Some(arg_field) = arg_fields
                        .iter()
                        .find(|field| field.name == param_field.name)
                    {
                        Self::extract_type_bindings(
                            &param_field.type_expr,
                            &arg_field.type_expr,
                            type_params,
                            bindings,
                        )?;
                    }
                }
                Ok(())
            }
            (
                TypeExpr::FnType {
                    params: p_params,
                    return_type: p_ret,
                },
                TypeExpr::FnType {
                    params: a_params,
                    return_type: a_ret,
                },
            ) => {
                for (param, arg) in p_params.iter().zip(a_params.iter()) {
                    Self::extract_type_bindings(param, arg, type_params, bindings)?;
                }
                Self::extract_type_bindings(p_ret, a_ret, type_params, bindings)
            }
            _ => Ok(()),
        }
    }

    fn apply_type_bindings(ty: &TypeExpr, bindings: &BTreeMap<String, TypeExpr>) -> TypeExpr {
        match ty {
            TypeExpr::Named(name) => bindings
                .get(name)
                .cloned()
                .unwrap_or_else(|| TypeExpr::Named(name.clone())),
            TypeExpr::Union(items) => TypeExpr::Union(
                items
                    .iter()
                    .map(|item| Self::apply_type_bindings(item, bindings))
                    .collect(),
            ),
            TypeExpr::Shape(fields) => TypeExpr::Shape(
                fields
                    .iter()
                    .map(|field| ShapeField {
                        name: field.name.clone(),
                        type_expr: Self::apply_type_bindings(&field.type_expr, bindings),
                        optional: field.optional,
                    })
                    .collect(),
            ),
            TypeExpr::List(inner) => {
                TypeExpr::List(Box::new(Self::apply_type_bindings(inner, bindings)))
            }
            TypeExpr::DictType(key, value) => TypeExpr::DictType(
                Box::new(Self::apply_type_bindings(key, bindings)),
                Box::new(Self::apply_type_bindings(value, bindings)),
            ),
            TypeExpr::FnType {
                params,
                return_type,
            } => TypeExpr::FnType {
                params: params
                    .iter()
                    .map(|param| Self::apply_type_bindings(param, bindings))
                    .collect(),
                return_type: Box::new(Self::apply_type_bindings(return_type, bindings)),
            },
        }
    }

    fn infer_list_literal_type(&self, items: &[SNode], scope: &TypeScope) -> TypeExpr {
        let mut inferred: Option<TypeExpr> = None;
        for item in items {
            let Some(item_type) = self.infer_type(item, scope) else {
                return TypeExpr::Named("list".into());
            };
            inferred = Some(match inferred {
                None => item_type,
                Some(current) if current == item_type => current,
                Some(TypeExpr::Union(mut members)) => {
                    if !members.contains(&item_type) {
                        members.push(item_type);
                    }
                    TypeExpr::Union(members)
                }
                Some(current) => TypeExpr::Union(vec![current, item_type]),
            });
        }
        inferred
            .map(|item_type| TypeExpr::List(Box::new(item_type)))
            .unwrap_or_else(|| TypeExpr::Named("list".into()))
    }

    /// Extract bidirectional type refinements from a condition expression.
    fn extract_refinements(condition: &SNode, scope: &TypeScope) -> Refinements {
        match &condition.node {
            // --- Nil checks and type_of checks ---
            Node::BinaryOp { op, left, right } if op == "!=" || op == "==" => {
                let nil_ref = Self::extract_nil_refinements(op, left, right, scope);
                if !nil_ref.truthy.is_empty() || !nil_ref.falsy.is_empty() {
                    return nil_ref;
                }
                let typeof_ref = Self::extract_typeof_refinements(op, left, right, scope);
                if !typeof_ref.truthy.is_empty() || !typeof_ref.falsy.is_empty() {
                    return typeof_ref;
                }
                Refinements::empty()
            }

            // --- Logical AND: both must be true on truthy path ---
            Node::BinaryOp { op, left, right } if op == "&&" => {
                let left_ref = Self::extract_refinements(left, scope);
                let right_ref = Self::extract_refinements(right, scope);
                let mut truthy = left_ref.truthy;
                truthy.extend(right_ref.truthy);
                Refinements {
                    truthy,
                    falsy: vec![],
                }
            }

            // --- Logical OR: both must be false on falsy path ---
            Node::BinaryOp { op, left, right } if op == "||" => {
                let left_ref = Self::extract_refinements(left, scope);
                let right_ref = Self::extract_refinements(right, scope);
                let mut falsy = left_ref.falsy;
                falsy.extend(right_ref.falsy);
                Refinements {
                    truthy: vec![],
                    falsy,
                }
            }

            // --- Negation: swap truthy/falsy ---
            Node::UnaryOp { op, operand } if op == "!" => {
                Self::extract_refinements(operand, scope).inverted()
            }

            // --- Truthiness: bare identifier in condition position ---
            Node::Identifier(name) => {
                if let Some(Some(TypeExpr::Union(members))) = scope.get_var(name) {
                    if members
                        .iter()
                        .any(|m| matches!(m, TypeExpr::Named(n) if n == "nil"))
                    {
                        if let Some(narrowed) = remove_from_union(members, "nil") {
                            return Refinements {
                                truthy: vec![(name.clone(), Some(narrowed))],
                                falsy: vec![(name.clone(), Some(TypeExpr::Named("nil".into())))],
                            };
                        }
                    }
                }
                Refinements::empty()
            }

            // --- .has("key") on shapes ---
            Node::MethodCall {
                object,
                method,
                args,
            } if method == "has" && args.len() == 1 => {
                Self::extract_has_refinements(object, args, scope)
            }

            _ => Refinements::empty(),
        }
    }

    /// Extract nil-check refinements from `x != nil` / `x == nil` patterns.
    fn extract_nil_refinements(
        op: &str,
        left: &SNode,
        right: &SNode,
        scope: &TypeScope,
    ) -> Refinements {
        let var_node = if matches!(right.node, Node::NilLiteral) {
            left
        } else if matches!(left.node, Node::NilLiteral) {
            right
        } else {
            return Refinements::empty();
        };

        if let Node::Identifier(name) = &var_node.node {
            if let Some(Some(TypeExpr::Union(members))) = scope.get_var(name) {
                if let Some(narrowed) = remove_from_union(members, "nil") {
                    let neq_refs = Refinements {
                        truthy: vec![(name.clone(), Some(narrowed))],
                        falsy: vec![(name.clone(), Some(TypeExpr::Named("nil".into())))],
                    };
                    return if op == "!=" {
                        neq_refs
                    } else {
                        neq_refs.inverted()
                    };
                }
            }
        }
        Refinements::empty()
    }

    /// Extract type_of refinements from `type_of(x) == "typename"` patterns.
    fn extract_typeof_refinements(
        op: &str,
        left: &SNode,
        right: &SNode,
        scope: &TypeScope,
    ) -> Refinements {
        let (var_name, type_name) = if let (Some(var), Node::StringLiteral(tn)) =
            (extract_type_of_var(left), &right.node)
        {
            (var, tn.clone())
        } else if let (Node::StringLiteral(tn), Some(var)) =
            (&left.node, extract_type_of_var(right))
        {
            (var, tn.clone())
        } else {
            return Refinements::empty();
        };

        const KNOWN_TYPES: &[&str] = &[
            "int", "string", "float", "bool", "nil", "list", "dict", "closure",
        ];
        if !KNOWN_TYPES.contains(&type_name.as_str()) {
            return Refinements::empty();
        }

        if let Some(Some(TypeExpr::Union(members))) = scope.get_var(&var_name) {
            let narrowed = narrow_to_single(members, &type_name);
            let remaining = remove_from_union(members, &type_name);
            if narrowed.is_some() || remaining.is_some() {
                let eq_refs = Refinements {
                    truthy: narrowed
                        .map(|n| vec![(var_name.clone(), Some(n))])
                        .unwrap_or_default(),
                    falsy: remaining
                        .map(|r| vec![(var_name.clone(), Some(r))])
                        .unwrap_or_default(),
                };
                return if op == "==" {
                    eq_refs
                } else {
                    eq_refs.inverted()
                };
            }
        }
        Refinements::empty()
    }

    /// Extract .has("key") refinements on shape types.
    fn extract_has_refinements(object: &SNode, args: &[SNode], scope: &TypeScope) -> Refinements {
        if let Node::Identifier(var_name) = &object.node {
            if let Node::StringLiteral(key) = &args[0].node {
                if let Some(Some(TypeExpr::Shape(fields))) = scope.get_var(var_name) {
                    if fields.iter().any(|f| f.name == *key && f.optional) {
                        let narrowed_fields: Vec<ShapeField> = fields
                            .iter()
                            .map(|f| {
                                if f.name == *key {
                                    ShapeField {
                                        name: f.name.clone(),
                                        type_expr: f.type_expr.clone(),
                                        optional: false,
                                    }
                                } else {
                                    f.clone()
                                }
                            })
                            .collect();
                        return Refinements {
                            truthy: vec![(
                                var_name.clone(),
                                Some(TypeExpr::Shape(narrowed_fields)),
                            )],
                            falsy: vec![],
                        };
                    }
                }
            }
        }
        Refinements::empty()
    }

    /// Check whether a block definitely exits (return/throw/break/continue).
    fn block_definitely_exits(stmts: &[SNode]) -> bool {
        stmts.iter().any(|s| match &s.node {
            Node::ReturnStmt { .. }
            | Node::ThrowStmt { .. }
            | Node::BreakStmt
            | Node::ContinueStmt => true,
            Node::IfElse {
                then_body,
                else_body: Some(else_body),
                ..
            } => Self::block_definitely_exits(then_body) && Self::block_definitely_exits(else_body),
            _ => false,
        })
    }

    fn check_match_exhaustiveness(
        &mut self,
        value: &SNode,
        arms: &[MatchArm],
        scope: &TypeScope,
        span: Span,
    ) {
        // Detect pattern: match <expr>.variant { "VariantA" -> ... }
        let enum_name = match &value.node {
            Node::PropertyAccess { object, property } if property == "variant" => {
                // Infer the type of the object
                match self.infer_type(object, scope) {
                    Some(TypeExpr::Named(name)) => {
                        if scope.get_enum(&name).is_some() {
                            Some(name)
                        } else {
                            None
                        }
                    }
                    _ => None,
                }
            }
            _ => {
                // Direct match on an enum value: match <expr> { ... }
                match self.infer_type(value, scope) {
                    Some(TypeExpr::Named(name)) if scope.get_enum(&name).is_some() => Some(name),
                    _ => None,
                }
            }
        };

        let Some(enum_name) = enum_name else {
            // Try union type exhaustiveness instead
            self.check_match_exhaustiveness_union(value, arms, scope, span);
            return;
        };
        let Some(variants) = scope.get_enum(&enum_name) else {
            return;
        };

        // Collect variant names covered by match arms
        let mut covered: Vec<String> = Vec::new();
        let mut has_wildcard = false;

        for arm in arms {
            match &arm.pattern.node {
                // String literal pattern (matching on .variant): "VariantA"
                Node::StringLiteral(s) => covered.push(s.clone()),
                // Identifier pattern acts as a wildcard/catch-all
                Node::Identifier(name) if name == "_" || !variants.contains(name) => {
                    has_wildcard = true;
                }
                // Direct enum construct pattern: EnumName.Variant
                Node::EnumConstruct { variant, .. } => covered.push(variant.clone()),
                // PropertyAccess pattern: EnumName.Variant (no args)
                Node::PropertyAccess { property, .. } => covered.push(property.clone()),
                _ => {
                    // Unknown pattern shape — conservatively treat as wildcard
                    has_wildcard = true;
                }
            }
        }

        if has_wildcard {
            return;
        }

        let missing: Vec<&String> = variants.iter().filter(|v| !covered.contains(v)).collect();
        if !missing.is_empty() {
            let missing_str = missing
                .iter()
                .map(|s| format!("\"{}\"", s))
                .collect::<Vec<_>>()
                .join(", ");
            self.warning_at(
                format!(
                    "Non-exhaustive match on enum {}: missing variants {}",
                    enum_name, missing_str
                ),
                span,
            );
        }
    }

    /// Check exhaustiveness for match on union types (e.g. `string | int | nil`).
    fn check_match_exhaustiveness_union(
        &mut self,
        value: &SNode,
        arms: &[MatchArm],
        scope: &TypeScope,
        span: Span,
    ) {
        let Some(TypeExpr::Union(members)) = self.infer_type(value, scope) else {
            return;
        };
        // Only check unions of named types (string, int, nil, bool, etc.)
        if !members.iter().all(|m| matches!(m, TypeExpr::Named(_))) {
            return;
        }

        let mut has_wildcard = false;
        let mut covered_types: Vec<String> = Vec::new();

        for arm in arms {
            match &arm.pattern.node {
                // type_of(x) == "string" style patterns are common but hard to detect here
                // Literal patterns cover specific types
                Node::NilLiteral => covered_types.push("nil".into()),
                Node::BoolLiteral(_) => {
                    if !covered_types.contains(&"bool".into()) {
                        covered_types.push("bool".into());
                    }
                }
                Node::IntLiteral(_) => {
                    if !covered_types.contains(&"int".into()) {
                        covered_types.push("int".into());
                    }
                }
                Node::FloatLiteral(_) => {
                    if !covered_types.contains(&"float".into()) {
                        covered_types.push("float".into());
                    }
                }
                Node::StringLiteral(_) => {
                    if !covered_types.contains(&"string".into()) {
                        covered_types.push("string".into());
                    }
                }
                Node::Identifier(name) if name == "_" => {
                    has_wildcard = true;
                }
                _ => {
                    has_wildcard = true;
                }
            }
        }

        if has_wildcard {
            return;
        }

        let type_names: Vec<&str> = members
            .iter()
            .filter_map(|m| match m {
                TypeExpr::Named(n) => Some(n.as_str()),
                _ => None,
            })
            .collect();
        let missing: Vec<&&str> = type_names
            .iter()
            .filter(|t| !covered_types.iter().any(|c| c == **t))
            .collect();
        if !missing.is_empty() {
            let missing_str = missing
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            self.warning_at(
                format!(
                    "Non-exhaustive match on union type: missing {}",
                    missing_str
                ),
                span,
            );
        }
    }

    fn check_call(&mut self, name: &str, args: &[SNode], scope: &mut TypeScope, span: Span) {
        // Check against known function signatures
        let has_spread = args.iter().any(|a| matches!(&a.node, Node::Spread(_)));
        if let Some(sig) = scope.get_fn(name).cloned() {
            if !has_spread
                && !is_builtin(name)
                && !sig.has_rest
                && (args.len() < sig.required_params || args.len() > sig.params.len())
            {
                let expected = if sig.required_params == sig.params.len() {
                    format!("{}", sig.params.len())
                } else {
                    format!("{}-{}", sig.required_params, sig.params.len())
                };
                self.warning_at(
                    format!(
                        "Function '{}' expects {} arguments, got {}",
                        name,
                        expected,
                        args.len()
                    ),
                    span,
                );
            }
            // Build a scope that includes the function's generic type params
            // so they are treated as compatible with any concrete type.
            let call_scope = if sig.type_param_names.is_empty() {
                scope.clone()
            } else {
                let mut s = scope.child();
                for tp_name in &sig.type_param_names {
                    s.generic_type_params.insert(tp_name.clone());
                }
                s
            };
            let mut type_bindings: BTreeMap<String, TypeExpr> = BTreeMap::new();
            let type_param_set: std::collections::BTreeSet<String> =
                sig.type_param_names.iter().cloned().collect();
            for (arg, (_param_name, param_type)) in args.iter().zip(sig.params.iter()) {
                if let Some(param_ty) = param_type {
                    if let Some(arg_ty) = self.infer_type(arg, scope) {
                        if let Err(message) = Self::extract_type_bindings(
                            param_ty,
                            &arg_ty,
                            &type_param_set,
                            &mut type_bindings,
                        ) {
                            self.error_at(message, arg.span);
                        }
                    }
                }
            }
            for (i, (arg, (param_name, param_type))) in
                args.iter().zip(sig.params.iter()).enumerate()
            {
                if let Some(expected) = param_type {
                    let actual = self.infer_type(arg, scope);
                    if let Some(actual) = &actual {
                        let expected = Self::apply_type_bindings(expected, &type_bindings);
                        if !self.types_compatible(&expected, actual, &call_scope) {
                            self.error_at(
                                format!(
                                    "Argument {} ('{}'): expected {}, got {}",
                                    i + 1,
                                    param_name,
                                    format_type(&expected),
                                    format_type(actual)
                                ),
                                arg.span,
                            );
                        }
                    }
                }
            }
            if !sig.where_clauses.is_empty() {
                for (type_param, bound) in &sig.where_clauses {
                    if let Some(concrete_type) = type_bindings.get(type_param) {
                        let concrete_name = format_type(concrete_type);
                        if let Some(reason) =
                            self.interface_mismatch_reason(&concrete_name, bound, scope)
                        {
                            self.error_at(
                                format!(
                                    "Type '{}' does not satisfy interface '{}': {} \
                                     (required by constraint `where {}: {}`)",
                                    concrete_name, bound, reason, type_param, bound
                                ),
                                span,
                            );
                        }
                    }
                }
            }
        }
        // Check args recursively
        for arg in args {
            self.check_node(arg, scope);
        }
    }

    /// Infer the type of an expression.
    fn infer_type(&self, snode: &SNode, scope: &TypeScope) -> InferredType {
        match &snode.node {
            Node::IntLiteral(_) => Some(TypeExpr::Named("int".into())),
            Node::FloatLiteral(_) => Some(TypeExpr::Named("float".into())),
            Node::StringLiteral(_) | Node::InterpolatedString(_) => {
                Some(TypeExpr::Named("string".into()))
            }
            Node::BoolLiteral(_) => Some(TypeExpr::Named("bool".into())),
            Node::NilLiteral => Some(TypeExpr::Named("nil".into())),
            Node::ListLiteral(items) => Some(self.infer_list_literal_type(items, scope)),
            Node::DictLiteral(entries) => {
                // Infer shape type when all keys are string literals
                let mut fields = Vec::new();
                for entry in entries {
                    let key = match &entry.key.node {
                        Node::StringLiteral(key) | Node::Identifier(key) => key.clone(),
                        _ => return Some(TypeExpr::Named("dict".into())),
                    };
                    let val_type = self
                        .infer_type(&entry.value, scope)
                        .unwrap_or(TypeExpr::Named("nil".into()));
                    fields.push(ShapeField {
                        name: key,
                        type_expr: val_type,
                        optional: false,
                    });
                }
                if !fields.is_empty() {
                    Some(TypeExpr::Shape(fields))
                } else {
                    Some(TypeExpr::Named("dict".into()))
                }
            }
            Node::Closure { params, body, .. } => {
                // If all params are typed and we can infer a return type, produce FnType
                let all_typed = params.iter().all(|p| p.type_expr.is_some());
                if all_typed && !params.is_empty() {
                    let param_types: Vec<TypeExpr> =
                        params.iter().filter_map(|p| p.type_expr.clone()).collect();
                    // Try to infer return type from last expression in body
                    let ret = body.last().and_then(|last| self.infer_type(last, scope));
                    if let Some(ret_type) = ret {
                        return Some(TypeExpr::FnType {
                            params: param_types,
                            return_type: Box::new(ret_type),
                        });
                    }
                }
                Some(TypeExpr::Named("closure".into()))
            }

            Node::Identifier(name) => scope.get_var(name).cloned().flatten(),

            Node::FunctionCall { name, args } => {
                // Struct constructor calls return the struct type
                if scope.get_struct(name).is_some() {
                    return Some(TypeExpr::Named(name.clone()));
                }
                // Check user-defined function return types
                if let Some(sig) = scope.get_fn(name) {
                    let mut return_type = sig.return_type.clone();
                    if let Some(ty) = return_type.take() {
                        if sig.type_param_names.is_empty() {
                            return Some(ty);
                        }
                        let mut bindings = BTreeMap::new();
                        let type_param_set: std::collections::BTreeSet<String> =
                            sig.type_param_names.iter().cloned().collect();
                        for (arg, (_param_name, param_type)) in args.iter().zip(sig.params.iter()) {
                            if let Some(param_ty) = param_type {
                                if let Some(arg_ty) = self.infer_type(arg, scope) {
                                    let _ = Self::extract_type_bindings(
                                        param_ty,
                                        &arg_ty,
                                        &type_param_set,
                                        &mut bindings,
                                    );
                                }
                            }
                        }
                        return Some(Self::apply_type_bindings(&ty, &bindings));
                    }
                    return None;
                }
                // Check builtin return types
                builtin_return_type(name)
            }

            Node::BinaryOp { op, left, right } => {
                let lt = self.infer_type(left, scope);
                let rt = self.infer_type(right, scope);
                infer_binary_op_type(op, &lt, &rt)
            }

            Node::UnaryOp { op, operand } => {
                let t = self.infer_type(operand, scope);
                match op.as_str() {
                    "!" => Some(TypeExpr::Named("bool".into())),
                    "-" => t, // negation preserves type
                    _ => None,
                }
            }

            Node::Ternary {
                condition,
                true_expr,
                false_expr,
            } => {
                let refs = Self::extract_refinements(condition, scope);

                let mut true_scope = scope.child();
                apply_refinements(&mut true_scope, &refs.truthy);
                let tt = self.infer_type(true_expr, &true_scope);

                let mut false_scope = scope.child();
                apply_refinements(&mut false_scope, &refs.falsy);
                let ft = self.infer_type(false_expr, &false_scope);

                match (&tt, &ft) {
                    (Some(a), Some(b)) if a == b => tt,
                    (Some(a), Some(b)) => Some(TypeExpr::Union(vec![a.clone(), b.clone()])),
                    (Some(_), None) => tt,
                    (None, Some(_)) => ft,
                    (None, None) => None,
                }
            }

            Node::EnumConstruct { enum_name, .. } => Some(TypeExpr::Named(enum_name.clone())),

            Node::PropertyAccess { object, property } => {
                // EnumName.Variant → infer as the enum type
                if let Node::Identifier(name) = &object.node {
                    if scope.get_enum(name).is_some() {
                        return Some(TypeExpr::Named(name.clone()));
                    }
                }
                // .variant on an enum value → string
                if property == "variant" {
                    let obj_type = self.infer_type(object, scope);
                    if let Some(TypeExpr::Named(name)) = &obj_type {
                        if scope.get_enum(name).is_some() {
                            return Some(TypeExpr::Named("string".into()));
                        }
                    }
                }
                // Shape field access: obj.field → field type
                let obj_type = self.infer_type(object, scope);
                if let Some(TypeExpr::Shape(fields)) = &obj_type {
                    if let Some(field) = fields.iter().find(|f| f.name == *property) {
                        return Some(field.type_expr.clone());
                    }
                }
                None
            }

            Node::SubscriptAccess { object, index } => {
                let obj_type = self.infer_type(object, scope);
                match &obj_type {
                    Some(TypeExpr::List(inner)) => Some(*inner.clone()),
                    Some(TypeExpr::DictType(_, v)) => Some(*v.clone()),
                    Some(TypeExpr::Shape(fields)) => {
                        // If index is a string literal, look up the field type
                        if let Node::StringLiteral(key) = &index.node {
                            fields
                                .iter()
                                .find(|f| &f.name == key)
                                .map(|f| f.type_expr.clone())
                        } else {
                            None
                        }
                    }
                    Some(TypeExpr::Named(n)) if n == "list" => None,
                    Some(TypeExpr::Named(n)) if n == "dict" => None,
                    Some(TypeExpr::Named(n)) if n == "string" => {
                        Some(TypeExpr::Named("string".into()))
                    }
                    _ => None,
                }
            }
            Node::SliceAccess { object, .. } => {
                // Slicing a list returns the same list type; slicing a string returns string
                let obj_type = self.infer_type(object, scope);
                match &obj_type {
                    Some(TypeExpr::List(_)) => obj_type,
                    Some(TypeExpr::Named(n)) if n == "list" => obj_type,
                    Some(TypeExpr::Named(n)) if n == "string" => {
                        Some(TypeExpr::Named("string".into()))
                    }
                    _ => None,
                }
            }
            Node::MethodCall { object, method, .. }
            | Node::OptionalMethodCall { object, method, .. } => {
                let obj_type = self.infer_type(object, scope);
                let is_dict = matches!(&obj_type, Some(TypeExpr::Named(n)) if n == "dict")
                    || matches!(&obj_type, Some(TypeExpr::DictType(..)))
                    || matches!(&obj_type, Some(TypeExpr::Shape(_)));
                match method.as_str() {
                    // Shared: bool-returning methods
                    "contains" | "starts_with" | "ends_with" | "empty" | "has" | "any" | "all" => {
                        Some(TypeExpr::Named("bool".into()))
                    }
                    // Shared: int-returning methods
                    "count" | "index_of" => Some(TypeExpr::Named("int".into())),
                    // String methods
                    "trim" | "lowercase" | "uppercase" | "reverse" | "replace" | "substring"
                    | "pad_left" | "pad_right" | "repeat" | "join" => {
                        Some(TypeExpr::Named("string".into()))
                    }
                    "split" | "chars" => Some(TypeExpr::Named("list".into())),
                    // filter returns dict for dicts, list for lists
                    "filter" => {
                        if is_dict {
                            Some(TypeExpr::Named("dict".into()))
                        } else {
                            Some(TypeExpr::Named("list".into()))
                        }
                    }
                    // List methods
                    "map" | "flat_map" | "sort" => Some(TypeExpr::Named("list".into())),
                    "reduce" | "find" | "first" | "last" => None,
                    // Dict methods
                    "keys" | "values" | "entries" => Some(TypeExpr::Named("list".into())),
                    "merge" | "map_values" | "rekey" | "map_keys" => {
                        // Rekey/map_keys transform keys; resulting dict still keys-by-string.
                        // Preserve the value-type parameter when known so downstream code can
                        // still rely on dict<string, V> typing after a key-rename.
                        if let Some(TypeExpr::DictType(_, v)) = &obj_type {
                            Some(TypeExpr::DictType(
                                Box::new(TypeExpr::Named("string".into())),
                                v.clone(),
                            ))
                        } else {
                            Some(TypeExpr::Named("dict".into()))
                        }
                    }
                    // Conversions
                    "to_string" => Some(TypeExpr::Named("string".into())),
                    "to_int" => Some(TypeExpr::Named("int".into())),
                    "to_float" => Some(TypeExpr::Named("float".into())),
                    _ => None,
                }
            }

            // TryOperator on Result<T, E> produces T
            Node::TryOperator { operand } => {
                match self.infer_type(operand, scope) {
                    Some(TypeExpr::Named(name)) if name == "Result" => None, // unknown inner type
                    _ => None,
                }
            }

            _ => None,
        }
    }

    /// Check if two types are compatible (actual can be assigned to expected).
    fn types_compatible(&self, expected: &TypeExpr, actual: &TypeExpr, scope: &TypeScope) -> bool {
        // Generic type parameters match anything.
        if let TypeExpr::Named(name) = expected {
            if scope.is_generic_type_param(name) {
                return true;
            }
        }
        if let TypeExpr::Named(name) = actual {
            if scope.is_generic_type_param(name) {
                return true;
            }
        }
        let expected = self.resolve_alias(expected, scope);
        let actual = self.resolve_alias(actual, scope);

        // Interface satisfaction: if expected is an interface name, check if actual type
        // has all required methods (Go-style implicit satisfaction).
        if let TypeExpr::Named(iface_name) = &expected {
            if scope.get_interface(iface_name).is_some() {
                if let TypeExpr::Named(type_name) = &actual {
                    return self.satisfies_interface(type_name, iface_name, scope);
                }
                return false;
            }
        }

        match (&expected, &actual) {
            (TypeExpr::Named(a), TypeExpr::Named(b)) => a == b || (a == "float" && b == "int"),
            // Union-to-Union: every member of actual must be compatible with
            // at least one member of expected.
            (TypeExpr::Union(exp_members), TypeExpr::Union(act_members)) => {
                act_members.iter().all(|am| {
                    exp_members
                        .iter()
                        .any(|em| self.types_compatible(em, am, scope))
                })
            }
            (TypeExpr::Union(members), actual_type) => members
                .iter()
                .any(|m| self.types_compatible(m, actual_type, scope)),
            (expected_type, TypeExpr::Union(members)) => members
                .iter()
                .all(|m| self.types_compatible(expected_type, m, scope)),
            (TypeExpr::Shape(_), TypeExpr::Named(n)) if n == "dict" => true,
            (TypeExpr::Named(n), TypeExpr::Shape(_)) if n == "dict" => true,
            (TypeExpr::Shape(ef), TypeExpr::Shape(af)) => ef.iter().all(|expected_field| {
                if expected_field.optional {
                    return true;
                }
                af.iter().any(|actual_field| {
                    actual_field.name == expected_field.name
                        && self.types_compatible(
                            &expected_field.type_expr,
                            &actual_field.type_expr,
                            scope,
                        )
                })
            }),
            // dict<K, V> expected, Shape actual → all field values must match V
            (TypeExpr::DictType(ek, ev), TypeExpr::Shape(af)) => {
                let keys_ok = matches!(ek.as_ref(), TypeExpr::Named(n) if n == "string");
                keys_ok
                    && af
                        .iter()
                        .all(|f| self.types_compatible(ev, &f.type_expr, scope))
            }
            // Shape expected, dict<K, V> actual → gradual: allow since dict may have the fields
            (TypeExpr::Shape(_), TypeExpr::DictType(_, _)) => true,
            (TypeExpr::List(expected_inner), TypeExpr::List(actual_inner)) => {
                self.types_compatible(expected_inner, actual_inner, scope)
            }
            (TypeExpr::Named(n), TypeExpr::List(_)) if n == "list" => true,
            (TypeExpr::List(_), TypeExpr::Named(n)) if n == "list" => true,
            (TypeExpr::DictType(ek, ev), TypeExpr::DictType(ak, av)) => {
                self.types_compatible(ek, ak, scope) && self.types_compatible(ev, av, scope)
            }
            (TypeExpr::Named(n), TypeExpr::DictType(_, _)) if n == "dict" => true,
            (TypeExpr::DictType(_, _), TypeExpr::Named(n)) if n == "dict" => true,
            // FnType compatibility: params match positionally and return types match
            (
                TypeExpr::FnType {
                    params: ep,
                    return_type: er,
                },
                TypeExpr::FnType {
                    params: ap,
                    return_type: ar,
                },
            ) => {
                ep.len() == ap.len()
                    && ep
                        .iter()
                        .zip(ap.iter())
                        .all(|(e, a)| self.types_compatible(e, a, scope))
                    && self.types_compatible(er, ar, scope)
            }
            // FnType is compatible with Named("closure") for backward compat
            (TypeExpr::FnType { .. }, TypeExpr::Named(n)) if n == "closure" => true,
            (TypeExpr::Named(n), TypeExpr::FnType { .. }) if n == "closure" => true,
            _ => false,
        }
    }

    fn resolve_alias<'a>(&self, ty: &'a TypeExpr, scope: &'a TypeScope) -> TypeExpr {
        if let TypeExpr::Named(name) = ty {
            if let Some(resolved) = scope.resolve_type(name) {
                return resolved.clone();
            }
        }
        ty.clone()
    }

    fn error_at(&mut self, message: String, span: Span) {
        self.diagnostics.push(TypeDiagnostic {
            message,
            severity: DiagnosticSeverity::Error,
            span: Some(span),
            help: None,
            fix: None,
        });
    }

    #[allow(dead_code)]
    fn error_at_with_help(&mut self, message: String, span: Span, help: String) {
        self.diagnostics.push(TypeDiagnostic {
            message,
            severity: DiagnosticSeverity::Error,
            span: Some(span),
            help: Some(help),
            fix: None,
        });
    }

    fn error_at_with_fix(&mut self, message: String, span: Span, fix: Vec<FixEdit>) {
        self.diagnostics.push(TypeDiagnostic {
            message,
            severity: DiagnosticSeverity::Error,
            span: Some(span),
            help: None,
            fix: Some(fix),
        });
    }

    fn warning_at(&mut self, message: String, span: Span) {
        self.diagnostics.push(TypeDiagnostic {
            message,
            severity: DiagnosticSeverity::Warning,
            span: Some(span),
            help: None,
            fix: None,
        });
    }

    #[allow(dead_code)]
    fn warning_at_with_help(&mut self, message: String, span: Span, help: String) {
        self.diagnostics.push(TypeDiagnostic {
            message,
            severity: DiagnosticSeverity::Warning,
            span: Some(span),
            help: Some(help),
            fix: None,
        });
    }

    /// Recursively validate binary operations in an expression tree.
    /// Unlike `check_node`, this only checks BinaryOp type compatibility
    /// without triggering other validations (e.g., function call arg checks).
    fn check_binops(&mut self, snode: &SNode, scope: &mut TypeScope) {
        match &snode.node {
            Node::BinaryOp { op, left, right } => {
                self.check_binops(left, scope);
                self.check_binops(right, scope);
                let lt = self.infer_type(left, scope);
                let rt = self.infer_type(right, scope);
                if let (Some(TypeExpr::Named(l)), Some(TypeExpr::Named(r))) = (&lt, &rt) {
                    let span = snode.span;
                    match op.as_str() {
                        "+" => {
                            let valid = matches!(
                                (l.as_str(), r.as_str()),
                                ("int" | "float", "int" | "float")
                                    | ("string", "string")
                                    | ("list", "list")
                                    | ("dict", "dict")
                            );
                            if !valid {
                                let msg =
                                    format!("Operator '+' is not valid for types {} and {}", l, r);
                                let fix = if l == "string" || r == "string" {
                                    self.build_interpolation_fix(left, right, l == "string", span)
                                } else {
                                    None
                                };
                                if let Some(fix) = fix {
                                    self.error_at_with_fix(msg, span, fix);
                                } else {
                                    self.error_at(msg, span);
                                }
                            }
                        }
                        "-" | "/" | "%" => {
                            let numeric = ["int", "float"];
                            if !numeric.contains(&l.as_str()) || !numeric.contains(&r.as_str()) {
                                self.error_at(
                                    format!(
                                        "Operator '{}' requires numeric operands, got {} and {}",
                                        op, l, r
                                    ),
                                    span,
                                );
                            }
                        }
                        "*" => {
                            let numeric = ["int", "float"];
                            let is_numeric =
                                numeric.contains(&l.as_str()) && numeric.contains(&r.as_str());
                            let is_string_repeat =
                                (l == "string" && r == "int") || (l == "int" && r == "string");
                            if !is_numeric && !is_string_repeat {
                                self.error_at(
                                    format!(
                                        "Operator '*' requires numeric operands or string * int, got {} and {}",
                                        l, r
                                    ),
                                    span,
                                );
                            }
                        }
                        _ => {}
                    }
                }
            }
            // Recurse into sub-expressions that might contain BinaryOps
            Node::UnaryOp { operand, .. } => self.check_binops(operand, scope),
            _ => {}
        }
    }

    /// Build a fix that converts `"str" + expr` or `expr + "str"` to string interpolation.
    fn build_interpolation_fix(
        &self,
        left: &SNode,
        right: &SNode,
        left_is_string: bool,
        expr_span: Span,
    ) -> Option<Vec<FixEdit>> {
        let src = self.source.as_ref()?;
        let (str_node, other_node) = if left_is_string {
            (left, right)
        } else {
            (right, left)
        };
        let str_text = src.get(str_node.span.start..str_node.span.end)?;
        let other_text = src.get(other_node.span.start..other_node.span.end)?;
        // Only handle simple double-quoted strings (not multiline/raw)
        let inner = str_text.strip_prefix('"')?.strip_suffix('"')?;
        // Skip if the expression contains characters that would break interpolation
        if other_text.contains('}') || other_text.contains('"') {
            return None;
        }
        let replacement = if left_is_string {
            format!("\"{inner}${{{other_text}}}\"")
        } else {
            format!("\"${{{other_text}}}{inner}\"")
        };
        Some(vec![FixEdit {
            span: expr_span,
            replacement,
        }])
    }
}

impl Default for TypeChecker {
    fn default() -> Self {
        Self::new()
    }
}

/// Infer the result type of a binary operation.
fn infer_binary_op_type(op: &str, left: &InferredType, right: &InferredType) -> InferredType {
    match op {
        "==" | "!=" | "<" | ">" | "<=" | ">=" | "&&" | "||" | "in" | "not_in" => {
            Some(TypeExpr::Named("bool".into()))
        }
        "+" => match (left, right) {
            (Some(TypeExpr::Named(l)), Some(TypeExpr::Named(r))) => {
                match (l.as_str(), r.as_str()) {
                    ("int", "int") => Some(TypeExpr::Named("int".into())),
                    ("float", _) | (_, "float") => Some(TypeExpr::Named("float".into())),
                    ("string", "string") => Some(TypeExpr::Named("string".into())),
                    ("list", "list") => Some(TypeExpr::Named("list".into())),
                    ("dict", "dict") => Some(TypeExpr::Named("dict".into())),
                    _ => None,
                }
            }
            _ => None,
        },
        "-" | "/" | "%" => match (left, right) {
            (Some(TypeExpr::Named(l)), Some(TypeExpr::Named(r))) => {
                match (l.as_str(), r.as_str()) {
                    ("int", "int") => Some(TypeExpr::Named("int".into())),
                    ("float", _) | (_, "float") => Some(TypeExpr::Named("float".into())),
                    _ => None,
                }
            }
            _ => None,
        },
        "*" => match (left, right) {
            (Some(TypeExpr::Named(l)), Some(TypeExpr::Named(r))) => {
                match (l.as_str(), r.as_str()) {
                    ("string", "int") | ("int", "string") => Some(TypeExpr::Named("string".into())),
                    ("int", "int") => Some(TypeExpr::Named("int".into())),
                    ("float", _) | (_, "float") => Some(TypeExpr::Named("float".into())),
                    _ => None,
                }
            }
            _ => None,
        },
        "??" => match (left, right) {
            // Union containing nil: strip nil, use non-nil members
            (Some(TypeExpr::Union(members)), _) => {
                let non_nil: Vec<_> = members
                    .iter()
                    .filter(|m| !matches!(m, TypeExpr::Named(n) if n == "nil"))
                    .cloned()
                    .collect();
                if non_nil.len() == 1 {
                    Some(non_nil[0].clone())
                } else if non_nil.is_empty() {
                    right.clone()
                } else {
                    Some(TypeExpr::Union(non_nil))
                }
            }
            // Left is nil: result is always the right side
            (Some(TypeExpr::Named(n)), _) if n == "nil" => right.clone(),
            // Left is a known non-nil type: right is unreachable, preserve left
            (Some(l), _) => Some(l.clone()),
            // Unknown left: use right as best guess
            (None, _) => right.clone(),
        },
        "|>" => None,
        _ => None,
    }
}

/// Format a type expression for display in error messages.
/// Produce a detail string describing why a Shape type is incompatible with
/// another Shape type — e.g. "missing field 'age' (int)" or "field 'name'
/// has type int, expected string".  Returns `None` if both types are not shapes.
pub fn shape_mismatch_detail(expected: &TypeExpr, actual: &TypeExpr) -> Option<String> {
    if let (TypeExpr::Shape(ef), TypeExpr::Shape(af)) = (expected, actual) {
        let mut details = Vec::new();
        for field in ef {
            if field.optional {
                continue;
            }
            match af.iter().find(|f| f.name == field.name) {
                None => details.push(format!(
                    "missing field '{}' ({})",
                    field.name,
                    format_type(&field.type_expr)
                )),
                Some(actual_field) => {
                    let e_str = format_type(&field.type_expr);
                    let a_str = format_type(&actual_field.type_expr);
                    if e_str != a_str {
                        details.push(format!(
                            "field '{}' has type {}, expected {}",
                            field.name, a_str, e_str
                        ));
                    }
                }
            }
        }
        if details.is_empty() {
            None
        } else {
            Some(details.join("; "))
        }
    } else {
        None
    }
}

/// Returns true when the type is obvious from the RHS expression
/// (e.g. `let x = 42` is obviously int — no hint needed).
fn is_obvious_type(value: &SNode, _ty: &TypeExpr) -> bool {
    matches!(
        &value.node,
        Node::IntLiteral(_)
            | Node::FloatLiteral(_)
            | Node::StringLiteral(_)
            | Node::BoolLiteral(_)
            | Node::NilLiteral
            | Node::ListLiteral(_)
            | Node::DictLiteral(_)
            | Node::InterpolatedString(_)
    )
}

pub fn format_type(ty: &TypeExpr) -> String {
    match ty {
        TypeExpr::Named(n) => n.clone(),
        TypeExpr::Union(types) => types
            .iter()
            .map(format_type)
            .collect::<Vec<_>>()
            .join(" | "),
        TypeExpr::Shape(fields) => {
            let inner: Vec<String> = fields
                .iter()
                .map(|f| {
                    let opt = if f.optional { "?" } else { "" };
                    format!("{}{opt}: {}", f.name, format_type(&f.type_expr))
                })
                .collect();
            format!("{{{}}}", inner.join(", "))
        }
        TypeExpr::List(inner) => format!("list<{}>", format_type(inner)),
        TypeExpr::DictType(k, v) => format!("dict<{}, {}>", format_type(k), format_type(v)),
        TypeExpr::FnType {
            params,
            return_type,
        } => {
            let params_str = params
                .iter()
                .map(format_type)
                .collect::<Vec<_>>()
                .join(", ");
            format!("fn({}) -> {}", params_str, format_type(return_type))
        }
    }
}

/// Remove a named type from a union, collapsing single-element unions.
fn remove_from_union(members: &[TypeExpr], to_remove: &str) -> InferredType {
    let remaining: Vec<TypeExpr> = members
        .iter()
        .filter(|m| !matches!(m, TypeExpr::Named(n) if n == to_remove))
        .cloned()
        .collect();
    match remaining.len() {
        0 => None,
        1 => Some(remaining.into_iter().next().unwrap()),
        _ => Some(TypeExpr::Union(remaining)),
    }
}

/// Narrow a union to just one named type, if that type is a member.
fn narrow_to_single(members: &[TypeExpr], target: &str) -> InferredType {
    if members
        .iter()
        .any(|m| matches!(m, TypeExpr::Named(n) if n == target))
    {
        Some(TypeExpr::Named(target.to_string()))
    } else {
        None
    }
}

/// Extract the variable name from a `type_of(x)` call.
fn extract_type_of_var(node: &SNode) -> Option<String> {
    if let Node::FunctionCall { name, args } = &node.node {
        if name == "type_of" && args.len() == 1 {
            if let Node::Identifier(var) = &args[0].node {
                return Some(var.clone());
            }
        }
    }
    None
}

/// Apply a list of refinements to a scope, tracking pre-narrowing types.
fn apply_refinements(scope: &mut TypeScope, refinements: &[(String, InferredType)]) {
    for (var_name, narrowed_type) in refinements {
        // Save the pre-narrowing type so we can restore it on reassignment
        if !scope.narrowed_vars.contains_key(var_name) {
            if let Some(original) = scope.get_var(var_name).cloned() {
                scope.narrowed_vars.insert(var_name.clone(), original);
            }
        }
        scope.define_var(var_name, narrowed_type.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Parser;
    use harn_lexer::Lexer;

    fn check_source(source: &str) -> Vec<TypeDiagnostic> {
        let mut lexer = Lexer::new(source);
        let tokens = lexer.tokenize().unwrap();
        let mut parser = Parser::new(tokens);
        let program = parser.parse().unwrap();
        TypeChecker::new().check(&program)
    }

    fn errors(source: &str) -> Vec<String> {
        check_source(source)
            .into_iter()
            .filter(|d| d.severity == DiagnosticSeverity::Error)
            .map(|d| d.message)
            .collect()
    }

    #[test]
    fn test_no_errors_for_untyped_code() {
        let errs = errors("pipeline t(task) { let x = 42\nlog(x) }");
        assert!(errs.is_empty());
    }

    #[test]
    fn test_correct_typed_let() {
        let errs = errors("pipeline t(task) { let x: int = 42 }");
        assert!(errs.is_empty());
    }

    #[test]
    fn test_type_mismatch_let() {
        let errs = errors(r#"pipeline t(task) { let x: int = "hello" }"#);
        assert_eq!(errs.len(), 1);
        assert!(errs[0].contains("Type mismatch"));
        assert!(errs[0].contains("int"));
        assert!(errs[0].contains("string"));
    }

    #[test]
    fn test_correct_typed_fn() {
        let errs = errors(
            "pipeline t(task) { fn add(a: int, b: int) -> int { return a + b }\nadd(1, 2) }",
        );
        assert!(errs.is_empty());
    }

    #[test]
    fn test_fn_arg_type_mismatch() {
        let errs = errors(
            r#"pipeline t(task) { fn add(a: int, b: int) -> int { return a + b }
add("hello", 2) }"#,
        );
        assert_eq!(errs.len(), 1);
        assert!(errs[0].contains("Argument 1"));
        assert!(errs[0].contains("expected int"));
    }

    #[test]
    fn test_return_type_mismatch() {
        let errs = errors(r#"pipeline t(task) { fn get() -> int { return "hello" } }"#);
        assert_eq!(errs.len(), 1);
        assert!(errs[0].contains("Return type mismatch"));
    }

    #[test]
    fn test_union_type_compatible() {
        let errs = errors(r#"pipeline t(task) { let x: string | nil = nil }"#);
        assert!(errs.is_empty());
    }

    #[test]
    fn test_union_type_mismatch() {
        let errs = errors(r#"pipeline t(task) { let x: string | nil = 42 }"#);
        assert_eq!(errs.len(), 1);
        assert!(errs[0].contains("Type mismatch"));
    }

    #[test]
    fn test_type_inference_propagation() {
        let errs = errors(
            r#"pipeline t(task) {
  fn add(a: int, b: int) -> int { return a + b }
  let result: string = add(1, 2)
}"#,
        );
        assert_eq!(errs.len(), 1);
        assert!(errs[0].contains("Type mismatch"));
        assert!(errs[0].contains("string"));
        assert!(errs[0].contains("int"));
    }

    #[test]
    fn test_generic_return_type_instantiates_from_callsite() {
        let errs = errors(
            r#"pipeline t(task) {
  fn identity<T>(x: T) -> T { return x }
  fn first<T>(items: list<T>) -> T { return items[0] }
  let n: int = identity(42)
  let s: string = first(["a", "b"])
}"#,
        );
        assert!(errs.is_empty(), "unexpected type errors: {errs:?}");
    }

    #[test]
    fn test_generic_type_param_must_bind_consistently() {
        let errs = errors(
            r#"pipeline t(task) {
  fn keep<T>(a: T, b: T) -> T { return a }
  keep(1, "x")
}"#,
        );
        assert_eq!(errs.len(), 2, "expected 2 errors, got: {:?}", errs);
        assert!(
            errs.iter()
                .any(|err| err.contains("type parameter 'T' was inferred as both int and string")),
            "missing generic binding conflict error: {:?}",
            errs
        );
        assert!(
            errs.iter()
                .any(|err| err.contains("Argument 2 ('b'): expected int, got string")),
            "missing instantiated argument mismatch error: {:?}",
            errs
        );
    }

    #[test]
    fn test_generic_list_binding_propagates_element_type() {
        let errs = errors(
            r#"pipeline t(task) {
  fn first<T>(items: list<T>) -> T { return items[0] }
  let bad: string = first([1, 2, 3])
}"#,
        );
        assert_eq!(errs.len(), 1, "expected 1 error, got: {:?}", errs);
        assert!(errs[0].contains("declared as string, but assigned int"));
    }

    #[test]
    fn test_builtin_return_type_inference() {
        let errs = errors(r#"pipeline t(task) { let x: string = to_int("42") }"#);
        assert_eq!(errs.len(), 1);
        assert!(errs[0].contains("string"));
        assert!(errs[0].contains("int"));
    }

    #[test]
    fn test_workflow_and_transcript_builtins_are_known() {
        let errs = errors(
            r#"pipeline t(task) {
  let flow = workflow_graph({name: "demo", entry: "act", nodes: {act: {kind: "stage"}}})
  let report: dict = workflow_policy_report(flow, {tools: tool_registry(), capabilities: {workspace: ["read_text"]}})
  let run: dict = workflow_execute("task", flow, [], {})
  let tree: dict = load_run_tree("run.json")
  let fixture: dict = run_record_fixture(run?.run)
  let suite: dict = run_record_eval_suite([{run: run?.run, fixture: fixture}])
  let diff: dict = run_record_diff(run?.run, run?.run)
  let manifest: dict = eval_suite_manifest({cases: [{run_path: "run.json"}]})
  let suite_report: dict = eval_suite_run(manifest)
  let wf: dict = artifact_workspace_file("src/main.rs", "fn main() {}", {source: "host"})
  let snap: dict = artifact_workspace_snapshot(["src/main.rs"], "snapshot")
  let selection: dict = artifact_editor_selection("src/main.rs", "main")
  let verify: dict = artifact_verification_result("verify", "ok")
  let test_result: dict = artifact_test_result("tests", "pass")
  let cmd: dict = artifact_command_result("cargo test", {status: 0})
  let patch: dict = artifact_diff("src/main.rs", "old", "new")
  let git: dict = artifact_git_diff("diff --git a b")
  let review: dict = artifact_diff_review(patch, "review me")
  let decision: dict = artifact_review_decision(review, "accepted")
  let proposal: dict = artifact_patch_proposal(review, "*** Begin Patch")
  let bundle: dict = artifact_verification_bundle("checks", [{name: "fmt", ok: true}])
  let apply: dict = artifact_apply_intent(review, "apply")
  let transcript = transcript_reset({metadata: {source: "test"}})
  let visible: string = transcript_render_visible(transcript_archive(transcript))
  let events: list = transcript_events(transcript)
  let context: string = artifact_context([], {max_artifacts: 1})
  println(report)
  println(run)
  println(tree)
  println(fixture)
  println(suite)
  println(diff)
  println(manifest)
  println(suite_report)
  println(wf)
  println(snap)
  println(selection)
  println(verify)
  println(test_result)
  println(cmd)
  println(patch)
  println(git)
  println(review)
  println(decision)
  println(proposal)
  println(bundle)
  println(apply)
  println(visible)
  println(events)
  println(context)
}"#,
        );
        assert!(errs.is_empty(), "unexpected type errors: {errs:?}");
    }

    #[test]
    fn test_binary_op_type_inference() {
        let errs = errors("pipeline t(task) { let x: string = 1 + 2 }");
        assert_eq!(errs.len(), 1);
    }

    #[test]
    fn test_comparison_returns_bool() {
        let errs = errors("pipeline t(task) { let x: bool = 1 < 2 }");
        assert!(errs.is_empty());
    }

    #[test]
    fn test_int_float_promotion() {
        let errs = errors("pipeline t(task) { let x: float = 42 }");
        assert!(errs.is_empty());
    }

    #[test]
    fn test_untyped_code_no_errors() {
        let errs = errors(
            r#"pipeline t(task) {
  fn process(data) {
    let result = data + " processed"
    return result
  }
  log(process("hello"))
}"#,
        );
        assert!(errs.is_empty());
    }

    #[test]
    fn test_type_alias() {
        let errs = errors(
            r#"pipeline t(task) {
  type Name = string
  let x: Name = "hello"
}"#,
        );
        assert!(errs.is_empty());
    }

    #[test]
    fn test_type_alias_mismatch() {
        let errs = errors(
            r#"pipeline t(task) {
  type Name = string
  let x: Name = 42
}"#,
        );
        assert_eq!(errs.len(), 1);
    }

    #[test]
    fn test_assignment_type_check() {
        let errs = errors(
            r#"pipeline t(task) {
  var x: int = 0
  x = "hello"
}"#,
        );
        assert_eq!(errs.len(), 1);
        assert!(errs[0].contains("cannot assign string"));
    }

    #[test]
    fn test_covariance_int_to_float_in_fn() {
        let errs = errors(
            "pipeline t(task) { fn scale(x: float) -> float { return x * 2.0 }\nscale(42) }",
        );
        assert!(errs.is_empty());
    }

    #[test]
    fn test_covariance_return_type() {
        let errs = errors("pipeline t(task) { fn get() -> float { return 42 } }");
        assert!(errs.is_empty());
    }

    #[test]
    fn test_no_contravariance_float_to_int() {
        let errs = errors("pipeline t(task) { fn add(a: int) -> int { return a + 1 }\nadd(3.14) }");
        assert_eq!(errs.len(), 1);
    }

    // --- Exhaustiveness checking tests ---

    fn warnings(source: &str) -> Vec<String> {
        check_source(source)
            .into_iter()
            .filter(|d| d.severity == DiagnosticSeverity::Warning)
            .map(|d| d.message)
            .collect()
    }

    #[test]
    fn test_exhaustive_match_no_warning() {
        let warns = warnings(
            r#"pipeline t(task) {
  enum Color { Red, Green, Blue }
  let c = Color.Red
  match c.variant {
    "Red" -> { log("r") }
    "Green" -> { log("g") }
    "Blue" -> { log("b") }
  }
}"#,
        );
        let exhaustive_warns: Vec<_> = warns
            .iter()
            .filter(|w| w.contains("Non-exhaustive"))
            .collect();
        assert!(exhaustive_warns.is_empty());
    }

    #[test]
    fn test_non_exhaustive_match_warning() {
        let warns = warnings(
            r#"pipeline t(task) {
  enum Color { Red, Green, Blue }
  let c = Color.Red
  match c.variant {
    "Red" -> { log("r") }
    "Green" -> { log("g") }
  }
}"#,
        );
        let exhaustive_warns: Vec<_> = warns
            .iter()
            .filter(|w| w.contains("Non-exhaustive"))
            .collect();
        assert_eq!(exhaustive_warns.len(), 1);
        assert!(exhaustive_warns[0].contains("Blue"));
    }

    #[test]
    fn test_non_exhaustive_multiple_missing() {
        let warns = warnings(
            r#"pipeline t(task) {
  enum Status { Active, Inactive, Pending }
  let s = Status.Active
  match s.variant {
    "Active" -> { log("a") }
  }
}"#,
        );
        let exhaustive_warns: Vec<_> = warns
            .iter()
            .filter(|w| w.contains("Non-exhaustive"))
            .collect();
        assert_eq!(exhaustive_warns.len(), 1);
        assert!(exhaustive_warns[0].contains("Inactive"));
        assert!(exhaustive_warns[0].contains("Pending"));
    }

    #[test]
    fn test_enum_construct_type_inference() {
        let errs = errors(
            r#"pipeline t(task) {
  enum Color { Red, Green, Blue }
  let c: Color = Color.Red
}"#,
        );
        assert!(errs.is_empty());
    }

    // --- Type narrowing tests ---

    #[test]
    fn test_nil_coalescing_strips_nil() {
        // After ??, nil should be stripped from the type
        let errs = errors(
            r#"pipeline t(task) {
  let x: string | nil = nil
  let y: string = x ?? "default"
}"#,
        );
        assert!(errs.is_empty());
    }

    #[test]
    fn test_shape_mismatch_detail_missing_field() {
        let errs = errors(
            r#"pipeline t(task) {
  let x: {name: string, age: int} = {name: "hello"}
}"#,
        );
        assert_eq!(errs.len(), 1);
        assert!(
            errs[0].contains("missing field 'age'"),
            "expected detail about missing field, got: {}",
            errs[0]
        );
    }

    #[test]
    fn test_shape_mismatch_detail_wrong_type() {
        let errs = errors(
            r#"pipeline t(task) {
  let x: {name: string, age: int} = {name: 42, age: 10}
}"#,
        );
        assert_eq!(errs.len(), 1);
        assert!(
            errs[0].contains("field 'name' has type int, expected string"),
            "expected detail about wrong type, got: {}",
            errs[0]
        );
    }

    // --- Match pattern type validation tests ---

    #[test]
    fn test_match_pattern_string_against_int() {
        let warns = warnings(
            r#"pipeline t(task) {
  let x: int = 42
  match x {
    "hello" -> { log("bad") }
    42 -> { log("ok") }
  }
}"#,
        );
        let pattern_warns: Vec<_> = warns
            .iter()
            .filter(|w| w.contains("Match pattern type mismatch"))
            .collect();
        assert_eq!(pattern_warns.len(), 1);
        assert!(pattern_warns[0].contains("matching int against string literal"));
    }

    #[test]
    fn test_match_pattern_int_against_string() {
        let warns = warnings(
            r#"pipeline t(task) {
  let x: string = "hello"
  match x {
    42 -> { log("bad") }
    "hello" -> { log("ok") }
  }
}"#,
        );
        let pattern_warns: Vec<_> = warns
            .iter()
            .filter(|w| w.contains("Match pattern type mismatch"))
            .collect();
        assert_eq!(pattern_warns.len(), 1);
        assert!(pattern_warns[0].contains("matching string against int literal"));
    }

    #[test]
    fn test_match_pattern_bool_against_int() {
        let warns = warnings(
            r#"pipeline t(task) {
  let x: int = 42
  match x {
    true -> { log("bad") }
    42 -> { log("ok") }
  }
}"#,
        );
        let pattern_warns: Vec<_> = warns
            .iter()
            .filter(|w| w.contains("Match pattern type mismatch"))
            .collect();
        assert_eq!(pattern_warns.len(), 1);
        assert!(pattern_warns[0].contains("matching int against bool literal"));
    }

    #[test]
    fn test_match_pattern_float_against_string() {
        let warns = warnings(
            r#"pipeline t(task) {
  let x: string = "hello"
  match x {
    3.14 -> { log("bad") }
    "hello" -> { log("ok") }
  }
}"#,
        );
        let pattern_warns: Vec<_> = warns
            .iter()
            .filter(|w| w.contains("Match pattern type mismatch"))
            .collect();
        assert_eq!(pattern_warns.len(), 1);
        assert!(pattern_warns[0].contains("matching string against float literal"));
    }

    #[test]
    fn test_match_pattern_int_against_float_ok() {
        // int and float are compatible for match patterns
        let warns = warnings(
            r#"pipeline t(task) {
  let x: float = 3.14
  match x {
    42 -> { log("ok") }
    _ -> { log("default") }
  }
}"#,
        );
        let pattern_warns: Vec<_> = warns
            .iter()
            .filter(|w| w.contains("Match pattern type mismatch"))
            .collect();
        assert!(pattern_warns.is_empty());
    }

    #[test]
    fn test_match_pattern_float_against_int_ok() {
        // float and int are compatible for match patterns
        let warns = warnings(
            r#"pipeline t(task) {
  let x: int = 42
  match x {
    3.14 -> { log("close") }
    _ -> { log("default") }
  }
}"#,
        );
        let pattern_warns: Vec<_> = warns
            .iter()
            .filter(|w| w.contains("Match pattern type mismatch"))
            .collect();
        assert!(pattern_warns.is_empty());
    }

    #[test]
    fn test_match_pattern_correct_types_no_warning() {
        let warns = warnings(
            r#"pipeline t(task) {
  let x: int = 42
  match x {
    1 -> { log("one") }
    2 -> { log("two") }
    _ -> { log("other") }
  }
}"#,
        );
        let pattern_warns: Vec<_> = warns
            .iter()
            .filter(|w| w.contains("Match pattern type mismatch"))
            .collect();
        assert!(pattern_warns.is_empty());
    }

    #[test]
    fn test_match_pattern_wildcard_no_warning() {
        let warns = warnings(
            r#"pipeline t(task) {
  let x: int = 42
  match x {
    _ -> { log("catch all") }
  }
}"#,
        );
        let pattern_warns: Vec<_> = warns
            .iter()
            .filter(|w| w.contains("Match pattern type mismatch"))
            .collect();
        assert!(pattern_warns.is_empty());
    }

    #[test]
    fn test_match_pattern_untyped_no_warning() {
        // When value has no known type, no warning should be emitted
        let warns = warnings(
            r#"pipeline t(task) {
  let x = some_unknown_fn()
  match x {
    "hello" -> { log("string") }
    42 -> { log("int") }
  }
}"#,
        );
        let pattern_warns: Vec<_> = warns
            .iter()
            .filter(|w| w.contains("Match pattern type mismatch"))
            .collect();
        assert!(pattern_warns.is_empty());
    }

    // --- Interface constraint type checking tests ---

    fn iface_errors(source: &str) -> Vec<String> {
        errors(source)
            .into_iter()
            .filter(|message| message.contains("does not satisfy interface"))
            .collect()
    }

    #[test]
    fn test_interface_constraint_return_type_mismatch() {
        let warns = iface_errors(
            r#"pipeline t(task) {
  interface Sizable {
    fn size(self) -> int
  }
  struct Box { width: int }
  impl Box {
    fn size(self) -> string { return "nope" }
  }
  fn measure<T>(item: T) where T: Sizable { log(item.size()) }
  measure(Box({width: 3}))
}"#,
        );
        assert_eq!(warns.len(), 1, "expected 1 warning, got: {:?}", warns);
        assert!(
            warns[0].contains("method 'size' returns 'string', expected 'int'"),
            "unexpected message: {}",
            warns[0]
        );
    }

    #[test]
    fn test_interface_constraint_param_type_mismatch() {
        let warns = iface_errors(
            r#"pipeline t(task) {
  interface Processor {
    fn process(self, x: int) -> string
  }
  struct MyProc { name: string }
  impl MyProc {
    fn process(self, x: string) -> string { return x }
  }
  fn run_proc<T>(p: T) where T: Processor { log(p.process(42)) }
  run_proc(MyProc({name: "a"}))
}"#,
        );
        assert_eq!(warns.len(), 1, "expected 1 warning, got: {:?}", warns);
        assert!(
            warns[0].contains("method 'process' parameter 1 has type 'string', expected 'int'"),
            "unexpected message: {}",
            warns[0]
        );
    }

    #[test]
    fn test_interface_constraint_missing_method() {
        let warns = iface_errors(
            r#"pipeline t(task) {
  interface Sizable {
    fn size(self) -> int
  }
  struct Box { width: int }
  impl Box {
    fn area(self) -> int { return self.width }
  }
  fn measure<T>(item: T) where T: Sizable { log(item.size()) }
  measure(Box({width: 3}))
}"#,
        );
        assert_eq!(warns.len(), 1, "expected 1 warning, got: {:?}", warns);
        assert!(
            warns[0].contains("missing method 'size'"),
            "unexpected message: {}",
            warns[0]
        );
    }

    #[test]
    fn test_interface_constraint_param_count_mismatch() {
        let warns = iface_errors(
            r#"pipeline t(task) {
  interface Doubler {
    fn double(self, x: int) -> int
  }
  struct Bad { v: int }
  impl Bad {
    fn double(self) -> int { return self.v * 2 }
  }
  fn run_double<T>(d: T) where T: Doubler { log(d.double(3)) }
  run_double(Bad({v: 5}))
}"#,
        );
        assert_eq!(warns.len(), 1, "expected 1 warning, got: {:?}", warns);
        assert!(
            warns[0].contains("method 'double' has 0 parameter(s), expected 1"),
            "unexpected message: {}",
            warns[0]
        );
    }

    #[test]
    fn test_interface_constraint_satisfied() {
        let warns = iface_errors(
            r#"pipeline t(task) {
  interface Sizable {
    fn size(self) -> int
  }
  struct Box { width: int, height: int }
  impl Box {
    fn size(self) -> int { return self.width * self.height }
  }
  fn measure<T>(item: T) where T: Sizable { log(item.size()) }
  measure(Box({width: 3, height: 4}))
}"#,
        );
        assert!(warns.is_empty(), "expected no warnings, got: {:?}", warns);
    }

    #[test]
    fn test_interface_constraint_untyped_impl_compatible() {
        // Gradual typing: untyped impl return should not trigger warning
        let warns = iface_errors(
            r#"pipeline t(task) {
  interface Sizable {
    fn size(self) -> int
  }
  struct Box { width: int }
  impl Box {
    fn size(self) { return self.width }
  }
  fn measure<T>(item: T) where T: Sizable { log(item.size()) }
  measure(Box({width: 3}))
}"#,
        );
        assert!(warns.is_empty(), "expected no warnings, got: {:?}", warns);
    }

    #[test]
    fn test_interface_constraint_int_float_covariance() {
        // int is compatible with float (covariance)
        let warns = iface_errors(
            r#"pipeline t(task) {
  interface Measurable {
    fn value(self) -> float
  }
  struct Gauge { v: int }
  impl Gauge {
    fn value(self) -> int { return self.v }
  }
  fn read_val<T>(g: T) where T: Measurable { log(g.value()) }
  read_val(Gauge({v: 42}))
}"#,
        );
        assert!(warns.is_empty(), "expected no warnings, got: {:?}", warns);
    }

    // --- Flow-sensitive type refinement tests ---

    #[test]
    fn test_nil_narrowing_then_branch() {
        // Existing behavior: x != nil narrows to string in then-branch
        let errs = errors(
            r#"pipeline t(task) {
  fn greet(name: string | nil) {
    if name != nil {
      let s: string = name
    }
  }
}"#,
        );
        assert!(errs.is_empty(), "got: {:?}", errs);
    }

    #[test]
    fn test_nil_narrowing_else_branch() {
        // NEW: x != nil narrows to nil in else-branch
        let errs = errors(
            r#"pipeline t(task) {
  fn check(x: string | nil) {
    if x != nil {
      let s: string = x
    } else {
      let n: nil = x
    }
  }
}"#,
        );
        assert!(errs.is_empty(), "got: {:?}", errs);
    }

    #[test]
    fn test_nil_equality_narrows_both() {
        // x == nil narrows then to nil, else to non-nil
        let errs = errors(
            r#"pipeline t(task) {
  fn check(x: string | nil) {
    if x == nil {
      let n: nil = x
    } else {
      let s: string = x
    }
  }
}"#,
        );
        assert!(errs.is_empty(), "got: {:?}", errs);
    }

    #[test]
    fn test_truthiness_narrowing() {
        // Bare identifier in condition removes nil
        let errs = errors(
            r#"pipeline t(task) {
  fn check(x: string | nil) {
    if x {
      let s: string = x
    }
  }
}"#,
        );
        assert!(errs.is_empty(), "got: {:?}", errs);
    }

    #[test]
    fn test_negation_narrowing() {
        // !x swaps truthy/falsy
        let errs = errors(
            r#"pipeline t(task) {
  fn check(x: string | nil) {
    if !x {
      let n: nil = x
    } else {
      let s: string = x
    }
  }
}"#,
        );
        assert!(errs.is_empty(), "got: {:?}", errs);
    }

    #[test]
    fn test_typeof_narrowing() {
        // type_of(x) == "string" narrows to string
        let errs = errors(
            r#"pipeline t(task) {
  fn check(x: string | int) {
    if type_of(x) == "string" {
      let s: string = x
    }
  }
}"#,
        );
        assert!(errs.is_empty(), "got: {:?}", errs);
    }

    #[test]
    fn test_typeof_narrowing_else() {
        // else removes the tested type
        let errs = errors(
            r#"pipeline t(task) {
  fn check(x: string | int) {
    if type_of(x) == "string" {
      let s: string = x
    } else {
      let i: int = x
    }
  }
}"#,
        );
        assert!(errs.is_empty(), "got: {:?}", errs);
    }

    #[test]
    fn test_typeof_neq_narrowing() {
        // type_of(x) != "string" removes string in then, narrows to string in else
        let errs = errors(
            r#"pipeline t(task) {
  fn check(x: string | int) {
    if type_of(x) != "string" {
      let i: int = x
    } else {
      let s: string = x
    }
  }
}"#,
        );
        assert!(errs.is_empty(), "got: {:?}", errs);
    }

    #[test]
    fn test_and_combines_narrowing() {
        // && combines truthy refinements
        let errs = errors(
            r#"pipeline t(task) {
  fn check(x: string | int | nil) {
    if x != nil && type_of(x) == "string" {
      let s: string = x
    }
  }
}"#,
        );
        assert!(errs.is_empty(), "got: {:?}", errs);
    }

    #[test]
    fn test_or_falsy_narrowing() {
        // || combines falsy refinements
        let errs = errors(
            r#"pipeline t(task) {
  fn check(x: string | nil, y: int | nil) {
    if x || y {
      // conservative: can't narrow
    } else {
      let xn: nil = x
      let yn: nil = y
    }
  }
}"#,
        );
        assert!(errs.is_empty(), "got: {:?}", errs);
    }

    #[test]
    fn test_guard_narrows_outer_scope() {
        let errs = errors(
            r#"pipeline t(task) {
  fn check(x: string | nil) {
    guard x != nil else { return }
    let s: string = x
  }
}"#,
        );
        assert!(errs.is_empty(), "got: {:?}", errs);
    }

    #[test]
    fn test_while_narrows_body() {
        let errs = errors(
            r#"pipeline t(task) {
  fn check(x: string | nil) {
    while x != nil {
      let s: string = x
      break
    }
  }
}"#,
        );
        assert!(errs.is_empty(), "got: {:?}", errs);
    }

    #[test]
    fn test_early_return_narrows_after_if() {
        // if then-body returns, falsy refinements apply after
        let errs = errors(
            r#"pipeline t(task) {
  fn check(x: string | nil) -> string {
    if x == nil {
      return "default"
    }
    let s: string = x
    return s
  }
}"#,
        );
        assert!(errs.is_empty(), "got: {:?}", errs);
    }

    #[test]
    fn test_early_throw_narrows_after_if() {
        let errs = errors(
            r#"pipeline t(task) {
  fn check(x: string | nil) {
    if x == nil {
      throw "missing"
    }
    let s: string = x
  }
}"#,
        );
        assert!(errs.is_empty(), "got: {:?}", errs);
    }

    #[test]
    fn test_no_narrowing_unknown_type() {
        // Gradual typing: untyped vars don't get narrowed
        let errs = errors(
            r#"pipeline t(task) {
  fn check(x) {
    if x != nil {
      let s: string = x
    }
  }
}"#,
        );
        // No narrowing possible, so assigning untyped x to string should be fine
        // (gradual typing allows it)
        assert!(errs.is_empty(), "got: {:?}", errs);
    }

    #[test]
    fn test_reassignment_invalidates_narrowing() {
        // After reassigning a narrowed var, the original type should be restored
        let errs = errors(
            r#"pipeline t(task) {
  fn check(x: string | nil) {
    var y: string | nil = x
    if y != nil {
      let s: string = y
      y = nil
      let s2: string = y
    }
  }
}"#,
        );
        // s2 should fail because y was reassigned, invalidating the narrowing
        assert_eq!(errs.len(), 1, "expected 1 error, got: {:?}", errs);
        assert!(
            errs[0].contains("Type mismatch"),
            "expected type mismatch, got: {}",
            errs[0]
        );
    }

    #[test]
    fn test_let_immutable_warning() {
        let all = check_source(
            r#"pipeline t(task) {
  let x = 42
  x = 43
}"#,
        );
        let warnings: Vec<_> = all
            .iter()
            .filter(|d| d.severity == DiagnosticSeverity::Warning)
            .collect();
        assert!(
            warnings.iter().any(|w| w.message.contains("immutable")),
            "expected immutability warning, got: {:?}",
            warnings
        );
    }

    #[test]
    fn test_nested_narrowing() {
        let errs = errors(
            r#"pipeline t(task) {
  fn check(x: string | int | nil) {
    if x != nil {
      if type_of(x) == "int" {
        let i: int = x
      }
    }
  }
}"#,
        );
        assert!(errs.is_empty(), "got: {:?}", errs);
    }

    #[test]
    fn test_match_narrows_arms() {
        let errs = errors(
            r#"pipeline t(task) {
  fn check(x: string | int) {
    match x {
      "hello" -> {
        let s: string = x
      }
      42 -> {
        let i: int = x
      }
      _ -> {}
    }
  }
}"#,
        );
        assert!(errs.is_empty(), "got: {:?}", errs);
    }

    #[test]
    fn test_has_narrows_optional_field() {
        let errs = errors(
            r#"pipeline t(task) {
  fn check(x: {name?: string, age: int}) {
    if x.has("name") {
      let n: {name: string, age: int} = x
    }
  }
}"#,
        );
        assert!(errs.is_empty(), "got: {:?}", errs);
    }

    // -----------------------------------------------------------------------
    // Autofix tests
    // -----------------------------------------------------------------------

    fn check_source_with_source(source: &str) -> Vec<TypeDiagnostic> {
        let mut lexer = Lexer::new(source);
        let tokens = lexer.tokenize().unwrap();
        let mut parser = Parser::new(tokens);
        let program = parser.parse().unwrap();
        TypeChecker::new().check_with_source(&program, source)
    }

    #[test]
    fn test_fix_string_plus_int_literal() {
        let source = "pipeline t(task) {\n  let x = \"hello \" + 42\n  log(x)\n}";
        let diags = check_source_with_source(source);
        let fixable: Vec<_> = diags.iter().filter(|d| d.fix.is_some()).collect();
        assert_eq!(fixable.len(), 1, "expected 1 fixable diagnostic");
        let fix = fixable[0].fix.as_ref().unwrap();
        assert_eq!(fix.len(), 1);
        assert_eq!(fix[0].replacement, "\"hello ${42}\"");
    }

    #[test]
    fn test_fix_int_plus_string_literal() {
        let source = "pipeline t(task) {\n  let x = 42 + \"hello\"\n  log(x)\n}";
        let diags = check_source_with_source(source);
        let fixable: Vec<_> = diags.iter().filter(|d| d.fix.is_some()).collect();
        assert_eq!(fixable.len(), 1, "expected 1 fixable diagnostic");
        let fix = fixable[0].fix.as_ref().unwrap();
        assert_eq!(fix[0].replacement, "\"${42}hello\"");
    }

    #[test]
    fn test_fix_string_plus_variable() {
        let source = "pipeline t(task) {\n  let n: int = 5\n  let x = \"count: \" + n\n  log(x)\n}";
        let diags = check_source_with_source(source);
        let fixable: Vec<_> = diags.iter().filter(|d| d.fix.is_some()).collect();
        assert_eq!(fixable.len(), 1, "expected 1 fixable diagnostic");
        let fix = fixable[0].fix.as_ref().unwrap();
        assert_eq!(fix[0].replacement, "\"count: ${n}\"");
    }

    #[test]
    fn test_no_fix_int_plus_int() {
        // int + float should error but no interpolation fix
        let source = "pipeline t(task) {\n  let x: int = 5\n  let y: float = 3.0\n  let z = x - y\n  log(z)\n}";
        let diags = check_source_with_source(source);
        let fixable: Vec<_> = diags.iter().filter(|d| d.fix.is_some()).collect();
        assert!(
            fixable.is_empty(),
            "no fix expected for numeric ops, got: {fixable:?}"
        );
    }

    #[test]
    fn test_no_fix_without_source() {
        let source = "pipeline t(task) {\n  let x = \"hello \" + 42\n  log(x)\n}";
        let diags = check_source(source);
        let fixable: Vec<_> = diags.iter().filter(|d| d.fix.is_some()).collect();
        assert!(
            fixable.is_empty(),
            "without source, no fix should be generated"
        );
    }

    // --- Union exhaustiveness tests ---

    #[test]
    fn test_union_exhaustive_match_no_warning() {
        let warns = warnings(
            r#"pipeline t(task) {
  let x: string | int | nil = nil
  match x {
    "hello" -> { log("s") }
    42 -> { log("i") }
    nil -> { log("n") }
  }
}"#,
        );
        let union_warns: Vec<_> = warns
            .iter()
            .filter(|w| w.contains("Non-exhaustive match on union"))
            .collect();
        assert!(union_warns.is_empty());
    }

    #[test]
    fn test_union_non_exhaustive_match_warning() {
        let warns = warnings(
            r#"pipeline t(task) {
  let x: string | int | nil = nil
  match x {
    "hello" -> { log("s") }
    42 -> { log("i") }
  }
}"#,
        );
        let union_warns: Vec<_> = warns
            .iter()
            .filter(|w| w.contains("Non-exhaustive match on union"))
            .collect();
        assert_eq!(union_warns.len(), 1);
        assert!(union_warns[0].contains("nil"));
    }

    // --- Nil-coalescing type inference tests ---

    #[test]
    fn test_nil_coalesce_non_union_preserves_left_type() {
        // When left is a known non-nil type, ?? should preserve it
        let errs = errors(
            r#"pipeline t(task) {
  let x: int = 42
  let y: int = x ?? 0
}"#,
        );
        assert!(errs.is_empty());
    }

    #[test]
    fn test_nil_coalesce_nil_returns_right_type() {
        let errs = errors(
            r#"pipeline t(task) {
  let x: string = nil ?? "fallback"
}"#,
        );
        assert!(errs.is_empty());
    }
}
