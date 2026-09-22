//! Evaluator behavior, measured through the mock backend.
//!
//! The instrument these tests read is the mock's request counter. A claim that
//! a refusal "makes zero provider requests" is only worth something because
//! the same counter reads one when a dispatch does happen, so every
//! zero-request test is paired with a dispatch that proves the counter moves.

use std::collections::BTreeMap;
use std::sync::Arc;

use super::answer::{AnswerBody, ConfidenceKind, EvidenceKind};
use super::backend::{
    ConfidenceProvenance, DecisionTransportError, RawAnswer, RawDecisionResponse, RefusalReason,
};
use super::contract::{DecisionContract, DecisionLimits, DecisionProtocol, DecisionQuestionKind};
use super::mock::MockDecisionBackend;
use super::question::{Question, QuestionBody, QuestionRefusalReason, QuestionSet};
use super::*;

fn contract(window: usize, max_questions: Option<usize>) -> DecisionContract {
    DecisionContract {
        protocol: DecisionProtocol::StructuredLlm,
        question_kinds: vec![
            DecisionQuestionKind::Boolean,
            DecisionQuestionKind::Choice,
            DecisionQuestionKind::Score,
        ],
        limits: DecisionLimits {
            max_questions,
            max_choice_options: 255,
            score_levels_min: 2,
            score_levels_max: 10,
            state_window_tokens: window,
            request_window_tokens: None,
        },
        input_price_per_mtok: Some(0.042),
        output_price_per_mtok: Some(0.0),
        served_model_id: "fixture".into(),
    }
}

fn boolean(id: &str) -> Question {
    Question {
        id: id.into(),
        instructions: "Is this supported?".into(),
        body: QuestionBody::Boolean,
    }
}

fn choice(id: &str, labels: &[&str]) -> Question {
    Question {
        id: id.into(),
        instructions: "Which one?".into(),
        body: QuestionBody::Choice(
            labels
                .iter()
                .map(|label| ((*label).to_string(), "because".to_string()))
                .collect(),
        ),
    }
}

fn score(id: &str, levels: &[&str]) -> Question {
    Question {
        id: id.into(),
        instructions: "How much?".into(),
        body: QuestionBody::Score(levels.iter().map(|level| (*level).to_string()).collect()),
    }
}

fn set(questions: Vec<Question>) -> QuestionSet {
    QuestionSet { questions }
}

#[test]
fn question_identity_includes_rubric_descriptions() {
    let original = set(vec![choice("action", &["read", "write"])]);
    let mut changed = original.clone();
    let QuestionBody::Choice(criteria) = &mut changed.questions[0].body else {
        unreachable!()
    };
    criteria[0].1 = "a different criterion with the same label".into();
    assert_ne!(
        question_set_digest(&original),
        question_set_digest(&changed)
    );
    assert_eq!(
        question_set_digest(&original),
        question_set_digest(&original.clone())
    );
}

fn distribution(pairs: &[(&str, f64)]) -> BTreeMap<String, f64> {
    pairs
        .iter()
        .map(|(label, probability)| ((*label).to_string(), *probability))
        .collect()
}

// --- Local admission: the refusals that must cost nothing -------------------

#[test]
fn a_choice_with_more_options_than_the_route_declares_is_refused() {
    let labels: Vec<String> = (0..256).map(|index| format!("label{index}")).collect();
    let question = Question {
        id: "wide".into(),
        instructions: "Which one?".into(),
        body: QuestionBody::Choice(
            labels
                .iter()
                .map(|label| (label.clone(), "because".to_string()))
                .collect(),
        ),
    };
    let refusal = set(vec![question])
        .admit(&contract(32_000, None))
        .expect_err("256 options exceeds the declared 255");
    assert_eq!(refusal.reason, QuestionRefusalReason::TooManyOptions);
    assert_eq!(refusal.question, "wide");

    // The positive control: one fewer option is admitted, so the refusal above
    // is about the count and not about choices in general.
    let labels: Vec<String> = (0..255).map(|index| format!("label{index}")).collect();
    let question = Question {
        id: "wide".into(),
        instructions: "Which one?".into(),
        body: QuestionBody::Choice(
            labels
                .iter()
                .map(|label| (label.clone(), "because".to_string()))
                .collect(),
        ),
    };
    set(vec![question])
        .admit(&contract(32_000, None))
        .expect("255 options is within the declared limit");
}

