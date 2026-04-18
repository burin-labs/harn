//! Cross-rule autofix behavior: multiple fixes in one source, and
//! graceful no-op when source is unavailable.

use super::*;

#[test]
fn test_fix_multiple_fixes_applied() {
    let source = "pipeline default(task) {\n  var x = 10\n  let y = x == true\n  log(y)\n}";
    let diags = lint_source(source);
    let result = apply_fixes(source, &diags);
    assert!(
        result.contains("let x = 10"),
        "var should be fixed to let, got: {result}"
    );
    assert!(
        result.contains("let y = x"),
        "comparison should be simplified, got: {result}"
    );
}

#[test]
fn test_no_fix_when_source_unavailable() {
    // lint without source — fixes should be None
    let source = "pipeline default(task) {\n  var x = 10\n  log(x)\n}";
    let mut lexer = Lexer::new(source);
    let tokens = lexer.tokenize().unwrap();
    let mut parser = Parser::new(tokens);
    let program = parser.parse().unwrap();
    let diags = lint(&program); // no source
    let fix = get_fix(&diags, "mutable-never-reassigned");
    assert!(
        fix.is_none(),
        "without source, fix should be None, got: {fix:?}"
    );
}
