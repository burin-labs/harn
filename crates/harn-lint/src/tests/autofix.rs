//! Cross-rule autofix behavior: graceful no-op when source is unavailable.
//!
//! Multi-fix application is covered by the per-rule suites, which assert a
//! real before/after delta — see `formatting.rs`, `imports.rs`,
//! `nil_coalesce.rs`, and `empty_blocks.rs`.

use super::*;

#[test]
fn test_no_fix_when_source_unavailable() {
    // lint without source — fixes should be None
    let source = "pipeline default(task) {\n  const x = 10\n  log(x)\n}";
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
