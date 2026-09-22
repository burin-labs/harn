//! Native decision transport. One send, with redirects and HTTP retries disabled.
//! Wire references: https://docs.typesafe.ai/api and
//! https://vercel.com/docs/ai-gateway/modalities/evaluation.
//! OpenRouter's TypeSafe shape was reached on 2026-09-22 at /api/alpha/decisions.

use std::collections::BTreeMap;

use serde_json::{json, Map, Value};

use super::backend::{
    ConfidenceProvenance, DecisionBackend, DecisionRequest, DecisionTransportError, RawAnswer,
    RawDecisionResponse, RefusalReason,
};
use super::contract::DecisionProtocol;
use super::question::QuestionBody;

pub struct NativeDecisionBackend;

pub(super) fn privacy_plan(
    request: &DecisionRequest<'_>,
) -> crate::llm::api::data_controls::DataControlsPlan {
    use crate::llm_config::{DataControlDialect, DataPosture};
    let dialect = match request.contract.protocol {
        DecisionProtocol::TypesafeSystemOne => DataControlDialect::TypesafeSystemOne,
        DecisionProtocol::VercelEvaluate => DataControlDialect::VercelEvaluate,
        DecisionProtocol::OpenrouterDecisions => DataControlDialect::OpenrouterDecisions,
        DecisionProtocol::StructuredLlm => DataControlDialect::OpenAiSse,
    };
    crate::llm::api::data_controls::resolve(
        request.provider,
        request.model,
        dialect,
        DataPosture::StrictestAvailable,
    )
}

#[cfg(test)]
#[path = "native_tests.rs"]
mod tests;

fn unsupported(message: &str) -> DecisionTransportError {
    DecisionTransportError::UnsupportedOptions {
        diagnostic: message.into(),
    }
}

fn malformed(message: &str) -> DecisionTransportError {
    DecisionTransportError::Refused {
        reason: RefusalReason::SchemaInvalid,
        diagnostic: message.into(),
    }
}

pub(super) fn request_body(request: &DecisionRequest<'_>) -> Result<Value, DecisionTransportError> {
    if !request.contract.protocol.is_native() {
        return Err(unsupported(
            "native backend requires a native decision protocol",
        ));
    }
    if request.temperature != 0.0 || !matches!(request.effort, "" | "none") {
        return Err(unsupported(
            "native decision routes do not support temperature or effort",
        ));
    }
    if !matches!(
        request.state,
        Value::String(_) | Value::Array(_) | Value::Object(_)
    ) {
        return Err(unsupported(
            "native decision state must be a string, object, or array",
        ));
    }
    let mut questions = Map::new();
    for question in &request.questions.questions {
        let mut value = json!({"instructions": question.instructions});
        match &question.body {
            QuestionBody::Boolean => {
                value["type"] = json!(if request.contract.protocol
                    == DecisionProtocol::VercelEvaluate
                {
                    "boolean"
                } else {
                    "noul"
                });
            }
            QuestionBody::Choice(criteria) => {
                value["type"] = json!("choice");
                value["criteria"] = json!(criteria.iter().cloned().collect::<BTreeMap<_, _>>());
            }
            QuestionBody::Score(levels) => {
                value["type"] = json!("score");
                value["criteria"] = json!(levels);
            }
        }
        questions.insert(question.id.clone(), value);
    }
    let mut body = json!({"model": request.contract.served_model_id, "state": request.state, "questions": questions});
    match request.contract.protocol {
        DecisionProtocol::VercelEvaluate => {
            body["providerOptions"] = json!({"gateway": {"only": ["typesafe-ai"]}});
        }
        DecisionProtocol::OpenrouterDecisions => {
            body["provider"] = json!({"allow_fallbacks": false});
        }
        _ => {}
    }
    privacy_plan(request).write_body(&mut body);
    Ok(body)
}

