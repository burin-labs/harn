//! Rust mentions a language server's rename does not reach.
//!
//! rust-analyzer resolves a name inside a `macro_rules!` body only through an
//! expansion whose call site spells the name, so `$crate::span::Span::new`
//! in a macro invoked as `make_span!(0, 1)` keeps the old name and the
//! renamed crate stops compiling. It leaves rustdoc intra-doc links such as
//! ``[`Span::new`]`` unrenamed, and it never sees a module it did not load,
//! such as one behind a disabled `cfg`.
//!
//! This pass renames `$crate::` macro-body paths that spell the seed's
//! module path, for module-level items only: a method shares its name with
//! free functions the path could mean. Doc links are resolved through the
//! server instead (see `rename_lsp`), as is whether anything is still missed.

use std::path::Path;

use tree_sitter::Node;

use crate::ast::{api as ast_api, Language};
use crate::error::HostlibError;

use super::move_symbol::{Family, Lang, ModuleRef};
use super::refactor_backend::FileEdits;
use super::refactor_core::{read_source, EditSpan, IdentifierSpan};
use super::state::IndexState;

/// The seed as a library path.
struct SeedPath {
    krate: String,
    module: Vec<String>,
}

impl SeedPath {
    /// Whether `$crate::<rest>::Name`, written in a file of `file_crate`,
    /// names the seed. Inside a macro body only `$crate` names the defining
    /// crate; a bare `crate` resolves where the macro is called.
    fn named_by_crate_path(&self, rest: &[String], file_crate: Option<&str>) -> bool {
        file_crate == Some(self.krate.as_str()) && rest == self.module
    }
}

pub(super) struct Residue<'a> {
    pub builtin: &'static str,
    pub state: &'a IndexState,
    pub seed_path: &'a str,
    pub name: &'a str,
    pub new_name: &'a str,
    /// Every workspace file that mentions `name`.
    pub files: &'a [String],
    /// The seed is a module-level item, not a method or associated item.
    pub module_level: bool,
    /// The seed is a type, so `Name::member` paths go through it; for any
    /// other item a following `::` means a same-named module.
    pub is_type: bool,
    pub session_id: Option<&'a str>,
}

/// Add edits for the resolvable residue to `changeset`.
pub(super) fn complete(
    residue: Residue<'_>,
    changeset: &mut Vec<FileEdits>,
) -> Result<(), HostlibError> {
    let Residue {
        builtin,
        state,
        seed_path,
        name,
        new_name,
        files,
        session_id,
        module_level,
        is_type,
    } = residue;
    if !module_level {
        return Ok(());
    }
    let mut lang = Lang::new(Family::Rust, state, &[]);
    let Some(ModuleRef::Rust { crate_dir, path }) = lang.module_of(seed_path) else {
        return Ok(());
    };
    let seed = SeedPath {
        krate: crate_dir,
        module: path,
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
        let Ok(tree) = ast_api::parse_tree(&source, Language::Rust) else {
            continue;
        };
        let file_crate = match lang.module_of(path) {
            Some(ModuleRef::Rust { crate_dir, .. }) => Some(crate_dir),
            _ => None,
        };
        let covered = |start: usize| {
            position.is_some_and(|at| {
                changeset[at]
                    .edits
                    .iter()
                    .any(|edit| edit.span.start_byte == start)
            })
        };
        let mut added = Vec::new();
        let mut macro_mentions = Vec::new();
        collect_macro_mentions(
            tree.root_node(),
            source.as_bytes(),
            name,
            false,
            &mut macro_mentions,
        );
        for node in macro_mentions {
            let span = IdentifierSpan::of(node);
            if covered(span.start_byte) || field_access(&source, span.start_byte) {
                continue;
            }
            let continues = source
                .get(span.end_byte..)
                .unwrap_or_default()
                .trim_start()
                .starts_with("::");
            if continues && !is_type {
                continue;
            }
            let segments = qualifier(&source, span.start_byte);
            let names_seed = segments.split_first().is_some_and(|(head, rest)| {
                head == "$crate" && seed.named_by_crate_path(rest, file_crate.as_deref())
            });
            if names_seed {
                added.push(span);
            }
        }

        if added.is_empty() {
            continue;
        }
        let added = added.into_iter().map(|span| EditSpan {
            span,
            before: name.to_string(),
            after: new_name.to_string(),
        });
        match position {
            Some(at) => {
                let edits = &mut changeset[at].edits;
                edits.extend(added);
                edits.sort_by_key(|edit| edit.span.start_byte);
            }
            None => changeset.push(FileEdits {
                path: path.clone(),
                source,
                edits: added.collect(),
            }),
        }
    }
    Ok(())
}

