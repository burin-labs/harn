use std::collections::HashMap;
use std::sync::Mutex;

use harn_lexer::{Lexer, LexerError, Span};
use harn_parser::{
    format_type, DictEntry, Node, Parser, ParserError, SNode, ShapeField, TypeChecker, TypeExpr,
};
use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer, LspService, Server};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Known builtin names with their signatures for completion.
/// Each entry is (name, detail) where detail shows the parameter signature.
const BUILTINS: &[(&str, &str)] = &[
    // I/O
    ("println", "println(msg) -> nil"),
    ("print", "print(msg) -> nil"),
    ("log", "log(msg) -> nil"),
    // Type conversion
    ("type_of", "type_of(value) -> string"),
    ("to_string", "to_string(value) -> string"),
    ("to_int", "to_int(value) -> int"),
    ("to_float", "to_float(value) -> float"),
    // JSON
    ("json_parse", "json_parse(str) -> value"),
    ("json_stringify", "json_stringify(value) -> string"),
    ("json_validate", "json_validate(data, schema) -> bool"),
    ("json_extract", "json_extract(text, key?) -> value"),
    // File system
    ("read_file", "read_file(path) -> string"),
    ("write_file", "write_file(path, content) -> nil"),
    ("file_exists", "file_exists(path) -> bool"),
    ("delete_file", "delete_file(path) -> nil"),
    ("list_dir", "list_dir(path) -> list"),
    ("mkdir", "mkdir(path) -> nil"),
    ("stat", "stat(path) -> dict"),
    ("copy_file", "copy_file(src, dst) -> nil"),
    ("append_file", "append_file(path, content) -> nil"),
    ("path_join", "path_join(parts...) -> string"),
    ("temp_dir", "temp_dir() -> string"),
    // Process
    ("exec", "exec(cmd, args...) -> dict"),
    ("shell", "shell(cmd) -> dict"),
    // Environment
    ("env", "env(name) -> string"),
    ("timestamp", "timestamp() -> float"),
    ("exit", "exit(code) -> nil"),
    // Regex
    ("regex_match", "regex_match(pattern, text) -> list"),
    (
        "regex_replace",
        "regex_replace(pattern, replacement, text) -> string",
    ),
    // HTTP
    ("http_get", "http_get(url) -> dict"),
    ("http_post", "http_post(url, body, headers?) -> dict"),
    ("http_put", "http_put(url, body, headers?) -> dict"),
    ("http_patch", "http_patch(url, body, headers?) -> dict"),
    ("http_delete", "http_delete(url) -> dict"),
    (
        "http_request",
        "http_request(method, url, options?) -> dict",
    ),
    // LLM
    ("llm_call", "llm_call(prompt, system?, options?) -> string"),
    (
        "llm_stream",
        "llm_stream(prompt, system?, options?) -> string",
    ),
    (
        "agent_loop",
        "agent_loop(prompt, system?, options?) -> string",
    ),
    // MCP
    ("mcp_connect", "mcp_connect(command, args?) -> client"),
    ("mcp_list_tools", "mcp_list_tools(client) -> list"),
    ("mcp_call", "mcp_call(client, name, args?) -> value"),
    ("mcp_server_info", "mcp_server_info(client) -> dict"),
    ("mcp_disconnect", "mcp_disconnect(client) -> nil"),
    // Concurrency
    ("sleep", "sleep(duration) -> nil"),
    ("channel", "channel(name?) -> channel"),
    ("send", "send(ch, value) -> nil"),
    ("receive", "receive(ch) -> value"),
    ("try_receive", "try_receive(ch) -> value"),
    ("close_channel", "close_channel(ch) -> nil"),
    ("select", "select(channels...) -> value"),
    ("atomic", "atomic(initial?) -> atomic"),
    ("atomic_get", "atomic_get(a) -> value"),
    ("atomic_set", "atomic_set(a, value) -> nil"),
    ("atomic_add", "atomic_add(a, delta) -> value"),
    ("atomic_cas", "atomic_cas(a, expected, new) -> bool"),
    // Assertions
    ("assert", "assert(condition, msg?) -> nil"),
    ("assert_eq", "assert_eq(a, b, msg?) -> nil"),
    ("assert_ne", "assert_ne(a, b, msg?) -> nil"),
    // Math
    ("abs", "abs(n) -> number"),
    ("min", "min(a, b) -> number"),
    ("max", "max(a, b) -> number"),
    ("floor", "floor(n) -> int"),
    ("ceil", "ceil(n) -> int"),
    ("round", "round(n) -> int"),
    ("sqrt", "sqrt(n) -> float"),
    ("pow", "pow(base, exp) -> number"),
    ("random", "random() -> float"),
    ("random_int", "random_int(min, max) -> int"),
    // String
    ("format", "format(template, args...) -> string"),
    ("trim", "trim(str) -> string"),
    ("lowercase", "lowercase(str) -> string"),
    ("uppercase", "uppercase(str) -> string"),
    ("split", "split(str, sep) -> list"),
    // Date/time
    ("date_now", "date_now() -> string"),
    ("date_format", "date_format(ts, fmt?) -> string"),
    ("date_parse", "date_parse(str) -> int"),
    // Logging
    ("log_debug", "log_debug(msg) -> nil"),
    ("log_info", "log_info(msg) -> nil"),
    ("log_warn", "log_warn(msg) -> nil"),
    ("log_error", "log_error(msg) -> nil"),
    ("log_set_level", "log_set_level(level) -> nil"),
    // Tracing
    ("trace_start", "trace_start(name) -> span"),
    ("trace_end", "trace_end(span) -> nil"),
    ("trace_id", "trace_id() -> string"),
    // Tool registry
    ("tool_registry", "tool_registry() -> registry"),
    (
        "tool_add",
        "tool_add(registry, name, desc, handler, params?) -> nil",
    ),
    ("tool_list", "tool_list(registry) -> list"),
    ("tool_find", "tool_find(registry, name) -> dict"),
    ("tool_describe", "tool_describe(registry) -> string"),
    ("tool_remove", "tool_remove(registry, name) -> nil"),
    ("tool_count", "tool_count(registry) -> int"),
    ("tool_schema", "tool_schema(registry) -> dict"),
    ("tool_prompt", "tool_prompt(registry) -> string"),
    ("tool_parse_call", "tool_parse_call(registry, text) -> dict"),
    (
        "tool_format_result",
        "tool_format_result(name, result) -> string",
    ),
    // User interaction
    ("prompt_user", "prompt_user(msg) -> string"),
    // Host interop
    ("host_call", "host_call(name, args) -> value"),
];