#[test]
fn score_levels_and_question_counts_are_admitted_against_the_declared_limits() {
    let route = contract(32_000, Some(2));
    assert_eq!(
        set(vec![score("s", &["only"])])
            .admit(&route)
            .expect_err("one level")
            .reason,
        QuestionRefusalReason::TooFewLevels
    );
    let eleven: Vec<&str> = vec!["a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k"];
    assert_eq!(
        set(vec![score("s", &eleven)])
            .admit(&route)
            .expect_err("eleven levels")
            .reason,
        QuestionRefusalReason::TooManyLevels
    );
    assert_eq!(
        set(vec![boolean("a"), boolean("b"), boolean("c")])
            .admit(&route)
            .expect_err("three questions against a declared two")
            .reason,
        QuestionRefusalReason::TooManyQuestions
    );
    set(vec![boolean("a"), boolean("b")])
        .admit(&route)
        .expect("two questions is the declared limit");
}

#[test]
fn a_question_kind_the_route_does_not_declare_is_refused() {
    let mut route = contract(32_000, None);
    route.question_kinds = vec![DecisionQuestionKind::Boolean];
    assert_eq!(
        set(vec![choice("c", &["a", "b"])])
            .admit(&route)
            .expect_err("choice against a boolean-only route")
            .reason,
        QuestionRefusalReason::UnsupportedQuestionKind
    );
    set(vec![boolean("b")])
        .admit(&route)
        .expect("boolean is declared");
}

#[test]
fn empty_instructions_are_refused_before_dispatch() {
    let question = Question {
        id: "blank".into(),
        instructions: "   ".into(),
        body: QuestionBody::Boolean,
    };
    assert_eq!(
        set(vec![question])
            .admit(&contract(32_000, None))
            .expect_err("blank instructions")
            .reason,
        QuestionRefusalReason::EmptyInstructions
    );
}

#[test]
fn runtime_vocabularies_refuse_unbound_and_duplicate_labels() {
    let route = contract(32_000, None);
    for (questions, reason) in [
        (set(vec![]), QuestionRefusalReason::EmptyQuestions),
        (
            set(vec![choice("tool", &[])]),
            QuestionRefusalReason::EmptyOptions,
        ),
        (
            set(vec![boolean("")]),
            QuestionRefusalReason::EmptyIdentifier,
        ),
        (
            set(vec![choice("tool", &[" "])]),
            QuestionRefusalReason::EmptyIdentifier,
        ),
        (
            set(vec![score("risk", &["low", "low"])]),
            QuestionRefusalReason::DuplicateLabels,
        ),
    ] {
        assert_eq!(
            questions
                .admit(&route)
                .expect_err("invalid runtime vocabulary")
                .reason,
            reason
        );
    }
    set(vec![choice("tool", &["one"])])
        .admit(&route)
        .expect("one label is a valid vocabulary");
}

// --- Answer projection ------------------------------------------------------

#[test]
fn a_boolean_confidence_is_in_the_verdict_it_selected() {
    // A confident "no" is a confident answer. Reading the yes-probability as
    // confidence would report 0.02 here and call a near-certain no uncertain.
    let answer = Answer::project(
        &boolean("q"),
        &RawAnswer::Boolean {
            probability: 0.02,
            reported_confidence: None,
            evidence: None,
        },
        ConfidenceProvenance::VendorDistribution,
    )
    .expect("boolean projects");
    assert_eq!(answer.confidence_kind, ConfidenceKind::BinaryProbability);
    assert!((answer.confidence - 0.98).abs() < 1e-9, "{answer:?}");
    assert!(matches!(
        answer.body,
        AnswerBody::Boolean { verdict: false, .. }
    ));
    assert_eq!(answer.evidence_kind, EvidenceKind::InputReference);
}

