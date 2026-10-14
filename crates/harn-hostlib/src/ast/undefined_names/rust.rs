//! Rust profile for \`ast.undefined_names\`.
//!
//! Only bare value identifiers are references. Paths (`a::b`), field and
//! method names, types, lifetimes, labels, and macro names never are: a path
//! segment can name an extern crate that no `use` mentions, and types live in
//! a namespace the value analysis does not track.

use super::*;

pub(super) static BUILTINS: std::sync::LazyLock<HashSet<&'static str>> =
    std::sync::LazyLock::new(|| {
        [
            "Some", "None", "Ok", "Err", "Box", "Vec", "String", "Option", "Result", "Default",
            "drop", "self", "Self", "super", "crate", "std", "core", "alloc", "_",
        ]
        .into_iter()
        .collect()
    });

pub(super) fn collect(
    root: Node<'_>,
    source: &str,
    defined: &mut HashSet<String>,
    refs: &mut Vec<UndefinedName>,
) {
    visit(root, source, defined, refs);
}

/// Subtrees that never hold a value reference.
fn is_opaque(kind: &str) -> bool {
    kind.ends_with("_type")
        || matches!(
            kind,
            "type_identifier"
                | "type_arguments"
                | "type_parameters"
                | "where_clause"
                | "attribute_item"
                | "inner_attribute_item"
                | "label"
                | "lifetime"
                | "line_comment"
                | "block_comment"
                | "visibility_modifier"
                | "scoped_identifier"
                | "scoped_type_identifier"
                | "field_identifier"
                | "string_literal"
                | "raw_string_literal"
                | "char_literal"
        )
}

fn visit(
    node: Node<'_>,
    source: &str,
    defined: &mut HashSet<String>,
    refs: &mut Vec<UndefinedName>,
) {
    let kind = node.kind();
    if is_opaque(kind) {
        return;
    }
    match kind {
        "function_item" | "function_signature_item" => {
            if let Some(name) = node.child_by_field_name("name") {
                defined.insert(node_text(name, source).to_string());
            }
            if let Some(params) = node.child_by_field_name("parameters") {
                bind_parameters(params, source, defined);
            }
            if let Some(body) = node.child_by_field_name("body") {
                visit(body, source, defined, refs);
            }
        }
        "closure_expression" => {
            if let Some(params) = node.child_by_field_name("parameters") {
                bind_parameters(params, source, defined);
            }
            if let Some(body) = node.child_by_field_name("body") {
                visit(body, source, defined, refs);
            }
        }
        "let_declaration" | "let_condition" => {
            if let Some(value) = node.child_by_field_name("value") {
                visit(value, source, defined, refs);
            }
            if let Some(alternative) = node.child_by_field_name("alternative") {
                visit(alternative, source, defined, refs);
            }
            if let Some(pattern) = node.child_by_field_name("pattern") {
                bind_pattern(pattern, source, defined);
            }
        }
        "for_expression" => {
            if let Some(value) = node.child_by_field_name("value") {
                visit(value, source, defined, refs);
            }
            if let Some(pattern) = node.child_by_field_name("pattern") {
                bind_pattern(pattern, source, defined);
            }
            if let Some(body) = node.child_by_field_name("body") {
                visit(body, source, defined, refs);
            }
        }
        "match_pattern" => {
            if let Some(pattern) = node.named_child(0) {
                bind_pattern(pattern, source, defined);
            }
            if let Some(condition) = node.child_by_field_name("condition") {
                visit(condition, source, defined, refs);
            }
        }
        "use_declaration" => {
            if let Some(argument) = node.child_by_field_name("argument") {
                bind_use_tree(argument, source, defined);
            }
        }
        // A tuple or unit struct is also a value (`Meters(3)`).
        "struct_item" | "enum_item" | "union_item" | "trait_item" | "type_item" => {
            if let Some(name) = node.child_by_field_name("name") {
                defined.insert(node_text(name, source).to_string());
            }
        }
        "const_item" | "static_item" | "mod_item" | "macro_definition" => {
            if let Some(name) = node.child_by_field_name("name") {
                defined.insert(node_text(name, source).to_string());
            }
            for field in ["value", "body"] {
                if let Some(child) = node.child_by_field_name(field) {
                    visit(child, source, defined, refs);
                }
            }
        }
        "field_expression" => {
            if let Some(value) = node.child_by_field_name("value") {
                visit(value, source, defined, refs);
            }
        }
        "generic_function" => {
            if let Some(function) = node.child_by_field_name("function") {
                visit(function, source, defined, refs);
            }
        }
        "struct_expression" => {
            if let Some(body) = node.child_by_field_name("body") {
                visit(body, source, defined, refs);
            }
        }
        "field_initializer" => {
            if let Some(value) = node.child_by_field_name("value") {
                visit(value, source, defined, refs);
            }
        }
        "macro_invocation" => {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                if child.kind() == "token_tree" {
                    visit_token_tree(child, source, refs);
                }
            }
        }
        "identifier" => add_reference(refs, node, source, "identifier"),
        _ => {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                visit(child, source, defined, refs);
            }
        }
    }
}

