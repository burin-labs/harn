//! Per-language syntax for `change_signature`: locating a function
//! declaration, reading its parameter and argument lists, and rendering
//! replacements that keep the original list layout.
//!
//! The builtin in [`super::change_signature`] owns the plan and the
//! refusals; this module only answers syntactic questions for the grammars
//! listed in [`Family`].

use std::ops::Range;

use tree_sitter::Node;

use crate::ast::Language;

/// Grammar families with a signature projection. Every language that maps
/// to a family reports `change_signature: true` in `ast.capabilities`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Family {
    Rust,
    TypeScript,
    Python,
}

impl Family {
    pub(super) fn of(language: Language) -> Option<Self> {
        match language {
            Language::Rust => Some(Self::Rust),
            Language::TypeScript | Language::Tsx => Some(Self::TypeScript),
            Language::Python => Some(Self::Python),
            _ => None,
        }
    }

    fn declaration_kinds(self) -> &'static [&'static str] {
        match self {
            Self::Rust => &["function_item", "function_signature_item"],
            Self::TypeScript => &[
                "function_declaration",
                "generator_function_declaration",
                "function_signature",
                "method_definition",
                "method_signature",
                "abstract_method_signature",
                "variable_declarator",
            ],
            Self::Python => &["function_definition"],
        }
    }

    fn comment_kinds(self) -> &'static [&'static str] {
        match self {
            Self::Rust => &["line_comment", "block_comment"],
            Self::TypeScript | Self::Python => &["comment"],
        }
    }
}

/// How the receiver binds at a call site.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Receiver {
    /// Free function, static method, or TypeScript method (`this` is implicit).
    None,
    /// Rust `self` or a Python instance method's first parameter: implicit in
    /// `value.m(..)`, passed as the first argument in `Type::m(value, ..)`.
    Instance,
    /// A Python `@classmethod`'s first parameter: never passed explicitly.
    Class,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ParamShape {
    Plain,
    /// `*args`, `...rest`.
    Variadic,
    /// `**kwargs`.
    KeywordVariadic,
}

/// One declared parameter, excluding the receiver.
#[derive(Clone, Debug)]
pub(super) struct Param {
    pub name: String,
    pub text: String,
    pub shape: ParamShape,
    /// Source text of the binding pattern (`mut x`, `x`, `{a, b}`).
    pub pattern: String,
    /// Byte range of the bare name inside `pattern`, when the pattern is
    /// (or wraps) one identifier.
    pub name_in_pattern: Option<Range<usize>>,
    pub type_text: Option<String>,
    pub default_text: Option<String>,
    /// TypeScript `x?: T`.
    pub optional: bool,
}

/// Layout of a parenthesized list, so a rewrite keeps one-line lists on one
/// line and multi-line lists one item per line.
#[derive(Clone, Debug)]
pub(super) struct ListLayout {
    multiline: Option<MultilineLayout>,
}

#[derive(Clone, Debug)]
struct MultilineLayout {
    item_indent: String,
    close_indent: String,
    trailing_comma: bool,
}

impl ListLayout {
    /// `items` rendered between parentheses.
    pub(super) fn render(&self, items: &[String]) -> String {
        match &self.multiline {
            Some(layout) if !items.is_empty() => {
                let mut out = String::from("(\n");
                for (i, item) in items.iter().enumerate() {
                    out.push_str(&layout.item_indent);
                    out.push_str(item);
                    if i + 1 < items.len() || layout.trailing_comma {
                        out.push(',');
                    }
                    out.push('\n');
                }
                out.push_str(&layout.close_indent);
                out.push(')');
                out
            }
            _ => format!("({})", items.join(", ")),
        }
    }
}

/// A located function declaration.
pub(super) struct Declaration {
    pub family: Family,
    /// The parenthesized parameter list.
    pub params_range: Range<usize>,
    pub params_start: (usize, usize),
    pub params_end: (usize, usize),
    pub layout: ListLayout,
    pub receiver: Receiver,
    /// Source text of the receiver parameter, kept first in the header.
    pub receiver_text: Option<String>,
    pub params: Vec<Param>,
    pub body: Option<Range<usize>>,
    pub is_method: bool,
    /// Name of the class or `impl` type a method belongs to.
    pub owner: Option<String>,
    /// Why other signatures must change with this one: it declares or
    /// implements a trait or interface method, or is an overload.
    pub contract: Option<String>,
}

