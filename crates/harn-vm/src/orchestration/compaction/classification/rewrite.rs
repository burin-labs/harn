use crate::value::{VmDictExt, VmError, VmValue};
use crate::vm::AsyncBuiltinCtx;

use super::{ClassificationConfig, ClassificationItem, ClassificationRewrite};

/// A failed rewrite preserves the original. Cancellation remains a control
/// event and propagates to the transactional lifecycle instead of being
/// interpreted as an unavailable optimization.
pub(crate) async fn rewrite_items(
    ctx: &AsyncBuiltinCtx,
    config: &ClassificationConfig,
    active: Option<&crate::llm::api::LlmCallOptions>,
    items: &[ClassificationItem],
) -> Result<Option<Vec<ClassificationRewrite>>, VmError> {
    if items.is_empty() {
        return Ok(Some(Vec::new()));
    }
    let result = if let Some(fixture) = &config.rewrite_fixture {
        let VmValue::Closure(fixture) = fixture else {
            return Err(VmError::Runtime(
                "compaction rewrite fixture must be a closure".into(),
            ));
        };
        let payload = crate::stdlib::json_to_vm_value(&serde_json::json!(items));
        let mut vm = ctx.child_vm();
        let result = vm.call_closure_pub(fixture, &[payload]).await;
        ctx.forward_output(&vm.take_output());
        result
    } else if let Some(active) = active {
        run_rewrite(active, items).await
    } else {
        return Ok(None);
    };
    match result {
        Ok(VmValue::Nil) => Ok(None),
        Ok(value) => serde_json::from_value(crate::llm::vm_value_to_json(&value))
            .map(Some)
            .map_err(|error| VmError::Runtime(format!("invalid compaction rewrites: {error}"))),
        Err(error)
            if error.is_uncatchable_control_flow()
                || crate::value::error_to_category(&error)
                    == crate::value::ErrorCategory::Cancelled =>
        {
            Err(error)
        }
        Err(_) => Ok(None),
    }
}

async fn run_rewrite(
    active: &crate::llm::api::LlmCallOptions,
    items: &[ClassificationItem],
) -> Result<VmValue, VmError> {
    let mut bindings = crate::value::DictMap::new();
    bindings.put_str("items_json", serde_json::json!(items).to_string());
    let prompt = crate::stdlib::template::render_stdlib_prompt_asset(
        "agent/prompts/compaction_rewrite.harn.prompt",
        Some(&bindings),
    )?;
    let schema = serde_json::json!({
        "type": "object",
        "properties": {"rewrites": {"type": "array", "items": {
            "type": "object",
            "properties": {"index": {"type": "integer"}, "text": {"type": "string"}},
            "required": ["index", "text"],
            "additionalProperties": false
        }}},
        "required": ["rewrites"],
        "additionalProperties": false
    });
    let envelope = Box::pin(
        crate::llm::structured_envelope::run_prepared_structured_call(
            active,
            prompt,
            schema,
            "compaction",
            "rewrite",
        ),
    )
    .await?;
    let fields = envelope.as_dict().ok_or_else(|| {
        VmError::Runtime("compaction rewrite returned no structured envelope".into())
    })?;
    if !matches!(fields.get("ok"), Some(VmValue::Bool(true))) {
        return Ok(VmValue::Nil);
    }
    Ok(fields
        .get("data")
        .and_then(VmValue::as_dict)
        .and_then(|data| data.get("rewrites"))
        .cloned()
        .unwrap_or(VmValue::Nil))
}
