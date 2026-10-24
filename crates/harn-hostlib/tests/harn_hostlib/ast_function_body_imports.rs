//! End-to-end coverage for the new `ast::function_body`,
//! `ast::function_bodies`, and `ast::extract_imports` builtins added by
//! issue #774. These complement the per-module unit tests in
//! `src/ast/function_body.rs` and `src/ast/imports.rs` by driving the
//! full Harn-side surface (parameter parsing → schema-shaped Dict
//! response).

use std::path::PathBuf;

use harn_vm::VmValue;

use super::support::*;

fn dict_string(value: &VmValue, key: &str) -> String {
    extract_str(&dict_field(value, key))
}

fn fixture_path(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/ast")
        .join(rel)
}

fn vstring(s: &str) -> VmValue {
    VmValue::string(s)
}

// -----------------------------------------------------------------------------
// function_body
// -----------------------------------------------------------------------------

#[test]
fn function_body_extracts_typescript_function_via_source() {
    let source = "function shout(s: string): string {\n  return s.toUpperCase();\n}\n";
    let registry = ast_registry();
    let payload = dict(&[
        ("source", vstring(source)),
        ("language", vstring("typescript")),
        ("function_name", vstring("shout")),
    ]);
    let result = call(&registry, "hostlib_ast_function_body", payload);

    assert!(extract_bool(&dict_field(&result, "found")));
    assert_eq!(extract_str(&dict_field(&result, "language")), "typescript");
    assert_eq!(extract_str(&dict_field(&result, "name")), "shout");
    assert!(extract_bool(&dict_field(&result, "brace_based")));
    let body = extract_str(&dict_field(&result, "body_text"));
    assert!(body.contains("toUpperCase"), "body was {body:?}");
    assert_eq!(extract_int(&dict_field(&result, "start_line")), 1);
    assert_eq!(extract_int(&dict_field(&result, "end_line")), 3);
}

#[test]
fn function_body_reports_not_found_with_zero_lines() {
    let source = "function a() {}\n";
    let registry = ast_registry();
    let payload = dict(&[
        ("source", vstring(source)),
        ("language", vstring("typescript")),
        ("function_name", vstring("missing")),
    ]);
    let result = call(&registry, "hostlib_ast_function_body", payload);

    assert!(!extract_bool(&dict_field(&result, "found")));
    assert_eq!(extract_int(&dict_field(&result, "start_line")), 0);
    assert_eq!(extract_int(&dict_field(&result, "end_line")), 0);
    assert_eq!(extract_str(&dict_field(&result, "body_text")), "");
}

#[test]
fn function_body_python_uses_indentation_block() {
    let source = "class Greeter:\n    def greet(self, name):\n        return f\"hi {name}\"\n";
    let registry = ast_registry();
    let payload = dict(&[
        ("source", vstring(source)),
        ("language", vstring("python")),
        ("function_name", vstring("greet")),
    ]);
    let result = call(&registry, "hostlib_ast_function_body", payload);

    assert!(extract_bool(&dict_field(&result, "found")));
    assert!(!extract_bool(&dict_field(&result, "brace_based")));
    let body = extract_str(&dict_field(&result, "body_text"));
    assert!(body.contains("return"), "body was {body:?}");
}

#[test]
fn function_body_filters_by_container() {
    let source = r#"
class Foo {
  greet() { return "foo"; }
}
class Bar {
  greet() { return "bar"; }
}
"#;
    let registry = ast_registry();
    let payload = dict(&[
        ("source", vstring(source)),
        ("language", vstring("typescript")),
        ("function_name", vstring("greet")),
        ("container", vstring("Bar")),
    ]);
    let result = call(&registry, "hostlib_ast_function_body", payload);

    assert!(extract_bool(&dict_field(&result, "found")));
    let body = extract_str(&dict_field(&result, "body_text"));
    assert!(
        body.contains("\"bar\""),
        "should match Bar.greet, got {body:?}"
    );
}

#[test]
fn function_body_filters_by_java_record_container() {
    let source = r#"
class Folder {
  String label() { return "folder"; }
}
record TagResponse(String name) {
  String label() { return "tag"; }
}
"#;
    let registry = ast_registry();
    let payload = dict(&[
        ("source", vstring(source)),
        ("language", vstring("java")),
        ("function_name", vstring("label")),
        ("container", vstring("TagResponse")),
    ]);
    let result = call(&registry, "hostlib_ast_function_body", payload);

    assert!(extract_bool(&dict_field(&result, "found")));
    let body = extract_str(&dict_field(&result, "body_text"));
    assert!(
        body.contains("\"tag\""),
        "should match TagResponse.label, got {body:?}"
    );
}

