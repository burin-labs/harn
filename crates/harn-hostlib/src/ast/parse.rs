//! `ast.parse_file` — flatten a tree-sitter parse tree to a wire-format
//! node list.
//!
//! Tree-sitter trees are pointer-rich and not directly JSON-serializable;
//! this module walks the tree once and assigns sequential node ids so the
//! response matches `schemas/ast/parse_file.response.json` (a flat
//! `nodes: [{id, parent_id, ...}]` array with `root_id`).

use std::path::PathBuf;

use harn_vm::VmValue;
use tree_sitter::{Node, Parser, Tree};

use crate::error::HostlibError;
use crate::tools::args::{
    build_dict, dict_arg, optional_int, optional_string, require_string, str_value,
};

use super::health::{ParserHealth, ParserOperation};
use super::language::Language;
use super::types::ParsedNode;

const BUILTIN: &str = "hostlib_ast_parse_file";

pub(super) fn run(args: &[VmValue]) -> Result<VmValue, HostlibError> {
    let raw = dict_arg(BUILTIN, args)?;
    let dict = raw.as_ref();

    let path_str = require_string(BUILTIN, dict, "path")?;
    let language_hint = optional_string(BUILTIN, dict, "language")?;
    let max_bytes = max_bytes_arg(BUILTIN, dict)?;

    let path = PathBuf::from(&path_str);
    let language = Language::detect(&path, language_hint.as_deref()).ok_or_else(|| {
        HostlibError::InvalidParameter {
            builtin: BUILTIN,
            param: "language",
            message: format!(
                "could not infer a tree-sitter grammar for `{path_str}` \
                 (extension or `language` field unrecognized)"
            ),
        }
    })?;

    let source = read_source(&path_str, max_bytes)?;
    let tree = parse_source(&source, language)?;
    let (root_id, nodes) = flatten(&tree);
    let had_errors = tree.root_node().has_error();
    let health = ParserHealth::observed(language, ParserOperation::Parse, had_errors);

    let nodes_list: Vec<VmValue> = nodes.iter().map(ParsedNode::to_vm_value).collect();

    Ok(build_dict([
        ("path", str_value(&path_str)),
        ("language", str_value(language.name())),
        ("root_id", VmValue::Int(root_id as i64)),
        ("nodes", VmValue::list(nodes_list)),
        ("had_errors", VmValue::Bool(had_errors)),
        ("health", health.to_vm_value()),
    ]))
}

/// The optional non-negative `max_bytes` request field (`0` = unlimited).
pub(super) fn max_bytes_arg(
    builtin: &'static str,
    dict: &harn_vm::value::DictMap,
) -> Result<usize, HostlibError> {
    let max_bytes = optional_int(builtin, dict, "max_bytes", 0)?;
    usize::try_from(max_bytes).map_err(|_| HostlibError::InvalidParameter {
        builtin,
        param: "max_bytes",
        message: "must be >= 0".into(),
    })
}

/// Request shared by the analyses that accept either in-memory `content`
/// or a `path` (plus an optional `language` hint and `max_bytes`).
pub(super) struct ContentOrPath {
    pub(super) content: Option<String>,
    pub(super) path: Option<String>,
    pub(super) language: Language,
    pub(super) max_bytes: usize,
}

impl ContentOrPath {
    pub(super) fn from_args(builtin: &'static str, args: &[VmValue]) -> Result<Self, HostlibError> {
        let raw = dict_arg(builtin, args)?;
        let dict = raw.as_ref();
        let content = optional_string(builtin, dict, "content")?;
        let path = optional_string(builtin, dict, "path")?;
        let language_hint = optional_string(builtin, dict, "language")?;
        let max_bytes = max_bytes_arg(builtin, dict)?;
        if content.is_none() && path.is_none() {
            return Err(HostlibError::MissingParameter {
                builtin,
                param: "content_or_path",
            });
        }
        let language = resolve_language(builtin, path.as_deref(), language_hint.as_deref())?;
        Ok(Self {
            content,
            path,
            language,
            max_bytes,
        })
    }

    /// `path` for the response, or `""` for in-memory content.
    pub(super) fn path_str(&self) -> &str {
        self.path.as_deref().unwrap_or("")
    }

    /// The source text: `content` clipped to `max_bytes` at a UTF-8
    /// boundary, or else the file at `path`.
    pub(super) fn load_source(&self) -> Result<String, HostlibError> {
        match (&self.content, &self.path) {
            (Some(text), _) => Ok(clip_to_char_boundary(text, self.max_bytes)),
            (None, Some(path)) => read_source(path, self.max_bytes),
            (None, None) => unreachable!("checked in from_args"),
        }
    }
}

