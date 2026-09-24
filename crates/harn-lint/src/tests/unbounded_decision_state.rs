use super::*;

const RULE: &str = "unbounded-native-decision-state";

const POLICY: &str = r#"{
    backend: "native_decision",
    provider: "typesafe",
    model: "jev-1",
    effort: "low",
    temperature: 0.0,
    threshold: 0.5,
    evaluation_cost_limit: 0.01,
    run_cost_limit: 0.1,
  }"#;

fn source(state_type: &str, backend: &str) -> String {
    format!(
        r#"
pipeline main(harness: Harness) {{
  const state: {state_type} = fetch_state()
  const policy = {policy}
  return harness.llm.evaluate("site.v1", state, {{}}, policy)
}}
"#,
        policy = POLICY.replace("native_decision", backend),
    )
}

#[test]
fn a_list_state_on_a_native_decision_route_is_reported() {
    let diagnostics = lint_source(&source("list<string>", "native_decision"));
    assert_eq!(count_rule(&diagnostics, RULE), 1);
}

#[test]
fn a_string_state_on_a_native_decision_route_is_reported() {
    let diagnostics = lint_source(&source("string", "native_decision"));
    assert_eq!(count_rule(&diagnostics, RULE), 1);
}

#[test]
fn a_record_carrying_an_unbounded_field_is_reported() {
    let diagnostics = lint_source(&source("{id: int, notes: list<string>}", "native_decision"));
    assert_eq!(count_rule(&diagnostics, RULE), 1);
}

#[test]
fn an_alias_of_an_unbounded_type_is_reported_through_the_alias() {
    let diagnostics = lint_source(
        r#"
type Transcript = list<string>

pipeline main(harness: Harness) {
  const state: Transcript = fetch_state()
  const policy = {
    backend: "native_decision",
    provider: "typesafe",
    model: "jev-1",
    effort: "low",
    temperature: 0.0,
    threshold: 0.5,
    evaluation_cost_limit: 0.01,
    run_cost_limit: 0.1,
  }
  return harness.llm.evaluate("site.v1", state, {}, policy)
}
"#,
    );
    assert_eq!(count_rule(&diagnostics, RULE), 1);
}

#[test]
fn a_bounded_record_is_not_reported() {
    let diagnostics = lint_source(&source(
        r#"{severity: "low" | "high", reopened: bool, age_days: int}"#,
        "native_decision",
    ));
    assert!(!has_rule(&diagnostics, RULE));
}

/// The negative control on the trigger. The same unbounded state is fine on a
/// structured-LLM route, which refuses an oversized state before dispatch. A
/// rule that reported both would be reporting the input type, not the
/// combination this warning is about.
#[test]
fn an_unbounded_state_on_a_structured_llm_route_is_not_reported() {
    let diagnostics = lint_source(&source("list<string>", "structured_llm"));
    assert!(!has_rule(&diagnostics, RULE));
}

/// Silence when the type is not visible is deliberate and stated on the rule,
/// so it is pinned rather than left to be discovered as a surprise.
#[test]
fn an_undeclared_state_is_not_reported() {
    let diagnostics = lint_source(
        r#"
pipeline main(harness: Harness) {
  const state = fetch_state()
  const policy = {
    backend: "native_decision",
    provider: "typesafe",
    model: "jev-1",
    effort: "low",
    temperature: 0.0,
    threshold: 0.5,
    evaluation_cost_limit: 0.01,
    run_cost_limit: 0.1,
  }
  return harness.llm.evaluate("site.v1", state, {}, policy)
}
"#,
    );
    assert!(!has_rule(&diagnostics, RULE));
}

#[test]
fn the_single_boolean_projection_reads_its_own_input_position() {
    let diagnostics = lint_source(
        r#"
pipeline main(harness: Harness) {
  const input: list<string> = fetch_state()
  const policy = {
    backend: "native_decision",
    provider: "typesafe",
    model: "jev-1",
    effort: "low",
    temperature: 0.0,
    threshold: 0.5,
    evaluation_cost_limit: 0.01,
    run_cost_limit: 0.1,
  }
  return harness.llm.evaluate_predicate("site.v1", "Safe?", input, policy)
}
"#,
    );
    assert_eq!(count_rule(&diagnostics, RULE), 1);
}
