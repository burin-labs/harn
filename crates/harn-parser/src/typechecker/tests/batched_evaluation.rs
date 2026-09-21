//! `harness.llm.evaluate` checks: the question set is read at the call, and
//! each answer is typed from its own question.
//!
//! The builders are declared locally rather than imported from `std/predicate`
//! on purpose. A question is recognized by the contract shape it carries, not
//! by the spelling of the callee, and these tests fail if that ever becomes a
//! name match. The stdlib import path is covered by the conformance fixtures
//! and the CLI end-to-end manifest test.

use super::*;
use crate::TypeCheckFacts;

const BUILDERS: &str = r#"
fn boolean(instructions: string) -> {kind: "boolean", instructions: string} {
  return {kind: "boolean", instructions: instructions}
}
fn choice(
  instructions: string,
  criteria: dict<string, string>,
) -> {kind: "choice", instructions: string, criteria: dict<string, string>} {
  return {kind: "choice", instructions: instructions, criteria: criteria}
}
fn score(
  instructions: string,
  levels: list<string>,
) -> {kind: "score", instructions: string, levels: list<string>} {
  return {kind: "score", instructions: instructions, levels: levels}
}
"#;

const POLICY: &str = r#"{
    backend: "structured_llm", provider: "mock", model: "fixture",
    effort: "low", temperature: 0.0, threshold: 0.8,
    evaluation_cost_limit: 0.01, run_cost_limit: 0.08,
}"#;

const QUESTIONS: &str = r#"{
      disposition: choice("Keep, reword, or drop?", {
        keep: "Still load-bearing",
        reword: "Useful but verbose",
        drop: "Superseded",
      }),
      risk: score("How much blast radius?", ["none", "low", "high"]),
      safe: boolean("Safe to run without asking?"),
    }"#;

fn facts(body: &str) -> TypeCheckFacts {
    let source =
        format!("{BUILDERS}\nfn main(harness: Harness) {{\nconst policy = {POLICY}\n{body}\n}}");
    let program = Parser::new(Lexer::new(&source).tokenize().unwrap())
        .parse()
        .unwrap();
    TypeChecker::new().check_with_facts(&program, &source)
}

