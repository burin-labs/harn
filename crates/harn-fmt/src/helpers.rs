use std::collections::BTreeMap;

use harn_parser::{
    Attribute, AttributeArg, BindingPattern, Node, SNode, TypeExpr, TypeParam, TypedParam,
    Variance, WhereClause,
};

use crate::{Formatter, AUTO_SEPARATOR_WIDTH};

/// Format a default-value expression in a destructuring pattern.
fn format_default_expr(node: &SNode) -> String {
    let fmt = Formatter::new("", BTreeMap::new(), 100, AUTO_SEPARATOR_WIDTH);
    fmt.format_expr(node, 0)
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
    let value = format_attribute_value(&arg.value);
    match &arg.name {
        Some(k) => format!("{k}: {value}"),
        None => value,
    }
}

fn format_attribute_value(node: &SNode) -> String {
    match &node.node {
        Node::StringLiteral(s) => format!("\"{}\"", escape_string(s)),
        Node::RawStringLiteral(s) => format_raw_string(s),
        Node::IntLiteral(i) => i.to_string(),
        Node::FloatLiteral(f) => format_float(*f),
        Node::BoolLiteral(b) => b.to_string(),
        Node::NilLiteral => "nil".to_string(),
        Node::Identifier(name) => name.clone(),
        Node::ListLiteral(items) => {
            let items = items
                .iter()
                .map(format_attribute_value)
                .collect::<Vec<_>>()
                .join(", ");
            format!("[{items}]")
        }
        Node::DictLiteral(entries) => {
            let entries = entries
                .iter()
                .map(|entry| {
                    let key = match &entry.key.node {
                        Node::Identifier(name) => name.clone(),
                        Node::StringLiteral(name) | Node::RawStringLiteral(name)
                            if is_identifier(name) =>
                        {
                            name.clone()
                        }
                        Node::StringLiteral(name) | Node::RawStringLiteral(name) => {
                            format!("\"{}\"", escape_string(name))
                        }
                        _ => format!("[{}]", format_attribute_value(&entry.key)),
                    };
                    format!("{key}: {}", format_attribute_value(&entry.value))
                })
                .collect::<Vec<_>>()
                .join(", ");
            format!("{{{entries}}}")
        }
        Node::FunctionCall { name, args, .. } => {
            let args = args
                .iter()
                .map(format_attribute_value)
                .collect::<Vec<_>>()
                .join(", ");
            format!("{name}({args})")
        }
        _ => format_default_expr(node),
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
                            s = format!("{s} = {default_str}");
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
        BindingPattern::Pair(a, b) => format!("({a}, {b})"),
    }
}

/// Escape a string for embedding in double-quoted output.
pub(crate) fn escape_string(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
        .replace('\0', "\\0")
        .replace("${", "\\${")
}

/// Render a raw-string literal value, choosing the narrowest delimiter that
/// can safely contain it. A value with no `"` uses `r"..."`; otherwise it uses
/// `r#"..."#` (or more `#`) so the body's quotes can't terminate the literal
/// early. The hash count is `1 + the longest run of `#` that immediately
/// follows a `"` in the body`, which is the minimal width that keeps the close
/// delimiter (`"` + hashes) from appearing inside the body. Deterministic, so
/// `fmt` is idempotent on its own output.
pub(crate) fn format_raw_string(s: &str) -> String {
    if !s.contains('"') {
        return format!("r\"{s}\"");
    }
    let chars: Vec<char> = s.chars().collect();
    let mut max_run = 0usize;
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '"' {
            let mut run = 0usize;
            let mut j = i + 1;
            while j < chars.len() && chars[j] == '#' {
                run += 1;
                j += 1;
            }
            if run > max_run {
                max_run = run;
            }
        }
        i += 1;
    }
    let hashes = "#".repeat(max_run + 1);
    format!("r{hashes}\"{s}\"{hashes}")
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
        TypeExpr::Union(types) => {
            if let Some(inner) = optional_sugar_inner(types) {
                return format!("{}?", format_type_expr(inner));
            }
            types
                .iter()
                .map(format_type_expr)
                .collect::<Vec<_>>()
                .join(" | ")
        }
        TypeExpr::Intersection(types) => types
            .iter()
            .map(|t| match t {
                // A `T | nil` arm renders as `T?`, which binds tighter than
                // `&` and round-trips through the parser without parens.
                TypeExpr::Union(members) if optional_sugar_inner(members).is_some() => {
                    format_type_expr(t)
                }
                // Other nested unions still get parenthesised for readability,
                // even though parens are not yet a valid type-grammar form
                // — that is a pre-existing limitation of the surface syntax.
                TypeExpr::Union(_) => format!("({})", format_type_expr(t)),
                _ => format_type_expr(t),
            })
            .collect::<Vec<_>>()
            .join(" & "),
        TypeExpr::Shape(fields) => format_shape_inline(fields),
        TypeExpr::List(inner) => {
            format!("list<{}>", format_type_expr(inner))
        }
        TypeExpr::Iter(inner) => {
            format!("iter<{}>", format_type_expr(inner))
        }
        TypeExpr::Generator(inner) => {
            format!("Generator<{}>", format_type_expr(inner))
        }
        TypeExpr::Stream(inner) => {
            format!("Stream<{}>", format_type_expr(inner))
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
        TypeExpr::Owned(inner) => format!("owned<{}>", format_type_expr(inner)),
    }
}

