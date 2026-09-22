use super::*;
use crate::llm::decision::{
    answer::Answer,
    contract::decision_contract_for_route,
    question::{Question, QuestionSet},
};

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
            effort: "none",
            temperature: 0.0,
            evaluation_cost_limit: None,
            run_cost_limit: None,
        };
        let base = crate::llm_config::provider_config(provider)
            .unwrap()
            .base_url;
        assert_eq!(endpoint(&base, contract.protocol).unwrap(), url);
        let body = request_body(&request).unwrap();
        assert_eq!(body["model"], contract.served_model_id);
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
        let response = read_response(&request, &tape).unwrap();
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
        let contradiction = read_response(&request, &contradictory).unwrap();
        let refusal = Answer::project(
            &questions.questions[1],
            &contradiction.answers["action"],
            contradiction.provenance,
        )
        .expect_err("a vendor label must not silently invert through projection");
        assert!(refusal.diagnostic.contains("contradicts"));
        tape["answers"].as_object_mut().unwrap().remove("risk");
        assert!(matches!(
            read_response(&request, &tape),
            Err(DecisionTransportError::Refused {
                reason: RefusalReason::SchemaInvalid,
                ..
            })
        ));
        let unsupported_request = DecisionRequest {
            temperature: 0.5,
            ..request
        };
        assert!(matches!(
            request_body(&unsupported_request),
            Err(DecisionTransportError::UnsupportedOptions { .. })
        ));
    }
}