/// `source[range]`, or empty when the range is not on character boundaries.
pub(super) fn slice(source: &str, range: Range<usize>) -> &str {
    source.get(range).unwrap_or("")
}

fn text<'a>(node: Node<'_>, bytes: &'a [u8]) -> &'a str {
    std::str::from_utf8(&bytes[node.start_byte()..node.end_byte()]).unwrap_or("")
}

fn named_children(node: Node<'_>) -> Vec<Node<'_>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor).collect()
}

/// Leading whitespace of the line that contains `byte`, if only whitespace
/// precedes it on that line.
fn indent_before(source: &str, byte: usize) -> Option<String> {
    let line_start = slice(source, 0..byte)
        .rfind('\n')
        .map(|i| i + 1)
        .unwrap_or(0);
    let prefix = slice(source, line_start..byte);
    prefix
        .chars()
        .all(|c| c == ' ' || c == '\t')
        .then(|| prefix.to_string())
}

/// Layout of the parenthesized `list` node whose items are `items`.
fn layout_of(list: Node<'_>, items: &[Node<'_>], source: &str) -> ListLayout {
    if !slice(source, list.byte_range()).contains('\n') {
        return ListLayout { multiline: None };
    }
    let close = list.end_byte().saturating_sub(1);
    let close_indent = indent_before(source, close).unwrap_or_default();
    let item_indent = items
        .first()
        .and_then(|first| indent_before(source, first.start_byte()))
        .unwrap_or_else(|| format!("{close_indent}    "));
    let before_close = slice(source, 0..close).trim_end();
    ListLayout {
        multiline: Some(MultilineLayout {
            item_indent,
            close_indent,
            trailing_comma: !items.is_empty() && before_close.ends_with(','),
        }),
    }
}

/// The parameter-list node and name node of a declaration-kind node.
fn declaration_parts<'t>(node: Node<'t>, family: Family) -> Option<(Node<'t>, Node<'t>)> {
    let name = node.child_by_field_name("name")?;
    if family == Family::TypeScript && node.kind() == "variable_declarator" {
        let value = node.child_by_field_name("value")?;
        if !matches!(
            value.kind(),
            "arrow_function" | "function_expression" | "function"
        ) {
            return None;
        }
        return Some((value.child_by_field_name("parameters")?, name));
    }
    Some((node.child_by_field_name("parameters")?, name))
}

fn function_body<'t>(node: Node<'t>, family: Family) -> Option<Node<'t>> {
    if family == Family::TypeScript && node.kind() == "variable_declarator" {
        return node
            .child_by_field_name("value")?
            .child_by_field_name("body");
    }
    node.child_by_field_name("body")
}

/// Find the declaration of `name` nearest to the 1-based `line`.
pub(super) fn find_declaration<'t>(
    root: Node<'t>,
    bytes: &[u8],
    family: Family,
    name: &str,
    line: u32,
) -> Option<Node<'t>> {
    let kinds = family.declaration_kinds();
    let mut best: Option<(u32, Node<'t>)> = None;
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if kinds.contains(&node.kind()) {
            if let Some((_, name_node)) = declaration_parts(node, family) {
                if text(name_node, bytes) == name {
                    let rows = [
                        node.start_position().row,
                        name_node.start_position().row,
                        node.parent()
                            .filter(|p| p.kind() == "decorated_definition")
                            .map(|p| p.start_position().row)
                            .unwrap_or(usize::MAX - 1),
                    ];
                    let distance = rows
                        .iter()
                        .map(|row| (*row as i64 + 1 - line as i64).unsigned_abs() as u32)
                        .min()
                        .unwrap_or(u32::MAX);
                    if best.is_none_or(|(d, _)| distance < d) {
                        best = Some((distance, node));
                    }
                }
            }
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            stack.push(child);
        }
    }
    best.map(|(_, node)| node)
}