#[test]
fn function_body_extracts_arrow_function_via_lexical_declaration() {
    let source = "const yell = (msg: string) => msg.toUpperCase();\n";
    let registry = ast_registry();
    let payload = dict(&[
        ("source", vstring(source)),
        ("language", vstring("typescript")),
        ("function_name", vstring("yell")),
    ]);
    let result = call(&registry, "hostlib_ast_function_body", payload);
    assert!(extract_bool(&dict_field(&result, "found")));
    let body = extract_str(&dict_field(&result, "body_text"));
    assert!(body.contains("toUpperCase"), "body was {body:?}");
}

#[test]
fn function_body_extracts_scala_expression_body_precisely() {
    let registry = ast_registry();
    let path = fixture_path("scala/source.scala");
    let payload = dict(&[
        ("path", vstring(path.to_string_lossy().as_ref())),
        ("function_name", vstring("shout")),
    ]);
    let result = call(&registry, "hostlib_ast_function_body", payload);

    assert!(extract_bool(&dict_field(&result, "found")));
    assert!(extract_bool(&dict_field(&result, "declaration_found")));
    assert_eq!(
        extract_str(&dict_field(&result, "isolation_status")),
        "isolated"
    );
    assert_eq!(
        extract_str(&dict_field(&result, "body_text")),
        "s.toUpperCase"
    );
    assert_eq!(extract_int(&dict_field(&result, "start_line")), 13);
    assert_eq!(extract_int(&dict_field(&result, "end_line")), 13);
    assert_eq!(extract_int(&dict_field(&result, "start_col")), 31);
    assert_eq!(extract_int(&dict_field(&result, "end_col")), 44);
}

#[test]
fn function_body_returns_declaration_fallback_when_body_is_not_isolated() {
    let registry = ast_registry();
    let path = fixture_path("scala/source.scala");
    let payload = dict(&[
        ("path", vstring(path.to_string_lossy().as_ref())),
        ("function_name", vstring("speak")),
        ("container", vstring("Speaker")),
    ]);
    let result = call(&registry, "hostlib_ast_function_body", payload);

    assert!(!extract_bool(&dict_field(&result, "found")));
    assert!(extract_bool(&dict_field(&result, "declaration_found")));
    assert_eq!(
        extract_str(&dict_field(&result, "isolation_status")),
        "declaration_fallback"
    );
    assert_eq!(
        extract_str(&dict_field(&result, "isolation_reason")),
        "body_not_isolated"
    );
    assert_eq!(
        extract_str(&dict_field(&result, "declaration_node_kind")),
        "function_declaration"
    );
    assert_eq!(
        extract_int(&dict_field(&result, "declaration_start_line")),
        2
    );
    assert_eq!(extract_int(&dict_field(&result, "declaration_end_line")), 2);
    assert_eq!(
        extract_str(&dict_field(&result, "fallback_action")),
        "replace_range"
    );
    assert!(
        extract_str(&dict_field(&result, "retarget_instruction")).contains("L2-L2"),
        "instruction was {:?}",
        dict_field(&result, "retarget_instruction")
    );
}

#[test]
fn function_body_falls_back_to_path_when_source_omitted() {
    let registry = ast_registry();
    let path = fixture_path("rust/source.rs");
    let payload = dict(&[
        ("path", vstring(path.to_string_lossy().as_ref())),
        ("function_name", vstring("shout")),
    ]);
    let result = call(&registry, "hostlib_ast_function_body", payload);

    assert!(extract_bool(&dict_field(&result, "found")));
    assert_eq!(extract_str(&dict_field(&result, "language")), "rust");
    assert!(extract_str(&dict_field(&result, "body_text")).contains("to_uppercase"));
}

#[test]
fn function_body_surfaces_return_object_fields_for_response_shape() {
    let source = r"function buildResponse(req) {
  return {
    id: req.id,
    name: req.name,
    createdAt: Date.now(),
  };
}
";
    let registry = ast_registry();
    let payload = dict(&[
        ("source", vstring(source)),
        ("language", vstring("typescript")),
        ("function_name", vstring("buildResponse")),
    ]);
    let result = call(&registry, "hostlib_ast_function_body", payload);
    assert!(extract_bool(&dict_field(&result, "found")));
    let fields = extract_list(&dict_field(&result, "return_object_fields"));
    let names: Vec<String> = fields.iter().map(extract_str).collect();
    assert_eq!(names, vec!["id", "name", "createdAt"]);
}