/// Known keywords for completion.
const KEYWORDS: &[&str] = &[
    "pipeline",
    "extends",
    "override",
    "let",
    "var",
    "if",
    "else",
    "for",
    "in",
    "match",
    "retry",
    "parallel",
    "parallel_map",
    "return",
    "import",
    "true",
    "false",
    "nil",
    "try",
    "catch",
    "throw",
    "fn",
    "spawn",
    "while",
    "break",
    "continue",
    "interface",
    "pub",
    "from",
    "struct",
    "enum",
    "type",
    "guard",
    "deadline",
    "yield",
    "mutex",
];

/// String methods offered after `.` on a string value.
const STRING_METHODS: &[&str] = &[
    "count",
    "empty",
    "trim",
    "split",
    "contains",
    "starts_with",
    "ends_with",
    "replace",
    "uppercase",
    "lowercase",
    "substring",
    "index_of",
    "chars",
    "repeat",
    "reverse",
    "pad_left",
    "pad_right",
];

/// List methods offered after `.` on a list value.
const LIST_METHODS: &[&str] = &[
    "count",
    "empty",
    "push",
    "pop",
    "map",
    "filter",
    "reduce",
    "find",
    "any",
    "all",
    "contains",
    "index_of",
    "join",
    "sort",
    "sort_by",
    "reverse",
    "flat_map",
    "flatten",
    "slice",
    "enumerate",
    "zip",
    "unique",
    "take",
    "skip",
    "sum",
    "min",
    "max",
];

/// Dict methods offered after `.` on a dict value.
const DICT_METHODS: &[&str] = &[
    "keys",
    "values",
    "entries",
    "count",
    "has",
    "merge",
    "map_values",
    "filter",
    "remove",
    "get",
];

// ---------------------------------------------------------------------------
// Symbol table (AST-based)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HarnSymbolKind {
    Pipeline,
    Function,
    Variable,
    Parameter,
    Enum,
    Struct,
}

#[derive(Debug, Clone)]
struct SymbolInfo {
    name: String,
    kind: HarnSymbolKind,
    def_span: Span,
    /// Type annotation or inferred type, when available.
    type_info: Option<TypeExpr>,
    /// For functions/pipelines: formatted signature for hover.
    signature: Option<String>,
    /// Span of the whole containing scope (for scope-aware completion).
    scope_span: Option<Span>,
}

/// Walk the parsed AST and collect all definitions.
fn build_symbol_table(program: &[SNode]) -> Vec<SymbolInfo> {
    let mut symbols = Vec::new();
    for snode in program {
        collect_symbols(snode, &mut symbols, None);
    }
    symbols
}

