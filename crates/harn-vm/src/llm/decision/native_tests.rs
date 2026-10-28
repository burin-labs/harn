use super::*;
use crate::llm::decision::{
    answer::Answer,
    contract::decision_contract_for_route,
    question::{BooleanCriteria, Question, QuestionSet},
};

#[test]
fn native_decision_does_not_write_project_invented_privacy_controls() {
    use crate::llm_config::{
        DataControlDef, DataControlDialect, DataControlEffect, DataControlLocation,
        DataControlValue, ProvidersConfig,
    };
    use crate::security::{
        EnvironmentPolicyKind, GrantAudience, GrantSourceSpec, GrantSpec, SessionEnvironment,
    };
    struct Clear;
    impl Drop for Clear {
        fn drop(&mut self) {
            crate::llm_config::clear_user_overrides();
        }
    }
    let _clear = Clear;
    let environment = SessionEnvironment::launch(
        EnvironmentPolicyKind::Granted,
        vec![GrantSpec {
            name: "host-boundary".into(),
            source: GrantSourceSpec::Literal {
                value: r#"{"reach":"any_hosted","allow_training_discounts":false}"#.into(),
            },
            expose_as_env: Some(crate::llm::api::inference_boundary::HOST_BOUNDARY_ENV.into()),
            for_command: None,
            expose_to: GrantAudience::InProcess,
        }],
        &|_| None,
    )
    .unwrap();
    let _session =
        crate::security::session_environment::declare_session_environment_if_absent(environment);
    let mut provider = crate::llm_config::provider_config("vercel_ai_gateway").unwrap();
    provider
        .data_controls
        .as_mut()
        .unwrap()
        .request_controls
        .push(DataControlDef {
            location: DataControlLocation::Body,
            name: "providerOptions.gateway.fakeNoTraining".into(),
            value: DataControlValue::Bool(true),
            effect: DataControlEffect::Training,
            applies_to: vec![DataControlDialect::VercelEvaluate],
            caveat: None,
        });
    let mut overlay = ProvidersConfig::default();
    overlay
        .providers
        .insert("vercel_ai_gateway".into(), provider);
    crate::llm_config::set_user_overrides(Some(overlay));
    let questions = QuestionSet {
        questions: vec![Question {
            id: "safe".into(),
            instructions: "Is it safe?".into(),
            body: QuestionBody::Boolean,
        }],
    };
    let state = json!("Read README.md");
    let contract =
        decision_contract_for_route("vercel_ai_gateway", "vercel/typesafe-ai/jev").unwrap();
    let request = DecisionRequest {
        provider: "vercel_ai_gateway",
        model: "vercel/typesafe-ai/jev",
        state: &state,
        questions: &questions,
        contract: &contract,
        effort: None,
        temperature: None,
        max_tokens: None,
        top_p: None,
        seed: None,
        timeout_ms: None,
        evaluation_cost_limit: None,
        run_cost_limit: None,
    };
    let body = request_body(&request).unwrap();
    assert_eq!(
        body["providerOptions"]["gateway"]["disallowPromptTraining"],
        json!(true)
    );
    assert!(body["providerOptions"]["gateway"]
        .get("fakeNoTraining")
        .is_none());
}

