//! One physical structured call, with the checkpoint defaults overridden.
//!
//! The ordinary structured path is forgiving on purpose: it re-prompts on a
//! schema miss, escalates the output cap on truncation, and can fail over to
//! another route. An evaluation must not do any of that, because a caller
//! wanting escalation declares a second evaluation whose receipt and cost stay
//! visible. So every one of those knobs is pinned here, at the boundary that
//! owns the profile, rather than left to a default someone could change.

use serde_json::Value as JsonValue;

use crate::llm::usage::LlmUsage;
use crate::llm::{extract_llm_options, llm_error_message};
use crate::value::{VmError, VmValue};

use super::backend::{DecisionRequest, DecisionTransportError, RefusalReason};
use super::contract::StructuredOutputStrategy;

/// Output tokens an evaluation may spend. The answers are labels, confidences,
/// and short citations; a cap this size is a real bound, not a formality.
const MAX_EVALUATION_OUTPUT_TOKENS: i64 = 2048;

pub struct StructuredResponse {
    pub usage: LlmUsage,
    pub data: JsonValue,
    pub served_model: Option<String>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub physical_attempts: u32,
}

/// Dispatch exactly once.
///
/// `schema_retries: 0` is the load-bearing setting: the ordinary structured
/// helper defaults it to 3, which would make one evaluation cost up to four
/// billed requests and hide a malformed answer behind a re-prompt.
pub async fn one_structured_call(
    request: &DecisionRequest<'_>,
    prompt: &str,
    system: &str,
    schema: &JsonValue,
) -> Result<StructuredResponse, DecisionTransportError> {
    let (extracted, options) = prepare(request, prompt, system, schema)?;
    // Boxed so the awaited call's state does not live in this frame. The
    // structured request is large enough that inlining it crosses the stack
    // frame budget, and an evaluation runs on the same stack as its caller.
    let outcome = Box::pin(crate::llm::call::execute_llm_call_outcome(
        None,
        extracted,
        Some(options),
        None,
        None,
    ))
    .await
    .map_err(|error| classify(&error))?;
    if outcome.usages.is_empty() {
        return Err(DecisionTransportError::TransportFailed {
            diagnostic: "completed structured call has no usage ledger".into(),
        });
    }
    let usage = LlmUsage::aggregate(&outcome.usages);
    let served_model = outcome
        .vm_result
        .as_dict()
        .and_then(|fields| fields.get("model"))
        .map(|model| model.as_str_cow().into_owned());
    if !outcome.errors.is_empty() {
        return Err(DecisionTransportError::Refused {
            reason: RefusalReason::SchemaInvalid,
            diagnostic: outcome.errors.join("; "),
        }
        .with_usage(usage, served_model));
    }
    read_response(&outcome.vm_result, usage.clone())
        .map_err(|error| error.with_usage(usage, served_model))
}

/// Build the one request. Split from the dispatch so the request's own
/// scratch space is released before the call is awaited.
fn prepare(
    request: &DecisionRequest<'_>,
    prompt: &str,
    system: &str,
    schema: &JsonValue,
) -> Result<(crate::llm::api::LlmCallOptions, crate::value::DictMap), DecisionTransportError> {
    let mut options = serde_json::json!({
        "provider": request.provider,
        "model": request.model,
        "temperature": 0.0,
        "effort": request.effort,
        "max_tokens": MAX_EVALUATION_OUTPUT_TOKENS,
        "output": {"schema": schema, "strict": true, "validation": "error"},
        // One physical request. No repair, no re-prompt, no failover.
        "schema_retries": 0,
    });
    let mut budget = serde_json::Map::new();
    if let Some(limit) = request.evaluation_cost_limit {
        budget.insert("max_cost_usd".into(), serde_json::json!(limit));
    }
    if let Some(limit) = request.run_cost_limit {
        budget.insert("admission".into(), serde_json::json!("conservative"));
        budget.insert("total_budget_usd".into(), serde_json::json!(limit));
    }
    if !budget.is_empty() {
        options["budget"] = JsonValue::Object(budget);
    }
    let mut args = [
        VmValue::string(prompt),
        VmValue::string(system),
        crate::schema::json_to_vm_value(&options),
    ];
    let prompt_validation = request.contract.structured_output_strategy
        == Some(StructuredOutputStrategy::PromptValidation);
    let schema_value = crate::schema::json_to_vm_value(schema);
    if prompt_validation {
        crate::llm::structured_envelope::apply_prompt_mode_structured_transport(
            &mut args,
            &schema_value,
        );
    }
    let mut extracted =
        extract_llm_options(&args).map_err(|error| unsupported_or_failed(&error))?;
    if prompt_validation {
        extracted = crate::llm::structured_envelope::install_prompt_mode_validation(
            extracted,
            &schema_value,
        );
    }
    let options_dict =
        args[2]
            .as_dict()
            .cloned()
            .ok_or_else(|| DecisionTransportError::TransportFailed {
                diagnostic: "evaluation options are not a record".into(),
            })?;
    Ok((extracted, options_dict))
}

