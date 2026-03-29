use harn_lexer::Span;
use harn_parser::{format_type, DictEntry, Node, SNode, ShapeField, TypeExpr};

// ---------------------------------------------------------------------------
// Symbol table (AST-based)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HarnSymbolKind {
    Pipeline,
    Function,
    Variable,
    Parameter,
    Enum,
    Struct,
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
}

/// Walk the parsed AST and collect all definitions.
pub(crate) fn build_symbol_table(program: &[SNode]) -> Vec<SymbolInfo> {
    let mut symbols = Vec::new();
    for snode in program {
        collect_symbols(snode, &mut symbols, None);
    }
    symbols
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
    }
}

fn collect_symbols(snode: &SNode, symbols: &mut Vec<SymbolInfo>, scope_span: Option<Span>) {
    match &snode.node {
        Node::Pipeline {
            name, params, body, ..
        } => {
            let sig = if params.is_empty() {
                format!("pipeline {name}")
            } else {
                format!("pipeline {name}({})", params.join(", "))
            };
            symbols.push(SymbolInfo {
                name: name.clone(),
                kind: HarnSymbolKind::Pipeline,
                def_span: snode.span,
                type_info: None,
                signature: Some(sig),
                scope_span,
            });
            // Params are plain strings (no individual spans), register them scoped to body.
            for p in params {
                symbols.push(SymbolInfo {
                    name: p.clone(),
                    kind: HarnSymbolKind::Parameter,
                    def_span: snode.span,
                    type_info: None,
                    signature: None,
                    scope_span: Some(snode.span),
                });
            }
            for s in body {
                collect_symbols(s, symbols, Some(snode.span));
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
                .map(|p| match &p.type_expr {
                    Some(t) => format!("{}: {}", p.name, format_type(t)),
                    None => p.name.clone(),
                })
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
            });
            for p in params {
                symbols.push(SymbolInfo {
                    name: p.name.clone(),
                    kind: HarnSymbolKind::Parameter,
                    def_span: snode.span,
                    type_info: p.type_expr.clone(),
                    signature: None,
                    scope_span: Some(snode.span),
                });
            }
            for s in body {
                collect_symbols(s, symbols, Some(snode.span));
            }
        }
        Node::LetBinding {
            pattern,
            type_ann,
            value,
        } => {
            for name in binding_pattern_names(pattern) {
                symbols.push(SymbolInfo {
                    name,
                    kind: HarnSymbolKind::Variable,
                    def_span: snode.span,
                    type_info: type_ann.clone().or_else(|| infer_literal_type(value)),
                    signature: None,
                    scope_span,
                });
            }
            collect_symbols(value, symbols, scope_span);
        }
        Node::VarBinding {
            pattern,
            type_ann,
            value,
        } => {
            for name in binding_pattern_names(pattern) {
                symbols.push(SymbolInfo {
                    name,
                    kind: HarnSymbolKind::Variable,
                    def_span: snode.span,
                    type_info: type_ann.clone().or_else(|| infer_literal_type(value)),
                    signature: None,
                    scope_span,
                });
            }
            collect_symbols(value, symbols, scope_span);
        }
        Node::EnumDecl { name, .. } => {
            symbols.push(SymbolInfo {
                name: name.clone(),
                kind: HarnSymbolKind::Enum,
                def_span: snode.span,
                type_info: None,
                signature: Some(format!("enum {name}")),
                scope_span,
            });
        }
        Node::StructDecl { name, fields } => {
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
                type_info: None,
                signature: Some(format!("struct {name} {{ {fields_str} }}")),
                scope_span,
            });
        }
        Node::InterfaceDecl { name, methods } => {
            let methods_str = methods
                .iter()
                .map(|m| {
                    let params = m
                        .params
                        .iter()
                        .map(|p| p.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ");
                    format!("fn {}({})", m.name, params)
                })
                .collect::<Vec<_>>()
                .join("; ");
            symbols.push(SymbolInfo {
                name: name.clone(),
                kind: HarnSymbolKind::Struct,
                def_span: snode.span,
                type_info: None,
                signature: Some(format!("interface {name} {{ {methods_str} }}")),
                scope_span,
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
            });
            for m in methods {
                collect_symbols(m, symbols, Some(snode.span));
            }
        }
        Node::ForIn {
            pattern,
            iterable,
            body,
        } => {
            for name in binding_pattern_names(pattern) {
                symbols.push(SymbolInfo {
                    name,
                    kind: HarnSymbolKind::Variable,
                    def_span: snode.span,
                    type_info: None,
                    signature: None,
                    scope_span: Some(snode.span),
                });
            }
            collect_symbols(iterable, symbols, scope_span);
            for s in body {
                collect_symbols(s, symbols, Some(snode.span));
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
                collect_symbols(s, symbols, Some(snode.span));
            }
            if let Some(var) = error_var {
                symbols.push(SymbolInfo {
                    name: var.clone(),
                    kind: HarnSymbolKind::Variable,
                    def_span: snode.span,
                    type_info: None,
                    signature: None,
                    scope_span: Some(snode.span),
                });
            }
            for s in catch_body {
                collect_symbols(s, symbols, Some(snode.span));
            }
            if let Some(fb) = finally_body {
                for s in fb {
                    collect_symbols(s, symbols, Some(snode.span));
                }
            }
        }
        Node::TryExpr { body } => {
            for s in body {
                collect_symbols(s, symbols, Some(snode.span));
            }
        }
        Node::Closure { params, body } => {
            for p in params {
                symbols.push(SymbolInfo {
                    name: p.name.clone(),
                    kind: HarnSymbolKind::Parameter,
                    def_span: snode.span,
                    type_info: p.type_expr.clone(),
                    signature: None,
                    scope_span: Some(snode.span),
                });
            }
            for s in body {
                collect_symbols(s, symbols, Some(snode.span));
            }
        }
        // Recurse into all child-bearing nodes
        Node::IfElse {
            condition,
            then_body,
            else_body,
        } => {
            collect_symbols(condition, symbols, scope_span);
            for s in then_body {
                collect_symbols(s, symbols, scope_span);
            }
            if let Some(eb) = else_body {
                for s in eb {
                    collect_symbols(s, symbols, scope_span);
                }
            }
        }
        Node::WhileLoop { condition, body } => {
            collect_symbols(condition, symbols, scope_span);
            for s in body {
                collect_symbols(s, symbols, scope_span);
            }
        }
        Node::Retry { count, body } => {
            collect_symbols(count, symbols, scope_span);
            for s in body {
                collect_symbols(s, symbols, scope_span);
            }
        }
        Node::Parallel { count, body, .. } => {
            collect_symbols(count, symbols, scope_span);
            for s in body {
                collect_symbols(s, symbols, scope_span);
            }
        }
        Node::ParallelMap {
            list,
            variable,
            body,
        } => {
            collect_symbols(list, symbols, scope_span);
            symbols.push(SymbolInfo {
                name: variable.clone(),
                kind: HarnSymbolKind::Variable,
                def_span: snode.span,
                type_info: None,
                signature: None,
                scope_span: Some(snode.span),
            });
            for s in body {
                collect_symbols(s, symbols, Some(snode.span));
            }
        }
        Node::MatchExpr { value, arms } => {
            collect_symbols(value, symbols, scope_span);
            for arm in arms {
                collect_symbols(&arm.pattern, symbols, scope_span);
                for s in &arm.body {
                    collect_symbols(s, symbols, scope_span);
                }
            }
        }
        Node::Block(stmts) => {
            for s in stmts {
                collect_symbols(s, symbols, scope_span);
            }
        }
        Node::BinaryOp { left, right, .. } => {
            collect_symbols(left, symbols, scope_span);
            collect_symbols(right, symbols, scope_span);
        }
        Node::UnaryOp { operand, .. } => {
            collect_symbols(operand, symbols, scope_span);
        }
        Node::TryOperator { operand } => {
            collect_symbols(operand, symbols, scope_span);
        }
        Node::FunctionCall { args, .. } => {
            for a in args {
                collect_symbols(a, symbols, scope_span);
            }
        }
        Node::MethodCall { object, args, .. } | Node::OptionalMethodCall { object, args, .. } => {
            collect_symbols(object, symbols, scope_span);
            for a in args {
                collect_symbols(a, symbols, scope_span);
            }
        }
        Node::PropertyAccess { object, .. } | Node::OptionalPropertyAccess { object, .. } => {
            collect_symbols(object, symbols, scope_span);
        }
        Node::SubscriptAccess { object, index } => {
            collect_symbols(object, symbols, scope_span);
            collect_symbols(index, symbols, scope_span);
        }
        Node::SliceAccess { object, start, end } => {
            collect_symbols(object, symbols, scope_span);
            if let Some(s) = start {
                collect_symbols(s, symbols, scope_span);
            }
            if let Some(e) = end {
                collect_symbols(e, symbols, scope_span);
            }
        }
        Node::Assignment { target, value, .. } => {
            collect_symbols(target, symbols, scope_span);
            collect_symbols(value, symbols, scope_span);
        }
        Node::ReturnStmt { value: Some(v) } => {
            collect_symbols(v, symbols, scope_span);
        }
        Node::ThrowStmt { value } => {
            collect_symbols(value, symbols, scope_span);
        }
        Node::Ternary {
            condition,
            true_expr,
            false_expr,
        } => {
            collect_symbols(condition, symbols, scope_span);
            collect_symbols(true_expr, symbols, scope_span);
            collect_symbols(false_expr, symbols, scope_span);
        }
        Node::SpawnExpr { body } | Node::MutexBlock { body } => {
            for s in body {
                collect_symbols(s, symbols, scope_span);
            }
        }
        Node::DeadlineBlock { duration, body } => {
            collect_symbols(duration, symbols, scope_span);
            for s in body {
                collect_symbols(s, symbols, scope_span);
            }
        }
        Node::GuardStmt {
            condition,
            else_body,
        } => {
            collect_symbols(condition, symbols, scope_span);
            for s in else_body {
                collect_symbols(s, symbols, scope_span);
            }
        }
        Node::RangeExpr { start, end, .. } => {
            collect_symbols(start, symbols, scope_span);
            collect_symbols(end, symbols, scope_span);
        }
        Node::ListLiteral(items) => {
            for item in items {
                collect_symbols(item, symbols, scope_span);
            }
        }
        Node::DictLiteral(entries) | Node::AskExpr { fields: entries } => {
            collect_dict_entries(entries, symbols, scope_span);
        }
        Node::StructConstruct { fields, .. } => {
            collect_dict_entries(fields, symbols, scope_span);
        }
        Node::EnumConstruct { args, .. } => {
            for a in args {
                collect_symbols(a, symbols, scope_span);
            }
        }
        Node::Spread(inner) => {
            collect_symbols(inner, symbols, scope_span);
        }
        Node::SelectExpr {
            cases,
            timeout,
            default_body,
        } => {
            for case in cases {
                collect_symbols(&case.channel, symbols, scope_span);
                symbols.push(SymbolInfo {
                    name: case.variable.clone(),
                    kind: HarnSymbolKind::Variable,
                    def_span: snode.span,
                    type_info: None,
                    signature: None,
                    scope_span: Some(snode.span),
                });
                for s in &case.body {
                    collect_symbols(s, symbols, Some(snode.span));
                }
            }
            if let Some((dur, body)) = timeout {
                collect_symbols(dur, symbols, scope_span);
                for s in body {
                    collect_symbols(s, symbols, Some(snode.span));
                }
            }
            if let Some(body) = default_body {
                for s in body {
                    collect_symbols(s, symbols, Some(snode.span));
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
            });
            for s in body {
                collect_symbols(s, symbols, Some(snode.span));
            }
        }
        Node::YieldExpr { value: Some(v) } => {
            collect_symbols(v, symbols, scope_span);
        }
        Node::InterpolatedString(_)
        | Node::StringLiteral(_)
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
) {
    for entry in entries {
        collect_symbols(&entry.key, symbols, scope_span);
        collect_symbols(&entry.value, symbols, scope_span);
    }
}

/// Literal type inference for hover/completion.
/// Mirrors the typechecker's `infer_type` for literals, including shape-type
/// inference for dict literals whose keys are all string literals.
pub(crate) fn infer_literal_type(snode: &SNode) -> Option<TypeExpr> {
    match &snode.node {
        Node::IntLiteral(_) => Some(TypeExpr::Named("int".into())),
        Node::FloatLiteral(_) => Some(TypeExpr::Named("float".into())),
        Node::StringLiteral(_) | Node::InterpolatedString(_) => {
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
            let mut all_string_keys = true;
            for entry in entries {
                if let Node::StringLiteral(key) = &entry.key.node {
                    let val_type =
                        infer_literal_type(&entry.value).unwrap_or(TypeExpr::Named("nil".into()));
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
        Node::Closure { .. } => Some(TypeExpr::Named("closure".into())),
        _ => None,
    }
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
