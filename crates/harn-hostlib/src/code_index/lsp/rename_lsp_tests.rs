//! Rename through real language servers.
//!
//! A machine without the server skips the positive cases unless
//! `HARN_LSP_FIXTURES_REQUIRED=1`, which CI's language-server job and the
//! fixture evidence runs set so a missing server fails instead of passing.

use std::fs;
use std::path::Path;

use harn_vm::VmValue;
use tempfile::tempdir;

use crate::code_index::state::IndexState;
use crate::code_index::CodeIndexCapability;
use crate::test_support::*;

fn write_tree(root: &Path, files: &[(&str, &str)]) {
    for (path, text) in files {
        let path = root.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }
}

fn index(root: &Path) -> CodeIndexCapability {
    let capability = CodeIndexCapability::new();
    let shared = capability.shared();
    let (state, _) = IndexState::build_from_root(root);
    *shared.lock().unwrap() = Some(state);
    capability
}

fn rename(
    capability: &CodeIndexCapability,
    path: &str,
    name: &str,
    new_name: &str,
    backend: &str,
) -> VmValue {
    let symbol = dict(&[
        ("name", vm_string(name)),
        ("path", vm_string(path)),
        ("kind", vm_string("Type")),
    ]);
    let request = dict(&[
        ("symbol_ref", symbol),
        ("new_name", vm_string(new_name)),
        ("scope", vm_string("workspace")),
        ("backend", vm_string(backend)),
    ]);
    crate::code_index::rename::run(&capability.shared(), &[request]).expect("rename runs")
}

fn required() -> bool {
    std::env::var("HARN_LSP_FIXTURES_REQUIRED").is_ok_and(|value| value == "1")
}

/// Rename through the server only, retrying while a cold server warms.
/// `None` means the server is not usable here and the case is skipped.
fn server_rename(
    capability: &CodeIndexCapability,
    path: &str,
    name: &str,
    new_name: &str,
) -> Option<VmValue> {
    let mut last = None;
    for _ in 0..8 {
        let result = rename(capability, path, name, new_name, "language_server");
        if s(field(&result, "result")) != "backend_unavailable" {
            return Some(result);
        }
        let details = s(field(&result, "details"));
        // Only a server that is still loading is worth another attempt.
        if !details.contains("still loading") {
            last = Some(details);
            break;
        }
        last = Some(details);
    }
    let details = last.unwrap_or_default();
    assert!(
        !required(),
        "language server required but unavailable: {details}"
    );
    eprintln!("SKIP: language server unavailable: {details}");
    None
}

