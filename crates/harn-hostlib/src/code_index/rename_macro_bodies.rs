//! Rust `macro_rules!` bodies a language server's rename does not reach.
//!
//! rust-analyzer resolves a name inside a macro body only through an
//! expansion whose call site spells the name, so `$crate::span::Span::new`
//! in a macro invoked as `make_span!(0, 1)` keeps the old name and the
//! renamed crate stops compiling. An absolute path (`$crate::` or `crate::`)
//! resolves without expansion, so this pass renames those that name the seed
//! and refuses on any other mention it cannot resolve.

use std::path::Path;

use tree_sitter::Node;

use crate::ast::{api as ast_api, Language};
use crate::error::HostlibError;

use super::move_symbol::{Family, Lang, ModuleRef};
use super::refactor_backend::FileEdits;
use super::refactor_core::{read_source, EditSpan, IdentifierSpan};
use super::state::IndexState;

/// A macro-body mention that could not be resolved, as `path:line`.
pub(super) struct Unresolved(pub String);

/// Add edits for absolute macro-body paths to `seed_path`'s `name` into
/// `changeset`, reading `files` that the server's edit did not cover.
pub(super) fn complete(
    builtin: &'static str,
    state: &IndexState,
    seed_path: &str,
    name: &str,
    new_name: &str,
    files: &[String],
    session_id: Option<&str>,
    changeset: &mut Vec<FileEdits>,
) -> Result<Result<(), Unresolved>, HostlibError> {
    let mut lang = Lang::new(Family::Rust, state, &[]);
    let Some(ModuleRef::Rust {
        crate_dir: seed_crate,
        path: seed_module,
    }) = lang.module_of(seed_path)
    else {
        return Ok(Ok(()));
    };
    for path in files {
        if Language::detect(Path::new(path), None) != Some(Language::Rust) {
            continue;
        }
        let position = changeset.iter().position(|file| &file.path == path);
        let source = match position {
            Some(at) => changeset[at].source.clone(),
            None => read_source(builtin, &state.root, path, session_id)?,
        };
        if !source.contains("macro_rules!") {
            continue;
        }
        let Ok(tree) = ast_api::parse_tree(&source, Language::Rust) else {
            continue;
        };
        let mut mentions = Vec::new();
        collect(
            tree.root_node(),
            source.as_bytes(),
            name,
            false,
            &mut mentions,
        );
        let file_crate = match lang.module_of(path) {
            Some(ModuleRef::Rust { crate_dir, .. }) => Some(crate_dir),
            _ => None,
        };
        let mut added = Vec::new();
        for node in mentions {
            let span = IdentifierSpan::of(node);
            let covered = position.is_some_and(|at| {
                changeset[at]
                    .edits
                    .iter()
                    .any(|edit| edit.span.start_byte == span.start_byte)
            });
            if covered {
                continue;
            }
            let segments = qualifier(&source, span.start_byte);
            let absolute = matches!(
                segments.first().map(String::as_str),
                Some("$crate" | "crate")
            );
            if !absolute || file_crate.is_none() {
                return Ok(Err(Unresolved(format!("{path}:{}", span.start_row + 1))));
            }
            if file_crate.as_deref() == Some(seed_crate.as_str())
                && segments[1..] == seed_module[..]
            {
                added.push(EditSpan {
                    span,
                    before: name.to_string(),
                    after: new_name.to_string(),
                });
            }
        }
        if added.is_empty() {
            continue;
        }
        match position {
            Some(at) => {
                let edits = &mut changeset[at].edits;
                edits.extend(added);
                edits.sort_by_key(|edit| edit.span.start_byte);
            }
            None => changeset.push(FileEdits {
                path: path.clone(),
                source,
                edits: added,
            }),
        }
    }
    Ok(Ok(()))
}

fn collect<'t>(node: Node<'t>, bytes: &[u8], name: &str, inside: bool, out: &mut Vec<Node<'t>>) {
    let inside = inside || node.kind() == "macro_definition";
    if inside && node.kind() == "identifier" && node.utf8_text(bytes) == Ok(name) {
        out.push(node);
        return;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect(child, bytes, name, inside, out);
    }
}

/// The path segments written before the identifier at `start`, outermost
/// first: `$crate::span::Span` yields `["$crate", "span"]`.
fn qualifier(source: &str, start: usize) -> Vec<String> {
    let mut segments = Vec::new();
    let mut rest = source.get(..start).unwrap_or_default();
    loop {
        let trimmed = rest.trim_end();
        let Some(before) = trimmed.strip_suffix("::") else {
            break;
        };
        let before = before.trim_end();
        let segment_start = before
            .char_indices()
            .rev()
            .take_while(|(_, ch)| ch.is_alphanumeric() || *ch == '_' || *ch == '$')
            .last()
            .map(|(at, _)| at);
        let Some(at) = segment_start else {
            break;
        };
        let (head, segment) = before.split_at(at);
        segments.push(segment.to_string());
        rest = head;
    }
    segments.reverse();
    segments
}

#[cfg(test)]
mod tests {
    use super::qualifier;

    #[test]
    fn qualifier_reads_segments_before_the_name() {
        let source = "x = $crate::span :: Span::new(1)";
        let at = source.find("Span").unwrap();
        assert_eq!(qualifier(source, at), ["$crate", "span"]);
        let bare = "let Span = 1";
        assert!(qualifier(bare, bare.find("Span").unwrap()).is_empty());
    }
}