#[test]
fn a_model_reported_number_is_never_labelled_as_a_measured_distribution() {
    let answer = Answer::project(
        &choice("q", &["keep", "drop"]),
        &RawAnswer::Choice {
            probabilities: distribution(&[("keep", 0.9), ("drop", 0.1)]),
            reported_confidence: Some(0.9),
            evidence: Some("cited".into()),
        },
        ConfidenceProvenance::ModelReported,
    )
    .expect("choice projects");
    assert_eq!(answer.confidence_kind, ConfidenceKind::ModelRationale);
    assert_eq!(answer.evidence_kind, EvidenceKind::ModelRationale);

    let measured = Answer::project(
        &choice("q", &["keep", "drop"]),
        &RawAnswer::Choice {
            probabilities: distribution(&[("keep", 0.9), ("drop", 0.1)]),
            reported_confidence: Some(0.9),
            evidence: None,
        },
        ConfidenceProvenance::VendorDistribution,
    )
    .expect("choice projects");
    assert_eq!(measured.confidence_kind, ConfidenceKind::DistributionShape);
    assert_eq!(measured.evidence_kind, EvidenceKind::InputReference);
}

#[test]
fn a_distribution_that_does_not_match_its_question_is_rejected() {
    // An answer naming a label the question does not declare answered a
    // different question. Coercing it would silently change the decision.
    let rejection = Answer::project(
        &choice("q", &["keep", "drop"]),
        &RawAnswer::Choice {
            probabilities: distribution(&[("keep", 0.6), ("delete", 0.4)]),
            reported_confidence: None,
            evidence: None,
        },
        ConfidenceProvenance::VendorDistribution,
    )
    .expect_err("undeclared label");
    assert_eq!(rejection.question_id, "q");

    // A non-finite probability is not a probability.
    Answer::project(
        &boolean("q"),
        &RawAnswer::Boolean {
            probability: f64::NAN,
            reported_confidence: None,
            evidence: None,
        },
        ConfidenceProvenance::VendorDistribution,
    )
    .expect_err("NaN is not a probability");

    // A kind mismatch is a rejection, not a coerced verdict.
    Answer::project(
        &boolean("q"),
        &RawAnswer::Choice {
            probabilities: distribution(&[("keep", 1.0)]),
            reported_confidence: None,
            evidence: None,
        },
        ConfidenceProvenance::VendorDistribution,
    )
    .expect_err("choice answer to a boolean question");

    // The positive control, through the same path.
    Answer::project(
        &choice("q", &["keep", "drop"]),
        &RawAnswer::Choice {
            probabilities: distribution(&[("keep", 0.6), ("drop", 0.4)]),
            reported_confidence: None,
            evidence: None,
        },
        ConfidenceProvenance::VendorDistribution,
    )
    .expect("a matching distribution projects");
}

#[test]
fn evidence_is_bounded_on_a_character_boundary() {
    let long = "é".repeat(4000);
    let answer = Answer::project(
        &boolean("q"),
        &RawAnswer::Boolean {
            probability: 0.9,
            reported_confidence: None,
            evidence: Some(long),
        },
        ConfidenceProvenance::VendorDistribution,
    )
    .expect("boolean projects");
    assert!(answer.evidence.len() <= 2048);
    // Still valid UTF-8, which a naive byte truncation of a 2-byte code point
    // would not be.
    assert!(answer.evidence.chars().all(|c| c == 'é'));
}

// --- The generated schema ---------------------------------------------------