/// Read a declaration node found by [`find_declaration`]. `Err` names the
/// parameter-list construct the rewrite cannot preserve.
pub(super) fn read_declaration(
    node: Node<'_>,
    source: &str,
    family: Family,
) -> Result<Declaration, String> {
    let bytes = source.as_bytes();
    let (list, _) = declaration_parts(node, family)
        .ok_or_else(|| "declaration has no parenthesized parameter list".to_string())?;
    if list.kind() == "identifier" {
        return Err("arrow function parameter is not parenthesized".into());
    }
    let children = named_children(list);
    if children
        .iter()
        .any(|child| family.comment_kinds().contains(&child.kind()))
    {
        return Err("the parameter list contains a comment".into());
    }
    let is_method = is_method(node, family);
    let mut receiver = Receiver::None;
    let mut receiver_text = None;
    let mut params = Vec::new();
    for (index, child) in children.iter().enumerate() {
        match (family, child.kind()) {
            (Family::Rust, "self_parameter") => {
                receiver = Receiver::Instance;
                receiver_text = Some(text(*child, bytes).to_string());
            }
            (Family::Rust, "attribute_item") => {
                return Err("a parameter carries an attribute".into());
            }
            (Family::Python, "keyword_separator" | "positional_separator") => {
                return Err(
                    "keyword-only (`*`) and positional-only (`/`) markers are not supported".into(),
                );
            }
            (Family::Python, _) if index == 0 && is_method => match python_receiver(node, bytes) {
                Receiver::None => params.push(read_param(*child, bytes, family)?),
                kind => {
                    receiver = kind;
                    receiver_text = Some(text(*child, bytes).to_string());
                }
            },
            _ => params.push(read_param(*child, bytes, family)?),
        }
    }
    Ok(Declaration {
        family,
        params_range: list.byte_range(),
        params_start: (list.start_position().row, list.start_position().column),
        params_end: (list.end_position().row, list.end_position().column),
        layout: layout_of(list, &children, source),
        receiver,
        receiver_text,
        params,
        body: function_body(node, family).map(|b| b.byte_range()),
        is_method,
        owner: is_method.then(|| owner_name(node, bytes)).flatten(),
        contract: contract(node, bytes, family),
    })
}

fn enclosing<'t>(node: Node<'t>, kinds: &[&str]) -> Option<Node<'t>> {
    let mut ancestor = node.parent();
    while let Some(current) = ancestor {
        if kinds.contains(&current.kind()) {
            return Some(current);
        }
        ancestor = current.parent();
    }
    None
}

fn owner_name(node: Node<'_>, bytes: &[u8]) -> Option<String> {
    let owner = enclosing(
        node,
        &[
            "impl_item",
            "trait_item",
            "class_definition",
            "class_declaration",
            "class",
        ],
    )?;
    let name = owner
        .child_by_field_name("type")
        .or_else(|| owner.child_by_field_name("name"))?;
    Some(text(name, bytes).to_string())
}

fn contract(node: Node<'_>, bytes: &[u8], family: Family) -> Option<String> {
    match family {
        Family::Rust => {
            let owner = enclosing(node, &["impl_item", "trait_item", "function_item"])?;
            match owner.kind() {
                "trait_item" => Some("declares a trait method".into()),
                "impl_item" => owner
                    .child_by_field_name("trait")
                    .map(|t| format!("implements trait `{}`", text(t, bytes))),
                _ => None,
            }
        }
        Family::TypeScript => match node.kind() {
            "method_signature" => Some("declares an interface method".into()),
            "abstract_method_signature" => Some("declares an abstract method".into()),
            "function_signature" => Some("declares an overload".into()),
            _ => None,
        },
        Family::Python => None,
    }
}

fn is_method(node: Node<'_>, family: Family) -> bool {
    match family {
        Family::Rust => {
            let mut ancestor = node.parent();
            while let Some(current) = ancestor {
                match current.kind() {
                    "impl_item" | "trait_item" => return true,
                    "function_item" | "mod_item" | "source_file" => return false,
                    _ => ancestor = current.parent(),
                }
            }
            false
        }
        Family::TypeScript => matches!(
            node.kind(),
            "method_definition" | "method_signature" | "abstract_method_signature"
        ),
        Family::Python => {
            let holder = node
                .parent()
                .filter(|p| p.kind() == "decorated_definition")
                .unwrap_or(node);
            holder
                .parent()
                .filter(|p| p.kind() == "block")
                .and_then(|block| block.parent())
                .is_some_and(|p| p.kind() == "class_definition")
        }
    }
}

fn python_receiver(node: Node<'_>, bytes: &[u8]) -> Receiver {
    let decorators: Vec<String> = node
        .parent()
        .filter(|p| p.kind() == "decorated_definition")
        .map(|p| {
            named_children(p)
                .into_iter()
                .filter(|c| c.kind() == "decorator")
                .map(|c| text(c, bytes).trim_start_matches('@').trim().to_string())
                .collect()
        })
        .unwrap_or_default();
    if decorators.iter().any(|d| d == "staticmethod") {
        Receiver::None
    } else if decorators.iter().any(|d| d == "classmethod") {
        Receiver::Class
    } else {
        Receiver::Instance
    }
}

