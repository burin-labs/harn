//! Rename through real language servers.
//!
//! A machine without the server skips the positive cases unless
//! `HARN_LSP_FIXTURES_REQUIRED=1`, which CI's test lanes and the fixture
//! evidence runs set so a missing server fails instead of passing.
//!
//! These tests wait on real server processes, so their bounds are real time
//! (the startup and request budgets); no virtual clock can drive a child
//! process. Each wait ends on the server's own readiness signal.

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
    // rust-analyzer leaves intra-doc links alone; the Rust residue pass
    // renames these because they are in the declaration's own file.
    assert!(
        span.contains("see [`TextSpan`] and [`TextSpan::new`]"),
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
fn an_unresolvable_macro_body_mention_falls_back_without_writes() {
    let _env = crate::process::test_support::declare_inherited();
    let dir = tempdir().unwrap();
    let mut files = SMALL_CRATE.to_vec();
    files[1] = (
        "src/lib.rs",
        "pub mod reading;\npub mod use_it;\n\nmacro_rules! fresh {\n    () => {\n        Reading\n    };\n}\n",
    );
    write_tree(dir.path(), &files);
    let capability = index(dir.path());
    let Some(strict) = strict_rename(&capability, "src/reading.rs", "Reading", "Measurement")
    else {
        return;
    };
    assert_eq!(
        s(field(&strict, "result")),
        "backend_unavailable",
        "{strict:?}"
    );
    assert!(
        s(field(&strict, "details")).contains("src/lib.rs:6"),
        "{strict:?}"
    );
    assert_eq!(
        fs::read_to_string(dir.path().join("src/reading.rs")).unwrap(),
        "pub struct Reading;\n"
    );
}

/// Rename with `backend: "language_server"` after confirming the server
/// runs here on a fixture it can answer; `None` skips the case.
fn strict_rename(
    capability: &CodeIndexCapability,
    path: &str,
    name: &str,
    new_name: &str,
) -> Option<VmValue> {
    rust_analyzer_runs().then(|| rename(capability, path, name, new_name, "language_server"))
}

/// Whether rust-analyzer answers here, probed on a throwaway crate.
fn rust_analyzer_runs() -> bool {
    let probe = tempdir().unwrap();
    write_tree(probe.path(), SMALL_CRATE);
    server_rename(&index(probe.path()), "src/reading.rs", "Reading", "Probe").is_some()
}

#[test]
fn an_unloaded_module_with_a_seed_path_falls_back() {
    let _env = crate::process::test_support::declare_inherited();
    let dir = tempdir().unwrap();
    let mut files = SMALL_CRATE.to_vec();
    // `cfg(any())` is never true, so rust-analyzer never loads the module.
    files[1] = (
        "src/lib.rs",
        "pub mod reading;\npub mod use_it;\n#[cfg(any())]\npub mod hidden;\n",
    );
    files.push((
        "src/hidden.rs",
        "use crate::reading::Reading;\n\npub fn hide(value: Reading) -> Reading {\n    value\n}\n",
    ));
    write_tree(dir.path(), &files);
    let capability = index(dir.path());
    let Some(strict) = strict_rename(&capability, "src/reading.rs", "Reading", "Measurement")
    else {
        return;
    };
    assert_eq!(
        s(field(&strict, "result")),
        "backend_unavailable",
        "{strict:?}"
    );
    assert!(
        s(field(&strict, "details")).contains("src/hidden.rs:1"),
        "{strict:?}"
    );
    let auto = rename(
        &capability,
        "src/reading.rs",
        "Reading",
        "Measurement",
        "auto",
    );
    assert_eq!(s(field(&auto, "result")), "applied", "{auto:?}");
    assert_eq!(s(field(field(&auto, "backend"), "name")), "syntax_tree");
    assert_eq!(
        fs::read_to_string(dir.path().join("src/hidden.rs")).unwrap(),
        "use crate::reading::Measurement;\n\npub fn hide(value: Measurement) -> Measurement {\n    value\n}\n"
    );
}

#[test]
fn a_reexported_macro_body_path_is_not_left_stale() {
    let _env = crate::process::test_support::declare_inherited();
    let dir = tempdir().unwrap();
    let mut files = SMALL_CRATE.to_vec();
    files[1] = (
        "src/lib.rs",
        "pub mod reading;\npub mod use_it;\npub use reading::Reading;\n\n#[macro_export]\nmacro_rules! reading {\n    () => {\n        $crate::Reading\n    };\n}\n",
    );
    write_tree(dir.path(), &files);
    let capability = index(dir.path());
    let Some(strict) = strict_rename(&capability, "src/reading.rs", "Reading", "Measurement")
    else {
        return;
    };
    assert_eq!(
        s(field(&strict, "result")),
        "backend_unavailable",
        "{strict:?}"
    );
    assert!(
        s(field(&strict, "details")).contains("src/lib.rs:8"),
        "{strict:?}"
    );
    assert_eq!(
        fs::read_to_string(dir.path().join("src/reading.rs")).unwrap(),
        "pub struct Reading;\n"
    );
}

#[test]
fn unanalyzed_relative_imports_and_macro_arguments_fall_back() {
    let _env = crate::process::test_support::declare_inherited();
    let cases: [(&str, &str, &str, &str); 3] = [
        (
            "a raw identifier in a module rust-analyzer never loads",
            "pub mod reading;\npub mod use_it;\n#[cfg(any())]\npub mod hidden;\n",
            "src/hidden.rs",
            "pub fn hide(value: crate::reading::r#Reading) -> u8 {\n    let _ = value;\n    0\n}\n",
        ),
        (
            "a relative import in a module rust-analyzer never loads",
            "pub mod reading;\npub mod use_it;\n#[cfg(any())]\npub mod hidden;\n",
            "src/hidden.rs",
            "use super::reading::Reading;\n\npub fn hide(value: Reading) -> Reading {\n    value\n}\n",
        ),
        (
            "an argument to a macro rust-analyzer cannot expand",
            "pub mod reading;\npub mod use_it;\npub mod queries;\n",
            "src/queries.rs",
            "use crate::reading::Reading;\n\npub fn load() {\n    external_query!(Reading, \"select 1\");\n}\n",
        ),
    ];
    for (label, lib, extra_path, extra) in cases {
        let dir = tempdir().unwrap();
        let mut files = SMALL_CRATE.to_vec();
        files[1] = ("src/lib.rs", lib);
        files.push((extra_path, extra));
        write_tree(dir.path(), &files);
        let capability = index(dir.path());
        let Some(strict) = strict_rename(&capability, "src/reading.rs", "Reading", "Measurement")
        else {
            return;
        };
        assert_eq!(
            s(field(&strict, "result")),
            "backend_unavailable",
            "{label}: {strict:?}"
        );
        assert!(
            s(field(&strict, "details")).contains(extra_path),
            "{label}: {strict:?}"
        );
        assert_eq!(
            fs::read_to_string(dir.path().join(extra_path)).unwrap(),
            extra,
            "{label}"
        );
    }
}

#[test]
fn a_narrow_scope_with_outside_references_refuses() {
    let _env = crate::process::test_support::declare_inherited();
    let dir = tempdir().unwrap();
    write_tree(dir.path(), SMALL_CRATE);
    let capability = index(dir.path());
    if !rust_analyzer_runs() {
        return;
    }
    let request = dict(&[
        (
            "symbol_ref",
            dict(&[
                ("name", vm_string("Reading")),
                ("path", vm_string("src/reading.rs")),
                ("kind", vm_string("Type")),
            ]),
        ),
        ("new_name", vm_string("Measurement")),
        ("scope", vm_string("file")),
        ("backend", vm_string("auto")),
    ]);
    let result = crate::code_index::rename::run(&capability.shared(), &[request]).unwrap();
    assert_eq!(s(field(&result, "result")), "scope_exceeded", "{result:?}");
    assert_eq!(
        fs::read_to_string(dir.path().join("src/reading.rs")).unwrap(),
        "pub struct Reading;\n"
    );
}

#[test]
fn a_method_rename_leaves_a_free_function_path_in_a_macro_alone() {
    let _env = crate::process::test_support::declare_inherited();
    let dir = tempdir().unwrap();
    let lib = "pub mod parser;\n\n#[macro_export]\nmacro_rules! run {\n    () => {\n        $crate::parser::parse()\n    };\n}\n";
    let parser = "pub fn parse() -> u8 {\n    1\n}\n\npub struct Parser;\n\nimpl Parser {\n    pub fn parse(&self) -> u8 {\n        2\n    }\n}\n";
    write_tree(
        dir.path(),
        &[
            (
                "Cargo.toml",
                "[package]\nname = \"small\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
            ),
            ("src/lib.rs", lib),
            ("src/parser.rs", parser),
        ],
    );
    let capability = index(dir.path());
    if !rust_analyzer_runs() {
        return;
    }
    let request = dict(&[
        (
            "symbol_ref",
            dict(&[
                ("name", vm_string("parse")),
                ("path", vm_string("src/parser.rs")),
                ("line", VmValue::Int(8)),
                ("kind", vm_string("Function")),
            ]),
        ),
        ("new_name", vm_string("parse_all")),
        ("scope", vm_string("workspace")),
        ("backend", vm_string("language_server")),
    ]);
    let result = crate::code_index::rename::run(&capability.shared(), &[request]).unwrap();
    assert_ne!(s(field(&result, "result")), "applied", "{result:?}");
    assert_eq!(
        fs::read_to_string(dir.path().join("src/lib.rs")).unwrap(),
        lib
    );
}

#[test]
fn doc_links_the_server_resolves_to_the_declaration_are_renamed() {
    let _env = crate::process::test_support::declare_inherited();
    let dir = tempdir().unwrap();
    let mut files = SMALL_CRATE.to_vec();
    files[3] = (
        "src/use_it.rs",
        "use crate::reading::Reading;\n\n/// Keeps a [`Reading`] as it is.\npub fn keep(value: Reading) -> Reading {\n    value\n}\n",
    );
    write_tree(dir.path(), &files);
    let capability = index(dir.path());
    let Some(result) = strict_rename(&capability, "src/reading.rs", "Reading", "Measurement")
    else {
        return;
    };
    assert_eq!(s(field(&result, "result")), "applied", "{result:?}");
    assert_eq!(
        fs::read_to_string(dir.path().join("src/use_it.rs")).unwrap(),
        "use crate::reading::Measurement;\n\n/// Keeps a [`Measurement`] as it is.\npub fn keep(value: Measurement) -> Measurement {\n    value\n}\n"
    );
}

#[test]
fn only_a_target_starting_at_the_name_is_the_declaration() {
    use serde_json::json;
    let dir = tempdir().unwrap();
    let seed_file = dir.path().join("parse.rs");
    let other_file = dir.path().join("other.rs");
    fs::write(&seed_file, "pub fn parse() {}\n").unwrap();
    fs::write(&other_file, "pub fn other() {}\n").unwrap();
    let seed_file = seed_file.canonicalize().unwrap();
    let seed = super::edits::Position {
        line: 0,
        character: 7,
    };
    let location = |path: &Path, start: (u32, u32), end: (u32, u32)| {
        json!({"uri": super::edits::path_to_uri(path), "range": {
            "start": {"line": start.0, "character": start.1},
            "end": {"line": end.0, "character": end.1},
        }})
    };
    let classify = |answer| super::classify(&answer, &seed_file, seed).0;
    assert_eq!(
        classify(location(&seed_file, (0, 7), (0, 12))),
        super::Resolution::Seed
    );
    // A module answers with its whole file, which spans the name.
    assert_eq!(
        classify(location(&seed_file, (0, 0), (1, 0))),
        super::Resolution::Encloses
    );
    assert!(classify(location(&other_file, (0, 7), (0, 12))).is_elsewhere());
    assert_eq!(classify(json!([])), super::Resolution::Unknown);
    assert_eq!(
        classify(serde_json::Value::Null),
        super::Resolution::Unknown
    );
}

#[test]
fn doc_links_naming_other_items_are_not_renamed() {
    let _env = crate::process::test_support::declare_inherited();
    if !rust_analyzer_runs() {
        return;
    }
    // A member link on the declaration, a same-named type in an inline
    // module, and a reference-style link elsewhere.
    let reading = "/// See [`Reading::new`].\npub struct Reading;\n\nimpl Reading {\n    pub fn new() -> Self {\n        Reading\n    }\n}\n\npub mod inner {\n    pub struct Reading;\n\n    impl Reading {\n        pub fn fresh() -> u8 {\n            0\n        }\n    }\n\n    /// Built by [`Reading::fresh`].\n    pub fn made() -> u8 {\n        Reading::fresh()\n    }\n}\n";
    let use_it = "use crate::reading::Reading;\n\n/// Keeps a [Reading].\n///\n/// [Reading]: crate::reading::Reading\npub fn keep(value: Reading) -> Reading {\n    value\n}\n";
    let rename_at = |line: i64| {
        let dir = tempdir().unwrap();
        let mut files = SMALL_CRATE.to_vec();
        files[2] = ("src/reading.rs", reading);
        files[3] = ("src/use_it.rs", use_it);
        write_tree(dir.path(), &files);
        let capability = index(dir.path());
        let request = dict(&[
            (
                "symbol_ref",
                dict(&[
                    ("name", vm_string("Reading")),
                    ("path", vm_string("src/reading.rs")),
                    ("line", VmValue::Int(line)),
                    ("kind", vm_string("Type")),
                ]),
            ),
            ("new_name", vm_string("Measurement")),
            ("scope", vm_string("workspace")),
            ("backend", vm_string("language_server")),
        ]);
        let result = crate::code_index::rename::run(&capability.shared(), &[request]).unwrap();
        (dir, result)
    };

    let (dir, result) = rename_at(2);
    assert_eq!(s(field(&result, "result")), "applied", "{result:?}");
    let renamed = fs::read_to_string(dir.path().join("src/reading.rs")).unwrap();
    assert!(
        renamed.contains("/// See [`Measurement::new`]."),
        "{renamed}"
    );
    assert!(renamed.contains("pub struct Measurement;"), "{renamed}");
    assert!(
        renamed.contains("/// Built by [`Reading::fresh`]."),
        "inner link must stay: {renamed}"
    );
    let kept = fs::read_to_string(dir.path().join("src/use_it.rs")).unwrap();
    assert!(
        kept.contains("/// Keeps a [Reading].\n///\n/// [Reading]: crate::reading::Reading\n"),
        "reference-style link stays whole: {kept}"
    );
    let warnings = format!("{:?}", field(&result, "warnings"));
    for site in ["src/reading.rs:19", "src/use_it.rs:3", "src/use_it.rs:5"] {
        assert!(warnings.contains(site), "{site} unreported: {warnings}");
    }

    // Renaming the inner type must not touch the outer type's member link.
    let (dir, result) = rename_at(11);
    assert_eq!(s(field(&result, "result")), "applied", "{result:?}");
    let renamed = fs::read_to_string(dir.path().join("src/reading.rs")).unwrap();
    assert!(
        renamed.contains("/// See [`Reading::new`]."),
        "outer link must stay: {renamed}"
    );
    assert!(
        renamed.contains("/// Built by [`Measurement::fresh`]."),
        "{renamed}"
    );
}

#[test]
fn a_crate_path_into_a_same_named_module_falls_back() {
    let _env = crate::process::test_support::declare_inherited();
    if !rust_analyzer_runs() {
        return;
    }
    let lib = "pub mod span;\n\n#[macro_export]\nmacro_rules! options {\n    () => {\n        $crate::span::parse::Options\n    };\n}\n";
    let span = "pub fn parse() -> u8 {\n    0\n}\n\npub mod parse {\n    pub struct Options;\n}\n";
    let dir = tempdir().unwrap();
    write_tree(
        dir.path(),
        &[
            (
                "Cargo.toml",
                "[package]\nname = \"small\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
            ),
            ("src/lib.rs", lib),
            ("src/span.rs", span),
        ],
    );
    let capability = index(dir.path());
    let request = dict(&[
        (
            "symbol_ref",
            dict(&[
                ("name", vm_string("parse")),
                ("path", vm_string("src/span.rs")),
                ("line", VmValue::Int(1)),
                ("kind", vm_string("Function")),
            ]),
        ),
        ("new_name", vm_string("parse_all")),
        ("scope", vm_string("workspace")),
        ("backend", vm_string("language_server")),
    ]);
    let result = crate::code_index::rename::run(&capability.shared(), &[request]).unwrap();
    assert_ne!(s(field(&result, "result")), "applied", "{result:?}");
    assert_eq!(
        fs::read_to_string(dir.path().join("src/lib.rs")).unwrap(),
        lib
    );
}

#[test]
fn a_member_link_is_left_when_a_local_type_shares_the_name() {
    let _env = crate::process::test_support::declare_inherited();
    if !rust_analyzer_runs() {
        return;
    }
    let reading = "pub struct Reading;\n\nimpl Reading {\n    pub fn new() -> Self {\n        Reading\n    }\n}\n\npub fn local() -> u8 {\n    struct Reading;\n\n    impl Reading {\n        fn new() -> u8 {\n            0\n        }\n    }\n\n    /// Uses [`Reading::new`].\n    fn inner() -> u8 {\n        Reading::new()\n    }\n    inner()\n}\n";
    let dir = tempdir().unwrap();
    let mut files = SMALL_CRATE.to_vec();
    files[2] = ("src/reading.rs", reading);
    write_tree(dir.path(), &files);
    let capability = index(dir.path());
    let request = dict(&[
        (
            "symbol_ref",
            dict(&[
                ("name", vm_string("Reading")),
                ("path", vm_string("src/reading.rs")),
                ("line", VmValue::Int(1)),
                ("kind", vm_string("Type")),
            ]),
        ),
        ("new_name", vm_string("Measurement")),
        ("scope", vm_string("workspace")),
        ("backend", vm_string("language_server")),
    ]);
    let result = crate::code_index::rename::run(&capability.shared(), &[request]).unwrap();
    assert_eq!(s(field(&result, "result")), "applied", "{result:?}");
    let renamed = fs::read_to_string(dir.path().join("src/reading.rs")).unwrap();
    assert!(
        renamed.contains("/// Uses [`Reading::new`]."),
        "the local type's link must stay: {renamed}"
    );
}

const TS_PROJECT: &[(&str, &str)] = &[
    (
        "tsconfig.json",
        "{\"compilerOptions\": {\"strict\": true, \"target\": \"ES2020\", \"module\": \"ESNext\", \"moduleResolution\": \"Bundler\", \"noEmit\": true}, \"include\": [\"src\"]}\n",
    ),
    // Outside the tsconfig project; the server sees it only once opened.
    (
        "test/reading.test.ts",
        "import { Reading } from \"../src/reading\";\n\nexport const sample = new Reading(1);\n",
    ),
    (
        "src/reading.ts",
        "export class Reading {\n  constructor(public value: number) {}\n}\n\nexport function average(items: Reading[]): Reading {\n  const total = items.reduce((sum, item) => sum + item.value, 0);\n  return new Reading(total / items.length);\n}\n",
    ),
    (
        "src/util.ts",
        "export function pair<Reading>(value: Reading): [Reading, Reading] {\n  return [value, value];\n}\n",
    ),
    (
        "src/consumer.ts",
        "import { Reading, average } from \"./reading\";\nimport * as current from \"./reading\";\nexport { Reading as LegacyReading } from \"./reading\";\nexport { Reading } from \"./reading\";\n\nexport const labels = { Reading: \"sensor\", kind: \"Reading\" };\n\nexport function latest(all: Array<Reading>): current.Reading | undefined {\n  const first: Reading[] = all.slice(0, 1);\n  return first.length ? average(first) : undefined;\n}\n",
    ),
];

#[test]
fn typescript_server_renames_array_types_namespaces_and_reexports() {
    let _env = crate::process::test_support::declare_inherited();
    let dir = tempdir().unwrap();
    write_tree(dir.path(), TS_PROJECT);
    let capability = index(dir.path());
    let Some(result) = server_rename(&capability, "src/reading.ts", "Reading", "Measurement")
    else {
        return;
    };
    assert_eq!(s(field(&result, "result")), "applied", "{result:?}");
    let server = s(field(field(&result, "backend"), "server"));
    // TypeScript 6 and older run behind typescript-language-server; 7 is native.
    assert!(
        server == "typescript-language-server" || server == "tsc-lsp",
        "{result:?}"
    );
    eprintln!("typescript rename served by {server}");
    let read = |path: &str| fs::read_to_string(dir.path().join(path)).unwrap();
    assert_eq!(
        read("src/reading.ts"),
        "export class Measurement {\n  constructor(public value: number) {}\n}\n\nexport function average(items: Measurement[]): Measurement {\n  const total = items.reduce((sum, item) => sum + item.value, 0);\n  return new Measurement(total / items.length);\n}\n"
    );
    assert_eq!(
        read("src/util.ts"),
        TS_PROJECT[3].1,
        "generic parameter must stay"
    );
    assert_eq!(
        read("test/reading.test.ts"),
        "import { Measurement } from \"../src/reading\";\n\nexport const sample = new Measurement(1);\n",
        "a file outside the tsconfig project"
    );
    assert_eq!(
        read("src/consumer.ts"),
        "import { Measurement, average } from \"./reading\";\nimport * as current from \"./reading\";\nexport { Measurement as LegacyReading } from \"./reading\";\nexport { Measurement } from \"./reading\";\n\nexport const labels = { Reading: \"sensor\", kind: \"Reading\" };\n\nexport function latest(all: Array<Measurement>): current.Measurement | undefined {\n  const first: Measurement[] = all.slice(0, 1);\n  return first.length ? average(first) : undefined;\n}\n"
    );
}

const PY_PROJECT: &[(&str, &str)] = &[
    ("pyproject.toml", "[project]\nname = \"readings\"\nversion = \"0.1.0\"\n"),
    (
        "readings/model.py",
        "from dataclasses import dataclass\n\n\n@dataclass\nclass Reading:\n    value: float\n\n\ndef average(items: list[Reading]) -> Reading:\n    return Reading(sum(item.value for item in items) / len(items))\n",
    ),
    (
        "readings/report.py",
        "from typing import Optional\n\nfrom readings.model import Reading, average\n\nLABEL = \"Reading\"\n\n\ndef latest(items: list[Reading]) -> Optional[Reading]:\n    first: list[Reading] = items[:1]\n    return average(first) if first else None\n",
    ),
    ("readings/__init__.py", ""),
];

#[test]
fn python_server_renames_through_annotations_and_imports() {
    let _env = crate::process::test_support::declare_inherited();
    let dir = tempdir().unwrap();
    write_tree(dir.path(), PY_PROJECT);
    let capability = index(dir.path());
    let Some(result) = server_rename(&capability, "readings/model.py", "Reading", "Measurement")
    else {
        return;
    };
    assert_eq!(s(field(&result, "result")), "applied", "{result:?}");
    assert_eq!(s(field(field(&result, "backend"), "server")), "pyright");
    let read = |path: &str| fs::read_to_string(dir.path().join(path)).unwrap();
    assert_eq!(
        read("readings/model.py"),
        "from dataclasses import dataclass\n\n\n@dataclass\nclass Measurement:\n    value: float\n\n\ndef average(items: list[Measurement]) -> Measurement:\n    return Measurement(sum(item.value for item in items) / len(items))\n"
    );
    assert_eq!(
        read("readings/report.py"),
        "from typing import Optional\n\nfrom readings.model import Measurement, average\n\nLABEL = \"Reading\"\n\n\ndef latest(items: list[Measurement]) -> Optional[Measurement]:\n    first: list[Measurement] = items[:1]\n    return average(first) if first else None\n"
    );
}
