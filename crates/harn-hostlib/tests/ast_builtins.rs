//! End-to-end coverage for the wired-up `ast::*` builtins.
//!
//! Complements `ast_fixtures.rs` (golden-symbol/outline checks): this
//! file drives the full builtin path through the registration table —
//! parameter parsing, `VmValue::Dict` shape, schema field set — and
//! includes a perf smoke check against a known input.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Instant;

use harn_hostlib::{ast::AstCapability, BuiltinRegistry, HostlibCapability};
use harn_vm::VmValue;

fn ast_registry() -> BuiltinRegistry {
    let mut registry = BuiltinRegistry::new();
    AstCapability.register_builtins(&mut registry);
    registry
}

fn dict(pairs: &[(&str, VmValue)]) -> VmValue {
    let mut map: BTreeMap<String, VmValue> = BTreeMap::new();
    for (k, v) in pairs {
        map.insert((*k).into(), v.clone());
    }
    VmValue::Dict(Rc::new(map))
}

fn fixture_path(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/ast")
        .join(rel)
}

fn invoke(registry: &BuiltinRegistry, name: &str, payload: VmValue) -> VmValue {
    let entry = registry
        .find(name)
        .unwrap_or_else(|| panic!("builtin {name} not registered"));
    (entry.handler)(&[payload]).unwrap_or_else(|err| panic!("{name} failed: {err}"))
}

fn dict_field(value: &VmValue, key: &str) -> VmValue {
    match value {
        VmValue::Dict(d) => d
            .get(key)
            .cloned()
            .unwrap_or_else(|| panic!("missing field `{key}` on {value:?}")),
        other => panic!("expected dict, got {other:?}"),
    }
}

fn string_value(value: &VmValue) -> &str {
    match value {
        VmValue::String(s) => s.as_ref(),
        other => panic!("expected string, got {other:?}"),
    }
}

fn list_value(value: &VmValue) -> Rc<Vec<VmValue>> {
    match value {
        VmValue::List(l) => l.clone(),
        other => panic!("expected list, got {other:?}"),
    }
}

fn int_value(value: &VmValue) -> i64 {
    match value {
        VmValue::Int(n) => *n,
        other => panic!("expected int, got {other:?}"),
    }
}

#[test]
fn parse_file_produces_flat_node_list_with_root_id_zero() {
    let registry = ast_registry();
    let path = fixture_path("rust/source.rs");
    let payload = dict(&[(
        "path",
        VmValue::String(Rc::from(path.to_string_lossy().as_ref())),
    )]);

    let result = invoke(&registry, "hostlib_ast_parse_file", payload);

    assert_eq!(string_value(&dict_field(&result, "language")), "rust");
    assert_eq!(int_value(&dict_field(&result, "root_id")), 0);
    let nodes = list_value(&dict_field(&result, "nodes"));
    assert!(
        nodes.len() > 5,
        "expected non-trivial tree, got {} nodes",
        nodes.len()
    );

    // Root has parent_id = nil; every other node has an integer parent
    // pointing at a smaller id (BFS guarantees this).
    let first = &nodes[0];
    assert!(matches!(dict_field(first, "parent_id"), VmValue::Nil));
    for node in nodes.iter().skip(1) {
        match dict_field(node, "parent_id") {
            VmValue::Int(n) => assert!(
                n >= 0 && (n as usize) < nodes.len(),
                "parent_id out of range: {n}"
            ),
            other => panic!("expected int parent_id, got {other:?}"),
        }
    }
}

#[test]
fn parse_file_rejects_unknown_language() {
    let registry = ast_registry();
    let entry = registry.find("hostlib_ast_parse_file").expect("registered");
    let payload = dict(&[
        ("path", VmValue::String(Rc::from("foo.unknown"))),
        ("language", VmValue::String(Rc::from("klingon"))),
    ]);
    let err = (entry.handler)(&[payload]).expect_err("must reject unknown language");
    assert!(
        err.to_string().contains("klingon") || err.to_string().contains("could not infer"),
        "unexpected error message: {err}",
    );
}

