use harn_lexer::Span;
use harn_parser::{format_type, DictEntry, Node, SNode, ShapeField, TypeExpr, TypeParam};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HarnSymbolKind {
    Pipeline,
    Function,
    Variable,
    Parameter,
    Enum,
    Struct,
    Interface,
}

#[derive(Debug, Clone)]
pub(crate) struct EnumVariantInfo {
    pub(crate) name: String,
    pub(crate) fields: Vec<ShapeField>,
}

#[derive(Debug, Clone)]
pub(crate) struct SymbolInfo {
    pub(crate) name: String,
    pub(crate) kind: HarnSymbolKind,
    pub(crate) def_span: Span,
    /// Type annotation or inferred type, when available.
    pub(crate) type_info: Option<TypeExpr>,
    /// For functions/pipelines: formatted signature for hover.
    pub(crate) signature: Option<String>,
    /// Span of the whole containing scope (for scope-aware completion).
    pub(crate) scope_span: Option<Span>,
    /// Doc comment extracted from `///` lines above the definition.
    pub(crate) doc_comment: Option<String>,
    /// For methods inside `impl` blocks: the type name (e.g. "Point").
    pub(crate) impl_type: Option<String>,
    /// Structural fields for shape-like symbols such as structs.
    pub(crate) fields: Vec<ShapeField>,
    /// Enum variants available on this enum symbol.
    pub(crate) enum_variants: Vec<EnumVariantInfo>,
}

/// Walk the parsed AST and collect all definitions.
pub(crate) fn build_symbol_table(program: &[SNode], source: &str) -> Vec<SymbolInfo> {
    let mut symbols = Vec::new();
    for snode in program {
        collect_symbols(snode, &mut symbols, None, source, None);
    }
    symbols
}

/// Extract leading `///` lines immediately above a span in the source.
/// Falls back to plain `//` comments when no `///` block is present.
fn extract_doc_comment(source: &str, span: &Span) -> Option<String> {
    let lines: Vec<&str> = source.lines().collect();
    // span.line is 1-based
    let def_line = span.line.saturating_sub(1); // 0-based index of def line
    if def_line == 0 {
        return None;
    }

    for prefix in ["///", "//"] {
        let mut comment_lines = Vec::new();
        let mut line_idx = def_line - 1;
        loop {
            let line = lines.get(line_idx)?;
            let trimmed = line.trim();
            if trimmed.starts_with(prefix) {
                let text = trimmed.trim_start_matches(prefix).trim_start();
                comment_lines.push(text.to_string());
            } else {
                break;
            }
            if line_idx == 0 {
                break;
            }
            line_idx -= 1;
        }
        if !comment_lines.is_empty() {
            comment_lines.reverse();
            return Some(comment_lines.join("\n"));
        }
    }
    None
}

/// Format a default value AST node as a short string for display in signatures.
fn format_default_value(snode: &SNode) -> String {
    match &snode.node {
        Node::IntLiteral(n) => n.to_string(),
        Node::FloatLiteral(n) => n.to_string(),
        Node::StringLiteral(s) => format!("\"{s}\""),
        Node::RawStringLiteral(s) => format!("r\"{s}\""),
        Node::BoolLiteral(b) => b.to_string(),
        Node::NilLiteral => "nil".to_string(),
        Node::ListLiteral(items) if items.is_empty() => "[]".to_string(),
        Node::DictLiteral(entries) if entries.is_empty() => "{}".to_string(),
        _ => "...".to_string(),
    }
}

/// Extract all variable names from a binding pattern.
pub(crate) fn binding_pattern_names(pattern: &harn_parser::BindingPattern) -> Vec<String> {
    match pattern {
        harn_parser::BindingPattern::Identifier(name) => vec![name.clone()],
        harn_parser::BindingPattern::Dict(fields) => fields
            .iter()
            .map(|f| f.alias.as_deref().unwrap_or(&f.key).to_string())
            .collect(),
        harn_parser::BindingPattern::List(elements) => {
            elements.iter().map(|e| e.name.clone()).collect()
        }
        harn_parser::BindingPattern::Pair(a, b) => vec![a.clone(), b.clone()],
    }
}

/// Format a typed parameter for display in a signature, including default values.
fn format_param(p: &harn_parser::TypedParam) -> String {
    let base = match &p.type_expr {
        Some(t) => format!("{}: {}", p.name, format_type(t)),
        None => p.name.clone(),
    };
    match &p.default_value {
        Some(dv) => format!("{base} = {}", format_default_value(dv)),
        None => base,
    }
}

