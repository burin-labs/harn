//! Every hostlib surface that names a file's language must agree with the
//! `harn-languages` registry. The sweep covers every registered extension
//! and file name plus an unregistered control.

use harn_vm::value::VmDictExt as _;
use std::path::Path;

use harn_languages::Language;
use harn_vm::VmValue;

use crate::ast::Language as Grammar;
use crate::code_index::walker::indexable_language;
use crate::registry::{BuiltinRegistry, HostlibCapability};
use crate::scanner::extensions::{file_language, should_include};
use crate::tools::ToolsCapability;

fn registry_paths() -> Vec<(String, Language)> {
    let mut out = Vec::new();
    for language in Language::all() {
        for ext in language.extensions() {
            out.push((format!("src/sample.{ext}"), language));
            out.push((format!("src/SAMPLE.{}", ext.to_ascii_uppercase()), language));
        }
        for file_name in language.spec().file_names {
            out.push((format!("pkg/{file_name}"), language));
        }
    }
    out
}

fn outline(dir: &Path, rel: &str) -> (Option<String>, String, Option<String>) {
    let mut registry = BuiltinRegistry::new();
    ToolsCapability.register_builtins(&mut registry);
    let path = dir.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "").unwrap();
    let mut args = harn_vm::value::DictMap::new();
    args.put_str("path", path.to_string_lossy().as_ref());
    let entry = registry.find("hostlib_tools_get_file_outline").unwrap();
    let result = (entry.handler)(&[VmValue::dict(args)]).unwrap();
    let VmValue::Dict(dict) = result else {
        panic!("outline returned a non-dict");
    };
    let text = |key: &str| match dict.get(key) {
        Some(VmValue::String(s)) => Some(s.to_string()),
        Some(VmValue::Nil) | None => None,
        Some(other) => panic!("{key} is {other:?}"),
    };
    (text("language"), text("status").unwrap(), text("reason"))
}

#[test]
fn every_surface_reports_the_registry_language() {
    let dir = tempfile::tempdir().unwrap();
    for (rel, language) in registry_paths() {
        let path = Path::new(&rel);
        assert_eq!(file_language(&rel), Some(language), "scanner: {rel}");
        assert!(should_include(&rel), "scanner skips {rel}");
        assert_eq!(
            indexable_language(path),
            Some(language),
            "code index: {rel}"
        );
        let grammar = Grammar::detect(path, None);
        assert_eq!(grammar, Grammar::from_registry(language), "ast: {rel}");
        assert_eq!(grammar.map(Grammar::name), grammar.map(|_| language.name()));

        let (name, status, reason) = outline(dir.path(), &rel);
        assert_eq!(name.as_deref(), Some(language.name()), "outline: {rel}");
        let expected = match grammar {
            None => Some("no_grammar"),
            Some(g) if !g.supports_symbol_extraction() => Some("no_symbol_projection"),
            Some(_) => None,
        };
        assert_eq!(reason.as_deref(), expected, "outline reason: {rel}");
        assert_eq!(
            status,
            if expected.is_some() {
                "unsupported"
            } else {
                "ok"
            }
        );
    }
}

#[test]
fn an_unregistered_path_is_unknown_everywhere() {
    let dir = tempfile::tempdir().unwrap();
    for rel in ["src/notes.xyz", "README", "src/archive.tar"] {
        assert_eq!(file_language(rel), None, "scanner: {rel}");
        assert!(!should_include(rel));
        assert_eq!(
            indexable_language(Path::new(rel)),
            None,
            "code index: {rel}"
        );
        assert_eq!(Grammar::detect(Path::new(rel), None), None, "ast: {rel}");
        let (name, status, reason) = outline(dir.path(), rel);
        assert_eq!(name, None);
        assert_eq!(status, "unsupported");
        assert_eq!(reason.as_deref(), Some("unknown_language"));
    }
}
