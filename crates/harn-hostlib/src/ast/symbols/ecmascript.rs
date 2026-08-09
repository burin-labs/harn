//! JavaScript and TypeScript symbol projection.

use tree_sitter::Node;

use super::super::types::{Symbol, SymbolKind};
use super::helpers::{
    self, explicit_access_level, field_text, named_decl_with_keyword, point_pos, push_func,
    walk_named, NamedDeclArgs, NodePos, PushFuncArgs,
};
use super::{child_text_by_kind, enum_case_symbol, field_symbol};

pub(super) fn extract_typescript(root: Node<'_>, source: &str, out: &mut Vec<Symbol>) {
    walk_named(root, None, &mut |node, container| {
        let pos = point_pos(node);
        match node.kind() {
            "class_declaration" => named_decl_with_keyword(NamedDeclArgs {
                node,
                source,
                container,
                pos,
                kind: SymbolKind::Class,
                keyword: "class",
                out,
            }),
            "interface_declaration" => named_decl_with_keyword(NamedDeclArgs {
                node,
                source,
                container,
                pos,
                kind: SymbolKind::Interface,
                keyword: "interface",
                out,
            }),
            "type_alias_declaration" => {
                named_decl_with_keyword(NamedDeclArgs {
                    node,
                    source,
                    container,
                    pos,
                    kind: SymbolKind::Type,
                    keyword: "type",
                    out,
                });
                None
            }
            "enum_declaration" => extract_enum_declaration(node, source, container, pos, out),
            "enum_assignment" => {
                enum_case_symbol(node, source, container, pos, out);
                None
            }
            "public_field_definition" | "field_definition" => {
                push_field(node, source, container, pos, out);
                None
            }
            "function_declaration" => {
                push_func(PushFuncArgs {
                    node,
                    source,
                    container,
                    pos,
                    kind: SymbolKind::Function,
                    prefix: "function",
                    out,
                });
                None
            }
            "method_definition" => {
                push_func(PushFuncArgs {
                    node,
                    source,
                    container,
                    pos,
                    kind: SymbolKind::Method,
                    prefix: "",
                    out,
                });
                None
            }
            "lexical_declaration" | "variable_declaration" => {
                extract_bindings(node, source, container, out);
                None
            }
            _ => None,
        }
    });
}

pub(super) fn extract_javascript(root: Node<'_>, source: &str, out: &mut Vec<Symbol>) {
    walk_named(root, None, &mut |node, container| {
        let pos = point_pos(node);
        match node.kind() {
            "class_declaration" => named_decl_with_keyword(NamedDeclArgs {
                node,
                source,
                container,
                pos,
                kind: SymbolKind::Class,
                keyword: "class",
                out,
            }),
            "function_declaration" => {
                push_func(PushFuncArgs {
                    node,
                    source,
                    container,
                    pos,
                    kind: SymbolKind::Function,
                    prefix: "function",
                    out,
                });
                None
            }
            "method_definition" => {
                push_func(PushFuncArgs {
                    node,
                    source,
                    container,
                    pos,
                    kind: SymbolKind::Method,
                    prefix: "",
                    out,
                });
                None
            }
            "public_field_definition" | "field_definition" => {
                push_field(node, source, container, pos, out);
                None
            }
            "lexical_declaration" | "variable_declaration" => {
                extract_bindings(node, source, container, out);
                None
            }
            _ => None,
        }
    });
}

fn push_field(
    node: Node<'_>,
    source: &str,
    container: Option<&str>,
    pos: NodePos,
    out: &mut Vec<Symbol>,
) {
    let Some(name) = field_text(node, "name", source)
        .or_else(|| child_text_by_kind(node, source, &["property_identifier", "identifier"]))
    else {
        return;
    };
    let access = explicit_access_level(node, source).or(Some("public"));
    field_symbol(&name, name.clone(), container, access, pos, out);
}

fn extract_enum_declaration(
    node: Node<'_>,
    source: &str,
    container: Option<&str>,
    pos: NodePos,
    out: &mut Vec<Symbol>,
) -> Option<String> {
    let name = named_decl_with_keyword(NamedDeclArgs {
        node,
        source,
        container,
        pos,
        kind: SymbolKind::Enum,
        keyword: "enum",
        out,
    })?;
    push_enum_body(node, source, Some(&name), out);
    Some(name)
}

fn push_enum_body(node: Node<'_>, source: &str, container: Option<&str>, out: &mut Vec<Symbol>) {
    for child in helpers::children(node) {
        if node.kind() == "enum_body" && child.kind() == "property_identifier" {
            enum_case_symbol(child, source, container, point_pos(child), out);
        } else {
            push_enum_body(child, source, container, out);
        }
    }
}

/// Arrow and function-expression bindings are functions. Other module-level
/// bindings are variables.
fn extract_bindings(node: Node<'_>, source: &str, container: Option<&str>, out: &mut Vec<Symbol>) {
    let pos = point_pos(node);
    for declarator in helpers::children(node) {
        if !declarator.is_named() || declarator.kind() != "variable_declarator" {
            continue;
        }
        let Some(name_node) = declarator.child_by_field_name("name") else {
            continue;
        };
        let name = helpers::node_text(name_node, source);
        let value_kind = declarator
            .child_by_field_name("value")
            .map(|value| value.kind())
            .unwrap_or("");
        if matches!(
            value_kind,
            "arrow_function" | "function" | "function_expression"
        ) {
            out.push(helpers::sym(
                &name,
                SymbolKind::Function,
                container,
                format!("const {name} = (...) =>"),
                pos,
            ));
        } else if container.is_none() {
            let snippet = helpers::node_text(node, source)
                .chars()
                .take(100)
                .collect::<String>()
                .lines()
                .next()
                .unwrap_or(&name)
                .to_string();
            out.push(helpers::sym(
                &name,
                SymbolKind::Variable,
                container,
                snippet,
                pos,
            ));
        }
    }
}
