//! Static key lookups on dict literals, shared by the rules that read them.

use harn_parser::{DictEntry, Node, SNode};

/// The name a dict-literal key spells out: a bare identifier or a string
/// literal. Computed keys have no static name.
pub(crate) fn key_name(node: &SNode) -> Option<String> {
    match &node.node {
        Node::StringLiteral(value) | Node::RawStringLiteral(value) | Node::Identifier(value) => {
            Some(value.clone())
        }
        _ => None,
    }
}

/// The first entry whose key statically names `key`.
pub(crate) fn entry_for_key<'a>(entries: &'a [DictEntry], key: &str) -> Option<&'a DictEntry> {
    entries
        .iter()
        .find(|entry| key_name(&entry.key).as_deref() == Some(key))
}