pub(super) fn endpoint(
    base: &str,
    protocol: DecisionProtocol,
) -> Result<String, DecisionTransportError> {
    let base = base.trim_end_matches('/');
    Ok(match protocol {
        DecisionProtocol::TypesafeSystemOne => format!("{base}/systemone"),
        DecisionProtocol::VercelEvaluate => format!("{base}/evaluate"),
        DecisionProtocol::OpenrouterDecisions => format!(
            "{}/alpha/decisions",
            base.strip_suffix("/v1").unwrap_or(base)
        ),
        DecisionProtocol::StructuredLlm => {
            return Err(unsupported("structured protocol is not native"))
        }
    })
}

#[async_trait::async_trait]
impl DecisionBackend for NativeDecisionBackend {
    async fn evaluate(
        &self,
        request: DecisionRequest<'_>,
    ) -> Result<RawDecisionResponse, DecisionTransportError> {
        crate::llm::ensure_real_llm_allowed(request.provider)
            .map_err(|_| DecisionTransportError::AuthorityDenied)?;
        let body = request_body(&request)?;
        let definition = crate::llm_config::provider_config(request.provider)
            .ok_or_else(|| unsupported("decision provider is not configured"))?;
        let base = crate::llm_config::resolve_base_url(&definition);
        let url = endpoint(&base, request.contract.protocol)?;
        let key = crate::llm::resolve_api_key(request.provider)
            .map_err(|_| DecisionTransportError::AuthorityDenied)?;
        let allow_hosts = crate::egress::configured_provider_private_allow_host(&base)
            .into_iter()
            .collect::<Vec<_>>();
        let builder = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .connect_timeout(std::time::Duration::from_secs(10))
            .timeout(std::time::Duration::from_mins(1));
        let client =
            crate::egress::install_ssrf_guard_with_private_host_allowlist(builder, &allow_hosts)
                .build()
                .map_err(|_| unsupported("cannot configure native decision transport"))?;
        let mut pending = client.post(url).bearer_auth(key).json(&body);
        for (name, value) in privacy_plan(&request).headers {
            pending = pending.header(name, value);
        }
        let response =
            pending
                .send()
                .await
                .map_err(|_| DecisionTransportError::TransportFailed {
                    diagnostic: "native decision request failed".into(),
                })?;
        let status = response.status().as_u16();
        let retry_after_ms = response
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok())
            .and_then(|seconds| seconds.checked_mul(1000));
        if status == 429 {
            return Err(DecisionTransportError::RateLimited { retry_after_ms });
        }
        if matches!(status, 503 | 529) {
            return Err(DecisionTransportError::Overloaded);
        }
        let data: Value = response
            .json()
            .await
            .map_err(|_| malformed("native decision response is not JSON"))?;
        if !(200..300).contains(&status) {
            if data.pointer("/error/code").and_then(Value::as_str) == Some("max_tokens_exceeded") {
                return Err(DecisionTransportError::StateTooLarge {
                    provider_reason: "max_tokens_exceeded".into(),
                    limit_tokens: None,
                });
            }
            return Err(DecisionTransportError::Refused {
                reason: RefusalReason::ProviderRefusal,
                diagnostic: format!("native decision provider returned HTTP {status}"),
            });
        }
        read_response(&request, &data)
    }
}