#[test]
fn the_generated_schema_admits_only_the_declared_questions_and_labels() {
    let questions = set(vec![
        boolean("safe"),
        choice("disposition", &["keep", "drop"]),
        score("risk", &["none", "high"]),
    ]);
    let schema = super::structured::answers_schema(&questions);
    let answers = &schema["properties"]["answers"];
    assert_eq!(answers["additionalProperties"], serde_json::json!(false));
    assert_eq!(
        answers["required"],
        serde_json::json!(["safe", "disposition", "risk"])
    );
    assert_eq!(
        answers["properties"]["disposition"]["properties"]["choice"]["enum"],
        serde_json::json!(["keep", "drop"])
    );
    assert_eq!(
        answers["properties"]["risk"]["properties"]["level"]["enum"],
        serde_json::json!(["none", "high"])
    );
    assert_eq!(
        answers["properties"]["safe"]["required"],
        serde_json::json!(["verdict", "confidence", "evidence"])
    );
    // Every answer object refuses a field the contract does not name, which is
    // what makes "extra fields are refused" a schema fact rather than a hope.
    for id in ["safe", "disposition", "risk"] {
        assert_eq!(
            answers["properties"][id]["additionalProperties"],
            serde_json::json!(false),
            "{id}"
        );
    }
}

// --- Identity ---------------------------------------------------------------

#[test]
fn the_question_set_digest_separates_sets_a_concatenation_would_collide() {
    let one = super::question_set_digest(&set(vec![choice("q", &["ab", "c"])]));
    let two = super::question_set_digest(&set(vec![choice("q", &["a", "bc"])]));
    assert_ne!(one, two);
    // Order is significant: probabilities are keyed by label and levels are
    // ordered, so a reordered set is a different evaluation.
    assert_ne!(
        super::question_set_digest(&set(vec![choice("q", &["a", "b"])])),
        super::question_set_digest(&set(vec![choice("q", &["b", "a"])])),
    );
    // A known non-null read, so the inequalities above cannot be two empty
    // digests comparing equal to nothing.
    assert_eq!(
        super::question_set_digest(&set(vec![choice("q", &["a", "b"])])),
        super::question_set_digest(&set(vec![choice("q", &["a", "b"])])),
    );
    assert_ne!(
        super::question_set_digest(&set(vec![])),
        super::question_set_digest(&set(vec![boolean("q")])),
    );
}

#[test]
fn the_policy_digest_covers_the_threshold() {
    let policy = |threshold: f64| EvaluationPolicy {
        backend: BackendKind::StructuredLlm,
        provider: "mock".into(),
        model: "fixture".into(),
        effort: "low".into(),
        temperature: 0.0,
        native_options_supplied: false,
        threshold,
        evaluation_cost_limit: 1.0,
        run_cost_limit: 1.0,
    };
    // The same answers under a different threshold are a different decision. A
    // cache keyed without it would reuse an acceptance the caller withdrew.
    assert_ne!(policy(0.8).digest(), policy(0.9).digest());
    assert_eq!(policy(0.8).digest(), policy(0.8).digest());
}

// --- Through the evaluator, with the request counter as the instrument ------

fn policy_value(threshold: f64, evaluation_cost_limit: f64) -> VmValue {
    VmValue::dict(vec![
        ("backend", VmValue::String("structured_llm".into())),
        ("provider", VmValue::String("mock".into())),
        ("model", VmValue::String("fixture".into())),
        ("effort", VmValue::String("low".into())),
        ("temperature", VmValue::Float(0.0)),
        ("threshold", VmValue::Float(threshold)),
        (
            "evaluation_cost_limit",
            VmValue::Float(evaluation_cost_limit),
        ),
        ("run_cost_limit", VmValue::Float(1.0)),
    ])
}

fn questions_value() -> VmValue {
    VmValue::dict(vec![(
        "safe",
        VmValue::dict(vec![
            ("kind", VmValue::String("boolean".into())),
            (
                "instructions",
                VmValue::String("Is this safe to run?".into()),
            ),
        ]),
    )])
}

fn answering(probability: f64) -> RawDecisionResponse {
    RawDecisionResponse {
        native_transport: None,
        answers: BTreeMap::from([(
            "safe".to_string(),
            RawAnswer::Boolean {
                probability,
                reported_confidence: None,
                evidence: Some("observed".into()),
            },
        )]),
        provenance: ConfidenceProvenance::VendorDistribution,
        served_model: Some("mock-decision".into()),
        input_tokens: Some(100),
        output_tokens: Some(0),
        physical_attempts: 1,
    }
}

