//! `ast.extract_imports` — pull import statements out of a source file.
//!
//! Walks the parse tree, collects nodes whose kind is on the
//! [`IMPORT_NODE_TYPES`] list, and returns their text in document
//! order. Falls back to a top-level keyword scan when the grammar's
//! AST didn't surface anything (some shells emit imports as plain
//! commands).
//!
//! ## Wire format
//!
//! Accepts either an in-memory `source` (with `language`) or a `path`.
//! Response shape:
//!
//! ```json
//! {
//!   "path": "...",
//!   "language": "typescript",
//!   "supported": true,
//!   "statements": [
//!     { "text": "import foo from 'bar'", "line": 1 },
//!     ...
//!   ]
//! }
//! ```

use std::collections::BTreeMap;
use std::rc::Rc;

use harn_vm::VmValue;
use tree_sitter::{Node, Tree};

use crate::error::HostlibError;
use crate::tools::args::{build_dict, dict_arg, optional_string, str_value};

use super::language::Language;
use super::parse::{parse_source, read_source};
use super::symbols::helpers::{children, node_text};

const BUILTIN: &str = "hostlib_ast_extract_imports";

/// Tree-sitter node kinds that wrap an import declaration. The set is
/// the union of every grammar's import-like node type emitted by the
/// shipped grammars.
const IMPORT_NODE_TYPES: &[&str] = &[
    // TS / JS / Python
    "import_statement",
    // Python
    "import_from_statement",
    // Go / Java / Scala
    "import_declaration",
    // Go
    "import_spec",
    // Rust
    "use_declaration",
    // Kotlin
    "import_list",
    "import_header",
    // C / C++
    "preproc_include",
    // C#
    "using_directive",
    // Ruby — `call` filtered to require/require_relative below.
    "call",
    // PHP
    "namespace_use_declaration",
];

pub(super) fn run(args: &[VmValue]) -> Result<VmValue, HostlibError> {
    let raw = dict_arg(BUILTIN, args)?;
    let dict = raw.as_ref();

    let source_in = optional_string(BUILTIN, dict, "source")?;
    let path_in = optional_string(BUILTIN, dict, "path")?;
    let language_in = optional_string(BUILTIN, dict, "language")?;

    if source_in.is_none() && path_in.is_none() {
        return Err(HostlibError::MissingParameter {
            builtin: BUILTIN,
            param: "source",
        });
    }

    // Soft-fail mode: when language detection fails (unsupported file
    // extension, no language hint), return `{supported: false}` rather
    // than erroring out so callers can fall back to another scanner.
    let language_opt = match language_in.as_deref() {
        Some(name) if !name.is_empty() => Language::from_name(name),
        _ => path_in
            .as_deref()
            .and_then(|p| Language::detect(std::path::Path::new(p), None)),
    };

    let Some(language) = language_opt else {
        return Ok(unsupported(path_in.as_deref()));
    };

    let source = match (&source_in, &path_in) {
        (Some(s), _) => s.clone(),
        (None, Some(p)) => read_source(p, 0)?,
        (None, None) => unreachable!("guarded above"),
    };

    let tree = parse_source(&source, language)?;
    let statements = collect_imports(&tree, &source, language);

    let lines: Vec<&str> = source.split('\n').collect();
    let statement_values: Vec<VmValue> = statements
        .iter()
        .map(|stmt| {
            let line = locate_first_line(stmt, &lines);
            let mut entry: BTreeMap<String, VmValue> = BTreeMap::new();
            entry.insert("text".into(), str_value(stmt));
            entry.insert("line".into(), VmValue::Int(line as i64));
            VmValue::Dict(Rc::new(entry))
        })
        .collect();

    Ok(build_dict([
        (
            "path",
            match path_in {
                Some(ref p) => str_value(p),
                None => VmValue::Nil,
            },
        ),
        ("language", str_value(language.name())),
        ("supported", VmValue::Bool(true)),
        ("statements", VmValue::List(Rc::new(statement_values))),
    ]))
}

fn unsupported(path: Option<&str>) -> VmValue {
    build_dict([
        (
            "path",
            match path {
                Some(p) => str_value(p),
                None => VmValue::Nil,
            },
        ),
        ("language", str_value("")),
        ("supported", VmValue::Bool(false)),
        ("statements", VmValue::List(Rc::new(Vec::new()))),
    ])
}

