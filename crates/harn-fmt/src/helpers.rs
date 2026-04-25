use std::collections::BTreeMap;

use harn_parser::{
    Attribute, AttributeArg, BindingPattern, Node, SNode, TypeExpr, TypeParam, TypedParam,
    Variance, WhereClause,
};

use crate::Formatter;

/// Format a default-value expression in a destructuring pattern.
fn format_default_expr(node: &SNode) -> String {
    let fmt = Formatter::new("", BTreeMap::new(), 100, 80);
    fmt.format_expr(node)
}

/// Format a single attribute as `@name` or `@name(arg, key: value)`.
pub(crate) fn format_attribute(attr: &Attribute) -> String {
    if attr.args.is_empty() {
        format!("@{}", attr.name)
    } else {
        let args = attr
            .args
            .iter()
            .map(format_attribute_arg)
            .collect::<Vec<_>>()
            .join(", ");
        format!("@{}({})", attr.name, args)
    }
}

/// Format a list of attributes as newline-separated lines (trailing newline).
pub(crate) fn format_attributes(attrs: &[Attribute]) -> String {
    let mut s = String::new();
    for attr in attrs {
        s.push_str(&format_attribute(attr));
        s.push('\n');
    }
    s
}

fn format_attribute_arg(arg: &AttributeArg) -> String {
    let value = format_default_expr(&arg.value);
    match &arg.name {
        Some(k) => format!("{}: {}", k, value),
        None => value,
    }
}

/// Numeric precedence for binary operators (higher = tighter binding).
/// `??` sits between additive and multiplicative so `xs?.count ?? 0 > 0`
/// naturally groups as `(xs?.count ?? 0) > 0`; must track the parser.
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

        if c < p {
            return true;
        }

        // `**` is right-associative: `(a ** b) ** c` must keep its left grouping.
        if parent_op == "**" && child_op == "**" {
            return !is_right;
        }

        // Same-precedence right child: the formatter can't prove associativity
        // (e.g. `+`/`*` on floats aren't truly associative) and the AST has
        // already dropped explicit grouping nodes, so preserve right-grouping.
        if is_right && c == p {
            return true;
        }

        // Mixing `&&` and `||` always gets parens for clarity.
        if matches!((parent_op, child_op.as_str()), ("||", "&&") | ("&&", "||")) {
            return true;
        }
    }
    false
}

/// Operators for which the Harn parser accepts a line break before the
/// operator without a backslash continuation.
pub(crate) fn op_safe_after_newline(op: &str) -> bool {
    matches!(
        op,
        "|>" | "||"
            | "&&"
            | "=="
            | "!="
            | "<"
            | ">"
            | "<="
            | ">="
            | "??"
            | "+"
            | "*"
            | "/"
            | "%"
            | "**"
    )
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
        BindingPattern::Pair(a, b) => format!("({}, {})", a, b),
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
        TypeExpr::Iter(inner) => {
            format!("iter<{}>", format_type_expr(inner))
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
        TypeExpr::LitString(s) => format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"")),
        TypeExpr::LitInt(v) => v.to_string(),
    }
}

pub(crate) fn format_type_params(type_params: &[TypeParam]) -> String {
    if type_params.is_empty() {
        String::new()
    } else {
        let parts: Vec<String> = type_params
            .iter()
            .map(|tp| match tp.variance {
                Variance::Covariant => format!("out {}", tp.name),
                Variance::Contravariant => format!("in {}", tp.name),
                Variance::Invariant => tp.name.clone(),
            })
            .collect();
        format!("<{}>", parts.join(", "))
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
    let fmt = Formatter::new("", BTreeMap::new(), 100, 80);
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
            | Node::OptionalSubscriptAccess { .. }
            | Node::SliceAccess { .. }
            | Node::Ternary { .. }
            | Node::Assignment { .. }
            | Node::ListLiteral(_)
            | Node::DictLiteral(_)
            | Node::RangeExpr { .. }
            | Node::EnumConstruct { .. }
            | Node::TryOperator { .. }
            | Node::TryStar { .. }
            | Node::ReturnStmt { .. }
            | Node::BreakStmt
            | Node::ContinueStmt
            | Node::RequireStmt { .. }
    )
}