/// Macro arguments are unparsed tokens. An identifier there reads a value
/// unless it is a path segment, a field or method name, or a nested macro.
fn visit_token_tree(node: Node<'_>, source: &str, refs: &mut Vec<UndefinedName>) {
    let mut cursor = node.walk();
    let children: Vec<Node<'_>> = node.children(&mut cursor).collect();
    for (index, child) in children.iter().enumerate() {
        match child.kind() {
            "token_tree" => visit_token_tree(*child, source, refs),
            "identifier" => {
                let before = index.checked_sub(1).map(|i| children[i].kind());
                let after = children.get(index + 1).map(|n| n.kind());
                let qualified =
                    matches!(before, Some("::" | ".")) || matches!(after, Some("::" | "!" | ":"));
                if !qualified {
                    add_reference(refs, *child, source, "identifier");
                }
            }
            _ => {}
        }
    }
}

fn bind_parameters(params: Node<'_>, source: &str, defined: &mut HashSet<String>) {
    let mut cursor = params.walk();
    for child in params.named_children(&mut cursor) {
        match child.kind() {
            "parameter" => {
                if let Some(pattern) = child.child_by_field_name("pattern") {
                    bind_pattern(pattern, source, defined);
                }
            }
            "self_parameter" | "variadic_parameter" => {}
            _ => bind_pattern(child, source, defined),
        }
    }
}

/// Bind every name a pattern introduces. A capitalized bare identifier in a
/// pattern is a unit variant or constant (`None`), not a new binding.
fn bind_pattern(node: Node<'_>, source: &str, defined: &mut HashSet<String>) {
    match node.kind() {
        "identifier" | "shorthand_field_identifier" => {
            let name = node_text(node, source);
            if !name.starts_with(|c: char| c.is_uppercase()) {
                defined.insert(name.to_string());
            }
        }
        "scoped_identifier" | "type_identifier" | "field_identifier" => {}
        "field_pattern" => match node.child_by_field_name("pattern") {
            Some(pattern) => bind_pattern(pattern, source, defined),
            None => {
                if let Some(name) = node.child_by_field_name("name") {
                    bind_pattern(name, source, defined);
                }
            }
        },
        "tuple_struct_pattern" | "struct_pattern" => {
            let type_id = node.child_by_field_name("type").map(|n| n.id());
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                if Some(child.id()) != type_id {
                    bind_pattern(child, source, defined);
                }
            }
        }
        // `0..=MAX` and literal patterns compare, they never bind.
        "range_pattern" => {}
        _ => {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                bind_pattern(child, source, defined);
            }
        }
    }
}

/// Names a `use` tree brings into scope: the last segment of each path, or
/// its alias. A glob binds nothing the parse can see.
fn bind_use_tree(node: Node<'_>, source: &str, defined: &mut HashSet<String>) {
    match node.kind() {
        "identifier" => {
            defined.insert(node_text(node, source).to_string());
        }
        "scoped_identifier" => {
            if let Some(name) = node.child_by_field_name("name") {
                defined.insert(node_text(name, source).to_string());
            }
        }
        "use_as_clause" => {
            if let Some(alias) = node.child_by_field_name("alias") {
                defined.insert(node_text(alias, source).to_string());
            }
        }
        "scoped_use_list" => {
            if let Some(list) = node.child_by_field_name("list") {
                bind_use_tree(list, source, defined);
            }
        }
        "use_list" => {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                bind_use_tree(child, source, defined);
            }
        }
        _ => {}
    }
}