fn collect_imports(tree: &Tree, source: &str, language: Language) -> Vec<String> {
    let root = tree.root_node();
    let mut imports: Vec<String> = Vec::new();
    walk(root, source, language, &mut imports);

    if imports.is_empty() {
        if let Some(keywords) = fallback_keywords(language) {
            for child in children(root) {
                let text = node_text(child, source).trim().to_string();
                if keywords.iter().any(|kw| text.starts_with(*kw)) {
                    imports.push(text);
                }
            }
        }
    }

    imports
}

fn walk(node: Node<'_>, source: &str, language: Language, imports: &mut Vec<String>) {
    let kind = node.kind();
    if IMPORT_NODE_TYPES.contains(&kind) {
        let text = node_text(node, source).trim().to_string();
        if matches!(language, Language::Ruby) && kind == "call" {
            if text.starts_with("require") {
                imports.push(text);
            }
        } else if !text.is_empty() {
            imports.push(text);
        }
        return;
    }
    for child in children(node) {
        walk(child, source, language, imports);
    }
}

fn fallback_keywords(language: Language) -> Option<&'static [&'static str]> {
    Some(match language {
        Language::Go => &["import"],
        Language::Rust => &["use ", "extern crate"],
        Language::Python => &["import ", "from "],
        Language::C | Language::Cpp => &["#include"],
        Language::CSharp => &["using "],
        Language::Ruby => &["require"],
        Language::Php => &["use "],
        Language::Scala | Language::Java | Language::Kotlin | Language::Haskell => &["import "],
        Language::Zig => &["@import"],
        Language::Elixir => &["import ", "use ", "require ", "alias "],
        Language::Lua => &["require"],
        Language::R => &["library(", "require(", "source("],
        _ => return None,
    })
}

fn locate_first_line(statement: &str, lines: &[&str]) -> u32 {
    let first = statement.lines().next().unwrap_or(statement).trim();
    for (i, line) in lines.iter().enumerate() {
        if line.trim() == first {
            return (i + 1) as u32;
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::language::Language;

    fn run_for(source: &str, language: Language) -> Vec<(String, u32)> {
        let tree = parse_source(source, language).expect("parse");
        let statements = collect_imports(&tree, source, language);
        let lines: Vec<&str> = source.split('\n').collect();
        statements
            .into_iter()
            .map(|s| {
                let line = locate_first_line(&s, &lines);
                (s, line)
            })
            .collect()
    }

    #[test]
    fn typescript_imports() {
        let src = "import { foo } from 'bar';\nimport baz from \"./baz\";\nconst x = 1;";
        let stmts = run_for(src, Language::TypeScript);
        assert_eq!(stmts.len(), 2);
        assert_eq!(stmts[0].0, "import { foo } from 'bar';");
        assert_eq!(stmts[0].1, 1);
        assert_eq!(stmts[1].1, 2);
    }

    #[test]
    fn python_imports() {
        let src = "import os\nfrom typing import List\n\ndef f(): pass";
        let stmts = run_for(src, Language::Python);
        assert_eq!(stmts.len(), 2);
        assert_eq!(stmts[0].0, "import os");
        assert_eq!(stmts[1].0, "from typing import List");
    }

    #[test]
    fn rust_use_declarations() {
        let src = "use std::fs;\nuse crate::ast::Language;\nfn main() {}";
        let stmts = run_for(src, Language::Rust);
        assert_eq!(stmts.len(), 2);
        assert_eq!(stmts[0].0, "use std::fs;");
    }

    #[test]
    fn ruby_filters_call_nodes_to_require() {
        let src = "require 'json'\nrequire_relative 'helper'\nputs 'hi'";
        let stmts = run_for(src, Language::Ruby);
        let texts: Vec<_> = stmts.iter().map(|(s, _)| s.as_str()).collect();
        assert!(texts.iter().any(|t| t.starts_with("require 'json'")));
        assert!(texts.iter().any(|t| t.starts_with("require_relative")));
        assert!(!texts.iter().any(|t| t.contains("puts")));
    }
}
