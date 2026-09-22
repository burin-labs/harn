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