#[test]
fn function_body_requires_function_name() {
    let registry = ast_registry();
    let entry = registry
        .find("hostlib_ast_function_body")
        .expect("registered");
    let payload = dict(&[
        ("source", vstring("function f() {}")),
        ("language", vstring("typescript")),
    ]);
    let err = (entry.handler)(&[payload]).expect_err("must require function_name");
    assert!(
        err.to_string().contains("function_name"),
        "unexpected error: {err}"
    );
}

#[test]
fn function_body_requires_input_source_or_path() {
    let registry = ast_registry();
    let entry = registry
        .find("hostlib_ast_function_body")
        .expect("registered");
    let payload = dict(&[
        ("function_name", vstring("foo")),
        ("language", vstring("typescript")),
    ]);
    let err = (entry.handler)(&[payload]).expect_err("must require source or path");
    assert!(
        err.to_string().contains("source"),
        "unexpected error: {err}"
    );
}

// -----------------------------------------------------------------------------
// function_bodies
// -----------------------------------------------------------------------------

fn name_list(names: &[&str]) -> VmValue {
    let entries: Vec<VmValue> = names.iter().map(|s| vstring(s)).collect();
    VmValue::list(entries)
}

#[test]
fn function_bodies_returns_map_keyed_by_name() {
    let source = r"
function a() { return 1; }
function b() { return 2; }
function c() { return 3; }
";
    let registry = ast_registry();
    let payload = dict(&[
        ("source", vstring(source)),
        ("language", vstring("typescript")),
        ("names", name_list(&["a", "b", "c"])),
    ]);
    let result = call(&registry, "hostlib_ast_function_bodies", payload);

    let bodies = match dict_field(&result, "bodies") {
        VmValue::Dict(d) => d,
        other => panic!("expected dict, got {other:?}"),
    };
    assert_eq!(bodies.len(), 3);
    for n in &["a", "b", "c"] {
        let body = bodies.get(*n).unwrap_or_else(|| panic!("missing {n}"));
        assert_eq!(extract_str(&dict_field(body, "name")), *n);
    }
    let missing = extract_list(&dict_field(&result, "missing"));
    assert!(missing.is_empty());
}

#[test]
fn function_bodies_reports_missing_names() {
    let source = "function a() { return 1; }\n";
    let registry = ast_registry();
    let payload = dict(&[
        ("source", vstring(source)),
        ("language", vstring("typescript")),
        ("names", name_list(&["a", "ghost"])),
    ]);
    let result = call(&registry, "hostlib_ast_function_bodies", payload);

    let bodies = match dict_field(&result, "bodies") {
        VmValue::Dict(d) => d,
        other => panic!("expected dict, got {other:?}"),
    };
    assert!(bodies.contains_key("a"));
    assert!(!bodies.contains_key("ghost"));
    let missing = extract_list(&dict_field(&result, "missing"));
    let missing_names: Vec<String> = missing.iter().map(extract_str).collect();
    assert_eq!(missing_names, vec!["ghost"]);
}

#[test]
fn function_bodies_separates_declaration_fallbacks_from_missing_names() {
    let registry = ast_registry();
    let path = fixture_path("scala/source.scala");
    let payload = dict(&[
        ("path", vstring(path.to_string_lossy().as_ref())),
        ("container", vstring("Speaker")),
        ("names", name_list(&["speak", "ghost"])),
    ]);
    let result = call(&registry, "hostlib_ast_function_bodies", payload);

    let bodies = match dict_field(&result, "bodies") {
        VmValue::Dict(d) => d,
        other => panic!("expected dict, got {other:?}"),
    };
    assert!(!bodies.contains_key("speak"));

    let fallbacks = match dict_field(&result, "fallback_declarations") {
        VmValue::Dict(d) => d,
        other => panic!("expected dict, got {other:?}"),
    };
    let fallback = fallbacks.get("speak").expect("missing speak fallback");
    assert_eq!(
        extract_str(&dict_field(fallback, "fallback_action")),
        "replace_range"
    );
    assert_eq!(
        extract_str(&dict_field(fallback, "declaration_node_kind")),
        "function_declaration"
    );

    let missing = extract_list(&dict_field(&result, "missing"));
    let missing_names: Vec<String> = missing.iter().map(extract_str).collect();
    assert_eq!(missing_names, vec!["ghost"]);
}

