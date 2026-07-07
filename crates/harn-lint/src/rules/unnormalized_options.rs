//! `unnormalized-options` rule: nudge inline option dict literals passed
//! directly to `agent_loop` / `workflow_execute` toward the typed option
//! path — an annotated binding (`let opts: AgentLoopOptions = {...}`) or a
//! typed constructor (`agent_preset(...)`, `agent_options(...)`,
//! `workflow_stage_spec(...)`). Raw dicts still execute unchanged; this is
//! lint-level deprecation, not an error.

use harn_lexer::Span;
use harn_parser::{visit, DiagnosticCode as Code, Node, SNode};

use crate::diagnostic::{LintDiagnostic, LintSeverity};

const RULE_NAME: &str = "unnormalized-options";

/// `agent_loop(prompt, system?, options?)` — a dict literal in any
/// argument slot can only be the options dict, so every non-empty
/// dict-literal argument is flagged.
const AGENT_LOOP_CALLEE: &str = "agent_loop";

/// `workflow_execute(task, graph, artifacts?, options?)` — only the
/// options slot is flagged. The graph argument is a workflow definition,
/// not an options dict; per-stage typing is `workflow_stage_spec(...)` /
/// `StageSpec` annotations inside the graph, which this rule does not
/// police.
const WORKFLOW_EXECUTE_CALLEE: &str = "workflow_execute";
const WORKFLOW_EXECUTE_OPTIONS_INDEX: usize = 3;

/// Walk the program looking for inline option dict literals on the target
/// call surfaces. Emits one Info diagnostic per offending literal,
/// anchored to the literal's span.
pub(crate) fn check_unnormalized_options(program: &[SNode], diagnostics: &mut Vec<LintDiagnostic>) {
    visit::walk_program(program, &mut |node| {
        let Node::FunctionCall { name, args, .. } = &node.node else {
            return;
        };
        match name.as_str() {
            AGENT_LOOP_CALLEE => {
                for arg in args {
                    if is_nonempty_dict_literal(arg) {
                        diagnostics.push(make_diagnostic(
                            arg.span,
                            AGENT_LOOP_CALLEE,
                            "annotate a binding with `AgentLoopOptions` (std/agent/options) or build the options via `agent_preset(...)` / `agent_options(...)`",
                        ));
                    }
                }
            }
            WORKFLOW_EXECUTE_CALLEE => {
                if let Some(arg) = args.get(WORKFLOW_EXECUTE_OPTIONS_INDEX) {
                    if is_nonempty_dict_literal(arg) {
                        diagnostics.push(make_diagnostic(
                            arg.span,
                            WORKFLOW_EXECUTE_CALLEE,
                            "annotate a binding with `WorkflowExecuteOptions` (std/workflow/options) or build it via `workflow_execute_options(...)`",
                        ));
                    }
                }
            }
            _ => {}
        }
    });
}

/// Empty literals (`{}`) are exempt: there is nothing to typo-check and
/// they read as an explicit "no options" marker.
fn is_nonempty_dict_literal(arg: &SNode) -> bool {
    matches!(&arg.node, Node::DictLiteral(entries) if !entries.is_empty())
}

fn make_diagnostic(span: Span, callee: &str, hint: &str) -> LintDiagnostic {
    let message = format!(
        "inline options dict passed to `{callee}` bypasses the typed option constructors; {hint}"
    );
    LintDiagnostic {
        code: Code::LintUnnormalizedOptions,
        rule: RULE_NAME.into(),
        message,
        span,
        severity: LintSeverity::Info,
        suggestion: Some(format!(
            "bind the options first (e.g. `let opts: AgentLoopOptions = {{...}}`) and pass `opts` to `{callee}`; the dict still executes unchanged — this is lint-level deprecation"
        )),
        fix: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use harn_lexer::Lexer;
    use harn_parser::Parser;

    fn lint(source: &str) -> Vec<LintDiagnostic> {
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let program = Parser::new(tokens).parse().expect("parse");
        let mut diags = Vec::new();
        check_unnormalized_options(&program, &mut diags);
        diags
    }

    fn count_rule(diags: &[LintDiagnostic]) -> usize {
        diags.iter().filter(|d| d.rule == RULE_NAME).count()
    }

    #[test]
    fn triggers_on_agent_loop_inline_options() {
        let diags = lint(
            r#"
pipeline default(task) {
    agent_loop(task, "system", {loop_until_done: true, max_iterations: 8})
}
"#,
        );
        assert_eq!(count_rule(&diags), 1, "diags: {diags:?}");
        let diag = diags.iter().find(|d| d.rule == RULE_NAME).unwrap();
        assert!(
            diag.message.contains("AgentLoopOptions"),
            "hint names the alias: {}",
            diag.message
        );
        assert!(
            diag.message.contains("agent_preset"),
            "hint names the constructor: {}",
            diag.message
        );
    }

    #[test]
    fn severity_is_info_not_warning() {
        let diags = lint(
            r"
pipeline default(task) {
    agent_loop(task, nil, {loop_until_done: true})
}
",
        );
        let diag = diags.iter().find(|d| d.rule == RULE_NAME).unwrap();
        assert_eq!(diag.severity, LintSeverity::Info);
    }

    #[test]
    fn does_not_trigger_on_bound_options_variable() {
        let diags = lint(
            r#"
pipeline default(task) {
    const opts = {loop_until_done: true}
    agent_loop(task, "system", opts)
}
"#,
        );
        assert_eq!(count_rule(&diags), 0, "diags: {diags:?}");
    }

    #[test]
    fn does_not_trigger_on_typed_constructor_call() {
        let diags = lint(
            r#"
pipeline default(task) {
    agent_loop(task, "system", agent_preset("repair", {tools: []}))
}
"#,
        );
        assert_eq!(count_rule(&diags), 0, "diags: {diags:?}");
    }

    #[test]
    fn does_not_trigger_on_empty_dict_literal() {
        let diags = lint(
            r#"
pipeline default(task) {
    agent_loop(task, "system", {})
}
"#,
        );
        assert_eq!(count_rule(&diags), 0, "diags: {diags:?}");
    }

    #[test]
    fn triggers_on_workflow_execute_options_slot_only() {
        let diags = lint(
            r#"
pipeline default(task) {
    workflow_execute(task, {stages: [{id: "one", kind: "stage"}]}, nil, {trace: true})
}
"#,
        );
        assert_eq!(count_rule(&diags), 1, "diags: {diags:?}");
        let diag = diags.iter().find(|d| d.rule == RULE_NAME).unwrap();
        assert!(
            diag.message.contains("workflow_execute"),
            "msg: {}",
            diag.message
        );
    }

    #[test]
    fn does_not_trigger_on_workflow_graph_literal() {
        let diags = lint(
            r#"
pipeline default(task) {
    workflow_execute(task, {stages: [{id: "one", kind: "stage"}]})
}
"#,
        );
        assert_eq!(count_rule(&diags), 0, "diags: {diags:?}");
    }

    #[test]
    fn triggers_inside_nested_bodies() {
        let diags = lint(
            r"
fn helper(task) {
    if true {
        return agent_loop(task, nil, {max_iterations: 3})
    }
    return nil
}
",
        );
        assert_eq!(count_rule(&diags), 1, "diags: {diags:?}");
    }

    #[test]
    fn does_not_trigger_on_unrelated_callee() {
        let diags = lint(
            r#"
pipeline default(task) {
    llm_call(task, nil, {provider: "mock"})
}
"#,
        );
        assert_eq!(count_rule(&diags), 0, "diags: {diags:?}");
    }
}