fn read_param(node: Node<'_>, bytes: &[u8], family: Family) -> Result<Param, String> {
    let full = text(node, bytes).to_string();
    let field = |name: &str| node.child_by_field_name(name);
    let field_text = |name: &str| field(name).map(|n| text(n, bytes).to_string());
    let (pattern_node, shape, optional) = match (family, node.kind()) {
        (Family::Rust, "parameter") => (field("pattern"), ParamShape::Plain, false),
        (Family::Rust, "variadic_parameter") => (None, ParamShape::Variadic, false),
        (Family::TypeScript, "required_parameter" | "optional_parameter") => {
            let pattern = field("pattern");
            let shape = if pattern.is_some_and(|p| p.kind() == "rest_pattern") {
                ParamShape::Variadic
            } else {
                ParamShape::Plain
            };
            (pattern, shape, node.kind() == "optional_parameter")
        }
        (Family::Python, "identifier") => (Some(node), ParamShape::Plain, false),
        (Family::Python, "typed_parameter") => {
            let inner = named_children(node).into_iter().next();
            let shape = match inner.map(|n| n.kind()) {
                Some("list_splat_pattern") => ParamShape::Variadic,
                Some("dictionary_splat_pattern") => ParamShape::KeywordVariadic,
                _ => ParamShape::Plain,
            };
            (inner, shape, false)
        }
        (Family::Python, "default_parameter" | "typed_default_parameter") => {
            (field("name"), ParamShape::Plain, false)
        }
        (Family::Python, "list_splat_pattern") => (Some(node), ParamShape::Variadic, false),
        (Family::Python, "dictionary_splat_pattern") => {
            (Some(node), ParamShape::KeywordVariadic, false)
        }
        (_, other) => return Err(format!("unsupported parameter form `{other}`")),
    };
    let pattern_node = pattern_node.ok_or_else(|| format!("parameter `{full}` has no name"))?;
    let name_node = bare_name(pattern_node);
    let pattern = text(pattern_node, bytes).to_string();
    let name_in_pattern = name_node.map(|n| {
        let start = n.start_byte() - pattern_node.start_byte();
        start..start + (n.end_byte() - n.start_byte())
    });
    let name = name_node
        .map(|n| text(n, bytes).to_string())
        .unwrap_or_else(|| pattern.clone());
    // Rust and TypeScript put the type in a `type` field; TypeScript's
    // includes the leading `:` token in a type_annotation node.
    let type_text = field_text("type").map(|t| t.trim_start_matches(':').trim().to_string());
    let default_text = field_text("value");
    Ok(Param {
        name,
        text: full,
        shape,
        pattern,
        name_in_pattern,
        type_text,
        default_text,
        optional,
    })
}

