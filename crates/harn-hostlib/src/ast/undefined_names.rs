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
//! Profiles ship for Python, JavaScript, TypeScript, Go, Ruby, and Rust.
//! Other languages return `supported = false` so callers can fall back to an
//! external linter. [`scan_names`] exposes a profile's raw binding and
//! reference sets for one subtree, which `code_index.extract_function` uses
//! to compute a region's inputs and outputs.
//!
//! Single-file scope is a deliberate restriction: cross-file resolution,
//! re-exports, dynamic attribute access, and `exec`/`eval`-style name
//! discovery are out of scope.

use std::collections::HashSet;

use harn_vm::VmValue;
use tree_sitter::{Node, Tree};

use crate::error::HostlibError;
use crate::tools::args::{build_dict, str_value};

use super::language::Language;
use super::name_resolution;
use super::parse::{parse_source, ContentOrPath};
use super::types::UndefinedName;

const BUILTIN: &str = "hostlib_ast_undefined_names";

pub(super) fn run(args: &[VmValue]) -> Result<VmValue, HostlibError> {
    let request = ContentOrPath::from_args(BUILTIN, args)?;
    let language = request.language;
    let path_str = request.path_str();

    if !is_supported(language) {
        return Ok(unanalysed_response(path_str, language, false));
    }

    let source = request.load_source()?;

    let tree = match parse_source(&source, language) {
        Ok(tree) => tree,
        Err(_) => {
            return Ok(unanalysed_response(path_str, language, true));
        }
    };

    let diagnostics = diagnose(&tree, &source, language);
    let dlist: Vec<VmValue> = diagnostics.iter().map(UndefinedName::to_vm_value).collect();
    let resolution = name_resolution::assess(&tree, &source, language);

    Ok(build_dict([
        ("path", str_value(path_str)),
        ("language", str_value(language.name())),
        ("supported", VmValue::Bool(true)),
        ("resolution", resolution.to_vm_value()),
        ("diagnostics", VmValue::list(dlist)),
    ]))
}

/// No name analysis ran: either the language has no profile
/// (`supported = false`) or the file parsed badly enough that analysis was
/// skipped. An empty diagnostic list here means "nothing was checked", never
/// "nothing is wrong", so the resolution reading must say so.
fn unanalysed_response(path: &str, language: Language, supported: bool) -> VmValue {
    build_dict([
        ("path", str_value(path)),
        ("language", str_value(language.name())),
        ("supported", VmValue::Bool(supported)),
        (
            "resolution",
            name_resolution::Resolution::unanalysed(language).to_vm_value(),
        ),
        ("diagnostics", VmValue::list(Vec::new())),
    ])
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
            | Language::Rust
    )
}

/// Every name a subtree binds and every identifier reference it makes,
/// before builtins or file-level definitions are subtracted.
pub(crate) struct NameScan {
    pub defined: HashSet<String>,
    pub references: Vec<UndefinedName>,
}

/// Run `language`'s profile over `node`. `None` when no profile ships.
pub(crate) fn scan_names(node: Node<'_>, source: &str, language: Language) -> Option<NameScan> {
    let mut defined: HashSet<String> = HashSet::new();
    let mut references: Vec<UndefinedName> = Vec::new();
    match language {
        Language::Python => python::collect(node, source, &mut defined, &mut references),
        Language::JavaScript | Language::Jsx => {
            javascript::collect(node, source, &mut defined, &mut references, false);
        }
        Language::TypeScript | Language::Tsx => {
            javascript::collect(node, source, &mut defined, &mut references, true);
        }
        Language::Go => go::collect(node, source, &mut defined, &mut references),
        Language::Ruby => ruby::collect(node, source, &mut defined, &mut references),
        Language::Rust => rust::collect(node, source, &mut defined, &mut references),
        _ => return None,
    }
    Some(NameScan {
        defined,
        references,
    })
}

/// Run the appropriate per-language profile against `tree` / `source`
/// and return the deduplicated undefined-name list.
fn diagnose(tree: &Tree, source: &str, language: Language) -> Vec<UndefinedName> {
    let Some(NameScan {
        defined,
        references,
    }) = scan_names(tree.root_node(), source, language)
    else {
        return Vec::new();
    };

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
        Language::Rust => &rust::BUILTINS,
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
pub(super) fn field_name<'tree>(node: Node<'tree>) -> Option<&'tree str> {
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
mod rust;

pub(super) use rust::bind_use_tree as bind_rust_use_tree;

#[cfg(test)]
mod tests {
    use super::*;
    use harn_vm::value::VmDictExt;

    fn run_with(content: &str, language: &str) -> VmValue {
        let mut dict: harn_vm::value::DictMap = Default::default();
        dict.put_str("content", content);
        dict.put_str("language", language);
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
    fn rust_flags_only_unbound_value_identifiers() {
        let src = "use std::collections::HashMap;\nuse crate::a::{b as c, d};\n\
                   struct Meters(u32);\n\
                   fn total(items: &[u32], Meters(base): Meters) -> u32 {\n\
                       let mut sum = base;\n\
                       for (i, x) in items.iter().enumerate() { sum += x * i as u32; }\n\
                       let f = |y: u32| y + offset;\n\
                       if let Some(z) = items.first() { sum += z; }\n\
                       match sum { 0 => helper(c, d), n => n.max(1) };\n\
                       let _m: HashMap<u32, u32> = HashMap::new();\n\
                       println!(\"{}\", f(sum) + missing);\n\
                       Meters(sum).0 + total(items, Meters(0))\n\
                   }\n";
        let result = run_with(src, "rust");
        assert!(supported(&result));
        assert_eq!(names(&result), vec!["offset", "helper", "missing"]);
    }

    #[test]
    fn deduplicates_repeated_references() {
        let src = "missing()\nmissing()\nmissing()\n";
        let result = run_with(src, "py");
        let n = names(&result);
        assert_eq!(n, vec!["missing".to_string()]);
    }
}
