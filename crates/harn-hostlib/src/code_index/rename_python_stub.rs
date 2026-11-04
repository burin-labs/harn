//! A Python module and its same-stem type stub declare one symbol twice.
//!
//! `x.py` and `x.pyi` are one module to a type checker: importers bind to the
//! stub and running code binds to the implementation. pyright renames from the
//! implementation without touching the stub, so a language-server rename asks
//! at both declarations. The pairing is by file and scope, never by name alone:
//! a same-named declaration in another file or another scope is unrelated.

use std::path::PathBuf;

use tree_sitter::Node;

use crate::ast::{api as ast_api, Language};
use crate::error::HostlibError;

use super::lsp::{Resolution, Target};
use super::refactor_backend::FileEdits;
use super::refactor_core::{collect_identifier_spans, read_source};
use super::rename::BUILTIN;
use super::state::IndexState;

/// The seed's declarations in its sibling file.
pub(super) struct Counterpart {
    /// Index-relative path of the sibling.
    pub path: String,
    pub source: String,
    /// Byte offset of each declaration's name, in source order: a stub's
    /// overloads, or a binding repeated across conditional branches.
    pub offsets: Vec<usize>,
    canonical: PathBuf,
}

impl Counterpart {
    /// The counterpart of the Python declaration whose name starts at byte
    /// `offset` of `seed_source`. `None` for a declaration local to a function,
    /// which a stub cannot describe, or when the sibling declares nothing at
    /// the same scope. `mentions` lists every file that names the declaration.
    pub(super) fn find(
        state: &IndexState,
        seed_path: &str,
        seed_source: &str,
        offset: usize,
        name: &str,
        mentions: &[String],
        session_id: Option<&str>,
    ) -> Result<Option<Self>, HostlibError> {
        let Some(path) = sibling(seed_path).filter(|path| mentions.contains(path)) else {
            return Ok(None);
        };
        let Ok(seed_tree) = ast_api::parse_tree(seed_source, Language::Python) else {
            return Ok(None);
        };
        let Some(scope) = seed_tree
            .root_node()
            .named_descendant_for_byte_range(offset, offset + name.len())
            .and_then(|identifier| scope_of(identifier, seed_source))
        else {
            return Ok(None);
        };
        let source = read_source(BUILTIN, &state.root, &path, session_id)?;
        let offsets = declarations(&source, name, &scope);
        if offsets.is_empty() {
            return Ok(None);
        }
        let canonical = state.root.join(&path);
        let canonical = canonical.canonicalize().unwrap_or(canonical);
        Ok(Some(Self {
            path,
            source,
            offsets,
            canonical,
        }))
    }

    /// Whether a mention left at `site` is this declaration or a reference to
    /// it. The server answers with the sibling alone when it does not map one
    /// file to the other, which must not read as an unrelated declaration.
    pub(super) fn names(&self, site: (&str, usize), resolution: &Resolution) -> bool {
        (site.0 == self.path && self.offsets.contains(&site.1))
            || matches!(resolution, Resolution::Elsewhere(Some(Target { path, offset }))
                if *path == self.canonical && self.offsets.contains(offset))
    }
}

/// Add `more` to `changeset`; an edit both answers contain is applied once.
pub(super) fn merge(changeset: &mut Vec<FileEdits>, more: Vec<FileEdits>) {
    for file in more {
        let Some(existing) = changeset.iter_mut().find(|known| known.path == file.path) else {
            changeset.push(file);
            continue;
        };
        for edit in file.edits {
            let start = edit.span.start_byte;
            if !existing
                .edits
                .iter()
                .any(|known| known.span.start_byte == start)
            {
                existing.edits.push(edit);
            }
        }
        existing.edits.sort_by_key(|edit| edit.span.start_byte);
    }
}

fn sibling(path: &str) -> Option<String> {
    if let Some(stem) = path.strip_suffix(".pyi") {
        return Some(format!("{stem}.py"));
    }
    path.strip_suffix(".py").map(|stem| format!("{stem}.pyi"))
}

