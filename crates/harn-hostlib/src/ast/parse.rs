//! `ast.parse_file` — flatten a tree-sitter parse tree to a wire-format
//! node list.
//!
//! Tree-sitter trees are pointer-rich and not directly JSON-serializable;
//! this module walks the tree once and assigns sequential node ids so the
//! response matches `schemas/ast/parse_file.response.json` (a flat
//! `nodes: [{id, parent_id, ...}]` array with `root_id`).

use std::path::PathBuf;
use std::rc::Rc;

use harn_vm::VmValue;
use tree_sitter::{Node, Parser, Tree};

use crate::error::HostlibError;
use crate::tools::args::{
    build_dict, dict_arg, optional_int, optional_string, require_string, str_value,
};

use super::language::Language;
use super::types::ParsedNode;

const BUILTIN: &str = "hostlib_ast_parse_file";

pub(super) fn run(args: &[VmValue]) -> Result<VmValue, HostlibError> {
    let raw = dict_arg(BUILTIN, args)?;
    let dict = raw.as_ref();

    let path_str = require_string(BUILTIN, dict, "path")?;
    let language_hint = optional_string(BUILTIN, dict, "language")?;
    let max_bytes = optional_int(BUILTIN, dict, "max_bytes", 0)?;
    if max_bytes < 0 {
        return Err(HostlibError::InvalidParameter {
            builtin: BUILTIN,
            param: "max_bytes",
            message: "must be >= 0".into(),
        });
    }

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

    let source = read_source(&path_str, max_bytes as usize)?;
    let tree = parse_source(&source, language)?;
    let (root_id, nodes) = flatten(&tree);

    let nodes_list: Vec<VmValue> = nodes.iter().map(ParsedNode::to_vm_value).collect();

    Ok(build_dict([
        ("path", str_value(&path_str)),
        ("language", str_value(language.name())),
        ("root_id", VmValue::Int(root_id as i64)),
        ("nodes", VmValue::List(Rc::new(nodes_list))),
        ("had_errors", VmValue::Bool(tree.root_node().has_error())),
    ]))
}

/// Read the file, optionally truncating to the first `max_bytes` bytes.
/// `max_bytes == 0` means unlimited (per the request schema).
pub(super) fn read_source(path: &str, max_bytes: usize) -> Result<String, HostlibError> {
    let bytes = std::fs::read(path).map_err(|err| HostlibError::Backend {
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
    let mut parser = Parser::new();
    parser
        .set_language(&language.ts_language())
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
            if let Some(child) = node.child(i as u32) {
                queue.push_back((child, Some(id)));
            }
        }
    }

    (0, nodes)
}