/// Extract all variable names from a binding pattern.
fn binding_pattern_names(pattern: &harn_parser::BindingPattern) -> Vec<String> {
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
fn infer_literal_type(snode: &SNode) -> Option<TypeExpr> {
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
fn format_shape_expanded(ty: &TypeExpr, indent: usize) -> String {
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

// ---------------------------------------------------------------------------
// Reference collection (AST-based)
// ---------------------------------------------------------------------------

/// Find all identifier references matching `target_name` in the AST.
fn find_references(program: &[SNode], target_name: &str) -> Vec<Span> {
    let mut refs = Vec::new();
    for snode in program {
        collect_references(snode, target_name, &mut refs);
    }
    refs
}

fn collect_references(snode: &SNode, target_name: &str, refs: &mut Vec<Span>) {
    match &snode.node {
        Node::Identifier(name) if name == target_name => {
            refs.push(snode.span);
        }
        Node::FunctionCall { name, args } => {
            if name == target_name {
                refs.push(snode.span);
            }
            for a in args {
                collect_references(a, target_name, refs);
            }
        }
        // For definitions, the name itself is a "reference" too
        Node::Pipeline {
            name, body, params, ..
        } => {
            if name == target_name {
                refs.push(snode.span);
            }
            for p in params {
                if p == target_name {
                    refs.push(snode.span);
                }
            }
            for s in body {
                collect_references(s, target_name, refs);
            }
        }
        Node::FnDecl {
            name, params, body, ..
        } => {
            if name == target_name {
                refs.push(snode.span);
            }
            for p in params {
                if p.name == target_name {
                    refs.push(snode.span);
                }
            }
            for s in body {
                collect_references(s, target_name, refs);
            }
        }
        Node::LetBinding { pattern, value, .. } | Node::VarBinding { pattern, value, .. } => {
            if binding_pattern_names(pattern)
                .iter()
                .any(|n| n == target_name)
            {
                refs.push(snode.span);
            }
            collect_references(value, target_name, refs);
        }
        Node::ForIn {
            pattern,
            iterable,
            body,
        } => {
            if binding_pattern_names(pattern)
                .iter()
                .any(|n| n == target_name)
            {
                refs.push(snode.span);
            }
            collect_references(iterable, target_name, refs);
            for s in body {
                collect_references(s, target_name, refs);
            }
        }
        Node::IfElse {
            condition,
            then_body,
            else_body,
        } => {
            collect_references(condition, target_name, refs);
            for s in then_body {
                collect_references(s, target_name, refs);
            }
            if let Some(eb) = else_body {
                for s in eb {
                    collect_references(s, target_name, refs);
                }
            }
        }
        Node::WhileLoop { condition, body } => {
            collect_references(condition, target_name, refs);
            for s in body {
                collect_references(s, target_name, refs);
            }
        }
        Node::Retry { count, body } => {
            collect_references(count, target_name, refs);
            for s in body {
                collect_references(s, target_name, refs);
            }
        }
        Node::TryCatch {
            body,
            error_var,
            catch_body,
            ..
        } => {
            for s in body {
                collect_references(s, target_name, refs);
            }
            if let Some(var) = error_var {
                if var == target_name {
                    refs.push(snode.span);
                }
            }
            for s in catch_body {
                collect_references(s, target_name, refs);
            }
        }
        Node::MatchExpr { value, arms } => {
            collect_references(value, target_name, refs);
            for arm in arms {
                collect_references(&arm.pattern, target_name, refs);
                for s in &arm.body {
                    collect_references(s, target_name, refs);
                }
            }
        }
        Node::BinaryOp { left, right, .. } => {
            collect_references(left, target_name, refs);
            collect_references(right, target_name, refs);
        }
        Node::UnaryOp { operand, .. } => {
            collect_references(operand, target_name, refs);
        }
        Node::MethodCall { object, args, .. } | Node::OptionalMethodCall { object, args, .. } => {
            collect_references(object, target_name, refs);
            for a in args {
                collect_references(a, target_name, refs);
            }
        }
        Node::PropertyAccess { object, .. } | Node::OptionalPropertyAccess { object, .. } => {
            collect_references(object, target_name, refs);
        }
        Node::SubscriptAccess { object, index } => {
            collect_references(object, target_name, refs);
            collect_references(index, target_name, refs);
        }
        Node::SliceAccess { object, start, end } => {
            collect_references(object, target_name, refs);
            if let Some(s) = start {
                collect_references(s, target_name, refs);
            }
            if let Some(e) = end {
                collect_references(e, target_name, refs);
            }
        }
        Node::Assignment { target, value, .. } => {
            collect_references(target, target_name, refs);
            collect_references(value, target_name, refs);
        }
        Node::ReturnStmt { value: Some(v) } => {
            collect_references(v, target_name, refs);
        }
        Node::ThrowStmt { value } => {
            collect_references(value, target_name, refs);
        }
        Node::Ternary {
            condition,
            true_expr,
            false_expr,
        } => {
            collect_references(condition, target_name, refs);
            collect_references(true_expr, target_name, refs);
            collect_references(false_expr, target_name, refs);
        }
        Node::Block(stmts) | Node::SpawnExpr { body: stmts } | Node::MutexBlock { body: stmts } => {
            for s in stmts {
                collect_references(s, target_name, refs);
            }
        }
        Node::Parallel { count, body, .. } => {
            collect_references(count, target_name, refs);
            for s in body {
                collect_references(s, target_name, refs);
            }
        }
        Node::ParallelMap {
            list,
            body,
            variable,
        } => {
            collect_references(list, target_name, refs);
            if variable == target_name {
                refs.push(snode.span);
            }
            for s in body {
                collect_references(s, target_name, refs);
            }
        }
        Node::Closure { body, params } => {
            for p in params {
                if p.name == target_name {
                    refs.push(snode.span);
                }
            }
            for s in body {
                collect_references(s, target_name, refs);
            }
        }
        Node::DeadlineBlock { duration, body } => {
            collect_references(duration, target_name, refs);
            for s in body {
                collect_references(s, target_name, refs);
            }
        }
        Node::GuardStmt {
            condition,
            else_body,
        } => {
            collect_references(condition, target_name, refs);
            for s in else_body {
                collect_references(s, target_name, refs);
            }
        }
        Node::RangeExpr { start, end, .. } => {
            collect_references(start, target_name, refs);
            collect_references(end, target_name, refs);
        }
        Node::ListLiteral(items) => {
            for item in items {
                collect_references(item, target_name, refs);
            }
        }
        Node::DictLiteral(entries) | Node::AskExpr { fields: entries } => {
            for entry in entries {
                collect_references(&entry.key, target_name, refs);
                collect_references(&entry.value, target_name, refs);
            }
        }
        Node::StructConstruct { fields, .. } => {
            for entry in fields {
                collect_references(&entry.key, target_name, refs);
                collect_references(&entry.value, target_name, refs);
            }
        }
        Node::EnumConstruct { args, .. } => {
            for a in args {
                collect_references(a, target_name, refs);
            }
        }
        Node::OverrideDecl { name, body, .. } => {
            if name == target_name {
                refs.push(snode.span);
            }
            for s in body {
                collect_references(s, target_name, refs);
            }
        }
        Node::YieldExpr { value: Some(v) } => {
            collect_references(v, target_name, refs);
        }
        Node::EnumDecl { name, .. }
        | Node::StructDecl { name, .. }
        | Node::InterfaceDecl { name, .. } => {
            if name == target_name {
                refs.push(snode.span);
            }
        }
        // Terminals
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Document state: caches parse results per file
// ---------------------------------------------------------------------------

struct DocumentState {
    source: String,
    ast: Option<Vec<SNode>>,
    symbols: Vec<SymbolInfo>,
    diagnostics: Vec<Diagnostic>,
}

impl DocumentState {
    fn new(source: String) -> Self {
        let mut state = Self {
            source,
            ast: None,
            symbols: Vec::new(),
            diagnostics: Vec::new(),
        };
        state.reparse();
        state
    }

    fn update(&mut self, source: String) {
        self.source = source;
        self.reparse();
    }

    fn reparse(&mut self) {
        self.diagnostics.clear();
        self.symbols.clear();
        self.ast = None;

        // Lex
        let mut lexer = Lexer::new(&self.source);
        let tokens = match lexer.tokenize() {
            Ok(t) => t,
            Err(e) => {
                self.diagnostics.push(lexer_error_to_diagnostic(&e));
                return;
            }
        };

        // Parse (with error recovery — report all errors)
        let mut parser = Parser::new(tokens);
        let program = match parser.parse() {
            Ok(p) => p,
            Err(_) => {
                for e in parser.all_errors() {
                    self.diagnostics.push(parser_error_to_diagnostic(e));
                }
                return;
            }
        };

        // Type check
        let type_diags = TypeChecker::new().check(&program);
        for diag in type_diags {
            let severity = match diag.severity {
                harn_parser::DiagnosticSeverity::Error => DiagnosticSeverity::ERROR,
                harn_parser::DiagnosticSeverity::Warning => DiagnosticSeverity::WARNING,
            };
            let range = if let Some(span) = &diag.span {
                span_to_range(span)
            } else {
                Range {
                    start: Position::new(0, 0),
                    end: Position::new(0, 1),
                }
            };
            self.diagnostics.push(Diagnostic {
                range,
                severity: Some(severity),
                source: Some("harn-typecheck".to_string()),
                message: diag.message,
                ..Default::default()
            });
        }

        // Build symbol table
        self.symbols = build_symbol_table(&program);
        self.ast = Some(program);
    }
}

// ---------------------------------------------------------------------------
// Position / span utilities
// ---------------------------------------------------------------------------

/// Convert a 1-based Span to a 0-based LSP Range.
fn span_to_range(span: &Span) -> Range {
    Range {
        start: Position::new(
            span.line.saturating_sub(1) as u32,
            span.column.saturating_sub(1) as u32,
        ),
        end: Position::new(span.line.saturating_sub(1) as u32, span.column as u32),
    }
}

/// Convert a Span to an LSP Range using byte offsets for accurate end position.
fn span_to_full_range(span: &Span, source: &str) -> Range {
    let start_line = span.line.saturating_sub(1) as u32;
    let start_col = span.column.saturating_sub(1) as u32;

    // Calculate end position from byte offset
    let mut end_line = start_line;
    let mut end_col = start_col;
    if span.end > span.start && span.end <= source.len() {
        let segment = &source[span.start..span.end];
        for ch in segment.chars() {
            if ch == '\n' {
                end_line += 1;
                end_col = 0;
            } else {
                end_col += 1;
            }
        }
        // If we only advanced columns (single line), set end_col relative to start
        if end_line == start_line {
            end_col = start_col + segment.chars().count() as u32;
        }
    } else {
        end_col = start_col + 1;
    }

    Range {
        start: Position::new(start_line, start_col),
        end: Position::new(end_line, end_col),
    }
}

/// Check whether a 0-based LSP Position falls within a 1-based Span.
fn position_in_span(pos: &Position, span: &Span, source: &str) -> bool {
    let r = span_to_full_range(span, source);
    if pos.line < r.start.line || pos.line > r.end.line {
        return false;
    }
    if pos.line == r.start.line && pos.character < r.start.character {
        return false;
    }
    if pos.line == r.end.line && pos.character > r.end.character {
        return false;
    }
    true
}

/// Convert a 0-based LSP Position to a byte offset in the source string.
fn lsp_position_to_offset(source: &str, pos: Position) -> usize {
    let mut offset = 0;
    for (i, line) in source.split('\n').enumerate() {
        if i == pos.line as usize {
            return offset + (pos.character as usize).min(line.len());
        }
        offset += line.len() + 1; // +1 for the newline
    }
    source.len()
}

// ---------------------------------------------------------------------------
// LSP backend
// ---------------------------------------------------------------------------

struct HarnLsp {
    client: Client,
    documents: Mutex<HashMap<Url, DocumentState>>,
}

impl HarnLsp {
    fn new(client: Client) -> Self {
        Self {
            client,
            documents: Mutex::new(HashMap::new()),
        }
    }

    /// Get the word at a given position.
    fn word_at_position(source: &str, position: Position) -> Option<String> {
        let lines: Vec<&str> = source.lines().collect();
        let line = lines.get(position.line as usize)?;
        let col = position.character as usize;
        if col > line.len() {
            return None;
        }

        let chars: Vec<char> = line.chars().collect();
        let mut start = col;
        while start > 0 && (chars[start - 1].is_alphanumeric() || chars[start - 1] == '_') {
            start -= 1;
        }
        let mut end = col;
        while end < chars.len() && (chars[end].is_alphanumeric() || chars[end] == '_') {
            end += 1;
        }

        if start == end {
            return None;
        }
        Some(chars[start..end].iter().collect())
    }

    /// Check if cursor is right after a `.` (for method completion).
    fn char_before_position(source: &str, position: Position) -> Option<char> {
        let lines: Vec<&str> = source.lines().collect();
        let line = lines.get(position.line as usize)?;
        let col = position.character as usize;
        if col == 0 {
            return None;
        }
        line.chars().nth(col - 1)
    }

    /// Try to figure out what type the expression before `.` is.
    fn infer_dot_receiver_type(
        source: &str,
        position: Position,
        symbols: &[SymbolInfo],
    ) -> Option<String> {
        // Walk backwards from the dot to find the identifier
        let lines: Vec<&str> = source.lines().collect();
        let line = lines.get(position.line as usize)?;
        let col = position.character as usize;
        if col < 2 {
            return None;
        }

        let chars: Vec<char> = line.chars().collect();
        // Position is after the `.`, so chars[col-1] is `.`. Walk back from col-2.
        let mut end = col - 1; // the dot
        if end == 0 {
            return None;
        }
        end -= 1; // char before dot

        // Skip trailing whitespace (unusual but handle it)
        while end > 0 && chars[end] == ' ' {
            end -= 1;
        }

        // Check for string literal ending in "
        if chars[end] == '"' {
            return Some("string".to_string());
        }
        // Check for ] (list subscript or literal)
        if chars[end] == ']' {
            return Some("list".to_string());
        }
        // Check for } (dict literal)
        if chars[end] == '}' {
            return Some("dict".to_string());
        }

        // Otherwise try to extract an identifier
        if !chars[end].is_alphanumeric() && chars[end] != '_' {
            return None;
        }
        let id_end = end + 1;
        let mut id_start = end;
        while id_start > 0 && (chars[id_start - 1].is_alphanumeric() || chars[id_start - 1] == '_')
        {
            id_start -= 1;
        }
        let name: String = chars[id_start..id_end].iter().collect();

        // Look up the variable's type in the symbol table
        for sym in symbols.iter().rev() {
            if sym.name == name {
                if let Some(ref ty) = sym.type_info {
                    return Some(format_type(ty));
                }
            }
        }
        None
    }
}

#[tower_lsp::async_trait]
impl LanguageServer for HarnLsp {
    async fn initialize(&self, _: InitializeParams) -> Result<InitializeResult> {
        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::FULL,
                )),
                completion_provider: Some(CompletionOptions {
                    trigger_characters: Some(vec![".".to_string()]),
                    ..Default::default()
                }),
                definition_provider: Some(OneOf::Left(true)),
                references_provider: Some(OneOf::Left(true)),
                document_symbol_provider: Some(OneOf::Left(true)),
                hover_provider: Some(HoverProviderCapability::Simple(true)),
                ..Default::default()
            },
            ..Default::default()
        })
    }

    async fn initialized(&self, _: InitializedParams) {
        self.client
            .log_message(MessageType::INFO, "Harn LSP initialized")
            .await;
    }

    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let uri = params.text_document.uri.clone();
        let source = params.text_document.text.clone();

        let state = DocumentState::new(source);
        let diagnostics = state.diagnostics.clone();
        self.documents.lock().unwrap().insert(uri.clone(), state);

        self.client
            .publish_diagnostics(uri, diagnostics, None)
            .await;
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let uri = params.text_document.uri.clone();
        if let Some(change) = params.content_changes.into_iter().last() {
            let source = change.text;
            let diagnostics;
            {
                let mut docs = self.documents.lock().unwrap();
                let entry = docs
                    .entry(uri.clone())
                    .or_insert_with(|| DocumentState::new(String::new()));
                entry.update(source);
                diagnostics = entry.diagnostics.clone();
            }
            self.client
                .publish_diagnostics(uri, diagnostics, None)
                .await;
        }
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        self.documents
            .lock()
            .unwrap()
            .remove(&params.text_document.uri);
    }

    // -----------------------------------------------------------------------
    // Completion (scope-aware + method completion)
    // -----------------------------------------------------------------------
    async fn completion(&self, params: CompletionParams) -> Result<Option<CompletionResponse>> {
        let uri = &params.text_document_position.text_document.uri;
        let position = params.text_document_position.position;

        let docs = self.documents.lock().unwrap();
        let state = match docs.get(uri) {
            Some(s) => s,
            None => return Ok(None),
        };

        let source = state.source.clone();
        let symbols = state.symbols.clone();
        drop(docs);

        let mut items = Vec::new();

        // Check if this is a dot-completion
        if Self::char_before_position(&source, position) == Some('.') {
            let type_name = Self::infer_dot_receiver_type(&source, position, &symbols);
            let methods = match type_name.as_deref() {
                Some("string") => STRING_METHODS,
                Some("list") => LIST_METHODS,
                Some("dict") => DICT_METHODS,
                _ => {
                    // Unknown type — offer all methods
                    for m in STRING_METHODS
                        .iter()
                        .chain(LIST_METHODS.iter())
                        .chain(DICT_METHODS.iter())
                    {
                        items.push(CompletionItem {
                            label: m.to_string(),
                            kind: Some(CompletionItemKind::METHOD),
                            ..Default::default()
                        });
                    }
                    // Deduplicate by label
                    items.sort_by(|a, b| a.label.cmp(&b.label));
                    items.dedup_by(|a, b| a.label == b.label);
                    return Ok(Some(CompletionResponse::Array(items)));
                }
            };
            for m in methods {
                items.push(CompletionItem {
                    label: m.to_string(),
                    kind: Some(CompletionItemKind::METHOD),
                    ..Default::default()
                });
            }
            return Ok(Some(CompletionResponse::Array(items)));
        }

        // Scope-aware: find symbols visible at cursor position
        for sym in &symbols {
            // A symbol is visible if:
            // 1. It has no scope_span (top-level), or
            // 2. The cursor is inside its scope_span
            let visible = match sym.scope_span {
                None => true,
                Some(ref scope) => position_in_span(&position, scope, &source),
            };
            if !visible {
                continue;
            }
            let (kind, detail) = match sym.kind {
                HarnSymbolKind::Pipeline => (CompletionItemKind::FUNCTION, "pipeline"),
                HarnSymbolKind::Function => (CompletionItemKind::FUNCTION, "function"),
                HarnSymbolKind::Variable => (CompletionItemKind::VARIABLE, "variable"),
                HarnSymbolKind::Parameter => (CompletionItemKind::VARIABLE, "parameter"),
                HarnSymbolKind::Enum => (CompletionItemKind::ENUM, "enum"),
                HarnSymbolKind::Struct => (CompletionItemKind::STRUCT, "struct"),
            };
            items.push(CompletionItem {
                label: sym.name.clone(),
                kind: Some(kind),
                detail: Some(sym.signature.as_deref().unwrap_or(detail).to_string()),
                ..Default::default()
            });
        }

        // Add builtins
        for &(name, detail) in BUILTINS {
            items.push(CompletionItem {
                label: name.to_string(),
                kind: Some(CompletionItemKind::FUNCTION),
                detail: Some(detail.to_string()),
                ..Default::default()
            });
        }

        // Add keywords
        for kw in KEYWORDS {
            items.push(CompletionItem {
                label: kw.to_string(),
                kind: Some(CompletionItemKind::KEYWORD),
                ..Default::default()
            });
        }

        Ok(Some(CompletionResponse::Array(items)))
    }

    // -----------------------------------------------------------------------
    // Go-to-definition (AST-based symbol table)
    // -----------------------------------------------------------------------
    async fn goto_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> Result<Option<GotoDefinitionResponse>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;

        let docs = self.documents.lock().unwrap();
        let state = match docs.get(uri) {
            Some(s) => s,
            None => return Ok(None),
        };
        let source = state.source.clone();
        let symbols = state.symbols.clone();
        drop(docs);

        let word = match Self::word_at_position(&source, position) {
            Some(w) => w,
            None => return Ok(None),
        };

        // Look up the name in the symbol table — find the first definition-like symbol
        for sym in &symbols {
            if sym.name == word
                && matches!(
                    sym.kind,
                    HarnSymbolKind::Pipeline
                        | HarnSymbolKind::Function
                        | HarnSymbolKind::Variable
                        | HarnSymbolKind::Parameter
                        | HarnSymbolKind::Enum
                        | HarnSymbolKind::Struct
                )
            {
                let range = span_to_full_range(&sym.def_span, &source);
                return Ok(Some(GotoDefinitionResponse::Scalar(Location {
                    uri: uri.clone(),
                    range,
                })));
            }
        }

        Ok(None)
    }

    // -----------------------------------------------------------------------
    // Find references (AST-based)
    // -----------------------------------------------------------------------
    async fn references(&self, params: ReferenceParams) -> Result<Option<Vec<Location>>> {
        let uri = &params.text_document_position.text_document.uri;
        let position = params.text_document_position.position;

        let docs = self.documents.lock().unwrap();
        let state = match docs.get(uri) {
            Some(s) => s,
            None => return Ok(None),
        };
        let source = state.source.clone();
        let ast = state.ast.clone();
        drop(docs);

        let word = match Self::word_at_position(&source, position) {
            Some(w) => w,
            None => return Ok(None),
        };

        let program = match ast {
            Some(p) => p,
            None => return Ok(None),
        };

        let ref_spans = find_references(&program, &word);
        if ref_spans.is_empty() {
            return Ok(None);
        }

        let locations: Vec<Location> = ref_spans
            .iter()
            .map(|span| Location {
                uri: uri.clone(),
                range: span_to_full_range(span, &source),
            })
            .collect();

        Ok(Some(locations))
    }

    // -----------------------------------------------------------------------
    // Document symbols (AST-based with proper spans)
    // -----------------------------------------------------------------------
    #[allow(deprecated)]
    async fn document_symbol(
        &self,
        params: DocumentSymbolParams,
    ) -> Result<Option<DocumentSymbolResponse>> {
        let uri = &params.text_document.uri;
        let docs = self.documents.lock().unwrap();
        let state = match docs.get(uri) {
            Some(s) => s,
            None => return Ok(None),
        };
        let source = state.source.clone();
        let symbols = state.symbols.clone();
        drop(docs);

        let mut doc_symbols = Vec::new();
        for sym in &symbols {
            // Only include top-level definitions for document symbols
            let kind = match sym.kind {
                HarnSymbolKind::Pipeline => SymbolKind::FUNCTION,
                HarnSymbolKind::Function => SymbolKind::FUNCTION,
                HarnSymbolKind::Variable => SymbolKind::VARIABLE,
                HarnSymbolKind::Enum => SymbolKind::ENUM,
                HarnSymbolKind::Struct => SymbolKind::STRUCT,
                HarnSymbolKind::Parameter => continue, // skip params from outline
            };
            // Only show top-level and direct-child symbols
            if sym.scope_span.is_some()
                && !matches!(
                    sym.kind,
                    HarnSymbolKind::Function | HarnSymbolKind::Variable
                )
            {
                continue;
            }
            let range = span_to_full_range(&sym.def_span, &source);
            let detail = match sym.kind {
                HarnSymbolKind::Pipeline => "pipeline",
                HarnSymbolKind::Function => "function",
                HarnSymbolKind::Variable => "variable",
                HarnSymbolKind::Enum => "enum",
                HarnSymbolKind::Struct => "struct",
                HarnSymbolKind::Parameter => "parameter",
            };
            doc_symbols.push(DocumentSymbol {
                name: sym.name.clone(),
                detail: Some(detail.to_string()),
                kind,
                range,
                selection_range: range,
                tags: None,
                deprecated: None,
                children: None,
            });
        }

        Ok(Some(DocumentSymbolResponse::Nested(doc_symbols)))
    }

    // -----------------------------------------------------------------------
    // Hover (with type information)
    // -----------------------------------------------------------------------
    async fn hover(&self, params: HoverParams) -> Result<Option<Hover>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;

        let docs = self.documents.lock().unwrap();
        let state = match docs.get(uri) {
            Some(s) => s,
            None => return Ok(None),
        };
        let source = state.source.clone();
        let symbols = state.symbols.clone();
        drop(docs);

        let word = match Self::word_at_position(&source, position) {
            Some(w) => w,
            None => return Ok(None),
        };

        // Check builtins first
        if let Some(doc) = builtin_doc(&word) {
            return Ok(Some(Hover {
                contents: HoverContents::Markup(MarkupContent {
                    kind: MarkupKind::Markdown,
                    value: doc,
                }),
                range: None,
            }));
        }

        // Check keywords
        if let Some(doc) = keyword_doc(&word) {
            return Ok(Some(Hover {
                contents: HoverContents::Markup(MarkupContent {
                    kind: MarkupKind::Markdown,
                    value: doc,
                }),
                range: None,
            }));
        }

        // Check user-defined symbols — prefer the innermost scope that
        // contains the cursor position so that shadowed bindings resolve
        // to the closest definition.
        let cursor_offset = lsp_position_to_offset(&source, position);
        let mut best: Option<&SymbolInfo> = None;
        for sym in &symbols {
            if sym.name != word {
                continue;
            }
            // If the symbol has a scope_span, check if the cursor byte
            // offset falls within it.
            let in_scope = match sym.scope_span {
                Some(sp) => cursor_offset >= sp.start && cursor_offset <= sp.end,
                None => true, // top-level symbol is always visible
            };
            if !in_scope {
                continue;
            }
            // Prefer the symbol with the narrowest (innermost) scope.
            match best {
                None => best = Some(sym),
                Some(prev) => {
                    let prev_scope_size = match prev.scope_span {
                        Some(sp) => sp.end.saturating_sub(sp.start),
                        None => usize::MAX,
                    };
                    let this_scope_size = match sym.scope_span {
                        Some(sp) => sp.end.saturating_sub(sp.start),
                        None => usize::MAX,
                    };
                    if this_scope_size < prev_scope_size {
                        best = Some(sym);
                    }
                }
            }
        }
        if let Some(sym) = best {
            let mut hover_text = String::new();

            // Show signature if available (functions, pipelines, structs, enums)
            if let Some(ref sig) = sym.signature {
                hover_text.push_str(&format!("```harn\n{sig}\n```\n"));
            } else {
                // For variables/parameters, build a code-block declaration
                // with the type annotation when known.
                let keyword = match sym.kind {
                    HarnSymbolKind::Variable => "let",
                    HarnSymbolKind::Parameter => "param",
                    _ => "",
                };
                if let Some(ref ty) = sym.type_info {
                    hover_text.push_str(&format!(
                        "```harn\n{keyword} {}: {}\n```\n",
                        sym.name,
                        format_type(ty)
                    ));
                } else {
                    let kind_str = match sym.kind {
                        HarnSymbolKind::Pipeline => "pipeline",
                        HarnSymbolKind::Function => "function",
                        HarnSymbolKind::Variable => "variable",
                        HarnSymbolKind::Parameter => "parameter",
                        HarnSymbolKind::Enum => "enum",
                        HarnSymbolKind::Struct => "struct",
                    };
                    hover_text.push_str(&format!("**{kind_str}** `{}`", sym.name));
                }
            }

            // For functions with a return type, show it below the signature
            // (signatures already include "-> type", so only add for
            // variables/params where the type is a shape and worth expanding).
            if sym.signature.is_none() {
                if let Some(ref ty) = sym.type_info {
                    if matches!(ty, TypeExpr::Shape(_)) {
                        // Already shown in the code block above; add a
                        // human-readable breakdown for complex shapes.
                        let expanded = format_shape_expanded(ty, 0);
                        if !expanded.is_empty() {
                            hover_text.push_str(&format!("\n{expanded}"));
                        }
                    }
                }
            }

            return Ok(Some(Hover {
                contents: HoverContents::Markup(MarkupContent {
                    kind: MarkupKind::Markdown,
                    value: hover_text,
                }),
                range: None,
            }));
        }

        Ok(None)
    }
}