#[test]
fn function_bodies_dedupes_repeated_names() {
    let source = "function a() { return 1; }\n";
    let registry = ast_registry();
    let payload = dict(&[
        ("source", vstring(source)),
        ("language", vstring("typescript")),
        ("names", name_list(&["a", "a", "a"])),
    ]);
    let result = call(&registry, "hostlib_ast_function_bodies", payload);
    let bodies = match dict_field(&result, "bodies") {
        VmValue::Dict(d) => d,
        other => panic!("expected dict, got {other:?}"),
    };
    assert_eq!(bodies.len(), 1);
}

#[test]
fn function_bodies_rejects_empty_names() {
    let registry = ast_registry();
    let entry = registry
        .find("hostlib_ast_function_bodies")
        .expect("registered");
    let payload = dict(&[
        ("source", vstring("function a() {}")),
        ("language", vstring("typescript")),
        ("names", VmValue::list(vec![])),
    ]);
    let err = (entry.handler)(&[payload]).expect_err("must reject empty names");
    assert!(
        err.to_string().contains("at least one"),
        "unexpected: {err}"
    );
}

// -----------------------------------------------------------------------------
// extract_imports
// -----------------------------------------------------------------------------

#[test]
fn extract_imports_typescript() {
    let source = "import { foo } from 'bar';\nimport baz from \"./baz\";\nconst x = 1;";
    let registry = ast_registry();
    let payload = dict(&[
        ("source", vstring(source)),
        ("language", vstring("typescript")),
    ]);
    let result = call(&registry, "hostlib_ast_extract_imports", payload);

    assert!(extract_bool(&dict_field(&result, "supported")));
    let stmts = extract_list(&dict_field(&result, "statements"));
    assert_eq!(stmts.len(), 2);
    let first = &stmts[0];
    assert_eq!(
        extract_str(&dict_field(first, "text")),
        "import { foo } from 'bar';"
    );
    assert_eq!(extract_int(&dict_field(first, "line")), 1);
}

#[test]
fn extract_imports_python_handles_from_imports() {
    let source = "import os\nfrom typing import List, Optional\n\ndef f(): pass\n";
    let registry = ast_registry();
    let payload = dict(&[("source", vstring(source)), ("language", vstring("python"))]);
    let result = call(&registry, "hostlib_ast_extract_imports", payload);
    let stmts = extract_list(&dict_field(&result, "statements"));
    let texts: Vec<String> = stmts.iter().map(|s| dict_string(s, "text")).collect();
    assert_eq!(
        texts,
        vec!["import os", "from typing import List, Optional"]
    );
}

#[test]
fn extract_imports_falls_back_to_supported_false_when_unknown_language() {
    let registry = ast_registry();
    let payload = dict(&[
        ("source", vstring("anything")),
        ("language", vstring("klingon")),
    ]);
    let result = call(&registry, "hostlib_ast_extract_imports", payload);
    assert!(!extract_bool(&dict_field(&result, "supported")));
    let stmts = extract_list(&dict_field(&result, "statements"));
    assert!(stmts.is_empty());
}

#[test]
fn extract_imports_reads_from_path_when_source_omitted() {
    let registry = ast_registry();
    let path = fixture_path("typescript/source.ts");
    let payload = dict(&[("path", vstring(path.to_string_lossy().as_ref()))]);
    let result = call(&registry, "hostlib_ast_extract_imports", payload);
    assert!(extract_bool(&dict_field(&result, "supported")));
    assert_eq!(extract_str(&dict_field(&result, "language")), "typescript");
}

#[test]
fn extract_imports_empty_when_no_imports() {
    let source = "function f() { return 1; }\n";
    let registry = ast_registry();
    let payload = dict(&[
        ("source", vstring(source)),
        ("language", vstring("typescript")),
    ]);
    let result = call(&registry, "hostlib_ast_extract_imports", payload);
    let stmts = extract_list(&dict_field(&result, "statements"));
    assert!(stmts.is_empty());
    assert!(extract_bool(&dict_field(&result, "supported")));
}

// -----------------------------------------------------------------------------
// schemas
// -----------------------------------------------------------------------------
