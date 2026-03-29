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
    /// Interface declarations: name → method signatures.
    interfaces: BTreeMap<String, Vec<InterfaceMethod>>,
    /// Struct declarations: name → field types.
    structs: BTreeMap<String, Vec<(String, InferredType)>>,
    /// Generic type parameter names in scope (treated as compatible with any type).
    generic_type_params: std::collections::BTreeSet<String>,
    parent: Option<Box<TypeScope>>,
}

#[derive(Debug, Clone)]
struct FnSignature {
    params: Vec<(String, InferredType)>,
    return_type: InferredType,
    /// Generic type parameter names declared on the function.
    type_param_names: Vec<String>,
    /// Number of required parameters (those without defaults).
    required_params: usize,
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
            generic_type_params: std::collections::BTreeSet::new(),
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
            generic_type_params: std::collections::BTreeSet::new(),
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

    fn get_enum(&self, name: &str) -> Option<&Vec<String>> {
        self.enums
            .get(name)
            .or_else(|| self.parent.as_ref()?.get_enum(name))
    }

    #[allow(dead_code)]
    fn get_interface(&self, name: &str) -> Option<&Vec<InterfaceMethod>> {
        self.interfaces
            .get(name)
            .or_else(|| self.parent.as_ref()?.get_interface(name))
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
        "log" | "print" | "println" | "write_file" | "sleep" | "cancel" | "exit"
        | "delete_file" | "mkdir" | "copy_file" | "append_file" => {
            Some(TypeExpr::Named("nil".into()))
        }
        "type_of" | "to_string" | "json_stringify" | "read_file" | "http_get" | "http_post"
        | "llm_call" | "regex_replace" | "path_join" | "temp_dir"
        | "date_format" | "format" | "compute_content_hash" => {
            Some(TypeExpr::Named("string".into()))
        }
        "to_int" | "timer_end" | "elapsed" => Some(TypeExpr::Named("int".into())),
        "to_float" | "timestamp" | "date_parse" => Some(TypeExpr::Named("float".into())),
        "file_exists" | "json_validate" => Some(TypeExpr::Named("bool".into())),
        "list_dir" | "mcp_list_tools" | "mcp_list_resources" | "mcp_list_prompts" => {
            Some(TypeExpr::Named("list".into()))
        }
        "stat" | "exec" | "shell" | "date_now" | "agent_loop" | "llm_info" | "llm_usage"
        | "timer_start" | "metadata_get" | "mcp_server_info" | "mcp_get_prompt" => {
            Some(TypeExpr::Named("dict".into()))
        }
        "metadata_set" | "metadata_save" | "metadata_refresh_hashes"
        | "invalidate_facts" | "log_json" | "mcp_disconnect" => {
            Some(TypeExpr::Named("nil".into()))
        }
        "env" | "regex_match" => Some(TypeExpr::Union(vec![
            TypeExpr::Named("string".into()),
            TypeExpr::Named("nil".into()),
        ])),
        "json_parse" | "json_extract" => None, // could be any type
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
            | "file_exists"
            | "delete_file"
            | "list_dir"
            | "mkdir"
            | "path_join"
            | "copy_file"
            | "append_file"
            | "temp_dir"
            | "stat"
            | "exec"
            | "shell"
            | "date_now"
            | "date_format"
            | "date_parse"
            | "format"
            | "json_validate"
            | "json_extract"
            | "trim"
            | "lowercase"
            | "uppercase"
            | "split"
            | "starts_with"
            | "ends_with"
            | "contains"
            | "replace"
            | "join"
            | "len"
            | "substring"
            | "dirname"
            | "basename"
            | "extname"
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
                    body,
                    ..
                } => {
                    let required_params = params
                        .iter()
                        .filter(|p| p.default_value.is_none())
                        .count();
                    let sig = FnSignature {
                        params: params
                            .iter()
                            .map(|p| (p.name.clone(), p.type_expr.clone()))
                            .collect(),
                        return_type: return_type.clone(),
                        type_param_names: type_params.iter().map(|tp| tp.name.clone()).collect(),
                        required_params,
                    };
                    self.scope.define_fn(name, sig);
                    self.check_fn_body(type_params, params, return_type, body);
                }
                _ => {
                    let mut scope = self.scope.clone();
                    self.check_node(snode, &mut scope);
                    // Merge any new definitions back into the top-level scope
                    for (name, ty) in scope.vars {
                        self.scope.vars.entry(name).or_insert(ty);
                    }
                }
            }
        }

        self.diagnostics
    }

    /// Register type, enum, interface, and struct declarations from AST nodes into a scope.
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
                Node::InterfaceDecl { name, methods } => {
                    scope.interfaces.insert(name.clone(), methods.clone());
                }
                Node::StructDecl { name, fields } => {
                    let field_types: Vec<(String, InferredType)> = fields
                        .iter()
                        .map(|f| (f.name.clone(), f.type_expr.clone()))
                        .collect();
                    scope.structs.insert(name.clone(), field_types);
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
    fn define_pattern_vars(pattern: &BindingPattern, scope: &mut TypeScope) {
        match pattern {
            BindingPattern::Identifier(name) => {
                scope.define_var(name, None);
            }
            BindingPattern::Dict(fields) => {
                for field in fields {
                    let name = field.alias.as_deref().unwrap_or(&field.key);
                    scope.define_var(name, None);
                }
            }
            BindingPattern::List(elements) => {
                for elem in elements {
                    scope.define_var(&elem.name, None);
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
                    let ty = type_ann.clone().or(inferred);
                    scope.define_var(name, ty);
                } else {
                    Self::define_pattern_vars(pattern, scope);
                }
            }

            Node::VarBinding {
                pattern,
                type_ann,
                value,
            } => {
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
                    let ty = type_ann.clone().or(inferred);
                    scope.define_var(name, ty);
                } else {
                    Self::define_pattern_vars(pattern, scope);
                }
            }

            Node::FnDecl {
                name,
                type_params,
                params,
                return_type,
                body,
                ..
            } => {
                let required_params = params
                    .iter()
                    .filter(|p| p.default_value.is_none())
                    .count();
                let sig = FnSignature {
                    params: params
                        .iter()
                        .map(|p| (p.name.clone(), p.type_expr.clone()))
                        .collect(),
                    return_type: return_type.clone(),
                    type_param_names: type_params.iter().map(|tp| tp.name.clone()).collect(),
                    required_params,
                };
                scope.define_fn(name, sig.clone());
                scope.define_var(name, None);
                self.check_fn_body(type_params, params, return_type, body);
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
                    Self::define_pattern_vars(pattern, &mut loop_scope);
                }
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
                    if let Some(Some(var_type)) = scope.get_var(name) {
                        let value_type = self.infer_type(value, scope);
                        let assigned = if let Some(op) = op {
                            let var_inferred = scope.get_var(name).cloned().flatten();
                            infer_binary_op_type(op, &var_inferred, &value_type)
                        } else {
                            value_type
                        };
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

            Node::StructDecl { name, fields } => {
                let field_types: Vec<(String, InferredType)> = fields
                    .iter()
                    .map(|f| (f.name.clone(), f.type_expr.clone()))
                    .collect();
                scope.structs.insert(name.clone(), field_types);
            }

            Node::InterfaceDecl { name, methods } => {
                scope.interfaces.insert(name.clone(), methods.clone());
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
                        "-" | "*" | "/" | "%" => {
                            let numeric = ["int", "float"];
                            if !numeric.contains(&l.as_str()) || !numeric.contains(&r.as_str()) {
                                self.warning_at(
                                    format!(
                                        "Operator '{op}' may not be valid for types {} and {}",
                                        l, r
                                    ),
                                    span,
                                );
                            }
                        }
                        "+" => {
                            // + is valid for int, float, string, list, dict
                            let valid = ["int", "float", "string", "list", "dict"];
                            if !valid.contains(&l.as_str()) && !valid.contains(&r.as_str()) {
                                self.warning_at(
                                    format!(
                                        "Operator '+' may not be valid for types {} and {}",
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
            Node::UnaryOp { operand, .. } => {
                self.check_node(operand, scope);
            }
            Node::MethodCall { object, args, .. }
            | Node::OptionalMethodCall { object, args, .. } => {
                self.check_node(object, scope);
                for arg in args {
                    self.check_node(arg, scope);
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

            // Terminals — nothing to check
            _ => {}
        }
    }

    fn check_fn_body(
        &mut self,
        type_params: &[TypeParam],
        params: &[TypedParam],
        return_type: &Option<TypeExpr>,
        body: &[SNode],
    ) {
        let mut fn_scope = self.scope.child();
        // Register generic type parameters so they are treated as compatible
        // with any concrete type during type checking.
        for tp in type_params {
            fn_scope.generic_type_params.insert(tp.name.clone());
        }
        for param in params {
            fn_scope.define_var(&param.name, param.type_expr.clone());
            if let Some(default) = &param.default_value {
                self.check_node(default, &mut fn_scope);
            }
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
            if !is_builtin(name)
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
            for (i, (arg, (param_name, param_type))) in
                args.iter().zip(sig.params.iter()).enumerate()
            {
                if let Some(expected) = param_type {
                    let actual = self.infer_type(arg, scope);
                    if let Some(actual) = &actual {
                        if !self.types_compatible(expected, actual, &call_scope) {
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
            Node::DictLiteral(entries) => {
                // Infer shape type when all keys are string literals
                let mut fields = Vec::new();
                let mut all_string_keys = true;
                for entry in entries {
                    if let Node::StringLiteral(key) = &entry.key.node {
                        let val_type = self
                            .infer_type(&entry.value, scope)
                            .unwrap_or(TypeExpr::Named("nil".into()));
                        fields.push(ShapeField {
                            name: key.clone(),
                            type_expr: val_type,
                            optional: false,
                        });
                    } else {
                        all_string_keys = false;
                        break;
                    }
                }
                if all_string_keys && !fields.is_empty() {
                    Some(TypeExpr::Shape(fields))
                } else {
                    Some(TypeExpr::Named("dict".into()))
                }
            }
            Node::Closure { params, body } => {
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
                    || matches!(&obj_type, Some(TypeExpr::DictType(..)));
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
                    "merge" | "map_values" => Some(TypeExpr::Named("dict".into())),
                    // Conversions
                    "to_string" => Some(TypeExpr::Named("string".into())),
                    "to_int" => Some(TypeExpr::Named("int".into())),
                    "to_float" => Some(TypeExpr::Named("float".into())),
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

        match (&expected, &actual) {
            (TypeExpr::Named(a), TypeExpr::Named(b)) => a == b || (a == "float" && b == "int"),
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
                    ("dict", "dict") => Some(TypeExpr::Named("dict".into())),
                    _ => Some(TypeExpr::Named("string".into())),
                }
            }
            _ => None,
        },
        "-" | "*" | "/" | "%" => match (left, right) {
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
}
