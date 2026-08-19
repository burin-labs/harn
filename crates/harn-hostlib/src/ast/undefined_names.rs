//! `ast.undefined_names` — language-aware undefined-identifier detection.
//!
//! Contract:
//!
//! - Walk the tree-sitter parse, collect every identifier reference and
//!   every name *defined* in this file (imports, parameters, locals,
//!   class names, etc.).
//! - Subtract definitions and a curated language-builtins stop-list from
//!   the references to produce the "undefined name" set.
//! - Deduplicate by name on first occurrence so callers see one
//!   diagnostic per missing import / typo, not one per usage.
//!
//! Profiles ship for Python, JavaScript, TypeScript, Go, and Ruby. Other
//! languages return `supported = false` so callers can fall back to an
//! external linter.
//!
//! Single-file scope is a deliberate restriction: cross-file resolution,
//! re-exports, dynamic attribute access, and `exec`/`eval`-style name
//! discovery are out of scope.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use harn_vm::VmValue;
use tree_sitter::{Node, Tree};

use crate::error::HostlibError;
use crate::tools::args::{build_dict, dict_arg, optional_int, optional_string, str_value};

use super::language::Language;
use super::name_resolution;
use super::parse::{parse_source, read_source};
use super::types::UndefinedName;

const BUILTIN: &str = "hostlib_ast_undefined_names";

pub(super) fn run(args: &[VmValue]) -> Result<VmValue, HostlibError> {
    let raw = dict_arg(BUILTIN, args)?;
    let dict = raw.as_ref();

    let content = optional_string(BUILTIN, dict, "content")?;
    let path_str = optional_string(BUILTIN, dict, "path")?;
    let language_hint = optional_string(BUILTIN, dict, "language")?;
    let max_bytes = optional_int(BUILTIN, dict, "max_bytes", 0)?;
    if max_bytes < 0 {
        return Err(HostlibError::InvalidParameter {
            builtin: BUILTIN,
            param: "max_bytes",
            message: "must be >= 0".into(),
        });
    }

    if content.is_none() && path_str.is_none() {
        return Err(HostlibError::MissingParameter {
            builtin: BUILTIN,
            param: "content_or_path",
        });
    }

    let language = resolve_language(path_str.as_deref(), language_hint.as_deref())?;

    if !is_supported(language) {
        return Ok(unsupported_response(path_str.as_deref(), language));
    }

    let source = match (&content, &path_str) {
        (Some(text), _) => clip(text, max_bytes as usize),
        (None, Some(path)) => read_source(path, max_bytes as usize)?,
        _ => unreachable!("guarded above"),
    };

    let tree = match parse_source(&source, language) {
        Ok(tree) => tree,
        Err(_) => {
            return Ok(empty_response(path_str.as_deref(), language));
        }
    };

    let diagnostics = diagnose(&tree, &source, language);
    let dlist: Vec<VmValue> = diagnostics.iter().map(UndefinedName::to_vm_value).collect();
    let resolution = name_resolution::assess(&tree, &source, language);

    Ok(build_dict([
        ("path", str_value(path_str.as_deref().unwrap_or(""))),
        ("language", str_value(language.name())),
        ("supported", VmValue::Bool(true)),
        ("resolution", resolution.to_vm_value()),
        ("diagnostics", VmValue::List(Arc::new(dlist))),
    ]))
}

fn unsupported_response(path: Option<&str>, language: Language) -> VmValue {
    build_dict([
        ("path", str_value(path.unwrap_or(""))),
        ("language", str_value(language.name())),
        ("supported", VmValue::Bool(false)),
        (
            "resolution",
            name_resolution::Resolution::unanalysed(language).to_vm_value(),
        ),
        ("diagnostics", VmValue::List(Arc::new(Vec::new()))),
    ])
}

/// The file parsed badly enough that no name analysis ran. An empty diagnostic
/// list here means "nothing was checked", never "nothing is wrong", so the
/// resolution reading must say so.
fn empty_response(path: Option<&str>, language: Language) -> VmValue {
    build_dict([
        ("path", str_value(path.unwrap_or(""))),
        ("language", str_value(language.name())),
        ("supported", VmValue::Bool(true)),
        (
            "resolution",
            name_resolution::Resolution::unanalysed(language).to_vm_value(),
        ),
        ("diagnostics", VmValue::List(Arc::new(Vec::new()))),
    ])
}

