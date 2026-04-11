use std::collections::BTreeMap;

use harn_parser::{BindingPattern, Node, SNode, TypeExpr, TypeParam, TypedParam, WhereClause};

use crate::Formatter;

/// Format a default-value expression in a destructuring pattern.
/// Creates a temporary formatter to render the expression.
fn format_default_expr(node: &SNode) -> String {
    let fmt = Formatter::new(BTreeMap::new(), 100);
    fmt.format_expr(node)
}

/// Return a numeric precedence for binary operators (higher = tighter binding).
///
/// `??` sits between additive and multiplicative — tighter than
/// `+ - < > == != && || ?:` but looser than `* / % **`. This matches the
/// parser's placement (harn-parser: parse_additive → parse_nil_coalescing →
/// parse_multiplicative) and the intuition `xs?.count ?? 0 > 0` →
/// `(xs?.count ?? 0) > 0`.
pub(crate) fn op_precedence(op: &str) -> u8 {
    match op {
        "|>" => 1,
        "||" => 3,
        "&&" => 4,
        "==" | "!=" => 5,
        "<" | ">" | "<=" | ">=" | "in" | "not_in" | "is" => 6,
        "+" | "-" => 7,
        "??" => 8,
        "*" | "/" | "%" => 9,
        "**" => 10,
        _ => 11,
    }
}

/// Whether `node` needs parentheses when used as the object of a postfix
/// operation (method call, property access, subscript, optional chain, try, slice).
pub(crate) fn needs_parens_as_postfix_object(node: &Node) -> bool {
    matches!(
        node,
        Node::BinaryOp { .. }
            | Node::UnaryOp { .. }
            | Node::Ternary { .. }
            | Node::RangeExpr { .. }
            | Node::Assignment { .. }
    )
}

/// Whether `node` needs parentheses as the operand of a unary prefix (`!`, `-`).
pub(crate) fn needs_parens_as_unary_operand(node: &Node) -> bool {
    matches!(
        node,
        Node::BinaryOp { .. }
            | Node::Ternary { .. }
            | Node::RangeExpr { .. }
            | Node::Assignment { .. }
    )
}

/// Determine whether a child BinaryOp needs parentheses when nested inside
/// a parent BinaryOp.  Covers both correctness (semantics-preserving) and
/// clarity (`&&` / `||` mixing).
pub(crate) fn child_needs_parens(parent_op: &str, child: &Node, is_right: bool) -> bool {
    if let Node::BinaryOp { op: child_op, .. } = child {
        let p = op_precedence(parent_op);
        let c = op_precedence(child_op);

        // Correctness: child binds less tightly than parent.
        if c < p {
            return true;
        }

        // Correctness: exponentiation is right-associative, so `(a ** b) ** c`
        // must keep its left grouping while `a ** b ** c` does not need
        // extra parens on the right.
        if parent_op == "**" && child_op == "**" {
            return !is_right;
        }

        // Correctness: right child at same precedence level needs parens.
        //
        // Even when the operator token matches, the formatter cannot prove
        // the operation is safely associative across all runtime types
        // (`+`/`*` on floats, for example), and the parser does not preserve
        // explicit grouping nodes. Preserve the user's right-grouping.
        if is_right && c == p {
            return true;
        }

        // Clarity: always parenthesise && inside || (and vice-versa).
        if matches!((parent_op, child_op.as_str()), ("||", "&&") | ("&&", "||")) {
            return true;
        }
    }
    false
}

/// Operators for which the Harn lexer/parser uses `check_skip_newlines`;
/// a line break before them is safe without a backslash continuation.
pub(crate) fn op_safe_after_newline(op: &str) -> bool {
    matches!(op, "|>" | "||" | "&&" | "+" | "*" | "/" | "%" | "**")
}

/// Format a binding pattern to a string.
pub(crate) fn format_pattern(pattern: &BindingPattern) -> String {
    match pattern {
        BindingPattern::Identifier(name) => name.clone(),
        BindingPattern::Dict(fields) => {
            let parts: Vec<String> = fields
                .iter()
                .map(|f| {
                    if f.is_rest {
                        format!("...{}", f.key)
                    } else {
                        let mut s = f.key.clone();
                        if let Some(alias) = &f.alias {
                            s = format!("{}: {}", f.key, alias);
                        }
                        if let Some(default) = &f.default_value {
                            let default_str = format_default_expr(default);
                            s = format!("{} = {}", s, default_str);
                        }
                        s
                    }
                })
                .collect();
            format!("{{{}}}", parts.join(", "))
        }
        BindingPattern::List(elements) => {
            let parts: Vec<String> = elements
                .iter()
                .map(|e| {
                    if e.is_rest {
                        format!("...{}", e.name)
                    } else if let Some(default) = &e.default_value {
                        let default_str = format_default_expr(default);
                        format!("{} = {}", e.name, default_str)
                    } else {
                        e.name.clone()
                    }
                })
                .collect();
            format!("[{}]", parts.join(", "))
        }
    }
}

/// Escape a string for embedding in double-quoted output.
pub(crate) fn escape_string(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\t', "\\t")
}

/// Format the `(error_var: Type)` portion of a catch clause.
pub(crate) fn format_catch_param(
    error_var: &Option<String>,
    error_type: &Option<TypeExpr>,
) -> String {
    match (error_var, error_type) {
        (Some(var), Some(ty)) => format!(" ({var}: {})", format_type_expr(ty)),
        (Some(var), None) => format!(" ({var})"),
        _ => String::new(),
    }
}