/// Run one evaluation against a scripted backend and report the outcome kind,
/// the receipt, and how many requests actually reached the backend.
fn run(
    state: VmValue,
    questions: VmValue,
    policy: VmValue,
    scripted: Vec<super::mock::MockOutcome>,
) -> (String, super::receipt::EvaluationReceipt, u32) {
    let backend = Arc::new(MockDecisionBackend::scripted(scripted));
    let _guard = super::install_backend(backend.clone());
    // The route the policies below name. A window of 4096 tokens is small
    // enough that the oversized-state test crosses it and large enough that
    // every other state here fits, so one route serves all of them.
    let _route = super::install_route("mock", "fixture", contract(4096, None));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let outcome = runtime.block_on(async {
        let mut vm = crate::Vm::new();
        crate::register_vm_stdlib(&mut vm);
        let ctx = crate::vm::AsyncBuiltinCtx::for_test(vm.child_vm());
        let result = super::evaluate(
            &ctx,
            &[
                VmValue::String("triage.v1".into()),
                state,
                questions,
                policy,
            ],
        )
        .await
        .expect("evaluation returns an outcome, not an error");
        let receipts = vm
            .execution_evidence(None, Vec::new())
            .evaluation_receipts
            .expect("VM reports its evaluation journal");
        assert_eq!(
            receipts.len(),
            1,
            "child evaluation belongs to the parent execution"
        );
        assert_eq!(receipts[0].outcome_kind, result.0.kind);
        result
    });
    let receipt = super::last_receipt().expect("every evaluation records a receipt");
    (outcome.0.kind.to_string(), receipt, backend.request_count())
}

#[test]
fn an_answered_batch_records_one_request_and_a_settled_receipt() {
    // This is the control every zero-request claim below depends on: the same
    // counter, on the same path, reads one when a dispatch happens.
    let (kind, receipt, requests) = run(
        VmValue::dict(vec![("text", VmValue::String("short".into()))]),
        questions_value(),
        policy_value(0.5, 1.0),
        vec![Ok(answering(0.95))],
    );
    assert_eq!(kind, "answered");
    assert_eq!(requests, 1);
    assert_eq!(receipt.physical_attempts, 1);
    assert_eq!(receipt.served_model.as_deref(), Some("mock-decision"));
    assert_eq!(
        receipt.accounting_status,
        super::receipt::AccountingStatus::Settled
    );
    assert_eq!(receipt.questions.len(), 1);
    assert_eq!(receipt.questions[0].id, "safe");
    // The raw probability reaches the receipt before conversion, which is what
    // a calibration study reads.
    assert_eq!(
        receipt.questions[0].raw_probabilities.get("true"),
        Some(&0.95)
    );
}

#[test]
fn an_answer_below_the_threshold_is_low_confidence_not_a_verdict() {
    let (kind, _, requests) = run(
        VmValue::dict(vec![("text", VmValue::String("short".into()))]),
        questions_value(),
        policy_value(0.99, 1.0),
        vec![Ok(answering(0.6))],
    );
    assert_eq!(kind, "low_confidence");
    assert_eq!(requests, 1);
}

#[test]
fn a_partial_answer_set_is_refused_rather_than_answered() {
    let questions = VmValue::dict(vec![
        (
            "safe",
            VmValue::dict(vec![
                ("kind", VmValue::String("boolean".into())),
                ("instructions", VmValue::String("Safe?".into())),
            ]),
        ),
        (
            "risky",
            VmValue::dict(vec![
                ("kind", VmValue::String("boolean".into())),
                ("instructions", VmValue::String("Risky?".into())),
            ]),
        ),
    ]);
    let (kind, receipt, requests) = run(
        VmValue::dict(vec![("text", VmValue::String("short".into()))]),
        questions,
        policy_value(0.5, 1.0),
        // Answers one of the two declared questions.
        vec![Ok(answering(0.95))],
    );
    assert_eq!(kind, "refused");
    // The request was made and may have been billed, so it is on the receipt.
    assert_eq!(requests, 1);
    assert_eq!(receipt.physical_attempts, 1);
}