fn resolve_language(
    path: Option<&str>,
    language_hint: Option<&str>,
) -> Result<Language, HostlibError> {
    if let Some(name) = language_hint.filter(|s| !s.is_empty()) {
        if let Some(lang) = Language::from_name(name) {
            return Ok(lang);
        }
        if let Some(lang) = Language::from_extension(name) {
            return Ok(lang);
        }
    }
    if let Some(p) = path.filter(|s| !s.is_empty()) {
        if let Some(lang) = Language::detect(&PathBuf::from(p), language_hint) {
            return Ok(lang);
        }
    }
    Err(HostlibError::InvalidParameter {
        builtin: BUILTIN,
        param: "language",
        message: format!(
            "could not infer a tree-sitter grammar (path = `{}`, language = `{}`)",
            path.unwrap_or(""),
            language_hint.unwrap_or("")
        ),
    })
}

fn clip(text: &str, max_bytes: usize) -> String {
    if max_bytes == 0 || text.len() <= max_bytes {
        return text.to_string();
    }
    let bytes = text.as_bytes();
    let mut end = max_bytes;
    while end > 0 && (bytes[end] & 0xC0) == 0x80 {
        end -= 1;
    }
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

/// Whether we ship a language profile for `language`.
pub(super) fn is_supported(language: Language) -> bool {
    matches!(
        language,
        Language::Python
            | Language::JavaScript
            | Language::Jsx
            | Language::TypeScript
            | Language::Tsx
            | Language::Go
            | Language::Ruby
    )
}

/// Run the appropriate per-language profile against `tree` / `source`
/// and return the deduplicated undefined-name list.
fn diagnose(tree: &Tree, source: &str, language: Language) -> Vec<UndefinedName> {
    let mut defined: HashSet<String> = HashSet::new();
    let mut references: Vec<UndefinedName> = Vec::new();
    let root = tree.root_node();

    match language {
        Language::Python => python::collect(root, source, &mut defined, &mut references),
        Language::JavaScript | Language::Jsx => {
            javascript::collect(root, source, &mut defined, &mut references, false);
        }
        Language::TypeScript | Language::Tsx => {
            javascript::collect(root, source, &mut defined, &mut references, true);
        }
        Language::Go => go::collect(root, source, &mut defined, &mut references),
        Language::Ruby => ruby::collect(root, source, &mut defined, &mut references),
        _ => return Vec::new(),
    }

    let builtins = builtins_for(language);
    let mut seen: HashSet<String> = HashSet::new();
    let mut out: Vec<UndefinedName> = Vec::new();
    for refr in references {
        if defined.contains(&refr.name) || builtins.contains(refr.name.as_str()) {
            continue;
        }
        if !seen.insert(refr.name.clone()) {
            continue;
        }
        out.push(refr);
    }
    out
}

fn builtins_for(language: Language) -> &'static HashSet<&'static str> {
    match language {
        Language::Python => &python::BUILTINS,
        Language::JavaScript | Language::Jsx => &javascript::JS_BUILTINS,
        Language::TypeScript | Language::Tsx => &javascript::TS_BUILTINS,
        Language::Go => &go::BUILTINS,
        Language::Ruby => &ruby::BUILTINS,
        _ => &EMPTY_BUILTINS,
    }
}

static EMPTY_BUILTINS: std::sync::LazyLock<HashSet<&'static str>> =
    std::sync::LazyLock::new(HashSet::new);

// ---------------------------------------------------------------------------
// Shared helpers

pub(super) fn node_text<'a>(node: Node<'_>, source: &'a str) -> &'a str {
    let bytes = source.as_bytes();
    let start = node.start_byte().min(bytes.len());
    let end = node.end_byte().min(bytes.len());
    std::str::from_utf8(&bytes[start..end]).unwrap_or("")
}

pub(super) fn position(node: Node<'_>) -> (u32, u32) {
    let p = node.start_position();
    (p.row as u32, p.column as u32)
}

/// Recursive depth-first walk over every child (named and anonymous).
/// Threads the tree's lifetime through `F` so closures can capture nodes
/// into outside containers (the `for<'r>` HRTB form would require any
/// lifetime, breaking that).
pub(super) fn walk<'tree, F>(node: Node<'tree>, visit: &mut F)
where
    F: FnMut(Node<'tree>),
{
    visit(node);
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk(child, visit);
    }
}

/// First identifier found in a depth-first walk of `node`.
pub(super) fn first_identifier<'tree>(node: Node<'tree>) -> Option<Node<'tree>> {
    if node.kind() == "identifier" {
        return Some(node);
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if let Some(found) = first_identifier(child) {
            return Some(found);
        }
    }
    None
}