#[test]
fn gateway_decisions_pin_the_serving_provider_and_refuse_an_absent_pin() {
    let questions = QuestionSet {
        questions: vec![Question {
            id: "safe".into(),
            instructions: "Is the command read only?".into(),
            body: QuestionBody::Boolean,
        }],
    };
    let state = json!("Read README.md");
    for (model, upstream) in [
        ("vercel/typesafe-ai/jev", "typesafe-ai"),
        ("vercel/convaiinnovations/laya", "boundless"),
        ("vercel/liquid/d1-ultra", "liquid"),
        ("vercel/perplexity/pplx-decider-v1.1-27b", "perplexity"),
    ] {
        let contract = decision_contract_for_route("vercel_ai_gateway", model).unwrap();
        let request = DecisionRequest {
            provider: "vercel_ai_gateway",
            model,
            state: &state,
            questions: &questions,
            contract: &contract,
            effort: None,
            temperature: None,
            max_tokens: None,
            top_p: None,
            seed: None,
            timeout_ms: None,
            evaluation_cost_limit: None,
            run_cost_limit: None,
        };
        let body = request_body(&request).unwrap();
        assert_eq!(
            body["providerOptions"]["gateway"]["only"],
            json!([upstream])
        );
        assert_eq!(body["model"], contract.served_model_id);
        let mut unpinned_contract = contract.clone();
        unpinned_contract.upstream_provider = None;
        let unpinned = DecisionRequest {
            contract: &unpinned_contract,
            ..request
        };
        assert!(matches!(
            request_body(&unpinned),
            Err(DecisionTransportError::UnsupportedOptions { .. })
        ));
    }
}

