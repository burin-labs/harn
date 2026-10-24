//! Shared Java grammar structure.

use tree_sitter::Node;

use super::symbols::helpers::children;

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