#[test]
fn symbols_filters_by_kind() {
    let registry = ast_registry();
    let path = fixture_path("rust/source.rs");
    let kinds = VmValue::List(Rc::new(vec![VmValue::String(Rc::from("function"))]));
    let payload = dict(&[
        (
            "path",
            VmValue::String(Rc::from(path.to_string_lossy().as_ref())),
        ),
        ("kinds", kinds),
    ]);
    let result = invoke(&registry, "hostlib_ast_symbols", payload);
    let symbols = list_value(&dict_field(&result, "symbols"));
    assert!(!symbols.is_empty());
    for sym in symbols.iter() {
        assert_eq!(string_value(&dict_field(sym, "kind")), "function");
    }
}

#[test]
fn outline_caps_depth_when_max_depth_supplied() {
    let registry = ast_registry();
    let path = fixture_path("python/source.py");
    let payload = dict(&[
        (
            "path",
            VmValue::String(Rc::from(path.to_string_lossy().as_ref())),
        ),
        ("max_depth", VmValue::Int(1)),
    ]);
    let result = invoke(&registry, "hostlib_ast_outline", payload);
    let items = list_value(&dict_field(&result, "items"));
    assert!(!items.is_empty());
    for item in items.iter() {
        let children = list_value(&dict_field(item, "children"));
        assert!(
            children.is_empty(),
            "max_depth=1 must cap to root level, got children {children:?}"
        );
    }
}

/// Perf smoke test from issue #564: parse a known file within a budget.
/// We don't pin the exact cutoff because tree-sitter performance varies
/// across CI machines, but a 20ms budget is comfortable on local dev
/// hardware and gives ample headroom on CI.
#[test]
fn parse_file_meets_perf_budget_on_a_known_input() {
    let registry = ast_registry();
    // Use the largest fixture we ship (Rust). Running against an in-tree
    // fixture keeps the test hermetic and CI-friendly.
    let path = fixture_path("rust/source.rs");
    let payload = dict(&[(
        "path",
        VmValue::String(Rc::from(path.to_string_lossy().as_ref())),
    )]);

    // Warm up: first call sometimes pays a one-time grammar load.
    let _ = invoke(&registry, "hostlib_ast_parse_file", payload.clone());

    let start = Instant::now();
    let _ = invoke(&registry, "hostlib_ast_parse_file", payload);
    let elapsed = start.elapsed();

    // 20ms target from the issue. Doubled to 40ms here so the test is
    // immune to noisy CI; the realistic warm-call latency is sub-1ms.
    assert!(
        elapsed.as_millis() < 40,
        "parse_file took {elapsed:?} (>40ms ceiling)",
    );
}

// ---------------------------------------------------------------------------
// Mutation + bracket-balance builtins (issue #775)
// ---------------------------------------------------------------------------

fn vm_string(s: &str) -> VmValue {
    VmValue::String(Rc::from(s))
}

#[test]
fn symbol_extract_returns_one_based_inclusive_lines() {
    let registry = ast_registry();
    let source = "fn alpha() {}\nfn beta() {}\nfn gamma() {}\n";
    let payload = dict(&[
        ("source", vm_string(source)),
        ("language", vm_string("rust")),
        ("symbol_name", vm_string("beta")),
    ]);
    let result = invoke(&registry, "hostlib_ast_symbol_extract", payload);
    assert_eq!(string_value(&dict_field(&result, "result")), "extracted");
    assert_eq!(int_value(&dict_field(&result, "start_line")), 2);
    assert_eq!(int_value(&dict_field(&result, "end_line")), 2);
    assert_eq!(string_value(&dict_field(&result, "text")), "fn beta() {}");
}

#[test]
fn symbol_delete_removes_function_and_keeps_syntax_valid() {
    let registry = ast_registry();
    let source = "fn alpha() {}\nfn beta() {}\nfn gamma() {}\n";
    let payload = dict(&[
        ("source", vm_string(source)),
        ("language", vm_string("rust")),
        ("symbol_name", vm_string("beta")),
    ]);
    let result = invoke(&registry, "hostlib_ast_symbol_delete", payload);
    assert_eq!(string_value(&dict_field(&result, "result")), "removed");
    let new_source = string_value(&dict_field(&result, "source")).to_string();
    assert!(!new_source.contains("beta"));
    assert!(new_source.contains("alpha"));
    assert!(new_source.contains("gamma"));
}

