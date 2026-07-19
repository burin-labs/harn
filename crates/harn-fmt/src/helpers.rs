use std::collections::BTreeMap;

use harn_parser::{
    Attribute, AttributeArg, BindingPattern, Node, SNode, TypeExpr, TypeParam, TypedParam,
    Variance, WhereClause,
};

use crate::{Formatter, AUTO_SEPARATOR_WIDTH};

/// Format a default-value expression in a destructuring pattern.
fn format_default_expr(node: &SNode) -> String {
    let fmt = Formatter::new("", BTreeMap::new(), 100, AUTO_SEPARATOR_WIDTH);
    fmt.format_expr(node, 0, 0)
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

/// Wrap an already-formatted expression in parens when `node` is a range.
/// Used for contexts a range can't legally occupy bare — a ternary
/// condition/branch, or another range's bound — where omitting the parens
/// would change the meaning or fail to re-parse.
pub(crate) fn paren_if_range(formatted: String, node: &Node) -> String {
    if matches!(node, Node::RangeExpr { .. }) {
        format!("({formatted})")
    } else {
        formatted
    }
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
    // A range (`a to b`) binds looser than every binary operator, so as an
    // operand of one it must be parenthesized — both to preserve grouping and
    // to stay parseable, since the parser only accepts a bare range at the top
    // of an expression (`parse_range` sits just above `|>`).
    if matches!(child, Node::RangeExpr { .. }) {
        return true;
    }
    if let Node::BinaryOp { op: child_op, .. } = child {
        let p = op_precedence(parent_op);
        let c = op_precedence(child_op);

        if c < p {
            return true;
        }

        // `??` intentionally binds tighter than comparisons, logical operators,
        // and arithmetic fallbacks, but people often read it the other way.
        // Keep the semantics unchanged while making mixed expressions obvious.
        if child_op == "??" && parent_op != "??" && c > p {
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
    harn_lexer::escape_string_literal(s)
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

pub(crate) fn format_type_ann_wrapped(
    type_ann: &Option<TypeExpr>,
    indent: usize,
    type_col: usize,
    line_width: usize,
) -> String {
    match type_ann {
        Some(te) => format!(
            ": {}",
            format_type_expr_wrapped(te, indent, type_col, line_width)
        ),
        None => String::new(),
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
        TypeExpr::OpenShape { fields, rests } => {
            let mut parts: Vec<String> = fields
                .iter()
                .map(|f| {
                    let opt = if f.optional { "?" } else { "" };
                    format!("{}{opt}: {}", f.name, format_type_expr(&f.type_expr))
                })
                .collect();
            for rest in rests {
                parts.push(format!("...{}", format_type_expr(rest)));
            }
            format!("{{{}}}", parts.join(", "))
        }
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

/// Like [`format_type_expr`], but wraps breakable nested structures when the
/// inline rendering, prefixed with `prefix_len` columns and aligned to
/// `indent` levels, would exceed `line_width`. Shapes use one field per line;
/// generic arguments and union/intersection arms use continuation lines.
pub(crate) fn format_type_expr_wrapped(
    te: &TypeExpr,
    indent: usize,
    prefix_len: usize,
    line_width: usize,
) -> String {
    let inline = format_type_expr(te);
    if prefix_len + text_width(&inline) <= line_width {
        return inline;
    }
    match te {
        TypeExpr::Shape(fields) => format_shape_wrapped(fields, indent, line_width),
        TypeExpr::OpenShape { fields, rests } => {
            format_open_shape_wrapped(fields, rests, indent, line_width)
        }
        TypeExpr::List(inner) => format!(
            "list<{}>",
            format_type_expr_wrapped(inner, indent, prefix_len + 6, line_width)
        ),
        TypeExpr::Iter(inner) => format!(
            "iter<{}>",
            format_type_expr_wrapped(inner, indent, prefix_len + 6, line_width)
        ),
        TypeExpr::Generator(inner) => format!(
            "Generator<{}>",
            format_type_expr_wrapped(inner, indent, prefix_len + 11, line_width)
        ),
        TypeExpr::Stream(inner) => format!(
            "Stream<{}>",
            format_type_expr_wrapped(inner, indent, prefix_len + 8, line_width)
        ),
        TypeExpr::DictType(key, value) => format!(
            "dict<{}, {}>",
            format_type_expr_wrapped(key, indent, prefix_len + 6, line_width),
            format_type_expr_wrapped(value, indent, prefix_len + 8, line_width)
        ),
        TypeExpr::Applied { name, args } => {
            let item_indent = "  ".repeat(indent + 1);
            let close_indent = "  ".repeat(indent);
            let wrapped_args = args
                .iter()
                .enumerate()
                .map(|(index, arg)| {
                    let value = format_type_expr_wrapped(
                        arg,
                        indent + 1,
                        item_indent.chars().count(),
                        line_width,
                    );
                    let comma = if index + 1 < args.len() { "," } else { "" };
                    format!("{item_indent}{value}{comma}")
                })
                .collect::<Vec<_>>();
            format!("{name}<\n{}\n{close_indent}>", wrapped_args.join("\n"))
        }
        TypeExpr::Owned(inner) => format!(
            "owned<{}>",
            format_type_expr_wrapped(inner, indent, prefix_len + 7, line_width)
        ),
        TypeExpr::Union(types) => {
            if let Some(inner) = optional_sugar_inner(types) {
                // The wrapped variant follows the inline form for `T?`;
                // the inner type may still wrap if it is itself a shape.
                let inner_str = format_type_expr_wrapped(inner, indent, prefix_len, line_width);
                return format!("{inner_str}?");
            }
            format_type_union_wrapped(types, "|", indent, prefix_len, line_width)
        }
        TypeExpr::Intersection(types) => {
            format_type_union_wrapped(types, "&", indent, prefix_len, line_width)
        }
        _ => inline,
    }
}

fn format_type_union_wrapped(
    types: &[TypeExpr],
    separator: &str,
    indent: usize,
    prefix_len: usize,
    line_width: usize,
) -> String {
    let mut rendered = types.iter().enumerate().map(|(index, arm)| {
        let arm_prefix = if index == 0 {
            prefix_len
        } else {
            (indent + 1) * 2 + separator.chars().count() + 1
        };
        format_type_expr_wrapped(arm, indent, arm_prefix, line_width)
    });
    let Some(first) = rendered.next() else {
        return String::new();
    };
    rendered.fold(first, |mut output, arm| {
        // Type declarations are newline-terminated, so a bare leading `|` or
        // `&` would start a new statement. A lexical continuation keeps the
        // type expression intact while making each arm independently visible.
        output.push_str(" \\");
        output.push('\n');
        output.push_str(&"  ".repeat(indent + 1));
        output.push_str(separator);
        output.push(' ');
        output.push_str(&arm);
        output
    })
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
            let prefix_len =
                text_width(&pad_inner) + text_width(f.name.as_str()) + text_width(opt) + 2;
            let value = format_type_expr_wrapped(&f.type_expr, indent + 1, prefix_len, line_width);
            format!("{pad_inner}{}{opt}: {value},", f.name)
        })
        .collect();
    format!("{{\n{}\n{pad_close}}}", lines.join("\n"))
}

fn format_open_shape_wrapped(
    fields: &[harn_parser::ShapeField],
    rests: &[TypeExpr],
    indent: usize,
    line_width: usize,
) -> String {
    let pad_inner = "  ".repeat(indent + 1);
    let pad_close = "  ".repeat(indent);
    let mut lines = fields
        .iter()
        .map(|f| {
            let opt = if f.optional { "?" } else { "" };
            let prefix_len =
                text_width(&pad_inner) + text_width(f.name.as_str()) + text_width(opt) + 2;
            let value = format_type_expr_wrapped(&f.type_expr, indent + 1, prefix_len, line_width);
            format!("{pad_inner}{}{opt}: {value},", f.name)
        })
        .collect::<Vec<_>>();
    lines.extend(
        rests
            .iter()
            .map(|rest| format!("{pad_inner}...{},", format_type_expr(rest))),
    );
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
            .map(|c| format!("{}: {}", c.type_name, format_type_expr(&c.bound)))
            .collect();
        format!(" where {}", parts.join(", "))
    }
}

/// Render an optional `throws` exception-channel clause. Returns
/// ` throws <type>` (leading space) or the empty string when absent, so call
/// sites can splice it directly after the rendered return type — mirroring
/// [`format_where_clauses`].
pub(crate) fn format_throws_clause(throws: &Option<TypeExpr>) -> String {
    match throws {
        Some(ty) => format!(" throws {}", format_type_expr(ty)),
        None => String::new(),
    }
}

/// Format an expression inline for use in parameter defaults.
pub(crate) fn format_inline_expr(node: &SNode) -> String {
    let fmt = Formatter::new("", BTreeMap::new(), 100, AUTO_SEPARATOR_WIDTH);
    fmt.format_expr(node, 0, 0)
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

/// Count the columns occupied by formatter text. Harn indentation and source
/// identifiers are Unicode-safe at the character level; byte length is not a
/// line-width measurement when a literal contains non-ASCII text.
pub(crate) fn text_width(s: &str) -> usize {
    s.chars().count()
}

/// Return the column immediately after `s` when it starts at `column`.
/// Newlines reset the column to the width of the rendered final line.
pub(crate) fn column_after(column: usize, s: &str) -> usize {
    if s.contains('\n') {
        last_line_width(s)
    } else {
        column + text_width(s)
    }
}

/// Column used while rendering a comma-sequence item. The extra column
/// reserves the separator that the enclosing sequence appends after it.
pub(crate) fn wrapped_item_column(indent: usize) -> usize {
    (indent + 1) * 2 + 1
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
