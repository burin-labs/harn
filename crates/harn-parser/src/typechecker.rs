use std::collections::BTreeMap;

use crate::ast::*;
use harn_lexer::Span;

/// A diagnostic produced by the type checker.
#[derive(Debug, Clone)]
pub struct TypeDiagnostic {
    pub message: String,
    pub severity: DiagnosticSeverity,
    pub span: Option<Span>,
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
    parent: Option<Box<TypeScope>>,
}

#[derive(Debug, Clone)]
struct FnSignature {
    params: Vec<(String, InferredType)>,
    return_type: InferredType,
}

impl TypeScope {
    fn new() -> Self {
        Self {
            vars: BTreeMap::new(),
            functions: BTreeMap::new(),
            type_aliases: BTreeMap::new(),
            enums: BTreeMap::new(),
            parent: None,
        }
    }

    fn child(&self) -> Self {
        Self {
            vars: BTreeMap::new(),
            functions: BTreeMap::new(),
            type_aliases: BTreeMap::new(),
            enums: BTreeMap::new(),
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

    fn get_enum(&self, name: &str) -> Option<&Vec<String>> {
        self.enums
            .get(name)
            .or_else(|| self.parent.as_ref()?.get_enum(name))
    }

    fn define_var(&mut self, name: &str, ty: InferredType) {
        self.vars.insert(name.to_string(), ty);
    }

    fn define_fn(&mut self, name: &str, sig: FnSignature) {
        self.functions.insert(name.to_string(), sig);
    }
}

/// Known return types for builtin functions.
fn builtin_return_type(name: &str) -> InferredType {
    match name {
        "log" | "print" | "println" | "write_file" | "sleep" | "cancel" | "exit" => {
            Some(TypeExpr::Named("nil".into()))
        }
        "type_of" | "to_string" | "json_stringify" | "read_file" | "http_get" | "http_post"
        | "llm_call" | "agent_loop" | "regex_replace" => Some(TypeExpr::Named("string".into())),
        "to_int" => Some(TypeExpr::Named("int".into())),
        "to_float" | "timestamp" => Some(TypeExpr::Named("float".into())),
        "env" | "regex_match" => Some(TypeExpr::Union(vec![
            TypeExpr::Named("string".into()),
            TypeExpr::Named("nil".into()),
        ])),
        "json_parse" => None, // could be any type
        _ => None,
    }
}

/// Check if a name is a known builtin.
fn is_builtin(name: &str) -> bool {
    matches!(
        name,
        "log"
            | "print"
            | "println"
            | "type_of"
            | "to_string"
            | "to_int"
            | "to_float"
            | "json_stringify"
            | "json_parse"
            | "env"
            | "timestamp"
            | "sleep"
            | "read_file"
            | "write_file"
            | "exit"
            | "regex_match"
            | "regex_replace"
            | "http_get"
            | "http_post"
            | "llm_call"
            | "agent_loop"
            | "await"
            | "cancel"
    )
}

/// The static type checker.
pub struct TypeChecker {
    diagnostics: Vec<TypeDiagnostic>,
    scope: TypeScope,
}

impl TypeChecker {
    pub fn new() -> Self {
        Self {
            diagnostics: Vec::new(),
            scope: TypeScope::new(),
        }
    }

    /// Check a program and return diagnostics.
    pub fn check(mut self, program: &[SNode]) -> Vec<TypeDiagnostic> {
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
                Node::Pipeline { body, .. } => {
                    let mut child = self.scope.child();
                    self.check_block(body, &mut child);
                }
                Node::FnDecl {
                    name,
                    params,
                    return_type,
                    body,
                } => {
                    let sig = FnSignature {
                        params: params
                            .iter()
                            .map(|p| (p.name.clone(), p.type_expr.clone()))
                            .collect(),
                        return_type: return_type.clone(),
                    };
                    self.scope.define_fn(name, sig);
                    self.check_fn_body(params, return_type, body);
                }
                _ => {
                    self.check_node(snode, &mut self.scope.clone());
                }
            }
        }

        self.diagnostics
    }