#[test]
fn symbol_replace_swaps_in_caller_text() {
    let registry = ast_registry();
    let source = "fn alpha() {}\nfn beta() -> i32 { 0 }\n";
    let payload = dict(&[
        ("source", vm_string(source)),
        ("language", vm_string("rust")),
        ("symbol_name", vm_string("beta")),
        ("new_text", vm_string("fn beta() -> i32 { 42 }")),
    ]);
    let result = invoke(&registry, "hostlib_ast_symbol_replace", payload);
    assert_eq!(string_value(&dict_field(&result, "result")), "replaced");
    assert!(string_value(&dict_field(&result, "source")).contains("42"));
}

#[test]
fn symbol_replace_flags_post_edit_syntax_error() {
    let registry = ast_registry();
    let source = "fn alpha() {}\nfn beta() {}\n";
    let payload = dict(&[
        ("source", vm_string(source)),
        ("language", vm_string("rust")),
        ("symbol_name", vm_string("beta")),
        // Unclosed paren intentionally breaks the edit.
        ("new_text", vm_string("fn beta( {")),
    ]);
    let result = invoke(&registry, "hostlib_ast_symbol_replace", payload);
    assert_eq!(
        string_value(&dict_field(&result, "result")),
        "syntax_error_after_edit"
    );
    assert!(!string_value(&dict_field(&result, "details")).is_empty());
}

#[test]
fn symbol_lookup_reports_ambiguity_with_match_count() {
    let registry = ast_registry();
    let source = "class A:\n    def greet(self): pass\nclass B:\n    def greet(self): pass\n";
    let payload = dict(&[
        ("source", vm_string(source)),
        ("language", vm_string("python")),
        ("symbol_name", vm_string("greet")),
    ]);
    let result = invoke(&registry, "hostlib_ast_symbol_extract", payload);
    assert_eq!(string_value(&dict_field(&result, "result")), "ambiguous");
    assert!(int_value(&dict_field(&result, "match_count")) >= 2);
}

#[test]
fn symbol_lookup_qualified_name_disambiguates() {
    let registry = ast_registry();
    let source = "class A:\n    def greet(self): pass\nclass B:\n    def greet(self): pass\n";
    let payload = dict(&[
        ("source", vm_string(source)),
        ("language", vm_string("python")),
        ("symbol_name", vm_string("B.greet")),
    ]);
    let result = invoke(&registry, "hostlib_ast_symbol_extract", payload);
    assert_eq!(string_value(&dict_field(&result, "result")), "extracted");
    assert!(string_value(&dict_field(&result, "text")).contains("greet"));
    assert_eq!(int_value(&dict_field(&result, "start_line")), 4);
}

#[test]
fn symbol_lookup_emits_suggestions_when_typo_misses() {
    let registry = ast_registry();
    let source = "fn parse_query() {}\nfn parse_other() {}\n";
    let payload = dict(&[
        ("source", vm_string(source)),
        ("language", vm_string("rust")),
        ("symbol_name", vm_string("parse_qury")),
    ]);
    let result = invoke(&registry, "hostlib_ast_symbol_extract", payload);
    assert_eq!(string_value(&dict_field(&result, "result")), "not_found");
    let suggestions = list_value(&dict_field(&result, "suggestions"));
    assert!(!suggestions.is_empty());
    let names: Vec<&str> = suggestions.iter().map(string_value).collect();
    assert!(names.contains(&"parse_query"));
}

