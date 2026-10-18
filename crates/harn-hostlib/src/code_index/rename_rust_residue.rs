//! Rust mentions a language server's rename does not reach.
//!
//! rust-analyzer resolves a name inside a `macro_rules!` body only through an
//! expansion whose call site spells the name, so `$crate::span::Span::new`
//! in a macro invoked as `make_span!(0, 1)` keeps the old name and the
//! renamed crate stops compiling. It leaves rustdoc intra-doc links such as
//! ``[`Span::new`]`` unrenamed, and it never sees a module it did not load,
//! such as one behind a disabled `cfg`.
//!
//! This pass renames what it can resolve without expansion: absolute
//! `$crate::`/`crate::` macro-body paths to the seed and unqualified doc links
//! in the seed's own file. It reports [`Incomplete`] for a macro-body mention
//! it cannot resolve and for an unedited path that provably names the seed,
//! because the server's edit then misses references. Other doc links are left
//! alone: a stale link cannot break the build.

use std::path::Path;

use tree_sitter::Node;

use crate::ast::{api as ast_api, Language};
use crate::error::HostlibError;

use super::move_symbol::{Family, Lang, ModuleRef};
use super::refactor_backend::FileEdits;
use super::refactor_core::{is_refactor_declaration, read_source, EditSpan, IdentifierSpan};
use super::state::IndexState;

/// Why the server's edit cannot be trusted as complete.
pub(super) struct Incomplete(pub String);

/// The seed as a library path.
struct SeedPath {
    file: String,
    krate: String,
    module: Vec<String>,
    /// How other crates spell the seed's crate.
    ident: Option<String>,
}

