//! C and C++ symbol projection.

use tree_sitter::Node;

use super::super::types::{Symbol, SymbolKind};
use super::helpers::{
    self, field_text, named_decl_with_keyword, normalized_access_level, point_pos, truncate_end,
    walk_named, NamedDeclArgs, NodePos,
};
use super::{enum_case_symbol, field_symbol};

pub(super) fn extract_c(root: Node<'_>, source: &str, out: &mut Vec<Symbol>) {
    walk_named(root, None, &mut |node, container| {
        let pos = point_pos(node);
        match node.kind() {
            "function_definition" => {
                push_function(
                    node,
                    source,
                    container,
                    None,
                    pos,
                    SymbolKind::Function,
                    out,
                );
                None
            }
            "struct_specifier" => push_specifier(
                node,
                source,
                container,
                pos,
                SymbolKind::Struct,
                "struct",
                out,
            ),
            "enum_specifier" => {
                push_specifier(node, source, container, pos, SymbolKind::Enum, "enum", out)
            }
            "enumerator" => {
                enum_case_symbol(node, source, container, pos, out);
                None
            }
            "field_declaration" => {
                push_field(node, source, container, None, pos, out);
                None
            }
            "type_definition" => {
                push_typedef(node, source, container, pos, out);
                None
            }
            _ => None,
        }
    });
}

pub(super) fn extract_cpp(root: Node<'_>, source: &str, out: &mut Vec<Symbol>) {
    walk_named(root, None, &mut |node, container| {
        let pos = point_pos(node);
        match node.kind() {
            "function_definition" => {
                let kind = if container.is_some() {
                    SymbolKind::Method
                } else {
                    SymbolKind::Function
                };
                push_function(
                    node,
                    source,
                    container,
                    member_access(node, source),
                    pos,
                    kind,
                    out,
                );
                None
            }
            "class_specifier" => push_specifier(
                node,
                source,
                container,
                pos,
                SymbolKind::Class,
                "class",
                out,
            ),
            "struct_specifier" => push_specifier(
                node,
                source,
                container,
                pos,
                SymbolKind::Struct,
                "struct",
                out,
            ),
            "enum_specifier" => {
                push_specifier(node, source, container, pos, SymbolKind::Enum, "enum", out)
            }
            "enumerator" => {
                enum_case_symbol(node, source, container, pos, out);
                None
            }
            // `int x() const;` in a class body is a member function
            // declaration, which the grammar parses as a field.
            "field_declaration" if declares_member_function(node) => {
                let kind = if container.is_some() {
                    SymbolKind::Method
                } else {
                    SymbolKind::Function
                };
                push_function(
                    node,
                    source,
                    container,
                    member_access(node, source),
                    pos,
                    kind,
                    out,
                );
                None
            }
            "field_declaration" => {
                push_field(
                    node,
                    source,
                    container,
                    member_access(node, source),
                    pos,
                    out,
                );
                None
            }
            "namespace_definition" => named_decl_with_keyword(NamedDeclArgs {
                node,
                source,
                container,
                pos,
                kind: SymbolKind::Module,
                keyword: "namespace",
                out,
            }),
            _ => None,
        }
    });
}

fn push_function(
    node: Node<'_>,
    source: &str,
    container: Option<&str>,
    access_level: Option<&str>,
    pos: NodePos,
    kind: SymbolKind,
    out: &mut Vec<Symbol>,
) {
    let Some(declarator) = node.child_by_field_name("declarator") else {
        return;
    };
    let Some(name) = declarator_name(declarator, source) else {
        return;
    };
    let params = declarator_params(declarator, source);
    out.push(helpers::sym_with_access_level(
        &name,
        kind,
        container,
        access_level,
        format!("{name}{}", truncate_end(&params, 80)),
        pos,
    ));
}

fn push_field(
    node: Node<'_>,
    source: &str,
    container: Option<&str>,
    access_level: Option<&str>,
    pos: NodePos,
    out: &mut Vec<Symbol>,
) {
    let declarator = node
        .child_by_field_name("declarator")
        .or_else(|| child_by_kind(node, "field_identifier"));
    let Some(declarator) = declarator else {
        return;
    };
    let Some(name) = declarator_name(declarator, source) else {
        return;
    };
    let field_type = field_text(node, "type", source).unwrap_or_default();
    field_symbol(
        &name,
        format!("{field_type} {name}").trim().to_string(),
        container,
        access_level,
        pos,
        out,
    );
}

/// Whether a C++ field declaration declares a member function rather than
/// data. Pointer and reference return types wrap the function declarator;
/// a function-pointer member parenthesizes its name, so it stays a field.
fn declares_member_function(field: Node<'_>) -> bool {
    let mut declarator = field.child_by_field_name("declarator");
    while let Some(node) = declarator {
        match node.kind() {
            "function_declarator" => {
                return node
                    .child_by_field_name("declarator")
                    .is_some_and(|name| name.kind() != "parenthesized_declarator");
            }
            "pointer_declarator" | "reference_declarator" => {
                declarator = node
                    .child_by_field_name("declarator")
                    .or_else(|| node.named_child(0));
            }
            _ => return false,
        }
    }
    false
}