#[test]
fn unsupported_language_short_circuits_on_mutation_builtins() {
    let registry = ast_registry();
    for builtin in &[
        "hostlib_ast_symbol_extract",
        "hostlib_ast_symbol_delete",
        "hostlib_ast_symbol_replace",
    ] {
        let mut entries = vec![
            ("source", vm_string("hello")),
            ("language", vm_string("klingon")),
            ("symbol_name", vm_string("greet")),
        ];
        if *builtin == "hostlib_ast_symbol_replace" {
            entries.push(("new_text", vm_string("replacement")));
        }
        let result = invoke(&registry, builtin, dict(&entries));
        assert_eq!(
            string_value(&dict_field(&result, "result")),
            "unsupported_language",
            "{builtin} did not short-circuit on klingon",
        );
    }
}

#[test]
fn bracket_balance_counts_unmatched_opener() {
    let registry = ast_registry();
    let payload = dict(&[
        ("source", vm_string("fn foo() {")),
        ("language", vm_string("rust")),
    ]);
    let result = invoke(&registry, "hostlib_ast_bracket_balance", payload);
    assert_eq!(int_value(&dict_field(&result, "parens")), 0);
    assert_eq!(int_value(&dict_field(&result, "brackets")), 0);
    assert_eq!(int_value(&dict_field(&result, "braces")), 1);
}

