use super::*;

#[test]
fn persona_body_must_call_step_functions() {
    let source = r#"
@persona(name: "merge_captain")
fn merge_captain(ctx) {
  plan(ctx)
  helper(ctx)
  __io_println("done")
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
        require_stdlib_metadata: false,
        engine_rules: &[],
    };
    let diagnostics = lint_with_options(&program, &[], Some(source), &HashSet::new(), &options);
    assert!(!has_rule(&diagnostics, "persona-body-must-call-steps"));
}

#[test]
fn step_hook_target_must_exist_in_matching_persona() {
    let source = r#"
@persona(name: "merge_captain")
fn merge_captain(ctx) {
  plan(ctx)
}

@step(name: "plan")
fn plan(ctx) {
  return ctx
}

pipeline default() {
  register_step_hook("merge_*", "publish", "PreStep", { ctx -> nil })
}
"#;

    let diagnostics = lint_source(source);
    let target_diags: Vec<_> = diagnostics
        .iter()
        .filter(|diag| diag.rule == "persona-hook-target")
        .collect();
    assert_eq!(target_diags.len(), 1);
    assert!(target_diags[0].message.contains("publish"));
}

#[test]
fn step_hook_target_accepts_declared_persona_step() {
    let source = r#"
@persona(name: "merge_captain")
fn merge_captain(ctx) {
  plan(ctx)
}

@step(name: "plan")
fn plan(ctx) {
  return ctx
}

pipeline default() {
  register_step_hook("merge_*", "plan", "PreStep", { ctx -> nil })
}
"#;

    let diagnostics = lint_source(source);
    assert!(!has_rule(&diagnostics, "persona-hook-target"));
}