    /// Register type and enum declarations from AST nodes into a scope.
    fn register_declarations_into(scope: &mut TypeScope, nodes: &[SNode]) {
        for snode in nodes {
            match &snode.node {
                Node::TypeDecl { name, type_expr } => {
                    scope.type_aliases.insert(name.clone(), type_expr.clone());
                }
                Node::EnumDecl { name, variants } => {
                    let variant_names: Vec<String> =
                        variants.iter().map(|v| v.name.clone()).collect();
                    scope.enums.insert(name.clone(), variant_names);
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

    fn check_node(&mut self, snode: &SNode, scope: &mut TypeScope) {
        let span = snode.span;
        match &snode.node {
            Node::LetBinding {
                name,
                type_ann,
                value,
            } => {
                let inferred = self.infer_type(value, scope);
                if let Some(expected) = type_ann {
                    if let Some(actual) = &inferred {
                        if !self.types_compatible(expected, actual, scope) {
                            self.error_at(
                                format!(
                                    "Type mismatch: '{}' declared as {}, but assigned {}",
                                    name,
                                    format_type(expected),
                                    format_type(actual)
                                ),
                                span,
                            );
                        }
                    }
                }
                let ty = type_ann.clone().or(inferred);
                scope.define_var(name, ty);
            }

            Node::VarBinding {
                name,
                type_ann,
                value,
            } => {
                let inferred = self.infer_type(value, scope);
                if let Some(expected) = type_ann {
                    if let Some(actual) = &inferred {
                        if !self.types_compatible(expected, actual, scope) {
                            self.error_at(
                                format!(
                                    "Type mismatch: '{}' declared as {}, but assigned {}",
                                    name,
                                    format_type(expected),
                                    format_type(actual)
                                ),
                                span,
                            );
                        }
                    }
                }
                let ty = type_ann.clone().or(inferred);
                scope.define_var(name, ty);
            }

            Node::FnDecl {
                name,
                params,
                return_type,
                body,
            } => {
                let sig = FnSignature {
                    params: params
                        .iter()
                        .map(|p| (p.name.clone(), p.type_expr.clone()))
                        .collect(),
                    return_type: return_type.clone(),
                };
                scope.define_fn(name, sig.clone());
                scope.define_var(name, None);
                self.check_fn_body(params, return_type, body);
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
                let mut then_scope = scope.child();
                self.check_block(then_body, &mut then_scope);
                if let Some(else_body) = else_body {
                    let mut else_scope = scope.child();
                    self.check_block(else_body, &mut else_scope);
                }
            }

            Node::ForIn {
                variable,
                iterable,
                body,
            } => {
                self.check_node(iterable, scope);
                let mut loop_scope = scope.child();
                loop_scope.define_var(variable, None);
                self.check_block(body, &mut loop_scope);
            }

            Node::WhileLoop { condition, body } => {
                self.check_node(condition, scope);
                let mut loop_scope = scope.child();
                self.check_block(body, &mut loop_scope);
            }

            Node::TryCatch {
                body,
                error_var,
                catch_body,
                ..
            } => {
                let mut try_scope = scope.child();
                self.check_block(body, &mut try_scope);
                let mut catch_scope = scope.child();
                if let Some(var) = error_var {
                    catch_scope.define_var(var, None);
                }
                self.check_block(catch_body, &mut catch_scope);
            }

            Node::ReturnStmt {
                value: Some(val), ..
            } => {
                self.check_node(val, scope);
            }

            Node::Assignment { target, value } => {
                self.check_node(value, scope);
                if let Node::Identifier(name) = &target.node {
                    if let Some(Some(var_type)) = scope.get_var(name) {
                        let assigned = self.infer_type(value, scope);
                        if let Some(actual) = &assigned {
                            if !self.types_compatible(var_type, actual, scope) {
                                self.error_at(
                                    format!(
                                        "Type mismatch: cannot assign {} to '{}' (declared as {})",
                                        format_type(actual),
                                        name,
                                        format_type(var_type)
                                    ),
                                    span,
                                );
                            }
                        }
                    }
                }
            }

            Node::TypeDecl { name, type_expr } => {
                scope.type_aliases.insert(name.clone(), type_expr.clone());
            }

            Node::EnumDecl { name, variants } => {
                let variant_names: Vec<String> = variants.iter().map(|v| v.name.clone()).collect();
                scope.enums.insert(name.clone(), variant_names);
            }

            Node::MatchExpr { value, arms } => {
                self.check_node(value, scope);
                for arm in arms {
                    self.check_node(&arm.pattern, scope);
                    let mut arm_scope = scope.child();
                    self.check_block(&arm.body, &mut arm_scope);
                }
                self.check_match_exhaustiveness(value, arms, scope, span);
            }

            // Recurse into nested expressions
            Node::BinaryOp { left, right, .. } => {
                self.check_node(left, scope);
                self.check_node(right, scope);
            }
            Node::UnaryOp { operand, .. } => {
                self.check_node(operand, scope);
            }
            Node::MethodCall { object, args, .. } => {
                self.check_node(object, scope);
                for arg in args {
                    self.check_node(arg, scope);
                }
            }
            Node::PropertyAccess { object, .. } => {
                self.check_node(object, scope);
            }
            Node::SubscriptAccess { object, index } => {
                self.check_node(object, scope);
                self.check_node(index, scope);
            }

            // Terminals — nothing to check
            _ => {}
        }
    }

    fn check_fn_body(
        &mut self,
        params: &[TypedParam],
        return_type: &Option<TypeExpr>,
        body: &[SNode],
    ) {
        let mut fn_scope = self.scope.child();
        for param in params {
            fn_scope.define_var(&param.name, param.type_expr.clone());
        }
        self.check_block(body, &mut fn_scope);

        // Check return statements against declared return type
        if let Some(ret_type) = return_type {
            for stmt in body {
                self.check_return_type(stmt, ret_type, &fn_scope);
            }
        }
    }

    fn check_return_type(&mut self, snode: &SNode, expected: &TypeExpr, scope: &TypeScope) {
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
                then_body,
                else_body,
                ..
            } => {
                for stmt in then_body {
                    self.check_return_type(stmt, expected, scope);
                }
                if let Some(else_body) = else_body {
                    for stmt in else_body {
                        self.check_return_type(stmt, expected, scope);
                    }
                }
            }
            _ => {}
        }
    }

    /// Check if a match expression on an enum's `.variant` property covers all variants.
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

    fn check_call(&mut self, name: &str, args: &[SNode], scope: &mut TypeScope, span: Span) {
        // Check against known function signatures
        if let Some(sig) = scope.get_fn(name).cloned() {
            if args.len() != sig.params.len() && !is_builtin(name) {
                self.warning_at(
                    format!(
                        "Function '{}' expects {} arguments, got {}",
                        name,
                        sig.params.len(),
                        args.len()
                    ),
                    span,
                );
            }
            for (i, (arg, (param_name, param_type))) in
                args.iter().zip(sig.params.iter()).enumerate()
            {
                if let Some(expected) = param_type {
                    let actual = self.infer_type(arg, scope);
                    if let Some(actual) = &actual {
                        if !self.types_compatible(expected, actual, scope) {
                            self.error_at(
                                format!(
                                    "Argument {} ('{}'): expected {}, got {}",
                                    i + 1,
                                    param_name,
                                    format_type(expected),
                                    format_type(actual)
                                ),
                                arg.span,
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
            Node::ListLiteral(_) => Some(TypeExpr::Named("list".into())),
            Node::DictLiteral(_) => Some(TypeExpr::Named("dict".into())),
            Node::Closure { .. } => Some(TypeExpr::Named("closure".into())),

            Node::Identifier(name) => scope.get_var(name).cloned().flatten(),

            Node::FunctionCall { name, .. } => {
                // Check user-defined function return types
                if let Some(sig) = scope.get_fn(name) {
                    return sig.return_type.clone();
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
                true_expr,
                false_expr,
                ..
            } => {
                let tt = self.infer_type(true_expr, scope);
                let ft = self.infer_type(false_expr, scope);
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
                None
            }

            Node::SubscriptAccess { .. } => None,
            Node::MethodCall { .. } => None,

            _ => None,
        }
    }

    /// Check if two types are compatible (actual can be assigned to expected).
    fn types_compatible(&self, expected: &TypeExpr, actual: &TypeExpr, scope: &TypeScope) -> bool {
        let expected = self.resolve_alias(expected, scope);
        let actual = self.resolve_alias(actual, scope);

        match (&expected, &actual) {
            (TypeExpr::Named(a), TypeExpr::Named(b)) => a == b || (a == "float" && b == "int"),
            (TypeExpr::Union(members), actual_type) => members
                .iter()
                .any(|m| self.types_compatible(m, actual_type, scope)),
            (expected_type, TypeExpr::Union(members)) => members
                .iter()
                .all(|m| self.types_compatible(expected_type, m, scope)),
            (TypeExpr::Shape(_), TypeExpr::Named(n)) if n == "dict" => true,
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
            (TypeExpr::List(expected_inner), TypeExpr::List(actual_inner)) => {
                self.types_compatible(expected_inner, actual_inner, scope)
            }
            (TypeExpr::Named(n), TypeExpr::List(_)) if n == "list" => true,
            (TypeExpr::List(_), TypeExpr::Named(n)) if n == "list" => true,
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
        });
    }

    fn warning_at(&mut self, message: String, span: Span) {
        self.diagnostics.push(TypeDiagnostic {
            message,
            severity: DiagnosticSeverity::Warning,
            span: Some(span),
        });
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
        "==" | "!=" | "<" | ">" | "<=" | ">=" | "&&" | "||" => Some(TypeExpr::Named("bool".into())),
        "+" => match (left, right) {
            (Some(TypeExpr::Named(l)), Some(TypeExpr::Named(r))) => {
                match (l.as_str(), r.as_str()) {
                    ("int", "int") => Some(TypeExpr::Named("int".into())),
                    ("float", _) | (_, "float") => Some(TypeExpr::Named("float".into())),
                    ("string", _) => Some(TypeExpr::Named("string".into())),
                    ("list", "list") => Some(TypeExpr::Named("list".into())),
                    _ => Some(TypeExpr::Named("string".into())),
                }
            }
            _ => None,
        },
        "-" | "*" => match (left, right) {
            (Some(TypeExpr::Named(l)), Some(TypeExpr::Named(r))) => {
                match (l.as_str(), r.as_str()) {
                    ("int", "int") => Some(TypeExpr::Named("int".into())),
                    ("float", _) | (_, "float") => Some(TypeExpr::Named("float".into())),
                    _ => None,
                }
            }
            _ => None,
        },
        "/" => match (left, right) {
            (Some(TypeExpr::Named(l)), Some(TypeExpr::Named(r))) => {
                match (l.as_str(), r.as_str()) {
                    ("int", "int") => Some(TypeExpr::Named("int".into())),
                    ("float", _) | (_, "float") => Some(TypeExpr::Named("float".into())),
                    _ => None,
                }
            }
            _ => None,
        },
        "??" => match (left, right) {
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
            _ => right.clone(),
        },
        "|>" => None,
        _ => None,
    }
}

/// Format a type expression for display in error messages.
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
        TypeExpr::List(inner) => format!("list[{}]", format_type(inner)),
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
    fn test_builtin_return_type_inference() {
        let errs = errors(r#"pipeline t(task) { let x: string = to_int("42") }"#);
        assert_eq!(errs.len(), 1);
        assert!(errs[0].contains("string"));
        assert!(errs[0].contains("int"));
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
}