/// The identifier a binding pattern names: itself, or the single identifier
/// inside `mut x`, `*args`, `**kwargs`, `...rest`.
fn bare_name(pattern: Node<'_>) -> Option<Node<'_>> {
    if pattern.kind() == "identifier" {
        return Some(pattern);
    }
    if matches!(
        pattern.kind(),
        "mut_pattern" | "list_splat_pattern" | "dictionary_splat_pattern" | "rest_pattern"
    ) {
        let children = named_children(pattern);
        if let [only] = children.as_slice() {
            return bare_name(*only);
        }
    }
    None
}

/// Render one parameter. `rename` replaces the bare name; `type_text` and
/// `default_text` replace (or add) those parts.
pub(super) fn render_param(
    family: Family,
    base: Option<&Param>,
    name: &str,
    type_text: Option<&str>,
    default_text: Option<&str>,
) -> String {
    let unchanged = base.is_some_and(|p| {
        p.name == name
            && type_text.is_none_or(|t| p.type_text.as_deref() == Some(t))
            && default_text.is_none_or(|d| p.default_text.as_deref() == Some(d))
    });
    if let (true, Some(param)) = (unchanged, base) {
        return param.text.clone();
    }
    let pattern = match base {
        Some(param) => match &param.name_in_pattern {
            Some(range) => {
                let mut pattern = param.pattern.clone();
                pattern.replace_range(range.clone(), name);
                pattern
            }
            None => param.pattern.clone(),
        },
        None => name.to_string(),
    };
    let ty = type_text
        .map(str::to_string)
        .or_else(|| base.and_then(|p| p.type_text.clone()));
    let default = default_text
        .map(str::to_string)
        .or_else(|| base.and_then(|p| p.default_text.clone()));
    let optional = base.is_some_and(|p| p.optional) && default.is_none();
    let mut out = pattern;
    if optional {
        out.push('?');
    }
    match (family, &ty, &default) {
        (Family::Python, None, Some(d)) => {
            out.push('=');
            out.push_str(d);
        }
        (_, ty, default) => {
            if let Some(t) = ty {
                out.push_str(": ");
                out.push_str(t);
            }
            if let Some(d) = default {
                out.push_str(" = ");
                out.push_str(d);
            }
        }
    }
    out
}

/// One argument at a call site.
#[derive(Clone, Debug)]
pub(super) enum Argument {
    Positional(Range<usize>),
    /// Python `name=value`; the range is the value's.
    Keyword(String, Range<usize>),
}

/// A call site's argument list.
pub(super) struct CallArguments {
    pub range: Range<usize>,
    pub start: (usize, usize),
    pub end: (usize, usize),
    pub layout: ListLayout,
    pub args: Vec<Argument>,
}

/// Read the argument list of the call expression spanning `call_range`.
/// `Err` says why the call cannot be rewritten.
pub(super) fn read_call_arguments(
    root: Node<'_>,
    source: &str,
    family: Family,
    call_range: Range<usize>,
) -> Result<CallArguments, String> {
    let bytes = source.as_bytes();
    let mut call = root
        .descendant_for_byte_range(call_range.start, call_range.end)
        .ok_or_else(|| "call expression not found".to_string())?;
    while call.byte_range() != call_range {
        call = call
            .parent()
            .ok_or_else(|| "call expression not found".to_string())?;
    }
    let list = call
        .child_by_field_name("arguments")
        .ok_or_else(|| "the call has no argument list".to_string())?;
    if !matches!(list.kind(), "arguments" | "argument_list") {
        return Err(format!("`{}` arguments cannot be rewritten", list.kind()));
    }
    let children = named_children(list);
    let mut args = Vec::new();
    for child in &children {
        match child.kind() {
            kind if family.comment_kinds().contains(&kind) => {
                return Err("the argument list contains a comment".into());
            }
            "spread_element" | "list_splat" | "dictionary_splat" => {
                return Err(format!("spread argument `{}`", text(*child, bytes)));
            }
            "keyword_argument" => {
                let name = child
                    .child_by_field_name("name")
                    .map(|n| text(n, bytes).to_string())
                    .unwrap_or_default();
                let value = child
                    .child_by_field_name("value")
                    .ok_or_else(|| "keyword argument without a value".to_string())?;
                args.push(Argument::Keyword(name, value.byte_range()));
            }
            _ => args.push(Argument::Positional(child.byte_range())),
        }
    }
    Ok(CallArguments {
        range: list.byte_range(),
        start: (list.start_position().row, list.start_position().column),
        end: (list.end_position().row, list.end_position().column),
        layout: layout_of(list, &children, source),
        args,
    })
}

/// How a Rust call inside a macro's token tree names the function.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum MacroCallShape {
    /// `f(..)`.
    Plain,
    /// `a::f(..)`.
    Qualified,
    /// `x.f(..)`.
    Method,
}

/// A Rust call written inside a macro invocation (`assert_eq!(f(x), y)`).
/// The grammar keeps macro arguments as tokens, so the identifier at
/// `ident` followed by a parenthesized token tree is the only call shape
/// left. `None` when `ident` is not such a call; `Err` when its arguments
/// cannot be split.
pub(super) fn read_macro_call(
    root: Node<'_>,
    source: &str,
    ident: Range<usize>,
) -> Option<Result<(MacroCallShape, CallArguments), String>> {
    let node = root.descendant_for_byte_range(ident.start, ident.end)?;
    if node.kind() != "identifier" || node.parent()?.kind() != "token_tree" {
        return None;
    }
    let list = node.next_sibling()?;
    if list.kind() != "token_tree" || !slice(source, list.byte_range()).starts_with('(') {
        return None;
    }
    let shape = match node.prev_sibling().map(|p| p.kind()) {
        Some(".") => MacroCallShape::Method,
        Some("::") => MacroCallShape::Qualified,
        _ => MacroCallShape::Plain,
    };
    let mut cursor = list.walk();
    let tokens: Vec<Node<'_>> = list.children(&mut cursor).collect();
    let inner = tokens
        .get(1..tokens.len().saturating_sub(1))
        .unwrap_or_default();
    let mut args = Vec::new();
    let mut firsts = Vec::new();
    let mut current: Option<Range<usize>> = None;
    for token in inner {
        if Family::Rust.comment_kinds().contains(&token.kind()) {
            return Some(Err("the argument list contains a comment".into()));
        }
        if token.kind() == "," {
            match current.take() {
                Some(range) => args.push(Argument::Positional(range)),
                None => return Some(Err("empty macro argument".into())),
            }
            continue;
        }
        current = Some(match current {
            Some(range) => range.start..token.end_byte(),
            None => {
                firsts.push(*token);
                token.byte_range()
            }
        });
    }
    if let Some(range) = current {
        args.push(Argument::Positional(range));
    }
    Some(Ok((
        shape,
        CallArguments {
            range: list.byte_range(),
            start: (list.start_position().row, list.start_position().column),
            end: (list.end_position().row, list.end_position().column),
            layout: layout_of(list, &firsts, source),
            args,
        },
    )))
}

