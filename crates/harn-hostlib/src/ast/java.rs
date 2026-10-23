//! Shared Java grammar structure.

use tree_sitter::Node;

use super::symbols::helpers::children;

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