/// Field name this node occupies in its parent, if any.
pub(super) fn field_name(node: Node<'_>) -> Option<&'static str> {
    let parent = node.parent()?;
    let mut cursor = parent.walk();
    for (idx, child) in parent.children(&mut cursor).enumerate() {
        if child.id() == node.id() {
            return parent.field_name_for_child(idx as u32);
        }
    }
    None
}

fn add_reference(refs: &mut Vec<UndefinedName>, node: Node<'_>, source: &str, kind: &'static str) {
    let (row, col) = position(node);
    refs.push(UndefinedName {
        name: node_text(node, source).to_string(),
        kind,
        row,
        column: col,
    });
}

// ---------------------------------------------------------------------------
// Profiles

mod go;
mod javascript;
mod python;
mod ruby;

#[cfg(test)]
mod tests {
    use super::*;

    fn run_with(content: &str, language: &str) -> VmValue {
        let mut dict: harn_vm::value::DictMap = Default::default();
        dict.insert(
            "content".into(),
            VmValue::String(arcstr::ArcStr::from(content)),
        );
        dict.insert(
            "language".into(),
            VmValue::String(arcstr::ArcStr::from(language)),
        );
        run(&[VmValue::dict(dict)]).expect("undefined_names run")
    }

    fn names(result: &VmValue) -> Vec<String> {
        let diagnostics = match result {
            VmValue::Dict(d) => match d.get("diagnostics") {
                Some(VmValue::List(l)) => l.clone(),
                _ => panic!("missing diagnostics"),
            },
            _ => panic!("expected dict"),
        };
        diagnostics
            .iter()
            .map(|d| match d {
                VmValue::Dict(dict) => match dict.get("name") {
                    Some(VmValue::String(s)) => s.to_string(),
                    _ => panic!("missing name"),
                },
                _ => panic!("expected dict"),
            })
            .collect()
    }

    fn supported(result: &VmValue) -> bool {
        match result {
            VmValue::Dict(d) => match d.get("supported") {
                Some(VmValue::Bool(b)) => *b,
                _ => panic!("missing supported"),
            },
            _ => panic!("expected dict"),
        }
    }

    #[test]
    fn python_flags_undefined_call() {
        let src = "def foo():\n    bar()\n";
        let result = run_with(src, "python");
        let n = names(&result);
        assert_eq!(n, vec!["bar".to_string()]);
    }

    #[test]
    fn python_imports_satisfy_references() {
        let src = "import os\nfrom collections import OrderedDict as OD\nos.path\nOD()\n";
        let result = run_with(src, "py");
        let n = names(&result);
        assert!(n.is_empty(), "expected no undefined, got {n:?}");
    }

    #[test]
    fn python_skips_attribute_rhs() {
        let src = "import os\nos.path.join('a', 'b')\n";
        let result = run_with(src, "py");
        let n = names(&result);
        assert!(n.is_empty(), "got {n:?}");
    }

    #[test]
    fn javascript_flags_typo() {
        let src = "import { foo } from './m';\nfoo(); baz();\n";
        let result = run_with(src, "js");
        let n = names(&result);
        assert_eq!(n, vec!["baz".to_string()]);
    }

    #[test]
    fn typescript_flags_unknown_type_reference() {
        let src = "function f(x: SomeType) { return x; }\n";
        let result = run_with(src, "ts");
        let n = names(&result);
        assert!(n.contains(&"SomeType".to_string()), "got {n:?}");
    }

    #[test]
    fn go_resolves_imports_and_decls() {
        let src = "package main\nimport \"fmt\"\nfunc main() { fmt.Println(\"hi\") }\n";
        let result = run_with(src, "go");
        let n = names(&result);
        assert!(n.is_empty(), "got {n:?}");
    }

    #[test]
    fn go_flags_unknown_call() {
        let src = "package main\nfunc main() { mystery() }\n";
        let result = run_with(src, "go");
        let n = names(&result);
        assert_eq!(n, vec!["mystery".to_string()]);
    }

    #[test]
    fn ruby_flags_unknown_call() {
        let src = "def greet(name)\n  hello(name)\nend\n";
        let result = run_with(src, "rb");
        let n = names(&result);
        assert_eq!(n, vec!["hello".to_string()]);
    }

    #[test]
    fn unsupported_language_returns_supported_false() {
        let src = "fn main() {}\n";
        let result = run_with(src, "rust");
        assert!(!supported(&result));
        let n = names(&result);
        assert!(n.is_empty());
    }

    #[test]
    fn missing_payload_is_rejected() {
        let dict: harn_vm::value::DictMap = harn_vm::value::DictMap::new();
        let err = run(&[VmValue::dict(dict)]).expect_err("must reject");
        match err {
            HostlibError::MissingParameter { builtin, .. } => assert_eq!(builtin, BUILTIN),
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn deduplicates_repeated_references() {
        let src = "missing()\nmissing()\nmissing()\n";
        let result = run_with(src, "py");
        let n = names(&result);
        assert_eq!(n, vec!["missing".to_string()]);
    }
}
