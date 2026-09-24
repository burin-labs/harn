//! Inline-source entrypoint and import-header regressions.

use super::{eval_source_for_code, split_eval_header};

#[test]
fn split_eval_header_no_imports_returns_full_body() {
    let (header, body) = split_eval_header("log(1 + 2)");
    assert_eq!(header, "");
    assert_eq!(body, "log(1 + 2)");
}

#[test]
fn split_eval_header_lifts_leading_imports() {
    let code = "import \"./lib\"\nimport { x } from \"std/math\"\nlog(x)";
    let (header, body) = split_eval_header(code);
    assert_eq!(header, "import \"./lib\"\nimport { x } from \"std/math\"");
    assert_eq!(body, "log(x)");
}

#[test]
fn split_eval_header_keeps_pub_import_and_comments_in_header() {
    let code = "// header comment\npub import { y } from \"./lib\"\n\nfoo()";
    let (header, body) = split_eval_header(code);
    assert_eq!(
        header,
        "// header comment\npub import { y } from \"./lib\"\n"
    );
    assert_eq!(body, "foo()");
}

#[test]
fn split_eval_header_does_not_lift_imports_after_other_statements() {
    let code = "const a = 1\nimport \"./lib\"";
    let (header, body) = split_eval_header(code);
    assert_eq!(header, "");
    assert_eq!(body, "const a = 1\nimport \"./lib\"");
}

#[test]
fn eval_source_wraps_pipeline_body_snippets() {
    assert_eq!(
        eval_source_for_code("let x = 1\n__io_println(x)"),
        "pipeline main(harness: Harness, task: unknown) {\nlet x = 1\n__io_println(x)\n}"
    );
}

#[test]
fn eval_source_keeps_full_harn_programs_unnested() {
    let code = "pipeline default(harness: Harness) {\n  harness.stdio.println(\"ok\")\n}\n";
    assert_eq!(eval_source_for_code(code), code);
}

#[test]
fn eval_source_keeps_imported_full_harn_programs_unnested() {
    let code =
        "import { x } from \"./lib\"\n\npipeline default(harness: Harness) {\n  harness.stdio.println(x)\n}\n";
    assert_eq!(eval_source_for_code(code), code);
}

#[test]
fn eval_source_keeps_function_entrypoints_unnested() {
    let code = "fn main(harness: Harness) { harness.stdio.println(\"body reached\") }";
    assert_eq!(eval_source_for_code(code), code);
}

#[test]
fn eval_source_still_wraps_a_helper_and_its_call() {
    let code = "fn answer() -> int { return 42 }\nreturn answer()";
    assert_eq!(
        eval_source_for_code(code),
        format!("pipeline main(harness: Harness, task: unknown) {{\n{code}\n}}")
    );
}