/// The classes enclosing the declaration named by `identifier`, outermost
/// first. `None` inside a function, where only its own body can reach it.
fn scope_of(identifier: Node<'_>, source: &str) -> Option<Vec<String>> {
    let mut classes = Vec::new();
    // The name's own `def` or `class` is what it declares, not its scope.
    let mut node = identifier.parent().and_then(|parent| parent.parent());
    while let Some(current) = node {
        match current.kind() {
            "function_definition" | "lambda" => return None,
            "class_definition" => {
                let name = current.child_by_field_name("name")?;
                classes.push(name.utf8_text(source.as_bytes()).ok()?.to_string());
            }
            _ => {}
        }
        node = current.parent();
    }
    classes.reverse();
    Some(classes)
}

/// Byte offsets of the names in `source` that bind `name` directly in the
/// class chain `scope`.
fn declarations(source: &str, name: &str, scope: &[String]) -> Vec<usize> {
    let Ok(tree) = ast_api::parse_tree(source, Language::Python) else {
        return Vec::new();
    };
    let Some(kinds) = Language::Python.rename_identifier_kinds() else {
        return Vec::new();
    };
    let mut spans = Vec::new();
    collect_identifier_spans(
        tree.root_node(),
        source.as_bytes(),
        name,
        "",
        kinds,
        "",
        &mut spans,
        &mut Vec::new(),
    );
    let mut offsets: Vec<usize> = spans
        .iter()
        .filter(|span| {
            tree.root_node()
                .named_descendant_for_byte_range(span.start_byte, span.end_byte)
                .is_some_and(|identifier| {
                    binds(identifier) && scope_of(identifier, source).as_deref() == Some(scope)
                })
        })
        .map(|span| span.start_byte)
        .collect();
    offsets.sort_unstable();
    offsets
}

/// Whether `identifier` introduces its name rather than reading it: a `def`,
/// a `class`, an assignment target, or a name an import binds.
fn binds(identifier: Node<'_>) -> bool {
    let Some(parent) = identifier.parent() else {
        return false;
    };
    let is_field = |node: Node<'_>, field: &str, child: Node<'_>| {
        node.child_by_field_name(field) == Some(child)
    };
    match parent.kind() {
        "function_definition" | "class_definition" => is_field(parent, "name", identifier),
        "aliased_import" => is_field(parent, "alias", identifier),
        "dotted_name" => parent.parent().is_some_and(|import| {
            import.kind() == "import_from_statement"
                && !is_field(import, "module_name", parent)
                && parent.named_child_count() == 1
        }),
        "assignment" => is_field(parent, "left", identifier),
        "pattern_list" | "tuple_pattern" | "list_pattern" => {
            let mut target = parent;
            while let Some(outer) = target.parent() {
                if outer.kind() == "assignment" {
                    return is_field(outer, "left", target);
                }
                if !matches!(
                    outer.kind(),
                    "pattern_list" | "tuple_pattern" | "list_pattern"
                ) {
                    return false;
                }
                target = outer;
            }
            false
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `.py` may bind what its stub declares with an import or an assignment;
    /// missing one would read a stale binding as an unrelated declaration.
    #[test]
    fn declarations_are_the_names_bound_at_the_same_scope() {
        let source = "from a import tee, other as tee2\nfrom b import x as tee3\nfrom tee import unrelated\nprint(tee)\ntee = 1\ntee, more = 1, 2\n\ndef tee(): ...\n\ndef run():\n    tee = 2\n\nclass Box:\n    def tee(self): ...\n";
        let at = |nth: usize| source.match_indices("tee").nth(nth).unwrap().0;
        assert_eq!(
            declarations(source, "tee", &[]),
            // The first `from` import, `tee =`, the tuple target, the `def`.
            vec![at(0), at(5), at(6), at(7)],
        );
        assert_eq!(
            declarations(source, "tee", &["Box".to_string()]),
            vec![at(9)],
        );
        assert_eq!(
            declarations(source, "tee3", &[]),
            vec![source.find("tee3").unwrap()]
        );
    }
}