// ---------------------------------------------------------------------------
// Error conversion helpers
// ---------------------------------------------------------------------------

fn lexer_error_to_diagnostic(err: &LexerError) -> Diagnostic {
    let (message, line, col) = match err {
        LexerError::UnexpectedCharacter(ch, span) => (
            format!("Unexpected character '{ch}'"),
            span.line,
            span.column,
        ),
        LexerError::UnterminatedString(span) => {
            ("Unterminated string".to_string(), span.line, span.column)
        }
        LexerError::UnterminatedBlockComment(span) => (
            "Unterminated block comment".to_string(),
            span.line,
            span.column,
        ),
    };

    Diagnostic {
        range: Range {
            start: Position::new((line - 1) as u32, (col - 1) as u32),
            end: Position::new((line - 1) as u32, col as u32),
        },
        severity: Some(DiagnosticSeverity::ERROR),
        source: Some("harn".to_string()),
        message,
        ..Default::default()
    }
}

fn parser_error_to_diagnostic(err: &ParserError) -> Diagnostic {
    match err {
        ParserError::Unexpected {
            got,
            expected,
            span,
        } => Diagnostic {
            range: Range {
                start: Position::new((span.line - 1) as u32, (span.column - 1) as u32),
                end: Position::new((span.line - 1) as u32, span.column as u32),
            },
            severity: Some(DiagnosticSeverity::ERROR),
            source: Some("harn".to_string()),
            message: format!("Expected {expected}, got {got}"),
            ..Default::default()
        },
        ParserError::UnexpectedEof { expected } => Diagnostic {
            range: Range {
                start: Position::new(0, 0),
                end: Position::new(0, 1),
            },
            severity: Some(DiagnosticSeverity::ERROR),
            source: Some("harn".to_string()),
            message: format!("Unexpected end of file, expected {expected}"),
            ..Default::default()
        },
    }
}