fn child_by_kind<'tree>(node: Node<'tree>, kind: &str) -> Option<Node<'tree>> {
    helpers::children(node).find(|child| child.kind() == kind)
}

fn member_access(node: Node<'_>, source: &str) -> Option<&'static str> {
    let list = node.parent()?;
    if list.kind() != "field_declaration_list" {
        return None;
    }
    let mut access = list.parent().and_then(|container| match container.kind() {
        "class_specifier" => Some("private"),
        "struct_specifier" => Some("public"),
        _ => None,
    });
    for child in helpers::children(list) {
        if child.id() == node.id() {
            return access;
        }
        if child.kind() == "access_specifier" {
            access = normalized_access_level(&helpers::node_text(child, source));
        }
    }
    access
}

fn push_specifier(
    node: Node<'_>,
    source: &str,
    container: Option<&str>,
    pos: NodePos,
    kind: SymbolKind,
    keyword: &str,
    out: &mut Vec<Symbol>,
) -> Option<String> {
    let name_node = node.child_by_field_name("name")?;
    let name = helpers::node_text(name_node, source);
    if name.is_empty() {
        return None;
    }
    out.push(helpers::sym(
        &name,
        kind,
        container,
        format!("{keyword} {name}"),
        pos,
    ));
    Some(name)
}

fn push_typedef(
    node: Node<'_>,
    source: &str,
    container: Option<&str>,
    pos: NodePos,
    out: &mut Vec<Symbol>,
) {
    for child in helpers::children(node) {
        if child.kind() == "type_identifier" {
            let name = helpers::node_text(child, source);
            out.push(helpers::sym(
                &name,
                SymbolKind::Type,
                container,
                format!("typedef {name}"),
                pos,
            ));
        }
    }
}

fn declarator_name(node: Node<'_>, source: &str) -> Option<String> {
    if matches!(
        node.kind(),
        "identifier"
            | "field_identifier"
            | "destructor_name"
            | "qualified_identifier"
            | "template_function"
    ) {
        return Some(helpers::node_text(node, source));
    }
    node.child_by_field_name("declarator")
        .or_else(|| node.child_by_field_name("name"))
        // Reference and parenthesized declarators wrap their inner
        // declarator without a field name.
        .or_else(|| {
            matches!(
                node.kind(),
                "reference_declarator" | "parenthesized_declarator"
            )
            .then(|| node.named_child(0))
            .flatten()
        })
        .and_then(|inner| declarator_name(inner, source))
}

fn declarator_params(node: Node<'_>, source: &str) -> String {
    if node.kind() == "function_declarator" {
        if let Some(params) = node.child_by_field_name("parameters") {
            return helpers::node_text(params, source);
        }
    }
    for child in helpers::children(node) {
        let result = declarator_params(child, source);
        if result != "()" {
            return result;
        }
    }
    "()".into()
}

#[cfg(all(test, feature = "grammar-systems"))]
mod tests {
    use crate::ast::api::symbols_from_source;
    use crate::ast::{Language, SymbolKind};

    fn kinds(source: &str, language: Language) -> Vec<(String, SymbolKind, String)> {
        symbols_from_source(source, language)
            .unwrap()
            .into_iter()
            .map(|s| (s.name, s.kind, s.signature))
            .collect()
    }

    #[test]
    fn cpp_member_function_declarations_are_methods_not_fields() {
        let source = "class Point {\npublic:\n  int x() const;\n  int& at(int i);\n  \
                      Point* clone() const;\n  virtual void draw() = 0;\n  int y;\n  \
                      int (*callback)(int);\n};\n";
        let symbols = kinds(source, Language::Cpp);
        let kind_of = |name: &str| {
            symbols
                .iter()
                .find(|(n, _, _)| n == name)
                .unwrap_or_else(|| panic!("{name} missing from {symbols:?}"))
                .clone()
        };
        assert_eq!(kind_of("x").1, SymbolKind::Method);
        assert_eq!(kind_of("x").2, "x()");
        assert_eq!(kind_of("at").1, SymbolKind::Method);
        assert_eq!(kind_of("clone").1, SymbolKind::Method);
        assert_eq!(kind_of("draw").1, SymbolKind::Method);
        // Data members, including a function pointer, stay fields.
        assert_eq!(kind_of("y").1, SymbolKind::Field);
        assert_eq!(kind_of("callback").1, SymbolKind::Field);
    }

    #[test]
    fn c_function_pointer_members_stay_fields() {
        let symbols = kinds("struct ops {\n  int (*run)(int);\n};\n", Language::C);
        assert!(
            symbols
                .iter()
                .any(|(n, k, _)| n == "run" && *k == SymbolKind::Field),
            "{symbols:?}"
        );
    }
}