pub(crate) fn format_type_ann(type_ann: &Option<TypeExpr>) -> String {
    if let Some(te) = type_ann {
        format!(": {}", format_type_expr(te))
    } else {
        String::new()
    }
}

pub(crate) fn format_type_expr(te: &TypeExpr) -> String {
    match te {
        TypeExpr::Named(name) => name.clone(),
        TypeExpr::Union(types) => types
            .iter()
            .map(format_type_expr)
            .collect::<Vec<_>>()
            .join(" | "),
        TypeExpr::Shape(fields) => {
            let items = fields
                .iter()
                .map(|f| {
                    let opt = if f.optional { "?" } else { "" };
                    format!("{}{opt}: {}", f.name, format_type_expr(&f.type_expr))
                })
                .collect::<Vec<_>>()
                .join(", ");
            format!("{{{items}}}")
        }
        TypeExpr::List(inner) => {
            format!("list<{}>", format_type_expr(inner))
        }
        TypeExpr::DictType(k, v) => {
            format!("dict<{}, {}>", format_type_expr(k), format_type_expr(v))
        }
        TypeExpr::Applied { name, args } => {
            let args = args
                .iter()
                .map(format_type_expr)
                .collect::<Vec<_>>()
                .join(", ");
            format!("{name}<{args}>")
        }
        TypeExpr::FnType {
            params,
            return_type,
        } => {
            let params_str = params
                .iter()
                .map(format_type_expr)
                .collect::<Vec<_>>()
                .join(", ");
            format!("fn({}) -> {}", params_str, format_type_expr(return_type))
        }
        TypeExpr::Never => "never".to_string(),
    }
}

pub(crate) fn format_type_params(type_params: &[TypeParam]) -> String {
    if type_params.is_empty() {
        String::new()
    } else {
        let names: Vec<&str> = type_params.iter().map(|tp| tp.name.as_str()).collect();
        format!("<{}>", names.join(", "))
    }
}

pub(crate) fn format_where_clauses(clauses: &[WhereClause]) -> String {
    if clauses.is_empty() {
        String::new()
    } else {
        let parts: Vec<String> = clauses
            .iter()
            .map(|c| format!("{}: {}", c.type_name, c.bound))
            .collect();
        format!(" where {}", parts.join(", "))
    }
}

/// Format an expression inline for use in parameter defaults.
pub(crate) fn format_inline_expr(node: &SNode) -> String {
    let fmt = Formatter::new(BTreeMap::new(), 100);
    fmt.format_expr(node)
}

/// Render typed params to individual strings (without joining).
pub(crate) fn render_typed_params(params: &[TypedParam]) -> Vec<String> {
    params
        .iter()
        .map(|p| {
            let prefix = if p.rest { "..." } else { "" };
            let mut s = if let Some(te) = &p.type_expr {
                format!("{prefix}{}: {}", p.name, format_type_expr(te))
            } else {
                format!("{prefix}{}", p.name)
            };
            if let Some(default) = &p.default_value {
                s.push_str(&format!(" = {}", format_inline_expr(default)));
            }
            s
        })
        .collect()
}

/// Render typed params joined inline (no wrapping).
pub(crate) fn format_typed_params(params: &[TypedParam]) -> String {
    render_typed_params(params).join(", ")
}

pub(crate) fn format_duration(ms: u64) -> String {
    if ms == 0 {
        return "0ms".to_string();
    }
    if ms.is_multiple_of(3_600_000) {
        format!("{}h", ms / 3_600_000)
    } else if ms.is_multiple_of(60_000) {
        format!("{}m", ms / 60_000)
    } else if ms.is_multiple_of(1_000) {
        format!("{}s", ms / 1_000)
    } else {
        format!("{ms}ms")
    }
}

pub(crate) fn format_float(f: f64) -> String {
    let s = f.to_string();
    if s.contains('.') {
        s
    } else {
        format!("{s}.0")
    }
}

pub(crate) fn is_identifier(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

pub(crate) fn is_simple_expr(node: &SNode) -> bool {
    matches!(
        &node.node,
        Node::StringLiteral(_)
            | Node::InterpolatedString(_)
            | Node::IntLiteral(_)
            | Node::FloatLiteral(_)
            | Node::BoolLiteral(_)
            | Node::NilLiteral
            | Node::Identifier(_)
            | Node::DurationLiteral(_)
            | Node::BinaryOp { .. }
            | Node::UnaryOp { .. }
            | Node::FunctionCall { .. }
            | Node::MethodCall { .. }
            | Node::OptionalMethodCall { .. }
            | Node::PropertyAccess { .. }
            | Node::OptionalPropertyAccess { .. }
            | Node::SubscriptAccess { .. }
            | Node::SliceAccess { .. }
            | Node::Ternary { .. }
            | Node::Assignment { .. }
            | Node::ListLiteral(_)
            | Node::DictLiteral(_)
            | Node::RangeExpr { .. }
            | Node::EnumConstruct { .. }
            | Node::TryOperator { .. }
            | Node::ReturnStmt { .. }
            | Node::BreakStmt
            | Node::ContinueStmt
            | Node::RequireStmt { .. }
    )
}