fn call(questions: &str) -> String {
    format!(r#"harness.llm.evaluate("triage.v1", {{text: "window"}}, {questions}, policy)"#)
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
fn batched_evaluation_records_every_question_and_its_labels() {
    let facts = facts(&format!(
        r#"
        const answers = {}
        match answers.kind {{
          "answered" -> {{ harness.stdio.println(answers.value.disposition.evidence) }}
          _ -> {{ harness.stdio.println(answers.receipt) }}
        }}
    "#,
        call(QUESTIONS)
    ));
    assert!(errors(&facts).is_empty(), "{:?}", facts.diagnostics);
    assert_eq!(facts.predicate_sites.len(), 1);
    let site = &facts.predicate_sites[0];
    assert_eq!(site.id, "triage.v1");
    assert_eq!(site.kind, crate::PredicateSiteKind::Evaluation);
    let questions: Vec<_> = site
        .questions
        .iter()
        .map(|question| (question.id.as_str(), question.kind, question.labels.len()))
        .collect();
    assert_eq!(
        questions,
        vec![
            ("disposition", crate::PredicateQuestionKind::Choice, 3),
            ("risk", crate::PredicateQuestionKind::Score, 3),
            ("safe", crate::PredicateQuestionKind::Boolean, 0),
        ]
    );
    assert_eq!(site.questions[0].labels, ["keep", "reword", "drop"]);
    assert_eq!(
        site.questions[2].instructions,
        "Safe to run without asking?"
    );
}

#[test]
fn a_choice_answer_matches_exhaustively_over_exactly_its_criteria_keys() {
    // The positive control: every declared label, and nothing else, satisfies
    // the match. Without it, the negative cases below could both be failing
    // for an unrelated reason.
    let exhaustive = facts(&format!(
        r#"
        const answers = {}
        match answers.kind {{
          "answered" -> {{
            match answers.value.disposition.choice {{
              "keep" -> {{ harness.stdio.println("keep") }}
              "reword" -> {{ harness.stdio.println("reword") }}
              "drop" -> {{ harness.stdio.println("drop") }}
            }}
          }}
          _ -> {{ harness.stdio.println(answers.receipt) }}
        }}
    "#,
        call(QUESTIONS)
    ));
    assert!(
        errors(&exhaustive).is_empty(),
        "{:?}",
        exhaustive.diagnostics
    );

    // A label outside the criteria leaves a declared label uncovered, so the
    // match over the answer is not exhaustive. A widened `choice: string`
    // would accept this silently.
    let outside = facts(&format!(
        r#"
        const answers = {}
        match answers.kind {{
          "answered" -> {{
            match answers.value.disposition.choice {{
              "keep" -> {{ harness.stdio.println("keep") }}
              "reword" -> {{ harness.stdio.println("reword") }}
              "delete" -> {{ harness.stdio.println("delete") }}
            }}
          }}
          _ -> {{ harness.stdio.println(answers.receipt) }}
        }}
    "#,
        call(QUESTIONS)
    ));
    assert!(
        errors(&outside).contains(&Code::NonExhaustiveMatch),
        "{:?}",
        outside.diagnostics
    );
}

#[test]
fn a_score_answer_level_is_the_literal_union_of_its_declared_levels() {
    let facts = facts(&format!(
        r#"
        const answers = {}
        match answers.kind {{
          "answered" -> {{
            match answers.value.risk.level {{
              "none" -> {{ harness.stdio.println("none") }}
              "low" -> {{ harness.stdio.println("low") }}
              "high" -> {{ harness.stdio.println("high") }}
            }}
          }}
          _ -> {{ harness.stdio.println(answers.receipt) }}
        }}
    "#,
        call(QUESTIONS)
    ));
    assert!(errors(&facts).is_empty(), "{:?}", facts.diagnostics);
}

#[test]
fn a_question_set_that_cannot_be_read_is_refused() {
    for questions in [
        // Not a literal at the call.
        "policy_questions",
        // No questions at all.
        "{}",
        // Criteria assembled elsewhere: the labels are gone by check time.
        r#"{a: choice("Which?", assembled)}"#,
        // A choice with no labels types no answer.
        r#"{a: choice("Which?", {})}"#,
        // A duplicate label would collide in the probability map.
        r#"{a: choice("Which?", {keep: "one", keep: "two"})}"#,
        // Instructions are part of the site identity.
        r"{a: boolean(subject)}",
        // Not a question at all.
        r#"{a: {kind: "vibes", instructions: "?"}}"#,
    ] {
        let facts = facts(&format!(
            "const assembled: dict<string, string> = {{}}\n\
             const policy_questions: dict<string, {{kind: \"boolean\", instructions: string}}> = {{}}\n\
             const subject = \"?\"\n\
             const answers = {}\n\
             match answers.kind {{ _ -> {{ harness.stdio.println(answers.receipt) }} }}",
            call(questions)
        ));
        assert!(
            errors(&facts).contains(&Code::PredicateQuestionSetInvalid),
            "{questions}: {:?}",
            facts.diagnostics
        );
        assert!(
            facts.predicate_sites.is_empty(),
            "{questions}: a refused question set declares no site"
        );
    }
}

#[test]
fn a_batched_outcome_is_not_a_boolean_and_cannot_be_discarded() {
    let boolean_use = facts(&format!(
        "const answers = {}\nif answers {{}}",
        call(QUESTIONS)
    ));
    assert!(
        errors(&boolean_use).contains(&Code::PredicateBooleanUse),
        "{:?}",
        boolean_use.diagnostics
    );
    let unused = facts(&format!("const _ = {}", call(QUESTIONS)));
    assert!(
        errors(&unused).contains(&Code::PredicateOutcomeUnused),
        "{:?}",
        unused.diagnostics
    );
    let unnarrowed = facts(&format!(
        "const answers = {}\nharness.stdio.println(answers.value.safe.evidence)",
        call(QUESTIONS)
    ));
    assert!(
        errors(&unnarrowed).contains(&Code::PredicateOutcomeUnnarrowed),
        "{:?}",
        unnarrowed.diagnostics
    );
}

#[test]
fn an_ordinary_field_named_evaluate_is_data_not_an_erased_capability() {
    // `evaluate` is a common field name, unlike `evaluate_predicate`. Reading
    // it off an untyped record is ordinary data access and must not be read as
    // a capability call whose receiver was erased.
    let field = facts(
        "const config: dict = {}\n\
         const handler = config?.evaluate\n\
         harness.stdio.println(handler)",
    );
    assert!(errors(&field).is_empty(), "{:?}", field.diagnostics);
    assert!(field.predicate_sites.is_empty());

    // The erasure that matters is still refused: an evaluation has to be
    // called, and a call through an untyped receiver hides its site.
    let erased = facts(
        "const llm: any = harness.llm\n\
         const answers = llm.evaluate(\"x\", {value: 1}, {a: boolean(\"Q?\")}, policy)\n\
         harness.stdio.println(answers)",
    );
    assert!(
        errors(&erased).contains(&Code::PredicateSiteInvalid),
        "{:?}",
        erased.diagnostics
    );
}

#[test]
fn two_sites_cannot_share_one_evaluation_id() {
    let facts = facts(&format!(
        "const first = {}\nconst second = {}\n\
         match first.kind {{ _ -> {{ harness.stdio.println(first.receipt) }} }}\n\
         match second.kind {{ _ -> {{ harness.stdio.println(second.receipt) }} }}",
        call(QUESTIONS),
        call(QUESTIONS)
    ));
    assert!(
        errors(&facts).contains(&Code::PredicateSiteInvalid),
        "{:?}",
        facts.diagnostics
    );
}