fn collect_symbols(
    snode: &SNode,
    symbols: &mut Vec<SymbolInfo>,
    scope_span: Option<Span>,
    source: &str,
    impl_type_name: Option<&str>,
) {
    // Helper macro for simple SymbolInfo with no doc/impl fields
    macro_rules! simple_sym {
        ($name:expr, $kind:expr, $span:expr, $type_info:expr, $sig:expr, $scope:expr) => {
            SymbolInfo {
                name: $name,
                kind: $kind,
                def_span: $span,
                type_info: $type_info,
                signature: $sig,
                scope_span: $scope,
                doc_comment: None,
                impl_type: None,
                fields: Vec::new(),
                enum_variants: Vec::new(),
            }
        };
    }

    // Shorthand for recursive calls
    macro_rules! recurse {
        ($child:expr, $scope:expr) => {
            collect_symbols($child, symbols, $scope, source, None)
        };
    }

    match &snode.node {
        Node::Pipeline {
            name,
            params,
            body,
            is_pub,
            ..
        } => {
            let pub_prefix = if *is_pub { "pub " } else { "" };
            let sig = if params.is_empty() {
                format!("{pub_prefix}pipeline {name}")
            } else {
                format!("{pub_prefix}pipeline {name}({})", params.join(", "))
            };
            symbols.push(SymbolInfo {
                name: name.clone(),
                kind: HarnSymbolKind::Pipeline,
                def_span: snode.span,
                type_info: None,
                signature: Some(sig),
                scope_span,
                doc_comment: extract_doc_comment(source, &snode.span),
                impl_type: None,
                fields: Vec::new(),
                enum_variants: Vec::new(),
            });
            // Params are plain strings (no individual spans), register them scoped to body.
            for p in params {
                symbols.push(simple_sym!(
                    p.clone(),
                    HarnSymbolKind::Parameter,
                    snode.span,
                    None,
                    None,
                    Some(snode.span)
                ));
            }
            for s in body {
                recurse!(s, Some(snode.span));
            }
        }
        Node::FnDecl {
            name,
            params,
            return_type,
            body,
            ..
        } => {
            let params_str = params
                .iter()
                .map(format_param)
                .collect::<Vec<_>>()
                .join(", ");
            let ret_str = match return_type {
                Some(t) => format!(" -> {}", format_type(t)),
                None => String::new(),
            };
            let sig = format!("fn {name}({params_str}){ret_str}");
            symbols.push(SymbolInfo {
                name: name.clone(),
                kind: HarnSymbolKind::Function,
                def_span: snode.span,
                type_info: return_type.clone(),
                signature: Some(sig),
                scope_span,
                doc_comment: extract_doc_comment(source, &snode.span),
                impl_type: impl_type_name.map(String::from),
                fields: Vec::new(),
                enum_variants: Vec::new(),
            });
            for p in params {
                symbols.push(simple_sym!(
                    p.name.clone(),
                    HarnSymbolKind::Parameter,
                    snode.span,
                    p.type_expr.clone(),
                    None,
                    Some(snode.span)
                ));
            }
            for s in body {
                recurse!(s, Some(snode.span));
            }
        }
        Node::ToolDecl {
            name,
            params,
            return_type,
            body,
            ..
        } => {
            let params_str = params
                .iter()
                .map(format_param)
                .collect::<Vec<_>>()
                .join(", ");
            let ret_str = match return_type {
                Some(t) => format!(" -> {}", format_type(t)),
                None => String::new(),
            };
            let sig = format!("tool {name}({params_str}){ret_str}");
            symbols.push(SymbolInfo {
                name: name.clone(),
                kind: HarnSymbolKind::Function,
                def_span: snode.span,
                type_info: return_type.clone(),
                signature: Some(sig),
                scope_span,
                doc_comment: extract_doc_comment(source, &snode.span),
                impl_type: None,
                fields: Vec::new(),
                enum_variants: Vec::new(),
            });
            for p in params {
                symbols.push(simple_sym!(
                    p.name.clone(),
                    HarnSymbolKind::Parameter,
                    snode.span,
                    p.type_expr.clone(),
                    None,
                    Some(snode.span)
                ));
            }
            for s in body {
                recurse!(s, Some(snode.span));
            }
        }
        Node::LetBinding {
            pattern,
            type_ann,
            value,
        } => {
            for name in binding_pattern_names(pattern) {
                symbols.push(simple_sym!(
                    name,
                    HarnSymbolKind::Variable,
                    snode.span,
                    type_ann
                        .clone()
                        .or_else(|| infer_symbol_type(value, symbols)),
                    None,
                    scope_span
                ));
            }
            recurse!(value, scope_span);
        }
        Node::VarBinding {
            pattern,
            type_ann,
            value,
        } => {
            for name in binding_pattern_names(pattern) {
                symbols.push(simple_sym!(
                    name,
                    HarnSymbolKind::Variable,
                    snode.span,
                    type_ann
                        .clone()
                        .or_else(|| infer_symbol_type(value, symbols)),
                    None,
                    scope_span
                ));
            }
            recurse!(value, scope_span);
        }
        Node::EnumDecl {
            name,
            type_params,
            variants,
            is_pub,
        } => {
            let pub_prefix = if *is_pub { "pub " } else { "" };
            let generics = format_type_params(type_params);
            symbols.push(SymbolInfo {
                name: name.clone(),
                kind: HarnSymbolKind::Enum,
                def_span: snode.span,
                type_info: Some(TypeExpr::Named(name.clone())),
                signature: Some(format!("{pub_prefix}enum {name}{generics}")),
                scope_span,
                doc_comment: extract_doc_comment(source, &snode.span),
                impl_type: None,
                fields: Vec::new(),
                enum_variants: variants
                    .iter()
                    .map(|variant| EnumVariantInfo {
                        name: variant.name.clone(),
                        fields: variant
                            .fields
                            .iter()
                            .map(|field| ShapeField {
                                name: field.name.clone(),
                                type_expr: field
                                    .type_expr
                                    .clone()
                                    .unwrap_or(TypeExpr::Named("any".to_string())),
                                optional: false,
                            })
                            .collect(),
                    })
                    .collect(),
            });
        }
        Node::StructDecl {
            name,
            type_params,
            fields,
            is_pub,
        } => {
            let pub_prefix = if *is_pub { "pub " } else { "" };
            let generics = format_type_params(type_params);
            let shape_fields = fields
                .iter()
                .map(|field| ShapeField {
                    name: field.name.clone(),
                    type_expr: field
                        .type_expr
                        .clone()
                        .unwrap_or(TypeExpr::Named("any".to_string())),
                    optional: field.optional,
                })
                .collect::<Vec<_>>();
            let fields_str = fields
                .iter()
                .map(|f| {
                    let opt = if f.optional { "?" } else { "" };
                    match &f.type_expr {
                        Some(t) => format!("{}{opt}: {}", f.name, format_type(t)),
                        None => format!("{}{opt}", f.name),
                    }
                })
                .collect::<Vec<_>>()
                .join(", ");
            symbols.push(SymbolInfo {
                name: name.clone(),
                kind: HarnSymbolKind::Struct,
                def_span: snode.span,
                type_info: Some(TypeExpr::Shape(shape_fields.clone())),
                signature: Some(format!(
                    "{pub_prefix}struct {name}{generics} {{ {fields_str} }}"
                )),
                scope_span,
                doc_comment: extract_doc_comment(source, &snode.span),
                impl_type: None,
                fields: shape_fields,
                enum_variants: Vec::new(),
            });
        }
        Node::InterfaceDecl {
            name,
            type_params,
            associated_types,
            methods,
        } => {
            let generics = format_type_params(type_params);
            let associated_types_str = associated_types
                .iter()
                .map(|(assoc_name, assoc_type)| match assoc_type {
                    Some(assoc_type) => format!("type {assoc_name} = {}", format_type(assoc_type)),
                    None => format!("type {assoc_name}"),
                })
                .collect::<Vec<_>>();
            let methods_str = methods
                .iter()
                .map(|m| {
                    let method_generics = format_type_params(&m.type_params);
                    let params = m
                        .params
                        .iter()
                        .map(|p| p.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ");
                    format!("fn {}{}({})", m.name, method_generics, params)
                })
                .collect::<Vec<_>>()
                .join("; ");
            let body_parts = associated_types_str
                .into_iter()
                .chain((!methods_str.is_empty()).then_some(methods_str))
                .collect::<Vec<_>>()
                .join("; ");
            symbols.push(SymbolInfo {
                name: name.clone(),
                kind: HarnSymbolKind::Interface,
                def_span: snode.span,
                type_info: None,
                signature: Some(format!("interface {name}{generics} {{ {body_parts} }}")),
                scope_span,
                doc_comment: extract_doc_comment(source, &snode.span),
                impl_type: None,
                fields: Vec::new(),
                enum_variants: Vec::new(),
            });
        }
        Node::ImplBlock {
            type_name, methods, ..
        } => {
            symbols.push(SymbolInfo {
                name: type_name.clone(),
                kind: HarnSymbolKind::Struct,
                def_span: snode.span,
                type_info: None,
                signature: Some(format!("impl {type_name}")),
                scope_span,
                doc_comment: None,
                impl_type: None,
                fields: Vec::new(),
                enum_variants: Vec::new(),
            });
            for m in methods {
                collect_symbols(m, symbols, Some(snode.span), source, Some(type_name));
            }
        }
        Node::ForIn {
            pattern,
            iterable,
            body,
        } => {
            for name in binding_pattern_names(pattern) {
                symbols.push(simple_sym!(
                    name,
                    HarnSymbolKind::Variable,
                    snode.span,
                    None,
                    None,
                    Some(snode.span)
                ));
            }
            recurse!(iterable, scope_span);
            for s in body {
                recurse!(s, Some(snode.span));
            }
        }
        Node::TryCatch {
            body,
            error_var,
            catch_body,
            finally_body,
            ..
        } => {
            for s in body {
                recurse!(s, Some(snode.span));
            }
            if let Some(var) = error_var {
                symbols.push(simple_sym!(
                    var.clone(),
                    HarnSymbolKind::Variable,
                    snode.span,
                    None,
                    None,
                    Some(snode.span)
                ));
            }
            for s in catch_body {
                recurse!(s, Some(snode.span));
            }
            if let Some(fb) = finally_body {
                for s in fb {
                    recurse!(s, Some(snode.span));
                }
            }
        }
        Node::TryExpr { body } => {
            for s in body {
                recurse!(s, Some(snode.span));
            }
        }
        Node::Closure { params, body, .. } => {
            for p in params {
                symbols.push(simple_sym!(
                    p.name.clone(),
                    HarnSymbolKind::Parameter,
                    snode.span,
                    p.type_expr.clone(),
                    None,
                    Some(snode.span)
                ));
            }
            for s in body {
                recurse!(s, Some(snode.span));
            }
        }
        // Recurse into all child-bearing nodes
        Node::IfElse {
            condition,
            then_body,
            else_body,
        } => {
            recurse!(condition, scope_span);
            for s in then_body {
                recurse!(s, scope_span);
            }
            if let Some(eb) = else_body {
                for s in eb {
                    recurse!(s, scope_span);
                }
            }
        }
        Node::WhileLoop { condition, body } => {
            recurse!(condition, scope_span);
            for s in body {
                recurse!(s, scope_span);
            }
        }
        Node::Retry { count, body } => {
            recurse!(count, scope_span);
            for s in body {
                recurse!(s, scope_span);
            }
        }
        Node::Parallel {
            expr,
            variable,
            body,
            ..
        } => {
            recurse!(expr, scope_span);
            if let Some(var) = variable {
                symbols.push(simple_sym!(
                    var.clone(),
                    HarnSymbolKind::Variable,
                    snode.span,
                    None,
                    None,
                    Some(snode.span)
                ));
            }
            for s in body {
                recurse!(
                    s,
                    if variable.is_some() {
                        Some(snode.span)
                    } else {
                        scope_span
                    }
                );
            }
        }
        Node::MatchExpr { value, arms } => {
            recurse!(value, scope_span);
            for arm in arms {
                recurse!(&arm.pattern, scope_span);
                for s in &arm.body {
                    recurse!(s, scope_span);
                }
            }
        }
        Node::Block(stmts) => {
            for s in stmts {
                recurse!(s, scope_span);
            }
        }
        Node::BinaryOp { left, right, .. } => {
            recurse!(left, scope_span);
            recurse!(right, scope_span);
        }
        Node::UnaryOp { operand, .. } => {
            recurse!(operand, scope_span);
        }
        Node::TryOperator { operand } => {
            recurse!(operand, scope_span);
        }
        Node::FunctionCall { args, .. } => {
            for a in args {
                recurse!(a, scope_span);
            }
        }
        Node::MethodCall { object, args, .. } | Node::OptionalMethodCall { object, args, .. } => {
            recurse!(object, scope_span);
            for a in args {
                recurse!(a, scope_span);
            }
        }
        Node::PropertyAccess { object, .. } | Node::OptionalPropertyAccess { object, .. } => {
            recurse!(object, scope_span);
        }
        Node::SubscriptAccess { object, index } => {
            recurse!(object, scope_span);
            recurse!(index, scope_span);
        }
        Node::SliceAccess { object, start, end } => {
            recurse!(object, scope_span);
            if let Some(s) = start {
                recurse!(s, scope_span);
            }
            if let Some(e) = end {
                recurse!(e, scope_span);
            }
        }
        Node::Assignment { target, value, .. } => {
            recurse!(target, scope_span);
            recurse!(value, scope_span);
        }
        Node::ReturnStmt { value: Some(v) } => {
            recurse!(v, scope_span);
        }
        Node::ThrowStmt { value } => {
            recurse!(value, scope_span);
        }
        Node::Ternary {
            condition,
            true_expr,
            false_expr,
        } => {
            recurse!(condition, scope_span);
            recurse!(true_expr, scope_span);
            recurse!(false_expr, scope_span);
        }
        Node::SpawnExpr { body } | Node::MutexBlock { body } | Node::DeferStmt { body } => {
            for s in body {
                recurse!(s, scope_span);
            }
        }
        Node::DeadlineBlock { duration, body } => {
            recurse!(duration, scope_span);
            for s in body {
                recurse!(s, scope_span);
            }
        }
        Node::GuardStmt {
            condition,
            else_body,
        } => {
            recurse!(condition, scope_span);
            for s in else_body {
                recurse!(s, scope_span);
            }
        }
        Node::RangeExpr { start, end, .. } => {
            recurse!(start, scope_span);
            recurse!(end, scope_span);
        }
        Node::ListLiteral(items) => {
            for item in items {
                recurse!(item, scope_span);
            }
        }
        Node::DictLiteral(entries) => {
            collect_dict_entries(entries, symbols, scope_span, source);
        }
        Node::StructConstruct { fields, .. } => {
            collect_dict_entries(fields, symbols, scope_span, source);
        }
        Node::EnumConstruct { args, .. } => {
            for a in args {
                recurse!(a, scope_span);
            }
        }
        Node::Spread(inner) => {
            recurse!(inner, scope_span);
        }
        Node::SelectExpr {
            cases,
            timeout,
            default_body,
        } => {
            for case in cases {
                recurse!(&case.channel, scope_span);
                symbols.push(simple_sym!(
                    case.variable.clone(),
                    HarnSymbolKind::Variable,
                    snode.span,
                    None,
                    None,
                    Some(snode.span)
                ));
                for s in &case.body {
                    recurse!(s, Some(snode.span));
                }
            }
            if let Some((dur, body)) = timeout {
                recurse!(dur, scope_span);
                for s in body {
                    recurse!(s, Some(snode.span));
                }
            }
            if let Some(body) = default_body {
                for s in body {
                    recurse!(s, Some(snode.span));
                }
            }
        }
        Node::OverrideDecl {
            name, params, body, ..
        } => {
            let sig = format!("override {name}({})", params.join(", "));
            symbols.push(SymbolInfo {
                name: name.clone(),
                kind: HarnSymbolKind::Function,
                def_span: snode.span,
                type_info: None,
                signature: Some(sig),
                scope_span,
                doc_comment: extract_doc_comment(source, &snode.span),
                impl_type: None,
                fields: Vec::new(),
                enum_variants: Vec::new(),
            });
            for s in body {
                recurse!(s, Some(snode.span));
            }
        }
        Node::YieldExpr { value: Some(v) } => {
            recurse!(v, scope_span);
        }
        Node::RequireStmt { condition, message } => {
            recurse!(condition, scope_span);
            if let Some(message) = message {
                recurse!(message, scope_span);
            }
        }
        Node::InterpolatedString(_)
        | Node::StringLiteral(_)
        | Node::RawStringLiteral(_)
        | Node::IntLiteral(_)
        | Node::FloatLiteral(_)
        | Node::BoolLiteral(_)
        | Node::NilLiteral
        | Node::Identifier(_)
        | Node::DurationLiteral(_)
        | Node::ImportDecl { .. }
        | Node::SelectiveImport { .. }
        | Node::TypeDecl { .. }
        | Node::ReturnStmt { value: None }
        | Node::YieldExpr { value: None }
        | Node::BreakStmt
        | Node::ContinueStmt => {}
    }
}

fn collect_dict_entries(
    entries: &[DictEntry],
    symbols: &mut Vec<SymbolInfo>,
    scope_span: Option<Span>,
    source: &str,
) {
    for entry in entries {
        collect_symbols(&entry.key, symbols, scope_span, source, None);
        collect_symbols(&entry.value, symbols, scope_span, source, None);
    }
}

/// Literal type inference for hover/completion.
/// Mirrors the typechecker's `infer_type` for literals, including shape-type
/// inference for dict literals whose keys are all string literals.
pub(crate) fn infer_literal_type(snode: &SNode) -> Option<TypeExpr> {
    match &snode.node {
        Node::IntLiteral(_) => Some(TypeExpr::Named("int".into())),
        Node::FloatLiteral(_) => Some(TypeExpr::Named("float".into())),
        Node::StringLiteral(_) | Node::RawStringLiteral(_) | Node::InterpolatedString(_) => {
            Some(TypeExpr::Named("string".into()))
        }
        Node::BoolLiteral(_) => Some(TypeExpr::Named("bool".into())),
        Node::NilLiteral => Some(TypeExpr::Named("nil".into())),
        Node::ListLiteral(items) => {
            // Try to infer element type from first item
            if let Some(first) = items.first() {
                if let Some(elem_ty) = infer_literal_type(first) {
                    return Some(TypeExpr::List(Box::new(elem_ty)));
                }
            }
            Some(TypeExpr::Named("list".into()))
        }
        Node::DictLiteral(entries) => {
            // Infer shape type when all keys are string literals
            let mut fields = Vec::new();
            for entry in entries {
                let key = match &entry.key.node {
                    Node::StringLiteral(key) | Node::Identifier(key) => key.clone(),
                    _ => return Some(TypeExpr::Named("dict".into())),
                };
                let val_type =
                    infer_literal_type(&entry.value).unwrap_or(TypeExpr::Named("nil".into()));
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
        Node::Closure { .. } => Some(TypeExpr::Named("closure".into())),
        _ => None,
    }
}

fn infer_symbol_type(snode: &SNode, symbols: &[SymbolInfo]) -> Option<TypeExpr> {
    infer_literal_type(snode).or_else(|| match &snode.node {
        Node::FunctionCall { name, .. } => symbols
            .iter()
            .find(|sym| sym.kind == HarnSymbolKind::Struct && sym.name == *name)
            .map(|_| TypeExpr::Named(name.clone())),
        Node::EnumConstruct { enum_name, .. } => Some(TypeExpr::Named(enum_name.clone())),
        Node::PropertyAccess { object, .. } => {
            if let Node::Identifier(name) = &object.node {
                symbols
                    .iter()
                    .find(|sym| sym.kind == HarnSymbolKind::Enum && sym.name == *name)
                    .map(|_| TypeExpr::Named(name.clone()))
            } else {
                None
            }
        }
        _ => None,
    })
}

/// Format a shape type with one field per line for complex hover tooltips.
/// Only produces output for `TypeExpr::Shape` with 2+ fields; returns empty
/// string otherwise (the compact one-liner is sufficient).
pub(crate) fn format_shape_expanded(ty: &TypeExpr, indent: usize) -> String {
    if let TypeExpr::Shape(fields) = ty {
        if fields.len() < 2 {
            return String::new();
        }
        let pad = "  ".repeat(indent + 1);
        let mut lines = Vec::new();
        lines.push("```harn".to_string());
        lines.push(format!("{}{{", "  ".repeat(indent)));
        for f in fields {
            let opt = if f.optional { "?" } else { "" };
            lines.push(format!(
                "{pad}{}{opt}: {}",
                f.name,
                format_type(&f.type_expr)
            ));
        }
        lines.push(format!("{}}}", "  ".repeat(indent)));
        lines.push("```".to_string());
        lines.join("\n")
    } else {
        String::new()
    }
}
fn format_type_params(type_params: &[TypeParam]) -> String {
    if type_params.is_empty() {
        String::new()
    } else {
        format!(
            "<{}>",
            type_params
                .iter()
                .map(|tp| tp.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
}
