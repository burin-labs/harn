use super::*;
use crate::TypeCheckFacts;

const POLICY: &str = r#"{
    backend: "structured_llm", provider: "mock", model: "fixture",
    effort: "low", temperature: 0.0, threshold: 0.8,
    evaluation_cost_limit: 0.01, run_cost_limit: 0.08,
}"#;

fn facts(body: &str) -> TypeCheckFacts {
    let source = format!("fn main(harness: Harness) {{\nconst policy = {POLICY}\n{body}\n}}");
    let program = Parser::new(Lexer::new(&source).tokenize().unwrap())
        .parse()
        .unwrap();
    TypeChecker::new().check_with_facts(&program, &source)
}

fn call(input: &str) -> String {
    format!(
        r#"harness.llm.evaluate_predicate("finding.v1", "Is this supported?", {input}, policy)"#
    )
}

fn errors(facts: &TypeCheckFacts) -> Vec<Code> {
    facts
        .diagnostics
        .iter()
        .filter(|d| d.severity == DiagnosticSeverity::Error)
        .map(|d| d.code)
        .collect()
}

#[test]
fn predicate_library_call_emits_typed_site_and_narrows_verdict() {
    let facts = facts(&format!(
        r#"
        const result = {}
        match result.kind {{
          "verdict" -> {{ if result.value.verdict {{ harness.stdio.println("accepted") }} }}
          _ -> {{ harness.stdio.println(result.receipt) }}
        }}
    "#,
        call(r#"{observation: "test failed", count: 1}"#)
    ));
    assert!(errors(&facts).is_empty(), "{:?}", facts.diagnostics);
    assert_eq!(facts.predicate_sites.len(), 1);
    let site = &facts.predicate_sites[0];
    assert_eq!(site.id, "finding.v1");
    assert_eq!(site.questions.len(), 1);
    assert_eq!(site.questions[0].instructions, "Is this supported?");
    assert!(matches!(&site.input_type, crate::TypeExpr::Shape(fields) if fields.len() == 2));
    assert!(site.start < site.end && site.line > 1);
}

#[test]
fn predicate_outcome_cannot_select_a_boolean_branch() {
    for use_site in [
        "if result {}",
        "while result { break }",
        "const inverted = !result",
        "const branch = result && true",
    ] {
        let facts = facts(&format!(
            "const result = {}\n{use_site}",
            call("{value: 1}")
        ));
        assert!(
            errors(&facts).contains(&Code::PredicateBooleanUse),
            "{use_site}: {:?}",
            facts.diagnostics
        );
    }
}

#[test]
fn predicate_variant_fields_require_narrowing() {
    for use_site in [
        "if result.value.verdict {}",
        "if result[\"value\"].verdict {}",
        "const key = \"value\"; harness.stdio.println(result[key])",
        "const {value} = result; if value.verdict {}",
    ] {
        let facts = facts(&format!(
            "const result = {}\n{use_site}",
            call("{value: 1}")
        ));
        assert!(
            errors(&facts).contains(&Code::PredicateOutcomeUnnarrowed),
            "{use_site}: {:?}",
            facts.diagnostics
        );
    }
}

#[test]
fn nullable_predicate_outcome_cannot_select_a_boolean_branch() {
    use crate::builtin_signatures::TyExt;
    let outcome = crate::format_type(&harn_builtin_meta::predicate::OUTCOME.to_type_expr());
    let facts = facts(&format!(
        "const result: {outcome} | nil = {}\nif result {{}}",
        call("{value: 1}")
    ));
    assert!(
        errors(&facts).contains(&Code::PredicateBooleanUse),
        "{:?}",
        facts.diagnostics
    );
}

#[test]
fn predicate_input_refuses_functions_and_gradual_values() {
    for (declaration, input) in [
        ("", "fn() { return true }"),
        ("const input: any = {value: 1}", "input"),
        ("const input: dict = {value: 1}", "input"),
        ("", "{nested: fn() { return true }}"),
        ("", "harness"),
    ] {
        let facts = facts(&format!(
            "{declaration}\nconst result = {}\nharness.stdio.println(result.kind)",
            call(input)
        ));
        assert!(
            errors(&facts).contains(&Code::PredicateInputInvalid),
            "{input}: {:?}",
            facts.diagnostics
        );
        assert!(
            facts.predicate_sites.is_empty(),
            "invalid input must not become an admitted site"
        );
    }
}

#[test]
fn predicate_outcome_cannot_be_silently_discarded() {
    for statement in [
        call("{value: 1}"),
        format!("const _ = {}", call("{value: 1}")),
        format!("const ignored = {}", call("{value: 1}")),
    ] {
        let facts = facts(&statement);
        assert!(
            errors(&facts).contains(&Code::PredicateOutcomeUnused),
            "{statement}: {:?}",
            facts.diagnostics
        );
    }
}

#[test]
fn predicate_unused_check_resolves_shadowing_and_closure_capture() {
    let shadow = facts(&format!(
        r#"
        const result = {}
        if true {{ const result = "other"; harness.stdio.println(result) }}
    "#,
        call("{value: 1}")
    ));
    assert!(errors(&shadow).contains(&Code::PredicateOutcomeUnused));
    let captured = facts(&format!(
        r"
        const result = {}
        const consume = fn() {{ harness.stdio.println(result.kind) }}
        consume()
    ",
        call("{value: 1}")
    ));
    assert!(errors(&captured).is_empty(), "{:?}", captured.diagnostics);
}

#[test]
fn predicate_site_requires_literal_unique_identity() {
    let duplicate = facts(&format!(
        "const one = {}\nconst two = {}\nharness.stdio.println(one.kind, two.kind)",
        call("{value: 1}"),
        call("{value: 2}")
    ));
    assert!(errors(&duplicate).contains(&Code::PredicateSiteInvalid));
    let dynamic = facts(
        r#"
        const id = "finding.v1"
        const result = harness.llm.evaluate_predicate(id, "Question?", {value: 1}, policy)
        harness.stdio.println(result.kind)
    "#,
    );
    assert!(errors(&dynamic).contains(&Code::PredicateSiteInvalid));
}

#[test]
fn ordinary_truthy_records_are_not_predicate_outcomes() {
    let facts = facts("const ordinary = {kind: \"verdict\", receipt: \"text\"}\nif ordinary {} ");
    assert!(errors(&facts).is_empty(), "{:?}", facts.diagnostics);
    assert!(facts.predicate_sites.is_empty());
}

#[test]
fn predicate_method_cannot_escape_site_checks_as_a_callable_value() {
    let escaped = facts(
        "const evaluate = harness.llm.evaluate_predicate\nevaluate(\"x\", \"Q?\", fn() {}, policy)",
    );
    assert!(
        errors(&escaped).contains(&Code::PredicateSiteInvalid),
        "{:?}",
        escaped.diagnostics
    );
    let optional = facts("const result = harness.llm?.evaluate_predicate(\"x\", \"Q?\", {value: 1}, policy)\nharness.stdio.println(result)");
    assert!(
        errors(&optional).contains(&Code::PredicateSiteInvalid),
        "{:?}",
        optional.diagnostics
    );
}

#[test]
fn predicate_method_requires_a_resolved_receiver_but_does_not_reserve_ordinary_methods() {
    let erased = facts("const llm: any = harness.llm\nconst result = llm.evaluate_predicate(\"x\", \"Q?\", fn() {}, policy)\nharness.stdio.println(result)");
    assert!(
        errors(&erased).contains(&Code::PredicateSiteInvalid),
        "{:?}",
        erased.diagnostics
    );
    let ordinary = facts("const ordinary = {evaluate_predicate: fn() { return true }}\nif ordinary.evaluate_predicate() {}");
    assert!(errors(&ordinary).is_empty(), "{:?}", ordinary.diagnostics);
    assert!(ordinary.predicate_sites.is_empty());
}

#[test]
fn predicate_outcome_is_checked_through_an_annotated_helper() {
    use crate::builtin_signatures::TyExt;
    let outcome = crate::format_type(&harn_builtin_meta::predicate::OUTCOME.to_type_expr());
    let policy = crate::format_type(&harn_builtin_meta::predicate::POLICY.to_type_expr());
    let source = format!(
        r#"
      type Outcome = {outcome}
      fn assess(llm: HarnessLlm, policy: {policy}) -> Outcome {{
        return llm.evaluate_predicate("helper.v1", "Supported?", {{value: 1}}, policy)
      }}
      fn main(harness: Harness) {{
        const policy = {POLICY}
        const result = assess(harness.llm, policy)
        if result {{}}
      }}
    "#
    );
    let program = Parser::new(Lexer::new(&source).tokenize().unwrap())
        .parse()
        .unwrap();
    let facts = TypeChecker::new().check_with_facts(&program, &source);
    assert_eq!(facts.predicate_sites.len(), 1);
    assert!(
        errors(&facts).contains(&Code::PredicateBooleanUse),
        "{:?}",
        facts.diagnostics
    );
    assert!(
        !errors(&facts).contains(&Code::PredicateOutcomeUnused),
        "returning to the caller transfers disposition: {:?}",
        facts.diagnostics
    );
}

#[test]
fn predicate_sites_survive_the_analysis_cache() {
    use crate::analysis::{AnalysisDatabase, SourceId, SourceVersion, TypeCheckConfig};
    let source = format!("fn main(harness: Harness) {{ const policy = {POLICY}\nconst result = {}\nharness.stdio.println(result.kind) }}", call("{value: 1}"));
    let mut database = AnalysisDatabase::new();
    let id = SourceId::new("predicate-cache");
    database.set_source(id.clone(), source, SourceVersion(1));
    let cold = database.typecheck(&id, TypeCheckConfig::new()).unwrap();
    let warm = database.typecheck(&id, TypeCheckConfig::new()).unwrap();
    assert_eq!(
        database.stats().typecheck_runs,
        1,
        "warm query must actually hit the cache"
    );
    assert_eq!(cold.predicate_sites.len(), 1);
    assert_eq!(
        serde_json::to_value(cold.predicate_sites).unwrap(),
        serde_json::to_value(warm.predicate_sites).unwrap()
    );
}

#[test]
fn predicate_route_is_captured_before_name_shadowing() {
    let source = format!(
        r#"
fn main(harness: Harness) {{
  const policy = {POLICY}
  const captured = policy
  if true {{
    const policy = {{backend: "structured_llm", provider: "other", model: "other", effort: "low", temperature: 0.0, threshold: 0.8, evaluation_cost_limit: 0.0, run_cost_limit: 0.0}}
    const result = harness.llm.evaluate_predicate("captured", "Supported?", {{value: 1}}, captured)
    harness.stdio.println(result.kind)
    harness.stdio.println(policy.model)
  }}
}}
"#
    );
    let program = Parser::new(Lexer::new(&source).tokenize().unwrap())
        .parse()
        .unwrap();
    let facts = TypeChecker::new().check_with_facts(&program, &source);
    assert!(errors(&facts).is_empty(), "{:?}", facts.diagnostics);
    assert_eq!(facts.predicate_sites.len(), 1);
    let route = facts.predicate_sites[0].model_route.as_ref().unwrap();
    assert_eq!((&*route.provider, &*route.model), ("mock", "fixture"));
}

#[test]
fn predicate_route_does_not_reuse_a_shadowed_constant() {
    let source = format!(
        r#"
const policy = {POLICY}
fn assess(harness: Harness, policy: {{backend: "structured_llm", provider: string, model: string, effort: string, temperature: float, threshold: float, evaluation_cost_limit: float, run_cost_limit: float}}) {{
  const result = harness.llm.evaluate_predicate("parameter", "Supported?", {{value: 1}}, policy)
  harness.stdio.println(result.kind)
}}
"#
    );
    let program = Parser::new(Lexer::new(&source).tokenize().unwrap())
        .parse()
        .unwrap();
    let facts = TypeChecker::new().check_with_facts(&program, &source);
    assert_eq!(facts.predicate_sites.len(), 1);
    assert!(facts.predicate_sites[0].model_route.is_none());
}

#[test]
fn predicate_route_is_not_constant_after_a_conditional_record_write() {
    let facts = facts(&format!(
        r#"
      if true {{ policy.model = "changed" }}
      const result = {}
      harness.stdio.println(result.kind)
    "#,
        call("{value: 1}")
    ));
    assert_eq!(facts.predicate_sites.len(), 1);
    assert!(
        facts.predicate_sites[0].model_route.is_none(),
        "a field write must invalidate the captured policy route"
    );
}

#[test]
fn a_shadowed_record_write_preserves_the_outer_predicate_policy() {
    let facts = facts(&format!(
        r#"
      if true {{ let policy = {{model: "local"}}; policy.model = "changed" }}
      const result = {}
      harness.stdio.println(result.kind)
    "#,
        call("{value: 1}")
    ));
    assert_eq!(facts.predicate_sites.len(), 1);
    assert_eq!(
        facts.predicate_sites[0].model_route.as_ref().unwrap().model,
        "fixture"
    );
}

#[test]
fn a_captured_record_write_invalidates_the_predicate_policy() {
    let facts = facts(&format!(
        r#"
      const change = fn() {{ policy.model = "changed" }}
      change()
      const result = {}
      harness.stdio.println(result.kind)
    "#,
        call("{value: 1}")
    ));
    assert_eq!(facts.predicate_sites.len(), 1);
    assert!(
        facts.predicate_sites[0].model_route.is_none(),
        "a captured field write must invalidate the policy route"
    );
}

#[test]
fn a_closure_local_write_preserves_the_outer_predicate_policy() {
    let facts = facts(&format!(
        r#"
      const change = fn() {{ let policy = {{model: "local"}}; policy.model = "changed" }}
      change()
      const result = {}
      harness.stdio.println(result.kind)
    "#,
        call("{value: 1}")
    ));
    assert_eq!(facts.predicate_sites.len(), 1);
    assert_eq!(
        facts.predicate_sites[0].model_route.as_ref().unwrap().model,
        "fixture"
    );
}