#[test]
fn a_rate_limit_is_its_own_arm_and_makes_exactly_one_request() {
    let (kind, receipt, requests) = run(
        VmValue::dict(vec![("text", VmValue::String("short".into()))]),
        questions_value(),
        policy_value(0.5, 1.0),
        vec![Err(DecisionTransportError::RateLimited {
            retry_after_ms: Some(1500),
        })],
    );
    assert_eq!(kind, "rate_limited");
    // Exactly one: nothing retried on the caller's behalf.
    assert_eq!(requests, 1);
    assert_eq!(receipt.physical_attempts, 1);

    let (kind, _, requests) = run(
        VmValue::dict(vec![("text", VmValue::String("short".into()))]),
        questions_value(),
        policy_value(0.5, 1.0),
        vec![Err(DecisionTransportError::Overloaded)],
    );
    assert_eq!(kind, "overloaded");
    assert_eq!(requests, 1);
}

#[test]
fn a_malformed_response_is_refused_without_a_second_request() {
    let (kind, _, requests) = run(
        VmValue::dict(vec![("text", VmValue::String("short".into()))]),
        questions_value(),
        policy_value(0.5, 1.0),
        vec![
            Err(DecisionTransportError::Refused {
                reason: RefusalReason::SchemaInvalid,
                diagnostic: "not an answers object".into(),
            }),
            // Scripted, but must never be consumed: a repair attempt would
            // take it and the count would read two.
            Ok(answering(0.95)),
        ],
    );
    assert_eq!(kind, "refused");
    assert_eq!(requests, 1, "no repair, no re-prompt");
}

#[test]
fn an_oversized_state_refuses_before_dispatch() {
    // A state far past the route's declared window. The pre-check decides it,
    // so the backend is never called and nothing is billed.
    let long = "word ".repeat(200_000);
    let (kind, receipt, requests) = run(
        VmValue::dict(vec![("text", VmValue::String(long.as_str().into()))]),
        questions_value(),
        policy_value(0.5, 1.0),
        vec![Ok(answering(0.95))],
    );
    assert_eq!(kind, "state_too_large");
    assert_eq!(requests, 0, "a local refusal dispatches nothing");
    assert_eq!(receipt.physical_attempts, 0);
    assert!(receipt
        .estimated_state_tokens
        .is_some_and(|tokens| tokens > 0));
    assert!(receipt.limit_tokens.is_some());
}

#[test]
fn an_invalid_question_refuses_before_dispatch() {
    let questions = VmValue::dict(vec![(
        "wide",
        VmValue::dict(vec![
            ("kind", VmValue::String("choice".into())),
            ("instructions", VmValue::String("Which?".into())),
            (
                "criteria",
                VmValue::dict(
                    (0..256)
                        .map(|index| (format!("label{index}"), VmValue::String("because".into())))
                        .collect::<Vec<_>>(),
                ),
            ),
        ]),
    )]);
    let (kind, receipt, requests) = run(
        VmValue::dict(vec![("text", VmValue::String("short".into()))]),
        questions,
        policy_value(0.5, 1.0),
        vec![Ok(answering(0.95))],
    );
    assert_eq!(kind, "question_invalid");
    assert_eq!(requests, 0, "a local refusal dispatches nothing");
    assert_eq!(receipt.physical_attempts, 0);
}

#[test]
fn an_empty_runtime_question_set_refuses_with_a_receipt_and_zero_requests() {
    let (kind, receipt, requests) = run(
        VmValue::dict(vec![("text", VmValue::String("short".into()))]),
        VmValue::dict(Vec::<(&str, VmValue)>::new()),
        policy_value(0.5, 1.0),
        vec![Ok(answering(0.95))],
    );
    assert_eq!(kind, "question_invalid");
    assert_eq!(requests, 0);
    assert_eq!(receipt.physical_attempts, 0);
}