pub(super) fn read_response(
    request: &DecisionRequest<'_>,
    data: &Value,
) -> Result<RawDecisionResponse, DecisionTransportError> {
    let fields = data
        .get("answers")
        .and_then(Value::as_object)
        .ok_or_else(|| malformed("missing answers"))?;
    if fields.len() != request.questions.questions.len() {
        return Err(malformed("answer question set differs from request"));
    }
    let mut answers = BTreeMap::new();
    for question in &request.questions.questions {
        let value = fields
            .get(&question.id)
            .ok_or_else(|| malformed("missing question answer"))?;
        let number = |key| {
            value
                .get(key)
                .and_then(Value::as_f64)
                .ok_or_else(|| malformed("missing numeric answer field"))
        };
        let probabilities = || -> Result<BTreeMap<String, f64>, DecisionTransportError> {
            serde_json::from_value(
                value
                    .get("probabilities")
                    .cloned()
                    .ok_or_else(|| malformed("missing probabilities"))?,
            )
            .map_err(|_| malformed("invalid probabilities"))
        };
        let kind = value.get("type").and_then(Value::as_str);
        let raw = match &question.body {
            QuestionBody::Boolean => {
                let vercel = request.contract.protocol == DecisionProtocol::VercelEvaluate;
                if kind != Some(if vercel { "boolean" } else { "noul" }) {
                    return Err(malformed("boolean answer kind mismatch"));
                }
                RawAnswer::Boolean {
                    probability: number(if vercel { "probability" } else { "noul" })?,
                    reported_confidence: None,
                    evidence: None,
                }
            }
            QuestionBody::Choice(_) => {
                if kind != Some("choice") {
                    return Err(malformed("choice answer kind mismatch"));
                }
                let selected = value
                    .get("choice")
                    .and_then(Value::as_str)
                    .ok_or_else(|| malformed("missing choice label"))?;
                let distribution = probabilities()?;
                if !distribution.contains_key(selected) {
                    return Err(malformed("choice label is absent from distribution"));
                }
                RawAnswer::Choice {
                    selected: Some(selected.to_string()),
                    probabilities: distribution,
                    reported_confidence: Some(number("confidence")?),
                    evidence: None,
                }
            }
            QuestionBody::Score(levels) => {
                if kind != Some("score") {
                    return Err(malformed("score answer kind mismatch"));
                }
                let indexed = probabilities()?;
                if indexed.len() != levels.len() {
                    return Err(malformed("score distribution length mismatch"));
                }
                let score = number("score")?;
                if !(0.0..=(levels.len().saturating_sub(1) as f64)).contains(&score) {
                    return Err(malformed("score lies outside the declared scale"));
                }
                let mapped = levels
                    .iter()
                    .enumerate()
                    .map(|(index, label)| {
                        indexed
                            .get(&index.to_string())
                            .map(|p| (label.clone(), *p))
                            .ok_or_else(|| malformed("score distribution index missing"))
                    })
                    .collect::<Result<BTreeMap<_, _>, _>>()?;
                RawAnswer::Score {
                    probabilities: mapped,
                    score: Some(score),
                    reported_confidence: Some(number("confidence")?),
                    evidence: None,
                }
            }
        };
        answers.insert(question.id.clone(), raw);
    }
    let vercel = request.contract.protocol == DecisionProtocol::VercelEvaluate;
    let usage = data.get("usage");
    Ok(RawDecisionResponse {
        native_transport: Some(super::receipt::NativeTransportReceipt {
            data_controls: privacy_plan(request).receipt,
            provider_attempts_reported: data
                .pointer("/providerMetadata/gateway/routing/totalProviderAttemptCount")
                .and_then(Value::as_u64),
            final_provider_reported: data
                .pointer("/providerMetadata/gateway/routing/finalProvider")
                .and_then(Value::as_str)
                .map(str::to_owned),
        }),
        answers,
        provenance: ConfidenceProvenance::VendorDistribution,
        served_model: Some(
            data.get("model")
                .and_then(Value::as_str)
                .ok_or_else(|| malformed("missing served model"))?
                .into(),
        ),
        input_tokens: usage
            .and_then(|u| {
                u.get(if vercel {
                    "inputTokens"
                } else {
                    "input_tokens"
                })
            })
            .and_then(Value::as_u64),
        output_tokens: usage
            .and_then(|u| {
                u.get(if vercel {
                    "outputTokens"
                } else {
                    "output_tokens"
                })
            })
            .and_then(Value::as_u64),
        physical_attempts: 1,
    })
}