const RUST_WORKSPACE: &[(&str, &str)] = &[
    (
        "Cargo.toml",
        "[workspace]\nmembers = [\"ledger_core\", \"ledger_app\"]\nresolver = \"2\"\n",
    ),
    (
        "ledger_core/Cargo.toml",
        "[package]\nname = \"ledger_core\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    ),
    (
        "ledger_core/src/lib.rs",
        "pub mod span;\npub mod util;\n\n#[macro_export]\nmacro_rules! make_span {\n    ($start:expr, $end:expr) => {\n        $crate::span::Span::new($start, $end)\n    };\n}\n",
    ),
    (
        "ledger_core/src/span.rs",
        "/// A half-open byte range; see [`Span`] and [`Span::new`].\n#[derive(Debug, Clone, Copy, PartialEq, Eq)]\npub struct Span {\n    pub start: usize,\n    pub end: usize,\n}\n\nimpl Span {\n    pub fn new(start: usize, end: usize) -> Self {\n        Span { start, end }\n    }\n}\n\npub fn describe(span: Span) -> String {\n    format!(\"{:?} is a {}\", span, \"Span\")\n}\n\n#[cfg(test)]\nmod tests {\n    use super::*;\n\n    #[test]\n    fn equal() {\n        assert_eq!(Span::new(1, 2), Span { start: 1, end: 2 });\n    }\n}\n",
    ),
    (
        "ledger_core/src/util.rs",
        "pub fn pair<Span: Clone>(value: Span) -> (Span, Span) {\n    (value.clone(), value)\n}\n\npub fn widen(span: crate::span::Span) -> crate::span::Span {\n    crate::span::Span::new(span.start, span.end + 1)\n}\n",
    ),
    (
        "ledger_app/Cargo.toml",
        "[package]\nname = \"ledger_app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\nledger_core = { path = \"../ledger_core\" }\n",
    ),
    (
        "ledger_app/src/main.rs",
        "use ledger_core::span::Span as Range;\n\nfn main() {\n    let first: Range = ledger_core::make_span!(0, 1);\n    let second = ledger_core::span::Span::new(2, 3);\n    let _ = (first, second);\n}\n",
    ),
];

#[test]
fn rust_analyzer_renames_through_derives_macros_and_crates() {
    let _env = crate::process::test_support::declare_inherited();
    let dir = tempdir().unwrap();
    write_tree(dir.path(), RUST_WORKSPACE);
    let capability = index(dir.path());
    let Some(result) = server_rename(&capability, "ledger_core/src/span.rs", "Span", "TextSpan")
    else {
        return;
    };
    assert_eq!(s(field(&result, "result")), "applied", "{result:?}");
    let backend = field(&result, "backend");
    assert_eq!(s(field(backend, "name")), "language_server");
    assert_eq!(s(field(backend, "server")), "rust-analyzer");

    let read = |path: &str| fs::read_to_string(dir.path().join(path)).unwrap();
    let span = read("ledger_core/src/span.rs");
    // rust-analyzer 1.95 leaves intra-doc links unrenamed; rustdoc reports
    // them as broken links rather than the build failing.
    assert!(
        span.contains("see [`Span`] and [`Span::new`]"),
        "doc link: {span}"
    );
    assert!(span.contains("pub struct TextSpan {"), "{span}");
    assert!(span.contains("impl TextSpan {"), "{span}");
    assert!(span.contains("        TextSpan { start, end }"), "{span}");
    assert!(span.contains("pub fn describe(span: TextSpan)"), "{span}");
    assert!(
        span.contains("\"Span\")"),
        "string literal must stay: {span}"
    );
    assert!(
        span.contains("assert_eq!(TextSpan::new(1, 2), TextSpan { start: 1, end: 2 });"),
        "macro call arguments: {span}"
    );
    let lib = read("ledger_core/src/lib.rs");
    assert!(
        lib.contains("$crate::span::TextSpan::new($start, $end)"),
        "macro_rules body: {lib}"
    );
    let util = read("ledger_core/src/util.rs");
    assert!(
        util.contains("pub fn pair<Span: Clone>(value: Span) -> (Span, Span) {"),
        "generic parameter must stay: {util}"
    );
    assert!(
        util.contains("pub fn widen(span: crate::span::TextSpan) -> crate::span::TextSpan {"),
        "{util}"
    );
    let check = std::process::Command::new("cargo")
        .args([
            "check",
            "--offline",
            "--quiet",
            "--workspace",
            "--all-targets",
        ])
        .env("CARGO_TARGET_DIR", dir.path().join("target"))
        .current_dir(dir.path())
        .output()
        .expect("cargo runs");
    assert!(
        check.status.success(),
        "renamed workspace must build: {}",
        String::from_utf8_lossy(&check.stderr)
    );
    assert_eq!(
        read("ledger_app/src/main.rs"),
        "use ledger_core::span::TextSpan as Range;\n\nfn main() {\n    let first: Range = ledger_core::make_span!(0, 1);\n    let second = ledger_core::span::TextSpan::new(2, 3);\n    let _ = (first, second);\n}\n"
    );
}

const SMALL_CRATE: &[(&str, &str)] = &[
    (
        "Cargo.toml",
        "[package]\nname = \"small\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    ),
    ("src/lib.rs", "pub mod reading;\npub mod use_it;\n"),
    ("src/reading.rs", "pub struct Reading;\n"),
    (
        "src/use_it.rs",
        "use crate::reading::Reading;\n\npub fn keep(value: Reading) -> Reading {\n    value\n}\n",
    ),
];

#[test]
fn a_missing_server_falls_back_and_says_so() {
    let _env = crate::process::test_support::declare_inherited();
    super::test_override::set("rust-analyzer", "/nonexistent/harn-test/rust-analyzer");
    let dir = tempdir().unwrap();
    write_tree(dir.path(), SMALL_CRATE);
    let capability = index(dir.path());

    let strict = rename(
        &capability,
        "src/reading.rs",
        "Reading",
        "Measurement",
        "language_server",
    );
    assert_eq!(
        s(field(&strict, "result")),
        "backend_unavailable",
        "{strict:?}"
    );
    assert!(
        s(field(&strict, "details")).contains("could not start"),
        "{strict:?}"
    );
    assert_eq!(
        fs::read_to_string(dir.path().join("src/reading.rs")).unwrap(),
        "pub struct Reading;\n"
    );

    let auto = rename(
        &capability,
        "src/reading.rs",
        "Reading",
        "Measurement",
        "auto",
    );
    assert_eq!(s(field(&auto, "result")), "applied", "{auto:?}");
    let backend = field(&auto, "backend");
    assert_eq!(s(field(backend, "name")), "syntax_tree");
    assert!(
        s(field(backend, "fallback_reason")).contains("could not start"),
        "{backend:?}"
    );
    assert_eq!(
        fs::read_to_string(dir.path().join("src/use_it.rs")).unwrap(),
        "use crate::reading::Measurement;\n\npub fn keep(value: Measurement) -> Measurement {\n    value\n}\n"
    );
}

#[test]
fn syntax_tree_choice_never_starts_a_server() {
    super::test_override::set("rust-analyzer", "/nonexistent/harn-test/rust-analyzer");
    let dir = tempdir().unwrap();
    write_tree(dir.path(), SMALL_CRATE);
    let capability = index(dir.path());
    let result = rename(
        &capability,
        "src/reading.rs",
        "Reading",
        "Measurement",
        "syntax_tree",
    );
    assert_eq!(s(field(&result, "result")), "applied", "{result:?}");
    let backend = field(&result, "backend");
    assert_eq!(s(field(backend, "name")), "syntax_tree");
    let VmValue::Dict(entries) = backend else {
        panic!("backend is a dict: {backend:?}");
    };
    assert!(!entries.contains_key("fallback_reason"), "{backend:?}");
}

#[test]
fn an_unresolvable_macro_body_mention_declines_without_writes() {
    let _env = crate::process::test_support::declare_inherited();
    let dir = tempdir().unwrap();
    let mut files = SMALL_CRATE.to_vec();
    files[1] = (
        "src/lib.rs",
        "pub mod reading;\npub mod use_it;\n\nmacro_rules! fresh {\n    () => {\n        Reading\n    };\n}\n",
    );
    write_tree(dir.path(), &files);
    let capability = index(dir.path());
    let Some(result) = server_rename(&capability, "src/reading.rs", "Reading", "Measurement")
    else {
        return;
    };
    assert_eq!(s(field(&result, "result")), "declined", "{result:?}");
    assert!(
        s(field(&result, "details")).contains("src/lib.rs:6"),
        "{result:?}"
    );
    assert_eq!(
        fs::read_to_string(dir.path().join("src/reading.rs")).unwrap(),
        "pub struct Reading;\n"
    );
}