/// How a function body mentions a parameter name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum UseKind {
    /// An identifier a rename rewrites in place.
    Identifier,
    /// `{ x }` / `S { x }` shorthand; a rename writes `x: new`.
    Shorthand,
    /// A use a rename cannot rewrite: a destructuring shorthand binding or a
    /// Rust inline format argument (`"{x}"`).
    Opaque(&'static str),
}

#[derive(Clone, Debug)]
pub(super) struct BodyUse {
    pub range: Range<usize>,
    pub row: usize,
    pub col: usize,
    pub end_row: usize,
    pub end_col: usize,
    pub kind: UseKind,
}

/// Every use of `name` inside the body range `body`, skipping comments,
/// string bodies (except interpolations), member names, and Python keyword
/// argument names.
pub(super) fn body_uses(
    root: Node<'_>,
    source: &str,
    family: Family,
    body: Range<usize>,
    name: &str,
) -> Vec<BodyUse> {
    let bytes = source.as_bytes();
    let Some(body) = root.descendant_for_byte_range(body.start, body.end) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut stack = vec![body];
    while let Some(node) = stack.pop() {
        let kind = node.kind();
        if family.comment_kinds().contains(&kind) {
            continue;
        }
        let push = |out: &mut Vec<BodyUse>, kind: UseKind| {
            out.push(BodyUse {
                range: node.byte_range(),
                row: node.start_position().row,
                col: node.start_position().column,
                end_row: node.end_position().row,
                end_col: node.end_position().column,
                kind,
            });
        };
        match kind {
            "string_literal" | "raw_string_literal" if family == Family::Rust => {
                let literal = text(node, bytes);
                if in_macro(node)
                    && (literal.contains(&format!("{{{name}}}"))
                        || literal.contains(&format!("{{{name}:")))
                {
                    push(&mut out, UseKind::Opaque("inline format argument"));
                }
                continue;
            }
            "string" if family == Family::TypeScript => continue,
            "identifier" => {
                if text(node, bytes) == name && !is_member_or_keyword_name(node) {
                    push(&mut out, UseKind::Identifier);
                }
                continue;
            }
            "shorthand_property_identifier" | "shorthand_field_identifier" => {
                if text(node, bytes) == name {
                    push(&mut out, UseKind::Shorthand);
                }
                continue;
            }
            "shorthand_property_identifier_pattern" => {
                if text(node, bytes) == name {
                    push(&mut out, UseKind::Opaque("destructuring shorthand"));
                }
                continue;
            }
            _ => {}
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            stack.push(child);
        }
    }
    out.sort_by_key(|u| u.range.start);
    out
}

fn in_macro(node: Node<'_>) -> bool {
    let mut ancestor = node.parent();
    while let Some(current) = ancestor {
        if current.kind() == "macro_invocation" {
            return true;
        }
        ancestor = current.parent();
    }
    false
}

fn is_member_or_keyword_name(node: Node<'_>) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    let is_field =
        |field: &str| parent.child_by_field_name(field).map(|c| c.id()) == Some(node.id());
    match parent.kind() {
        "attribute" => is_field("attribute"),
        "keyword_argument" => is_field("name"),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn family_matches_the_capability_column() {
        for language in Language::all() {
            assert_eq!(
                Family::of(*language).is_some(),
                language.edit_capabilities().change_signature,
                "{}",
                language.name()
            );
        }
    }
}