/// Read the validated envelope. A response that carried no validated data is
/// a schema refusal, not an empty answer.
fn read_response(
    response: &VmValue,
    settlement: LlmUsage,
) -> Result<StructuredResponse, DecisionTransportError> {
    let fields = response
        .as_dict()
        .ok_or_else(|| DecisionTransportError::Refused {
            reason: RefusalReason::SchemaInvalid,
            diagnostic: "evaluation response is not a record".into(),
        })?;
    let data = fields
        .get("data")
        .map(crate::llm::helpers::vm_value_to_json)
        .ok_or_else(|| DecisionTransportError::Refused {
            reason: RefusalReason::SchemaInvalid,
            diagnostic: "evaluation response carried no validated data".into(),
        })?;
    let usage = fields.get("usage").and_then(VmValue::as_dict);
    let tokens = |key: &str| {
        usage
            .and_then(|usage| usage.get(key))
            .and_then(VmValue::as_int)
            .and_then(|count| u64::try_from(count).ok())
    };
    Ok(StructuredResponse {
        usage: settlement,
        data,
        served_model: fields
            .get("model")
            .map(|model| model.as_str_cow().into_owned()),
        input_tokens: tokens("input_tokens"),
        output_tokens: tokens("output_tokens"),
        physical_attempts: 1,
    })
}

fn unsupported_or_failed(error: &VmError) -> DecisionTransportError {
    DecisionTransportError::UnsupportedOptions {
        diagnostic: llm_error_message(error),
    }
}

/// Map one transport error onto exactly one outcome arm.
///
/// The reason this reads the structured error fields rather than the message
/// is that error text is routinely mislabelled, and collapsing a rate limit
/// into a generic failure is exactly the confusion the typed arms exist to
/// prevent.
fn classify(error: &VmError) -> DecisionTransportError {
    let message = llm_error_message(error);
    let fields = match error {
        VmError::Thrown(VmValue::Dict(fields)) => Some(fields.clone()),
        _ => None,
    };
    let field = |key: &str| {
        fields
            .as_ref()
            .and_then(|fields| fields.get(key))
            .map(|value| value.as_str_cow().into_owned())
    };
    let status = fields
        .as_ref()
        .and_then(|fields| fields.get("status"))
        .and_then(VmValue::as_int);
    let reason = field("reason").unwrap_or_default();
    let category = field("category").unwrap_or_default();

    match (status, reason.as_str(), category.as_str()) {
        (Some(429), _, _) | (_, "rate_limit", _) | (_, _, "rate_limit") => {
            DecisionTransportError::RateLimited {
                retry_after_ms: fields
                    .as_ref()
                    .and_then(|fields| fields.get("retry_after_ms"))
                    .and_then(VmValue::as_int)
                    .and_then(|ms| u64::try_from(ms).ok()),
            }
        }
        (Some(529) | Some(503), _, _) | (_, "overloaded", _) => DecisionTransportError::Overloaded,
        (Some(400), _, _) if message.contains("max_tokens_exceeded") => {
            DecisionTransportError::StateTooLarge {
                provider_reason: message,
                limit_tokens: None,
            }
        }
        (_, _, "schema_validation") => DecisionTransportError::Refused {
            reason: RefusalReason::SchemaInvalid,
            diagnostic: message,
        },
        (_, "content_filter", _) | (_, "refusal", _) => DecisionTransportError::Refused {
            reason: RefusalReason::ProviderRefusal,
            diagnostic: message,
        },
        (_, "max_tokens", _) | (_, "length", _) => DecisionTransportError::Refused {
            reason: RefusalReason::OutputTruncated,
            diagnostic: message,
        },
        (_, _, "budget_exceeded") => DecisionTransportError::LocalAdmissionDenied {
            diagnostic: message,
        },
        _ if message.contains("max_tokens_exceeded") => DecisionTransportError::StateTooLarge {
            provider_reason: message,
            limit_tokens: None,
        },
        _ => DecisionTransportError::TransportFailed {
            diagnostic: message,
        },
    }
}