fn format_shape_inline(fields: &[harn_parser::ShapeField]) -> String {
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

/// Like [`format_type_expr`] but wraps top-level shape fields onto
/// multiple lines when the inline rendering, prefixed with
/// `prefix_len` columns and aligned to `indent` levels, would exceed
/// `line_width`. Mirrors the parser's preference for one-field-per-
/// line shapes in source — collapsing a 10-field options shape to a
/// single line on `harn fmt` was the source of repeated round-trip
/// noise in `stdlib_hitl.harn`, the example workflows, etc.
pub(crate) fn format_type_expr_wrapped(
    te: &TypeExpr,
    indent: usize,
    prefix_len: usize,
    line_width: usize,
) -> String {
    let inline = format_type_expr(te);
    if prefix_len + inline.len() <= line_width {
        return inline;
    }
    match te {
        TypeExpr::Shape(fields) => format_shape_wrapped(fields, indent, line_width),
        TypeExpr::Union(types) => {
            if let Some(inner) = optional_sugar_inner(types) {
                // The wrapped variant follows the inline form for `T?`;
                // the inner type may still wrap if it is itself a shape.
                let inner_str = format_type_expr_wrapped(inner, indent, prefix_len, line_width);
                return format!("{inner_str}?");
            }
            // Wrap each union arm; if any arm is itself a Shape it can
            // recurse and emit its own multi-line form.
            let arms: Vec<String> = types
                .iter()
                .map(|arm| format_type_expr_wrapped(arm, indent, prefix_len, line_width))
                .collect();
            arms.join(" | ")
        }
        _ => inline,
    }
}

/// If `types` is exactly two members and one is `nil`, return the
/// non-`nil` member when it can be safely rendered as `T?`. Returns
/// `None` for unions that need explicit `T | nil` form because the
/// non-`nil` arm would re-bind unexpectedly under postfix `?` (today:
/// `Union`, `Intersection`, and `FnType`, where `?` would attach to the
/// inner return type instead of the whole arm).
pub(crate) fn optional_sugar_inner(types: &[TypeExpr]) -> Option<&TypeExpr> {
    if types.len() != 2 {
        return None;
    }
    let nil_idx = types
        .iter()
        .position(|t| matches!(t, TypeExpr::Named(n) if n == "nil"))?;
    let inner = &types[1 - nil_idx];
    if matches!(
        inner,
        TypeExpr::Union(_) | TypeExpr::Intersection(_) | TypeExpr::FnType { .. }
    ) {
        return None;
    }
    if matches!(inner, TypeExpr::Named(n) if n == "nil") {
        return None;
    }
    Some(inner)
}

fn format_shape_wrapped(
    fields: &[harn_parser::ShapeField],
    indent: usize,
    line_width: usize,
) -> String {
    let pad_inner = "  ".repeat(indent + 1);
    let pad_close = "  ".repeat(indent);
    let lines: Vec<String> = fields
        .iter()
        .map(|f| {
            let opt = if f.optional { "?" } else { "" };
            // Recurse so nested shape fields wrap relative to their
            // own indent, not the outer one.
            let prefix_len = pad_inner.len() + f.name.len() + opt.len() + 2;
            let value = format_type_expr_wrapped(&f.type_expr, indent + 1, prefix_len, line_width);
            format!("{pad_inner}{}{opt}: {value},", f.name)
        })
        .collect();
    format!("{{\n{}\n{pad_close}}}", lines.join("\n"))
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
    let fmt = Formatter::new("", BTreeMap::new(), 100, AUTO_SEPARATOR_WIDTH);
    fmt.format_expr(node, 0)
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
    if ms.is_multiple_of(604_800_000) {
        format!("{}w", ms / 604_800_000)
    } else if ms.is_multiple_of(86_400_000) {
        format!("{}d", ms / 86_400_000)
    } else if ms.is_multiple_of(3_600_000) {
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

/// Visual width of the last (or only) line of `s`. Used to compute column
/// positions for wrap decisions when a sub-expression was rendered onto
/// multiple physical lines and the next token tails its last line.
pub(crate) fn last_line_width(s: &str) -> usize {
    match s.rfind('\n') {
        Some(idx) => s[idx + 1..].chars().count(),
        None => s.chars().count(),
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
