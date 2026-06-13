//! Cross-language edit-correctness conformance suite (B.7).
//!
//! Drives the `ast.apply_node` / `ast.insert_at_anchor` / `ast.capabilities`
//! builtins through the registration table for every tier-1 language so a
//! grammar regression (or a botched query in a new adapter) fails loudly.
//! The edit primitives are query-driven and grammar-agnostic, so "does a
//! real edit round-trip through parse → query → splice → re-validate" is
//! the single contract every language must satisfy.

use std::io::Write;
use std::sync::Arc;

use harn_hostlib::{ast::AstCapability, tools::permissions, BuiltinRegistry, HostlibCapability};
use harn_vm::VmValue;
use tempfile::NamedTempFile;

fn registry() -> BuiltinRegistry {
    let mut registry = BuiltinRegistry::new();
    AstCapability.register_builtins(&mut registry);
    registry
}

fn dict(pairs: &[(&str, VmValue)]) -> VmValue {
    let mut map: harn_vm::value::DictMap = Default::default();
    for (k, v) in pairs {
        map.insert((*k).into(), v.clone());
    }
    VmValue::dict(map)
}

fn vm_string(s: &str) -> VmValue {
    VmValue::String(Arc::from(s))
}

fn invoke(registry: &BuiltinRegistry, name: &str, payload: VmValue) -> VmValue {
    let entry = registry
        .find(name)
        .unwrap_or_else(|| panic!("builtin {name} not registered"));
    (entry.handler)(&[payload]).unwrap_or_else(|err| panic!("{name} failed: {err}"))
}

fn field<'a>(value: &'a VmValue, key: &str) -> &'a VmValue {
    match value {
        VmValue::Dict(d) => d
            .get(key)
            .unwrap_or_else(|| panic!("missing field `{key}`")),
        other => panic!("expected dict, got {other:?}"),
    }
}

fn s(value: &VmValue) -> String {
    match value {
        VmValue::String(s) => s.to_string(),
        other => panic!("expected string, got {other:?}"),
    }
}

fn b(value: &VmValue) -> bool {
    match value {
        VmValue::Bool(b) => *b,
        other => panic!("expected bool, got {other:?}"),
    }
}

fn write_temp(ext: &str, source: &str) -> NamedTempFile {
    let mut file = tempfile::Builder::new()
        .suffix(&format!(".{ext}"))
        .tempfile()
        .expect("temp file");
    file.write_all(source.as_bytes()).expect("write source");
    file
}

/// One real, format-preserving edit per tier-1 data/markup language. The
/// general-purpose languages already have dedicated `apply_node` tests in
/// the `ast::apply_node` module; this table closes the B.7 expansion set.
struct EditCase {
    language: &'static str,
    ext: &'static str,
    source: &'static str,
    query: &'static str,
    /// Multi-match selector; `"first"` where the snippet legitimately has
    /// several same-kind nodes (e.g. every YAML scalar value).
    select: &'static str,
    replacement: &'static str,
    expect_contains: &'static str,
    expect_absent: &'static str,
}

const CASES: &[EditCase] = &[
    EditCase {
        language: "json",
        ext: "json",
        source: "{\n  \"name\": \"harn\",\n  \"version\": 1\n}\n",
        query: "(pair value: (number) @target)",
        select: "unique",
        replacement: "2",
        expect_contains: "\"version\": 2",
        expect_absent: "\"version\": 1",
    },
    EditCase {
        language: "yaml",
        ext: "yaml",
        source: "name: harn\nversion: 0.1.0\n",
        query: "(block_mapping_pair value: (flow_node (plain_scalar (string_scalar) @target)))",
        select: "first",
        replacement: "renamed",
        expect_contains: "name: renamed",
        expect_absent: "name: harn",
    },
    EditCase {
        language: "toml",
        ext: "toml",
        source: "[package]\nname = \"harn\"\n",
        query: "(pair (string) @target)",
        select: "unique",
        replacement: "\"renamed\"",
        expect_contains: "name = \"renamed\"",
        expect_absent: "name = \"harn\"",
    },
    EditCase {
        language: "css",
        ext: "css",
        source: ".button {\n  color: red;\n}\n",
        query: "(declaration (plain_value) @target)",
        select: "unique",
        replacement: "blue",
        expect_contains: "color: blue;",
        expect_absent: "color: red;",
    },
    EditCase {
        language: "html",
        ext: "html",
        source: "<p>hi</p>\n",
        query: "(element (text) @target)",
        select: "unique",
        replacement: "bye",
        expect_contains: ">bye<",
        expect_absent: ">hi<",
    },
    EditCase {
        language: "sql",
        ext: "sql",
        source: "SELECT id FROM users;\n",
        query: "(relation (object_reference name: (identifier) @target))",
        select: "unique",
        replacement: "accounts",
        expect_contains: "FROM accounts",
        expect_absent: "FROM users",
    },
    EditCase {
        language: "markdown",
        ext: "md",
        source: "# Title\n\nbody text\n",
        query: "(atx_heading heading_content: (inline) @target)",
        select: "unique",
        replacement: "Renamed",
        expect_contains: "Renamed",
        expect_absent: "Title",
    },
];