// ---------------------------------------------------------------------------
// Hover documentation
// ---------------------------------------------------------------------------

fn builtin_doc(name: &str) -> Option<String> {
    let doc = match name {
        "log" => "**log(value)** — Print value to stdout with `[harn]` prefix",
        "print" => "**print(value)** — Print value to stdout (no newline)",
        "println" => "**println(value)** — Print value to stdout with newline",
        "type_of" => "**type_of(value)** → string — Returns the type name",
        "to_string" => "**to_string(value)** → string — Convert to string",
        "to_int" => "**to_int(value)** → int — Convert to integer",
        "to_float" => "**to_float(value)** → float — Convert to float",
        "json_parse" => "**json_parse(text)** → value — Parse JSON string into Harn value",
        "json_stringify" => "**json_stringify(value)** → string — Convert value to JSON string",
        "env" => "**env(name)** → string | nil — Get environment variable",
        "timestamp" => "**timestamp()** → float — Unix timestamp in seconds",
        "sleep" => "**sleep(ms)** → nil — Async sleep for milliseconds",
        "read_file" => "**read_file(path)** → string — Read file contents",
        "write_file" => "**write_file(path, content)** → nil — Write string to file",
        "exit" => "**exit(code)** — Terminate process with exit code",
        "regex_match" => "**regex_match(pattern, text)** → list | nil — Find all regex matches",
        "regex_replace" => {
            "**regex_replace(pattern, replacement, text)** → string — Replace regex matches"
        }
        "http_get" => "**http_get(url)** → string — HTTP GET request",
        "http_post" => "**http_post(url, body, headers?)** → string — HTTP POST request",
        "llm_call" => "**llm_call(prompt, system?, options?)** → string — Call an LLM API\n\nOptions: `{provider, model, max_tokens}`",
        "agent_loop" => "**agent_loop(prompt, system?, options?)** → string — Agent loop with tool dispatch\n\nOptions: `{provider, model, persistent, max_iterations, max_nudges, nudge}`\n\nIn persistent mode, loop continues until `##DONE##` sentinel is output.",
        "await" => "**await(handle)** → value — Wait for spawned task to complete",
        "cancel" => "**cancel(handle)** → nil — Cancel a spawned task",
        "abs" => "**abs(value)** → int | float — Absolute value",
        "min" => "**min(a, b)** → int | float — Minimum of two values",
        "max" => "**max(a, b)** → int | float — Maximum of two values",
        "floor" => "**floor(value)** → int — Floor of a float",
        "ceil" => "**ceil(value)** → int — Ceiling of a float",
        "round" => "**round(value)** → int — Round a float to nearest integer",
        "sqrt" => "**sqrt(value)** → float — Square root",
        "pow" => "**pow(base, exp)** → int | float — Exponentiation",
        "random" => "**random()** → float — Random float in [0, 1)",
        "random_int" => "**random_int(min, max)** → int — Random integer in [min, max]",
        "assert" => "**assert(condition, message?)** — Assert condition is truthy",
        "assert_eq" => "**assert_eq(actual, expected, message?)** — Assert two values are equal",
        "assert_ne" => "**assert_ne(actual, expected, message?)** — Assert two values are not equal",
        "file_exists" => "**file_exists(path)** → bool — Check if file or directory exists",
        "delete_file" => "**delete_file(path)** → nil — Delete a file or directory",
        "list_dir" => "**list_dir(path?)** → list — List directory entries (sorted)",
        "mkdir" => "**mkdir(path)** → nil — Create directory (and parents)",
        "path_join" => "**path_join(parts...)** → string — Join path segments",
        "copy_file" => "**copy_file(src, dst)** → nil — Copy a file",
        "append_file" => "**append_file(path, content)** → nil — Append to a file",
        "temp_dir" => "**temp_dir()** → string — System temp directory path",
        "stat" => "**stat(path)** → dict — File metadata: size, is_file, is_dir, readonly, modified",
        "exec" => "**exec(cmd, args...)** → dict — Run a command, returns {stdout, stderr, status, success}",
        "shell" => "**shell(cmd)** → dict — Run shell command, returns {stdout, stderr, status, success}",
        "date_now" => "**date_now()** → dict — Current UTC date: {year, month, day, hour, minute, second, weekday, timestamp}",
        "date_format" => "**date_format(timestamp, fmt?)** → string — Format timestamp (%Y, %m, %d, %H, %M, %S)",
        "date_parse" => "**date_parse(str)** → float — Parse date string to Unix timestamp",
        "format" => "**format(template, args...)** → string — String formatting with {} placeholders",
        "channel" => "**channel(name?, capacity?)** → channel — Create an async channel",
        "send" => "**send(channel, value)** → bool — Send a value on a channel",
        "receive" => "**receive(channel)** → value — Receive next value from channel (blocks)",
        "try_receive" => "**try_receive(channel)** → value | nil — Non-blocking receive",
        "close_channel" => "**close_channel(channel)** → nil — Close a channel",
        "atomic" => "**atomic(initial?)** → atomic — Create an atomic integer",
        "atomic_get" => "**atomic_get(a)** → int — Read atomic value",
        "atomic_set" => "**atomic_set(a, value)** → int — Set atomic value, returns old",
        "atomic_add" => "**atomic_add(a, n)** → int — Atomically add, returns previous value",
        "atomic_cas" => "**atomic_cas(a, expected, new)** → bool — Compare-and-swap",
        "select" => "**select(ch1, ch2, ...)** → dict — Wait for first channel with data: {index, value, channel}",
        "prompt_user" => "**prompt_user(message?)** → string — Read a line from stdin",
        _ => return None,
    };
    Some(doc.to_string())
}

