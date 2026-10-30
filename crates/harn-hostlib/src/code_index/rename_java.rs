//! Adapt the Java source binding resolver to the indexed rename seed.

use tree_sitter::Tree;

use super::java_bindings::JavaBindings;
use super::refactor_core::{IdentifierSpan, ShadowSite};
use super::state::IndexState;
use super::symbol_graph::Node;

pub(super) struct RenameSites {
    pub targets: Vec<IdentifierSpan>,
    pub shadows: Vec<ShadowSite>,
}

pub(super) fn is_constructor(tree: &Tree, source: &str, seed: &Node) -> bool {
    let positions = super::source_positions::LineIndex::new(source);
    let mut pending = vec![tree.root_node()];
    while let Some(node) = pending.pop() {
        if matches!(
            node.kind(),
            "constructor_declaration" | "compact_constructor_declaration"
        ) {
            if let Some(name) = node.child_by_field_name("name") {
                let line = seed.line as usize;
                if (node.start_position().row + 1 == line || name.start_position().row + 1 == line)
                    && source.get(name.byte_range()) == Some(seed.name.as_str())
                    && seed.col.is_some_and(|col| {
                        positions
                            .position(name.start_byte(), super::source_positions::Encoding::Utf16)
                            .character
                            + 1
                            == col
                    })
                {
                    return true;
                }
            }
        }
        let mut cursor = node.walk();
        pending.extend(node.named_children(&mut cursor));
    }
    false
}

pub(super) fn check_new_name(name: &str) -> Result<(), String> {
    if crate::ast::java::is_object_method(name) {
        Err("the proposed Java name may change an Object dispatch contract".into())
    } else {
        Ok(())
    }
}

pub(super) fn sites(
    state: &IndexState,
    seed: &Node,
    path: &str,
    source: &str,
    tree: &Tree,
    targets: Vec<IdentifierSpan>,
    shadows: Vec<ShadowSite>,
) -> Result<RenameSites, String> {
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
        let Some(occurrence) = occurrence(&bindings, tree, path, &span)? else {
            continue;
        };
        if occurrence.is_declaration
            && occurrence.owner.as_deref() == Some(owner)
            && (path != seed.path.as_ref()
                || span.start_row as u32 + 1 != seed.line
                || Some(
                    positions
                        .position(span.start_byte, super::source_positions::Encoding::Utf16)
                        .character
                        + 1,
                ) != seed.col)
        {
            return Err("the Java declaration changed since indexing; rebuild the index".into());
        }
        if occurrence.is_declaration
            && occurrence.owner.as_deref() == Some(owner)
            && !occurrence.closed_dispatch
        {
            return Err("Java method rename requires a final declaring type or a private method to preserve subtype dispatch".into());
        }
        if matches_owner(state, owner, &seed.name, occurrence, path, &span)? {
            selected.push(span);
        }
    }
    let mut selected_shadows = Vec::new();
    for shadow in shadows {
        let point = tree_sitter::Point::new(shadow.row, shadow.col);
        let node = tree
            .root_node()
            .named_descendant_for_point_range(point, point)
            .ok_or("the Java shadow occurrence has no syntax node")?;
        if node.start_position() != point {
            return Err("the Java shadow occurrence does not match its identifier position".into());
        }
        let span = IdentifierSpan::of(node);
        let name = source
            .get(span.start_byte..span.end_byte)
            .ok_or("the Java shadow occurrence has no source text")?;
        let Some(occurrence) = occurrence(&bindings, tree, path, &span)? else {
            continue;
        };
        if matches_owner(state, owner, name, occurrence, path, &span)? {
            selected_shadows.push(shadow);
        }
    }
    Ok(RenameSites {
        targets: selected,
        shadows: selected_shadows,
    })
}

struct Occurrence {
    owner: Option<String>,
    is_declaration: bool,
    closed_dispatch: bool,
}

fn occurrence(
    bindings: &JavaBindings<'_>,
    tree: &Tree,
    path: &str,
    span: &IdentifierSpan,
) -> Result<Option<Occurrence>, String> {
    let node = tree
        .root_node()
        .named_descendant_for_byte_range(span.start_byte, span.end_byte)
        .ok_or("the Java occurrence has no syntax node")?;
    let parent = node.parent().ok_or("the Java occurrence has no parent")?;
    if parent
        .child_by_field_name("name")
        .is_some_and(|name| name.id() == node.id())
    {
        match parent.kind() {
            "method_declaration" => {
                return Ok(Some(Occurrence {
                    owner: bindings.declaration_owner(parent),
                    is_declaration: true,
                    closed_dispatch: bindings.method_has_closed_dispatch(parent),
                }));
            }
            "method_invocation" => {
                return Ok(Some(Occurrence {
                    owner: bindings.receiver_owner(parent),
                    is_declaration: false,
                    closed_dispatch: false,
                }));
            }
            "class_declaration"
            | "interface_declaration"
            | "enum_declaration"
            | "record_declaration"
            | "annotation_type_declaration"
            | "constructor_declaration"
            | "compact_constructor_declaration" => return Ok(None),
            _ => {}
        }
    }
    Err(format!(
        "unsupported Java reference at {path}:{}:{} ({})",
        span.start_row + 1,
        span.start_col + 1,
        parent.kind()
    ))
}

fn matches_owner(
    state: &IndexState,
    owner: &str,
    name: &str,
    occurrence: Occurrence,
    path: &str,
    span: &IdentifierSpan,
) -> Result<bool, String> {
    match occurrence.owner.as_deref() {
        Some(actual) if actual == owner => Ok(true),
        Some(actual) if state.symbols.java_method(name, actual).is_some() => Ok(false),
        _ => Err(format!(
            "unresolved Java receiver at {path}:{}:{}",
            span.start_row + 1,
            span.start_col + 1
        )),
    }
}
