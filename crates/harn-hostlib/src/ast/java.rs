//! Shared Java grammar structure.

use tree_sitter::Node;

use super::symbols::helpers::children;

pub(crate) fn is_type(node: Node<'_>) -> bool {
    matches!(
        node.kind(),
        "class_declaration"
            | "interface_declaration"
            | "enum_declaration"
            | "record_declaration"
            | "annotation_type_declaration"
    )
}

pub(crate) fn has_modifier(node: Node<'_>, modifier: &str) -> bool {
    let Some(modifiers) = children(node).find(|child| child.kind() == "modifiers") else {
        return false;
    };
    let mut cursor = modifiers.walk();
    let found = modifiers
        .children(&mut cursor)
        .any(|child| child.kind() == modifier);
    found
}

/// Source name segments, excluding annotations and generic arguments.
pub(crate) fn type_path(node: Node<'_>, source: &str) -> Result<Vec<String>, String> {
    match node.kind() {
        "identifier" | "type_identifier" => Ok(vec![source
            .get(node.byte_range())
            .ok_or("the Java type name is outside its source")?
            .to_string()]),
        "generic_type" => type_path(
            node.named_child(0)
                .ok_or("the Java generic type is empty")?,
            source,
        ),
        "scoped_type_identifier" | "scoped_identifier" | "field_access" => {
            let mut parts = Vec::new();
            for child in children(node)
                .filter(|child| !matches!(child.kind(), "annotation" | "marker_annotation"))
            {
                parts.extend(type_path(child, source)?);
            }
            if parts.is_empty() {
                return Err("the Java qualified type is empty".into());
            }
            Ok(parts)
        }
        _ => Err(format!("unsupported Java type qualifier ({})", node.kind())),
    }
}

/// Check declaration grammar even when a caller disables rewrite validation.
pub(crate) fn is_type_name(name: &str) -> bool {
    // JLS 3.8 TypeIdentifier exclusions and literal/reserved spellings absent
    // from the grammar's keyword tokens. Declaration parsing alone tolerates
    // keywords in some identifier positions.
    // https://docs.oracle.com/javase/specs/jls/se25/html/jls-3.html#jls-3.8
    if matches!(
        name,
        "permits"
            | "record"
            | "sealed"
            | "var"
            | "yield"
            | "const"
            | "goto"
            | "_"
            | "true"
            | "false"
            | "null"
            | "this"
            | "super"
    ) {
        return false;
    }
    let Some(grammar) = super::Language::Java.ts_language() else {
        return false;
    };
    let contextual = matches!(
        name,
        "exports"
            | "opens"
            | "requires"
            | "uses"
            | "module"
            | "provides"
            | "to"
            | "when"
            | "open"
            | "transitive"
            | "with"
    );
    if !contextual && grammar.node_kind_for_id(grammar.id_for_node_kind(name, false)) == Some(name)
    {
        return false;
    }
    let source = format!("class {name} {{ {name}() {{}} }}");
    let Ok(tree) = super::api::parse_tree(&source, super::Language::Java) else {
        return false;
    };
    let root = tree.root_node();
    !root.has_error()
        && children(root).next().is_some_and(|declaration| {
            declaration.kind() == "class_declaration"
                && declaration
                    .child_by_field_name("name")
                    .is_some_and(|node| source.get(node.byte_range()) == Some(name))
        })
}

/// Object contracts cannot be renamed without a complete dispatch model.
pub(crate) fn is_object_method(name: &str) -> bool {
    matches!(
        name,
        "equals"
            | "hashCode"
            | "toString"
            | "clone"
            | "finalize"
            | "getClass"
            | "notify"
            | "notifyAll"
            | "wait"
    )
}

/// The declared name, after any annotations on the type parameter.
pub(crate) fn type_parameter_name(parameter: Node<'_>) -> Option<Node<'_>> {
    children(parameter).find(|node| node.kind() == "type_identifier")
}

/// Direct type members, including the grammar wrapper after enum constants.
pub(crate) fn direct_members(body: Node<'_>) -> Vec<Node<'_>> {
    let mut members = Vec::new();
    for member in children(body) {
        if member.kind() == "enum_body_declarations" {
            members.extend(children(member));
        } else {
            members.push(member);
        }
    }
    members
}
