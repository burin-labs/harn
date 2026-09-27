//! Structured internal requests retaining already admitted route authority.

use crate::value::{VmError, VmValue};

/// One logical structured batch without schema repair or a second output
/// attempt. Transport retries remain governed by the inherited route policy
/// and shared admission ledger. This is not a fresh credential lookup or a
/// provider-specific compaction caller.
pub(crate) async fn run_prepared_structured_call(
    active: &crate::llm::api::LlmCallOptions,
    prompt: String,
    schema: serde_json::Value,
    role: &str,
    stage: &str,
) -> Result<VmValue, VmError> {
    let options = prepare(active, prompt, schema, role, stage)?;
    let mut controls = crate::value::DictMap::new();
    controls.insert(crate::value::intern_key("schema_retries"), VmValue::Int(0));
    let outcome = Box::pin(crate::llm::call::execute_llm_call_outcome(
        None,
        options,
        Some(controls),
        None,
        None,
    ))
    .await?;
    if outcome.errors.is_empty() {
        Ok(super::envelope_success(&outcome, false, None))
    } else {
        Ok(super::envelope_failure(
            &outcome,
            super::classify_main_failure(&outcome),
            false,
        ))
    }
}

fn prepare(
    active: &crate::llm::api::LlmCallOptions,
    prompt: String,
    mut schema: serde_json::Value,
    role: &str,
    stage: &str,
) -> Result<crate::llm::api::LlmCallOptions, VmError> {
    use crate::llm::capabilities::StructuredOutputStrategy;
    let strategy = crate::llm::capabilities::lookup(&active.provider, &active.model)
        .structured_output_strategy;
    if strategy == StructuredOutputStrategy::Unsupported {
        return Err(VmError::Runtime(
            "prepared structured request: route declares structured output unsupported".into(),
        ));
    }
    crate::schema::normalize_provider_json_schema(&mut schema);
    let schema_value = crate::stdlib::json_to_vm_value(&schema);
    let prompt = if strategy == StructuredOutputStrategy::PromptValidation {
        super::prompt_with_schema_contract(&prompt, &schema_value)
    } else {
        prompt
    };
    let mut options = active.isolated_request(prompt, role, stage)?;
    options.output_validation = Some("error".into());
    options.output_schema = Some(schema.clone());
    if strategy != StructuredOutputStrategy::PromptValidation {
        options.output_format = crate::llm::api::OutputFormat::JsonSchema {
            schema,
            strict: true,
        };
    }
    Ok(options)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prepared_schema_reaches_shared_context_and_budget_projection() {
        use crate::llm::cost::{check_llm_preflight_budget, LlmBudgetEnvelope};
        use crate::llm::cost_context::project_llm_call_context_breakdown;

        let active = crate::llm::api::LlmCallOptions {
            provider: "openai".into(),
            model: "gpt-4.1-mini".into(),
            max_tokens: 100,
            ..Default::default()
        };
        let schema = serde_json::json!({
            "type": "object", "additionalProperties": false,
            "required": ["text"], "properties": {
                "text": {"type": "string", "description": "preserve source ".repeat(2000)}
            }
        });
        let mut prepared = prepare(
            &active,
            "selected source".into(),
            schema,
            "compaction",
            "rewrite",
        )
        .unwrap();
        let context = project_llm_call_context_breakdown(&prepared);
        let schema_tokens = context
            .segments
            .iter()
            .find(|segment| segment.id == "output_schema")
            .unwrap()
            .tokens;
        assert!(schema_tokens > 1000, "schema projection must actually fire");
        prepared.budget = Some(LlmBudgetEnvelope {
            max_input_tokens: Some(context.input_tokens - schema_tokens),
            ..Default::default()
        });
        assert!(check_llm_preflight_budget(&prepared).is_err());
        prepared.budget.as_mut().unwrap().max_input_tokens = Some(context.input_tokens);
        assert!(check_llm_preflight_budget(&prepared).is_ok());
    }
}