#[test]
fn apply_node_round_trips_every_tier1_language() {
    permissions::enable_for_test();
    let registry = registry();
    for case in CASES {
        let file = write_temp(case.ext, case.source);
        let path = file.path().to_string_lossy().to_string();
        let result = invoke(
            &registry,
            "hostlib_ast_apply_node",
            dict(&[
                ("path", vm_string(&path)),
                ("query", vm_string(case.query)),
                ("replacement", vm_string(case.replacement)),
                ("select", vm_string(case.select)),
            ]),
        );
        assert_eq!(
            s(field(&result, "result")),
            "applied",
            "{} apply_node did not apply: {result:?}",
            case.language
        );
        let preview = s(field(&result, "preview"));
        assert!(
            preview.contains(case.expect_contains),
            "{} preview missing `{}`:\n{preview}",
            case.language,
            case.expect_contains
        );
        assert!(
            !preview.contains(case.expect_absent),
            "{} preview still has `{}`:\n{preview}",
            case.language,
            case.expect_absent
        );
        // The on-disk file must carry the edit too (not just the preview).
        let on_disk = std::fs::read_to_string(file.path()).expect("read back");
        assert!(
            on_disk.contains(case.expect_contains),
            "{} on-disk edit missing `{}`",
            case.language,
            case.expect_contains
        );
    }
}

#[test]
fn capabilities_matrix_covers_every_language_and_gates_rename() {
    let registry = registry();
    let result = invoke(&registry, "hostlib_ast_capabilities", dict(&[]));
    assert_eq!(s(field(&result, "result")), "ok");
    let rows = match field(&result, "languages") {
        VmValue::List(l) => l.as_ref().clone(),
        other => panic!("expected list, got {other:?}"),
    };
    // Every row reports the two universal primitives.
    for row in &rows {
        let lang = s(field(row, "language"));
        assert!(
            b(field(row, "apply_node")),
            "{lang} should support apply_node"
        );
        assert!(
            b(field(row, "insert_at_anchor")),
            "{lang} should support insert_at_anchor"
        );
    }
    // Data formats edit but expose no symbol projection; code languages do.
    let find = |name: &str| {
        rows.iter()
            .find(|r| s(field(r, "language")) == name)
            .unwrap_or_else(|| panic!("no row for {name}"))
            .clone()
    };
    assert!(!b(field(&find("json"), "rename_symbol")));
    assert!(!b(field(&find("json"), "symbols")));
    assert!(b(field(&find("harn"), "rename_symbol")));
    assert!(b(field(&find("harn"), "symbols")));
    assert!(b(field(&find("rust"), "rename_symbol")));
    assert!(b(field(&find("rust"), "symbols")));
}

#[test]
fn unsupported_language_degrades_with_fallback_suggestion() {
    permissions::enable_for_test();
    let registry = registry();
    let file = write_temp("unknownext", "noop\n");
    let path = file.path().to_string_lossy().to_string();
    let result = invoke(
        &registry,
        "hostlib_ast_apply_node",
        dict(&[
            ("path", vm_string(&path)),
            ("query", vm_string("(x) @target")),
            ("replacement", vm_string("y")),
        ]),
    );
    assert_eq!(s(field(&result, "result")), "unsupported_language");
    assert!(
        !s(field(&result, "fallback_suggestion")).is_empty(),
        "unsupported_language must carry a fallback_suggestion"
    );
}

#[test]
fn insert_at_anchor_works_on_a_data_format() {
    permissions::enable_for_test();
    let registry = registry();
    let file = write_temp("css", ".button {\n  color: red;\n}\n");
    let path = file.path().to_string_lossy().to_string();
    let result = invoke(
        &registry,
        "hostlib_ast_insert_at_anchor",
        dict(&[
            ("path", vm_string(&path)),
            ("query", vm_string("(block) @anchor")),
            ("position", vm_string("last_child")),
            ("content", vm_string("background: blue;")),
        ]),
    );
    assert_eq!(
        s(field(&result, "result")),
        "applied",
        "css insert_at_anchor did not apply: {result:?}"
    );
    let preview = s(field(&result, "preview"));
    assert!(
        preview.contains("background: blue;"),
        "inserted declaration missing:\n{preview}"
    );
    assert!(preview.contains("color: red;"), "original lost:\n{preview}");
}