fn keyword_doc(name: &str) -> Option<String> {
    let doc = match name {
        "pipeline" => "**pipeline** — Declare a named pipeline\n\n```harn\npipeline name(params) {\n  // body\n}\n```",
        "fn" => "**fn** — Declare a function\n\n```harn\nfn name(params) -> return_type {\n  // body\n}\n```",
        "let" => "**let** — Immutable variable binding\n\n```harn\nlet x: type = value\n```",
        "var" => "**var** — Mutable variable binding\n\n```harn\nvar x: type = value\n```",
        "if" => "**if** — Conditional expression\n\n```harn\nif condition {\n  // then\n} else {\n  // else\n}\n```",
        "else" => "**else** — Else branch of an if expression",
        "for" => "**for** — For-in loop\n\n```harn\nfor item in iterable {\n  // body\n}\n```",
        "while" => "**while** — While loop\n\n```harn\nwhile condition {\n  // body\n}\n```",
        "match" => "**match** — Pattern matching expression\n\n```harn\nmatch value {\n  pattern => body\n}\n```",
        "return" => "**return** — Return a value from a function",
        "try" => "**try** — Try-catch error handling\n\n```harn\ntry {\n  // body\n} catch e {\n  // handle\n}\n```",
        "catch" => "**catch** — Catch block for error handling",
        "throw" => "**throw** — Throw an error value",
        "import" => "**import** — Import a module\n\n```harn\nimport \"path/to/module\"\n```",
        "spawn" => "**spawn** — Spawn an async task\n\n```harn\nlet handle = spawn {\n  // async body\n}\n```",
        "parallel" => "**parallel** — Execute N parallel tasks\n\n```harn\nparallel N {\n  // body\n}\n```",
        "parallel_map" => "**parallel_map** — Map over a list in parallel\n\n```harn\nparallel_map list as item {\n  // body\n}\n```",
        "retry" => "**retry** — Retry a block N times\n\n```harn\nretry N {\n  // body\n}\n```",
        "extends" => "**extends** — Inherit from another pipeline",
        "override" => "**override** — Override an inherited pipeline step",
        "true" | "false" => "**bool** — Boolean literal",
        "nil" => "**nil** — Nil value (absence of a value)",
        "in" => "**in** — Used in `for x in collection`",
        _ => return None,
    };
    Some(doc.to_string())
}

// ---------------------------------------------------------------------------
// main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();

    let (service, socket) = LspService::new(HarnLsp::new);

    Server::new(stdin, stdout, socket).serve(service).await;
}