impl SeedPath {
    /// Whether `segments`, the qualifier written before the name in `file`,
    /// names the seed.
    fn named_by(&self, segments: &[String], file: &str, file_crate: Option<&str>) -> bool {
        match segments.split_first() {
            None => file == self.file,
            Some((head, rest)) => match head.as_str() {
                "$crate" | "crate" => {
                    file_crate == Some(self.krate.as_str()) && rest == self.module
                }
                "self" => file == self.file && rest.is_empty(),
                name => {
                    file_crate != Some(self.krate.as_str())
                        && self.ident.as_deref() == Some(name)
                        && rest == self.module
                }
            },
        }
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
    pub session_id: Option<&'a str>,
}

/// Add edits for the resolvable residue to `changeset`.
pub(super) fn complete(
    residue: Residue<'_>,
    changeset: &mut Vec<FileEdits>,
) -> Result<Result<(), Incomplete>, HostlibError> {
    let Residue {
        builtin,
        state,
        seed_path,
        name,
        new_name,
        files,
        session_id,
    } = residue;
    let mut lang = Lang::new(Family::Rust, state, &[]);
    let Some(ModuleRef::Rust { crate_dir, path }) = lang.module_of(seed_path) else {
        return Ok(Ok(()));
    };
    let seed = SeedPath {
        file: seed_path.to_string(),
        ident: lang.rust_library_ident(&crate_dir),
        krate: crate_dir,
        module: path,
    };
    // Library modules that declare their own item spelled `name`.
    let mut namesakes: Vec<ModuleRef> = Vec::new();
    for id in state.symbols.nodes_named(name) {
        if let Some(node) = state.symbols.node(*id) {
            if is_refactor_declaration(node) && node.path.as_ref() != seed_path {
                namesakes.extend(lang.module_of(&node.path));
            }
        }
    }
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
        let at_line = |start: usize| {
            format!(
                "{path}:{}",
                source
                    .get(..start)
                    .unwrap_or_default()
                    .matches('\n')
                    .count()
                    + 1
            )
        };
        let mut added = Vec::new();

        let mut macro_mentions = Vec::new();
        let mut paths = Vec::new();
        collect(
            tree.root_node(),
            source.as_bytes(),
            name,
            false,
            &mut macro_mentions,
            &mut paths,
        );
        for node in macro_mentions {
            let span = IdentifierSpan::of(node);
            if covered(span.start_byte) || field_access(&source, span.start_byte) {
                continue;
            }
            let segments = qualifier(&source, span.start_byte);
            let absolute = matches!(
                segments.first().map(String::as_str),
                Some("$crate" | "crate")
            );
            if seed.named_by(&segments, path, file_crate.as_deref()) && absolute {
                added.push(span);
                continue;
            }
            let other_item = absolute
                && file_crate.as_ref().is_some_and(|krate| {
                    namesakes.contains(&ModuleRef::Rust {
                        crate_dir: krate.clone(),
                        path: segments[1..].to_vec(),
                    })
                });
            if !other_item {
                return Ok(Err(Incomplete(format!(
                    "the `macro_rules!` body at {} mentions `{name}` in a form that resolves only where the macro expands",
                    at_line(span.start_byte)
                ))));
            }
        }

        for node in paths {
            let start = node.start_byte();
            if covered(start) {
                continue;
            }
            let segments = path_qualifier(node, &source);
            if !segments.is_empty() && seed.named_by(&segments, path, file_crate.as_deref()) {
                return Ok(Err(Incomplete(format!(
                    "the language server left the path to `{name}` at {} unrenamed, so it did not analyze that code (a disabled `cfg` or an unloaded module)",
                    at_line(start)
                ))));
            }
        }

        for start in doc_link_mentions(tree.root_node(), &source, name) {
            if !covered(start)
                && seed.named_by(&qualifier(&source, start), path, file_crate.as_deref())
            {
                added.push(span_at(&source, start, start + name.len()));
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
    Ok(Ok(()))
}

/// Identifiers spelled `name` inside `macro_rules!` bodies go to `macros`;
/// outside them, the name segment of every qualified path or use-list entry
/// goes to `paths`.
fn collect<'t>(
    node: Node<'t>,
    bytes: &[u8],
    name: &str,
    inside: bool,
    macros: &mut Vec<Node<'t>>,
    paths: &mut Vec<Node<'t>>,
) {
    let inside = inside || node.kind() == "macro_definition";
    if matches!(node.kind(), "identifier" | "type_identifier") && node.utf8_text(bytes) == Ok(name)
    {
        if inside {
            macros.push(node);
        } else if node.parent().is_some_and(|parent| {
            matches!(
                parent.kind(),
                "scoped_identifier" | "scoped_type_identifier" | "use_list"
            )
        }) {
            paths.push(node);
        }
        return;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect(child, bytes, name, inside, macros, paths);
    }
}

/// The written qualifier of a path segment: `a::b` for `a::b::Name`, and the
/// list's prefix for `use a::b::{Name, ..}`.
fn path_qualifier(node: Node<'_>, source: &str) -> Vec<String> {
    let text = |node: Node<'_>| node.utf8_text(source.as_bytes()).unwrap_or_default();
    let split = |path: &str| -> Vec<String> {
        path.split("::")
            .map(|segment| segment.trim().to_string())
            .filter(|segment| !segment.is_empty())
            .collect()
    };
    let Some(parent) = node.parent() else {
        return Vec::new();
    };
    if parent.kind() == "use_list" {
        return parent
            .parent()
            .filter(|list| list.kind() == "scoped_use_list")
            .and_then(|list| list.child_by_field_name("path"))
            .map(|path| split(text(path)))
            .unwrap_or_default();
    }
    let is_name = parent
        .child_by_field_name("name")
        .is_some_and(|child| child.id() == node.id());
    match parent.child_by_field_name("path") {
        Some(path) if is_name => split(text(path)),
        _ => Vec::new(),
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
fn doc_link_mentions(root: Node<'_>, source: &str, name: &str) -> Vec<usize> {
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

fn span_at(source: &str, start: usize, end: usize) -> IdentifierSpan {
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
    use super::{doc_link_mentions, path_qualifier, qualifier};
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

    #[test]
    fn path_qualifiers_cover_use_lists_and_scoped_paths() {
        let source = "use crate::span::{Other, Span};\nuse other::Span as Range;\nfn f() -> crate::span::Span { todo!() }\n";
        let tree = ast_api::parse_tree(source, Language::Rust).unwrap();
        let mut macros = Vec::new();
        let mut paths = Vec::new();
        super::collect(
            tree.root_node(),
            source.as_bytes(),
            "Span",
            false,
            &mut macros,
            &mut paths,
        );
        let qualifiers: Vec<Vec<String>> = paths
            .iter()
            .map(|node| path_qualifier(*node, source))
            .collect();
        assert_eq!(
            qualifiers,
            [
                vec!["crate".to_string(), "span".to_string()],
                vec!["other".to_string()],
                vec!["crate".to_string(), "span".to_string()],
            ]
        );
    }
}