/// Resolve the grammar from an explicit hint (a wire name or a bare
/// extension) or, failing that, from `path`.
fn resolve_language(
    builtin: &'static str,
    path: Option<&str>,
    language_hint: Option<&str>,
) -> Result<Language, HostlibError> {
    if let Some(name) = language_hint.filter(|s| !s.is_empty()) {
        if let Some(lang) = Language::from_name(name).or_else(|| Language::from_extension(name)) {
            return Ok(lang);
        }
    }
    if let Some(p) = path.filter(|s| !s.is_empty()) {
        if let Some(lang) = Language::detect(&PathBuf::from(p), language_hint) {
            return Ok(lang);
        }
    }
    Err(HostlibError::InvalidParameter {
        builtin,
        param: "language",
        message: format!(
            "could not infer a tree-sitter grammar (path = `{}`, language = `{}`)",
            path.unwrap_or(""),
            language_hint.unwrap_or("")
        ),
    })
}

/// Trim to the last UTF-8 boundary at or below `max_bytes` (`0` keeps
/// everything) so tree-sitter never sees a half-codepoint.
fn clip_to_char_boundary(text: &str, max_bytes: usize) -> String {
    if max_bytes == 0 || text.len() <= max_bytes {
        return text.to_string();
    }
    text.get(..text.floor_char_boundary(max_bytes))
        .unwrap_or_default()
        .to_string()
}

/// Read the file, optionally truncating to the first `max_bytes` bytes.
/// `max_bytes == 0` means unlimited (per the request schema).
pub(super) fn read_source(path: &str, max_bytes: usize) -> Result<String, HostlibError> {
    let path_buf = PathBuf::from(path);
    let bytes = match crate::fs::read(&path_buf, None) {
        Some(result) => result,
        None => std::fs::read(path),
    }
    .map_err(|err| HostlibError::Backend {
        builtin: BUILTIN,
        message: format!("read `{path}`: {err}"),
    })?;
    let slice = if max_bytes == 0 || bytes.len() <= max_bytes {
        &bytes[..]
    } else {
        &bytes[..max_bytes]
    };
    // Lossy decode keeps tree-sitter happy when input contains stray
    // bytes; in practice every shipped grammar handles UTF-8.
    Ok(String::from_utf8_lossy(slice).into_owned())
}

/// Build a parser, point it at `language`'s grammar, and parse `source`.
/// Tree-sitter parser construction is cheap; we don't bother pooling.
pub(super) fn parse_source(source: &str, language: Language) -> Result<Tree, HostlibError> {
    parse_bytes(source.as_bytes(), language)
}

/// Parse raw `source` bytes for `language`. The byte offsets in the
/// returned tree index `source` directly, so the AST edit primitives can
/// splice on the original bytes and preserve any non-UTF-8 bytes verbatim
/// (lossy decoding the source first would corrupt them — see the
/// byte-preserving edit path in `edit_common`).
pub(super) fn parse_bytes(source: &[u8], language: Language) -> Result<Tree, HostlibError> {
    let ts_language = language
        .ts_language()
        .ok_or_else(|| HostlibError::Backend {
            builtin: BUILTIN,
            message: format!(
                "grammar for `{}` is not compiled into this build",
                language.name()
            ),
        })?;
    let mut parser = Parser::new();
    parser
        .set_language(&ts_language)
        .map_err(|err| HostlibError::Backend {
            builtin: BUILTIN,
            message: format!("set tree-sitter language `{}`: {err}", language.name()),
        })?;
    parser
        .parse(source, None)
        .ok_or_else(|| HostlibError::Backend {
            builtin: BUILTIN,
            message: format!(
                "tree-sitter parse failed for language `{}` (timeout or panic)",
                language.name()
            ),
        })
}

/// Walk the tree breadth-first, assigning sequential ids. Breadth-first
/// keeps siblings contiguous in the output, which makes the wire format
/// easier to read in dumps.
fn flatten(tree: &Tree) -> (u32, Vec<ParsedNode>) {
    let root = tree.root_node();
    let mut nodes: Vec<ParsedNode> = Vec::new();
    let mut queue: std::collections::VecDeque<(Node<'_>, Option<u32>)> =
        std::collections::VecDeque::new();
    queue.push_back((root, None));

    while let Some((node, parent_id)) = queue.pop_front() {
        let id = nodes.len() as u32;
        let s = node.start_position();
        let e = node.end_position();
        nodes.push(ParsedNode {
            id,
            parent_id,
            kind: node.kind().to_string(),
            is_named: node.is_named(),
            start_byte: node.start_byte() as u32,
            end_byte: node.end_byte() as u32,
            start_row: s.row as u32,
            start_col: s.column as u32,
            end_row: e.row as u32,
            end_col: e.column as u32,
        });
        for i in 0..node.child_count() {
            if let Some(child) = node.child(i) {
                queue.push_back((child, Some(id)));
            }
        }
    }

    (0, nodes)
}
