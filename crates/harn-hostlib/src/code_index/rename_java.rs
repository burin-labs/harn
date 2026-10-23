//! Adapt the Java source binding resolver to the indexed rename seed.

use tree_sitter::Tree;

use super::java_bindings::JavaBindings;
use super::refactor_core::IdentifierSpan;
use super::state::IndexState;
use super::symbol_graph::Node;

pub(super) fn targets(
    state: &IndexState,
    seed: &Node,
    path: &str,
    source: &str,
    tree: &Tree,
    targets: Vec<IdentifierSpan>,
) -> Result<Vec<IdentifierSpan>, String> {
    let owner = seed
        .owner_type
        .as_deref()
        .ok_or("the Java declaration owner is unresolved")?;
    if state
        .symbols
        .java_method(&seed.name, owner)
        .is_none_or(|node| node.id != seed.id)
    {
        return Err(
            "Java overloads require a language server to select the method signature".into(),
        );
    }
    let bindings = JavaBindings::new(tree, source);
    if bindings.is_unresolved() {
        return Err("Java source contains unsupported or invalid binding syntax".into());
    }
    let mut selected = Vec::new();
    let positions = super::source_positions::LineIndex::new(source);
    for span in targets {
        let node = tree
            .root_node()
            .named_descendant_for_byte_range(span.start_byte, span.end_byte)
            .ok_or("the Java occurrence has no syntax node")?;
        let parent = node.parent().ok_or("the Java occurrence has no parent")?;
        let is_name = parent
            .child_by_field_name("name")
            .is_some_and(|name| name.id() == node.id());
        let actual_owner = if is_name && parent.kind() == "method_declaration" {
            let actual = bindings.declaration_owner(parent);
            if actual.as_deref() == Some(owner)
                && (path != seed.path.as_ref()
                    || node.start_position().row as u32 + 1 != seed.line
                    || Some(
                        positions
                            .position(node.start_byte(), super::source_positions::Encoding::Utf16)
                            .character
                            + 1,
                    ) != seed.col)
            {
                return Err(
                    "the Java declaration changed since indexing; rebuild the index".into(),
                );
            }
            actual
        } else if is_name && parent.kind() == "method_invocation" {
            bindings.receiver_owner(parent)
        } else {
            return Err(format!(
                "unsupported Java reference at {path}:{}:{} ({})",
                span.start_row + 1,
                span.start_col + 1,
                parent.kind()
            ));
        };
        match actual_owner.as_deref() {
            Some(actual) if actual == owner => selected.push(span),
            Some(actual) if state.symbols.java_method(&seed.name, actual).is_some() => {}
            _ => {
                return Err(format!(
                    "unresolved Java receiver at {path}:{}:{}",
                    span.start_row + 1,
                    span.start_col + 1
                ))
            }
        }
    }
    Ok(selected)
}