fn collect_macro_mentions<'t>(
    node: Node<'t>,
    bytes: &[u8],
    name: &str,
    inside: bool,
    out: &mut Vec<Node<'t>>,
) {
    let inside = inside || node.kind() == "macro_definition";
    if inside && node.kind() == "identifier" && node.utf8_text(bytes) == Ok(name) {
        out.push(node);
        return;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_macro_mentions(child, bytes, name, inside, out);
    }
}

/// `x.Name`: a field or method, not a path.
fn field_access(source: &str, start: usize) -> bool {
    source
        .get(..start)
        .unwrap_or_default()
        .trim_end()
        .ends_with('.')
}

/// Byte offsets of `name` where it is a path segment of an intra-doc link:
/// ``[`Span`]``, `[Span::new]`, or the target of `[text](crate::span::Span)`.
pub(super) fn doc_link_mentions(root: Node<'_>, source: &str, name: &str) -> Vec<usize> {
    let mut comments = Vec::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if matches!(node.kind(), "line_comment" | "block_comment") {
            comments.push(node.byte_range());
            continue;
        }
        let mut cursor = node.walk();
        stack.extend(node.children(&mut cursor));
    }
    let path_char = |ch: char| ch.is_alphanumeric() || ch == '_' || ch == ':' || ch == '$';
    let mut found = Vec::new();
    for range in comments {
        let Some(text) = source.get(range.clone()) else {
            continue;
        };
        if !["///", "//!", "/**", "/*!"]
            .iter()
            .any(|marker| text.starts_with(marker))
        {
            continue;
        }
        for (at, _) in text.match_indices(name) {
            let (before, rest) = text.split_at(at);
            let after = rest.get(name.len()..).unwrap_or_default();
            let path_start = before.trim_end_matches(path_char);
            let path_end = after.trim_start_matches(path_char);
            let opens = path_start.ends_with('[')
                || path_start.ends_with("[`")
                || path_start.ends_with("](");
            let closes = [']', '`', ')', '(']
                .iter()
                .any(|ch| path_end.starts_with(*ch));
            let whole_segment = !before.ends_with(|ch: char| ch.is_alphanumeric() || ch == '_')
                && !after.starts_with(|ch: char| ch.is_alphanumeric() || ch == '_');
            if opens && closes && whole_segment {
                found.push(range.start + at);
            }
        }
    }
    found
}

pub(super) fn span_at(source: &str, start: usize, end: usize) -> IdentifierSpan {
    let position = |offset: usize| {
        let prefix = source.get(..offset).unwrap_or_default();
        let row = prefix.matches('\n').count();
        let col = offset - prefix.rfind('\n').map_or(0, |newline| newline + 1);
        (row, col)
    };
    let (start_row, start_col) = position(start);
    let (end_row, end_col) = position(end);
    IdentifierSpan {
        start_byte: start,
        end_byte: end,
        start_row,
        start_col,
        end_row,
        end_col,
    }
}

/// The path segments written before the identifier at `start`, outermost
/// first: `$crate::span::Span` yields `["$crate", "span"]`.
pub(super) fn qualifier(source: &str, start: usize) -> Vec<String> {
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
    use super::{doc_link_mentions, qualifier};
    use crate::ast::{api as ast_api, Language};

    #[test]
    fn qualifier_reads_segments_before_the_name() {
        let source = "x = $crate::span :: Span::new(1)";
        let at = source.find("Span").unwrap();
        assert_eq!(qualifier(source, at), ["$crate", "span"]);
        let bare = "let Span = 1";
        assert!(qualifier(bare, bare.find("Span").unwrap()).is_empty());
    }

    #[test]
    fn doc_links_are_found_and_prose_is_not() {
        let source = "/// See [`Span::new`], [Span], [a](crate::span::Span) and Span or [`Spanned`].\n// [`Span`] in a plain comment.\nstruct Span;\n";
        let tree = ast_api::parse_tree(source, Language::Rust).unwrap();
        let found = doc_link_mentions(tree.root_node(), source, "Span");
        let expected: Vec<usize> = ["[`Span::new`]", "[Span]", "span::Span)"]
            .iter()
            .map(|link| source.find(link).unwrap() + link.find("Span").unwrap())
            .collect();
        assert_eq!(found, expected);
    }
}