#[test]
fn bracket_balance_ignores_brackets_inside_strings() {
    let registry = ast_registry();
    let payload = dict(&[
        ("source", vm_string(r#"let s = "}{)";"#)),
        ("language", vm_string("rust")),
    ]);
    let result = invoke(&registry, "hostlib_ast_bracket_balance", payload);
    assert_eq!(int_value(&dict_field(&result, "parens")), 0);
    assert_eq!(int_value(&dict_field(&result, "braces")), 0);
}

#[test]
fn bracket_balance_python_uses_hash_comments() {
    let registry = ast_registry();
    let payload = dict(&[
        // Python `//` is integer division; must not be parsed as a comment.
        ("source", vm_string("x = 5 // 2  # cmt with [\n")),
        ("language", vm_string("python")),
    ]);
    let result = invoke(&registry, "hostlib_ast_bracket_balance", payload);
    assert_eq!(int_value(&dict_field(&result, "brackets")), 0);
    assert_eq!(int_value(&dict_field(&result, "parens")), 0);
}

#[test]
fn parse_errors_returns_clean_payload_for_valid_python() {
    let registry = ast_registry();
    let payload = dict(&[
        ("content", VmValue::String(Rc::from("x = 1\n"))),
        ("language", VmValue::String(Rc::from("python"))),
    ]);
    let result = invoke(&registry, "hostlib_ast_parse_errors", payload);

    assert_eq!(string_value(&dict_field(&result, "language")), "python");
    let errors = list_value(&dict_field(&result, "errors"));
    assert!(errors.is_empty(), "valid Python should have no errors");
    let supported = match dict_field(&result, "supported") {
        VmValue::Bool(b) => b,
        other => panic!("expected bool, got {other:?}"),
    };
    assert!(supported);
}

#[test]
fn parse_errors_flags_typescript_syntax_error() {
    let registry = ast_registry();
    let payload = dict(&[
        // Mismatched paren — tree-sitter will emit at least one ERROR or
        // MISSING node here.
        (
            "content",
            VmValue::String(Rc::from("function foo(\n  return 1;\n}")),
        ),
        ("language", VmValue::String(Rc::from("typescript"))),
    ]);
    let result = invoke(&registry, "hostlib_ast_parse_errors", payload);
    let errors = list_value(&dict_field(&result, "errors"));
    assert!(!errors.is_empty(), "expected errors, got {errors:?}");

    // Each error has the documented field set.
    for entry in errors.iter() {
        for field in [
            "start_row",
            "start_col",
            "end_row",
            "end_col",
            "start_byte",
            "end_byte",
            "message",
            "snippet",
            "missing",
        ] {
            let _ = dict_field(entry, field);
        }
    }
}

#[test]
fn parse_errors_top_level_decl_count_matches_swift_profile() {
    // Covers top-level declaration counts for a couple of canonical
    // language profiles. Drift in this number means our declaration map
    // changed.
    let registry = ast_registry();

    let cases = [
        ("rust", "fn a() {}\nfn b() {}\nstruct C;\n", 3),
        ("python", "def a():\n    pass\nclass B:\n    pass\n", 2),
        // TS lists `export_statement` as both a declaration and a
        // wrapper, so each `export X` contributes 2 (the export itself
        // plus the wrapped decl).
        (
            "typescript",
            "export function a() {}\nexport const b = 1;\nfunction c() {}\n",
            5,
        ),
    ];
    for (lang, src, want) in cases {
        let payload = dict(&[
            ("content", VmValue::String(Rc::from(src))),
            ("language", VmValue::String(Rc::from(lang))),
        ]);
        let result = invoke(&registry, "hostlib_ast_parse_errors", payload);
        let count = int_value(&dict_field(&result, "top_level_decl_count"));
        assert_eq!(count, want, "{lang} top_level_decl_count");
    }
}

#[test]
fn undefined_names_python_returns_dedup_diagnostics() {
    let registry = ast_registry();
    let payload = dict(&[
        (
            "content",
            VmValue::String(Rc::from(
                "import os\n\
                 def foo(x):\n    \
                     return x + missing()\n\
                 missing()\n\
                 missing()\n",
            )),
        ),
        ("language", VmValue::String(Rc::from("python"))),
    ]);
    let result = invoke(&registry, "hostlib_ast_undefined_names", payload);
    let supported = match dict_field(&result, "supported") {
        VmValue::Bool(b) => b,
        other => panic!("expected bool, got {other:?}"),
    };
    assert!(supported);

    let diagnostics = list_value(&dict_field(&result, "diagnostics"));
    let names: Vec<String> = diagnostics
        .iter()
        .map(|d| string_value(&dict_field(d, "name")).to_string())
        .collect();
    assert_eq!(
        names,
        vec!["missing".to_string()],
        "expected single dedup'd 'missing', got {names:?}"
    );

    // Each diagnostic carries the documented fields.
    for d in diagnostics.iter() {
        let kind_value = dict_field(d, "kind");
        let kind = string_value(&kind_value).to_string();
        assert!(kind == "identifier" || kind == "type", "kind = {kind}");
        let msg_value = dict_field(d, "message");
        let message = string_value(&msg_value).to_string();
        assert!(message.contains("undefined name"), "message = {message}");
    }
}

#[test]
fn undefined_names_marks_unsupported_languages() {
    let registry = ast_registry();
    let payload = dict(&[
        ("content", VmValue::String(Rc::from("fn main() {}\n"))),
        ("language", VmValue::String(Rc::from("rust"))),
    ]);
    let result = invoke(&registry, "hostlib_ast_undefined_names", payload);
    let supported = match dict_field(&result, "supported") {
        VmValue::Bool(b) => b,
        other => panic!("expected bool, got {other:?}"),
    };
    assert!(
        !supported,
        "rust isn't in the undefined-name profile set; must report supported = false"
    );
    let diagnostics = list_value(&dict_field(&result, "diagnostics"));
    assert!(diagnostics.is_empty());
}

#[test]
fn perf_smoke_against_external_file_when_available() {
    // Optional maintainer smoke test for a larger real-world file. CI
    // leaves this unset, so the test remains hermetic by default.
    let target = std::env::var("HARN_AST_PERF_SMOKE_PATH")
        .ok()
        .map(PathBuf::from);
    let Some(path) = target.filter(|p| p.exists()) else {
        return;
    };

    let registry = ast_registry();
    let payload = dict(&[(
        "path",
        VmValue::String(Rc::from(path.to_string_lossy().as_ref())),
    )]);
    let _warmup = invoke(&registry, "hostlib_ast_parse_file", payload.clone());
    let start = Instant::now();
    let _ = invoke(&registry, "hostlib_ast_parse_file", payload);
    let elapsed = start.elapsed();
    let bytes = fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    eprintln!(
        "external parse_file perf smoke: {elapsed:?} ({} bytes)",
        bytes
    );
    assert!(
        elapsed.as_millis() < 50,
        "Package.swift parse took {elapsed:?} (>50ms)"
    );
}
