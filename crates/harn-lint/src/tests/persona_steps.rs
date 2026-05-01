use super::*;

#[test]
fn persona_body_must_call_step_functions() {
    let source = r#"
@persona(name: "merge_captain")
fn merge_captain(ctx) {
  plan(ctx)
  helper(ctx)
  println("done")
}

@step(name: "plan")
fn plan(ctx) {
  return ctx
}

fn helper(ctx) {
  return ctx
}
"#;

    let diagnostics = lint_source(source);
    let persona_diags: Vec<_> = diagnostics
        .iter()
        .filter(|diag| diag.rule == "persona-body-must-call-steps")
        .collect();
    assert_eq!(persona_diags.len(), 1);
    assert!(persona_diags[0].message.contains("helper"));
}

#[test]
fn persona_body_allowlist_suppresses_step_requirement() {
    let source = r#"
@persona(name: "merge_captain")
fn merge_captain(ctx) {
  helper(ctx)
}

fn helper(ctx) {
  return ctx
}
"#;
    let mut lexer = Lexer::new(source);
    let tokens = lexer.tokenize().unwrap();
    let mut parser = Parser::new(tokens);
    let program = parser.parse().unwrap();
    let allow = vec!["helper".to_string()];
    let options = LintOptions {
        file_path: None,
        require_file_header: false,
        complexity_threshold: None,
        persona_step_allowlist: &allow,
    };
    let diagnostics = lint_with_options(&program, &[], Some(source), &HashSet::new(), &options);
    assert!(!has_rule(&diagnostics, "persona-body-must-call-steps"));
}