#[test]
fn native_routes_project_wire_answers_and_refuse_partial_tapes() {
    let questions = QuestionSet {
        questions: vec![
            Question {
                id: "safe".into(),
                instructions: "Was the command read only?".into(),
                body: QuestionBody::Boolean,
            },
            Question {
                id: "action".into(),
                instructions: "What happened?".into(),
                body: QuestionBody::Choice(vec![
                    ("read".into(), "read file".into()),
                    ("write".into(), "changed file".into()),
                ]),
            },
            Question {
                id: "risk".into(),
                instructions: "Risk of data loss?".into(),
                body: QuestionBody::Score(vec!["none".into(), "high".into()]),
            },
        ],
    };
    let state = json!("The read-only command printed README.md.");
    for (provider, model, url) in [
        (
            "typesafe",
            "typesafe/jev-latest",
            "https://api.typesafe.ai/v1/systemone",
        ),
        (
            "vercel_ai_gateway",
            "vercel/typesafe-ai/jev",
            "https://ai-gateway.vercel.sh/v1/evaluate",
        ),
        (
            "openrouter",
            "openrouter/typesafe/jev-1.13",
            "https://openrouter.ai/api/alpha/decisions",
        ),
    ] {
        let contract =
            decision_contract_for_route(provider, model).expect("catalog native contract");
        let request = DecisionRequest {
            provider,
            model,
            state: &state,
            questions: &questions,
            contract: &contract,
            effort: Some("none"),
            temperature: Some(0.0),
            max_tokens: None,
            top_p: None,
            seed: None,
            timeout_ms: None,
            evaluation_cost_limit: None,
            run_cost_limit: None,
        };
        let base = crate::llm_config::provider_config(provider)
            .unwrap()
            .base_url;
        assert_eq!(endpoint(&base, contract.protocol).unwrap(), url);
        let body = request_body(&request).unwrap();
        assert_eq!(body["model"], contract.served_model_id);
        let receipt = privacy_plan(&request).receipt;
        assert_eq!(receipt.requested_posture, "strictest_available");
        match contract.protocol {
            DecisionProtocol::TypesafeSystemOne => {
                assert_eq!(
                    receipt.outcome,
                    crate::llm::api::DataControlsOutcome::NoControlAvailable
                );
                assert_eq!(
                    receipt.control_scope,
                    Some(crate::llm_config::DataControlScope::Account)
                );
                assert_eq!(
                    receipt.training_default,
                    Some(crate::llm_config::TrainingDefault::DoesNotTrain)
                );
                assert!(receipt.applied.is_empty());
                assert!(body.get("providerOptions").is_none());
            }
            DecisionProtocol::VercelEvaluate => {
                assert_eq!(
                    receipt.outcome,
                    crate::llm::api::DataControlsOutcome::Applied
                );
                assert_eq!(receipt.applied.len(), 2);
                assert_eq!(
                    body["providerOptions"]["gateway"]["zeroDataRetention"],
                    json!(true)
                );
                assert_eq!(
                    body["providerOptions"]["gateway"]["disallowPromptTraining"],
                    json!(true)
                );
            }
            DecisionProtocol::OpenrouterDecisions => {
                assert_eq!(
                    receipt.outcome,
                    crate::llm::api::DataControlsOutcome::NoControlAvailable
                );
                assert!(receipt.applied.is_empty());
                assert_eq!(body["provider"], json!({"allow_fallbacks": false}));
            }
            DecisionProtocol::StructuredLlm => unreachable!(),
        }
        assert_eq!(
            body["questions"]["risk"]["criteria"],
            json!(["none", "high"])
        );
        let vercel = contract.protocol == DecisionProtocol::VercelEvaluate;
        let mut tape = json!({"model": "jev-served-identity", "answers": {
            "safe": if vercel { json!({"type": "boolean", "probability": 0.9}) } else { json!({"type": "noul", "noul": 0.9}) },
            "action": {"type": "choice", "choice": "read", "probabilities": {"read": 0.9, "write": 0.1}, "confidence": 0.8},
            "risk": {"type": "score", "score": 0.1, "probabilities": {"0": 0.9, "1": 0.1}, "confidence": 0.8}
        }, "usage": if vercel { json!({"inputTokens": 100, "outputTokens": 12}) } else { json!({"input_tokens": 100, "output_tokens": 12}) }});
        let recorded = match contract.protocol {
            DecisionProtocol::VercelEvaluate => Some(include_str!("fixtures/vercel-evaluate.json")),
            DecisionProtocol::OpenrouterDecisions => {
                Some(include_str!("fixtures/openrouter-decisions.json"))
            }
            _ => None, // Direct TypeSafe is a documented-shape fixture, not a live capture.
        };
        if let Some(recorded) = recorded {
            let recorded: Value = serde_json::from_str(recorded).unwrap();
            assert_eq!(
                body, recorded["request"],
                "wire request differs from live capture"
            );
            tape = recorded["response"].clone();
        }
        let mut rubric_questions = questions.clone();
        rubric_questions.questions[0].body = QuestionBody::BooleanWithCriteria(BooleanCriteria {
            yes: "Only reads".into(),
            no: "Mutates files".into(),
        });
        let rubric_request = DecisionRequest {
            questions: &rubric_questions,
            ..request
        };
        let rubric_body = request_body(&rubric_request).unwrap();
        assert_eq!(
            rubric_body["questions"]["safe"]["criteria"],
            json!({"true":"Only reads", "false":"Mutates files"})
        );
        let response = read_response(&request, &tape, None).unwrap();
        assert_eq!(
            response.input_tokens,
            Some(if recorded.is_some() { 356 } else { 100 })
        );
        assert_eq!(response.served_model.as_deref(), tape["model"].as_str());
        for question in &questions.questions {
            let answer = Answer::project(
                question,
                &response.answers[&question.id],
                response.provenance,
            )
            .unwrap();
            assert_eq!(
                answer.evidence_kind,
                crate::llm::decision::answer::EvidenceKind::InputReference
            );
        }
        let mut contradictory = tape.clone();
        contradictory["answers"]["action"]["choice"] = json!("write");
        contradictory["answers"]["action"]["probabilities"] = json!({"read": 0.9, "write": 0.1});
        let contradiction = read_response(&request, &contradictory, None).unwrap();
        let refusal = Answer::project(
            &questions.questions[1],
            &contradiction.answers["action"],
            contradiction.provenance,
        )
        .expect_err("a vendor label must not silently invert through projection");
        assert!(refusal.diagnostic.contains("contradicts"));
        tape["answers"].as_object_mut().unwrap().remove("risk");
        assert!(matches!(
            read_response(&request, &tape, None),
            Err(DecisionTransportError::Refused {
                reason: RefusalReason::SchemaInvalid,
                ..
            })
        ));
        let unsupported_request = DecisionRequest {
            temperature: Some(0.5),
            ..request
        };
        assert!(matches!(
            request_body(&unsupported_request),
            Err(DecisionTransportError::UnsupportedOptions { .. })
        ));
    }
}
