//! Import-related lints: `collect_selective_import_names`,
//! `unused-import` autofix, and `import-order`.

use super::*;

#[test]
fn test_collect_selective_import_names() {
    let source = r#"
import { foo, bar } from "module_a"
import { baz } from "module_b"
import "wildcard_module"
fn local() { return foo() + bar() + baz() }
"#;
    let mut lexer = Lexer::new(source);
    let tokens = lexer.tokenize().unwrap();
    let mut parser = Parser::new(tokens);
    let program = parser.parse().unwrap();

    let names = collect_selective_import_names(&program);
    assert!(names.contains("foo"), "should contain foo");
    assert!(names.contains("bar"), "should contain bar");
    assert!(names.contains("baz"), "should contain baz");
    assert_eq!(names.len(), 3, "should have exactly 3 names: {names:?}");
}

#[test]
fn test_fix_unused_import_all_unused() {
    let source = "import { foo, bar } from \"mod\"\npipeline default(task) {\n  log(task)\n}";
    let diags = lint_source(source);
    assert!(
        count_rule(&diags, "unused-import") >= 1,
        "expected unused-import warnings"
    );
    // When all names are unused, the fix should remove the entire import line
    let fix = get_fix(&diags, "unused-import");
    assert!(fix.is_some(), "expected fix for unused-import");
    let edits = fix.unwrap();
    assert_eq!(edits.len(), 1);
    assert!(
        edits[0].replacement.is_empty(),
        "expected deletion, got: {:?}",
        edits[0].replacement
    );
}

#[test]
fn test_fix_unused_import_partial() {
    let source = "import { foo, bar } from \"mod\"\npipeline default(task) {\n  log(foo)\n}";
    let diags = lint_source(source);
    // bar is unused, foo is used
    assert_eq!(
        count_rule(&diags, "unused-import"),
        1,
        "expected 1 unused-import warning"
    );
    let fix = get_fix(&diags, "unused-import");
    assert!(fix.is_some(), "expected fix for unused-import");
    let result = apply_fixes(source, &diags);
    assert!(
        result.contains("{ foo }") || result.contains("{foo}"),
        "expected bar removed from import, got: {result}"
    );
    assert!(
        !result.contains("bar"),
        "bar should be removed, got: {result}"
    );
}

#[test]
fn test_import_order_fires_when_out_of_order() {
    let source = "import \"std/io\"\nimport \"std/fs\"\n\nfn a() -> int { return 1 }\n";
    let diags = lint_source(source);
    assert!(
        has_rule(&diags, "import-order"),
        "expected import-order when out of order, got: {diags:?}"
    );
}

#[test]
fn test_import_order_canonical_does_not_fire() {
    let source = "import \"std/fs\"\nimport \"std/io\"\n\nfn a() -> int { return 1 }\n";
    let diags = lint_source(source);
    assert!(
        !has_rule(&diags, "import-order"),
        "canonical order should not fire, got: {diags:?}"
    );
}

#[test]
fn test_import_order_single_import_does_not_fire() {
    let source = "import \"std/io\"\n\nfn a() -> int { return 1 }\n";
    let diags = lint_source(source);
    assert!(
        !has_rule(&diags, "import-order"),
        "single import should not fire, got: {diags:?}"
    );
}

#[test]
fn test_import_order_stdlib_before_third_party() {
    let source = "import \"mypkg/util\"\nimport \"std/io\"\n\nfn a() -> int { return 1 }\n";
    let diags = lint_source(source);
    assert!(
        has_rule(&diags, "import-order"),
        "stdlib should come before third-party, got: {diags:?}"
    );
}