#[test]
fn an_unconfigured_route_refuses_before_dispatch() {
    let policy = VmValue::dict(vec![
        ("backend", VmValue::String("structured_llm".into())),
        ("provider", VmValue::String("nowhere".into())),
        ("model", VmValue::String("no-such-model".into())),
        ("effort", VmValue::String("low".into())),
        ("temperature", VmValue::Float(0.0)),
        ("threshold", VmValue::Float(0.5)),
        ("evaluation_cost_limit", VmValue::Float(1.0)),
        ("run_cost_limit", VmValue::Float(1.0)),
    ]);
    let (kind, receipt, requests) = run(
        VmValue::dict(vec![("text", VmValue::String("short".into()))]),
        questions_value(),
        policy,
        vec![Ok(answering(0.95))],
    );
    assert_eq!(kind, "unavailable");
    assert_eq!(requests, 0);
    assert_eq!(receipt.physical_attempts, 0);
}

#[test]
fn an_evaluation_cost_bound_over_the_limit_refuses_before_dispatch() {
    let (kind, _, requests) = run(
        VmValue::dict(vec![("text", VmValue::String("short".into()))]),
        questions_value(),
        // A limit no admitted bound can fit under.
        policy_value(0.5, 0.0),
        vec![Ok(answering(0.95))],
    );
    assert_eq!(kind, "budget_cut");
    assert_eq!(requests, 0);
}

#[test]
fn the_single_boolean_projection_shares_the_evaluator() {
    let args = super::predicate_arguments(&[
        VmValue::String("finding.v1".into()),
        VmValue::String("Is this supported?".into()),
        VmValue::dict(vec![("text", VmValue::String("observed".into()))]),
        policy_value(0.5, 1.0),
    ])
    .expect("predicate projects onto a batch");
    assert_eq!(args.len(), 4);
    let questions = args[2].as_dict().expect("one question");
    assert_eq!(questions.len(), 1);
    let question = questions
        .get("finding.v1")
        .and_then(VmValue::as_dict)
        .expect("the site names its one question");
    assert_eq!(
        question
            .get("kind")
            .map(|kind| kind.as_str_cow().into_owned()),
        Some("boolean".to_string())
    );
    assert_eq!(
        question
            .get("instructions")
            .map(|text| text.as_str_cow().into_owned()),
        Some("Is this supported?".to_string())
    );
}

#[test]
fn the_dispatched_request_carries_the_admitted_profile_unchanged() {
    // The profile is only worth declaring if it survives to the transport.
    // This reads the backend's own log of what it was handed, rather than
    // trusting that the policy it was built from was honored.
    let backend = Arc::new(MockDecisionBackend::scripted(vec![Ok(answering(0.95))]));
    let _guard = super::install_backend(backend.clone());
    let _route = super::install_route("mock", "fixture", contract(4096, None));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    runtime.block_on(async {
        let mut vm = crate::Vm::new();
        crate::register_vm_stdlib(&mut vm);
        let ctx = crate::vm::AsyncBuiltinCtx::for_test(vm);
        super::evaluate(
            &ctx,
            &[
                VmValue::String("triage.v1".into()),
                VmValue::dict(vec![("text", VmValue::String("short".into()))]),
                questions_value(),
                policy_value(0.5, 1.0),
            ],
        )
        .await
        .expect("evaluation returns an outcome, not an error")
    });
    let seen = backend.requests();
    assert_eq!(seen.len(), 1, "one evaluation, one physical request");
    assert_eq!(seen[0].provider, "mock");
    assert_eq!(seen[0].model, "fixture");
    assert_eq!(seen[0].effort, "low");
    assert_eq!(seen[0].temperature, 0.0);
    assert_eq!(seen[0].question_ids, vec!["safe".to_string()]);
    assert_eq!(
        seen[0].state.get("text").and_then(|text| text.as_str()),
        Some("short"),
        "the backend is handed the caller's state, not a summary of it"
    );
}
