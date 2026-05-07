use std::rc::Rc;

use crate::stdlib::registration::{
    async_builtin, register_builtin_group, AsyncBuiltin, BuiltinGroup,
};
use crate::value::{VmError, VmValue};
use crate::vm::{Vm, VmBuiltinArity};

const SELF_CERTAINTY_PROMPT_PREFIX: &str = "Repeat exactly the text between <text> tags. Do not add, omit, or alter any characters.\n<text>\n";
const SELF_CERTAINTY_PROMPT_SUFFIX: &str = "\n</text>";

pub(crate) fn register_rerank_builtins(vm: &mut Vm) {
    register_builtin_group(vm, RERANK_PRIMITIVES);
}

const RERANK_ASYNC_PRIMITIVES: &[AsyncBuiltin] =
    &[
        async_builtin!("__llm_self_certainty", self_certainty_builtin)
            .signature("__llm_self_certainty(text, options?)")
            .arity(VmBuiltinArity::Range { min: 1, max: 2 })
            .doc("Return length-normalized confidence from token log probabilities."),
    ];

const RERANK_PRIMITIVES: BuiltinGroup<'static> = BuiltinGroup::new()
    .category("llm.rerank")
    .async_(RERANK_ASYNC_PRIMITIVES);

async fn self_certainty_builtin(args: Vec<VmValue>) -> Result<VmValue, VmError> {
    let text = match args.first() {
        Some(VmValue::String(text)) => text.to_string(),
        Some(VmValue::Dict(dict)) => {
            if let Some(logprobs) = dict.get("logprobs") {
                let values = vm_logprobs(logprobs)?;
                return Ok(VmValue::Float(score_logprobs(&values)?));
            }
            return Err(VmError::Runtime(
                "__llm_self_certainty: first argument must be text or an llm result with logprobs"
                    .to_string(),
            ));
        }
        Some(other) => {
            return Err(VmError::Runtime(format!(
                "__llm_self_certainty: text must be a string, got {}",
                other.type_name()
            )))
        }
        None => String::new(),
    };

    let options = args
        .get(1)
        .and_then(VmValue::as_dict)
        .cloned()
        .unwrap_or_default();
    if let Some(value) = options.get("logprobs") {
        if !matches!(value, VmValue::Bool(_) | VmValue::Nil) {
            let values = vm_logprobs(value)?;
            return Ok(VmValue::Float(score_logprobs(&values)?));
        }
    }
    if text.trim().is_empty() {
        return Err(VmError::Runtime(
            "__llm_self_certainty: text must be non-empty when logprobs are not supplied"
                .to_string(),
        ));
    }

    let mut call_options = options;
    call_options.insert("logprobs".to_string(), VmValue::Bool(true));
    call_options
        .entry("stream".to_string())
        .or_insert(VmValue::Bool(false));
    call_options
        .entry("temperature".to_string())
        .or_insert(VmValue::Float(0.0));
    call_options
        .entry("max_tokens".to_string())
        .or_insert(VmValue::Int(estimate_echo_tokens(&text)));

    let prompt = format!("{SELF_CERTAINTY_PROMPT_PREFIX}{text}{SELF_CERTAINTY_PROMPT_SUFFIX}");
    let opts = super::helpers::extract_llm_options(&[
        VmValue::String(Rc::from(prompt)),
        VmValue::Nil,
        VmValue::Dict(Rc::new(call_options)),
    ])?;
    let result = super::api::vm_call_llm_full(&opts).await?;
    if result.logprobs.is_empty() {
        return Err(VmError::Runtime(format!(
            "__llm_self_certainty: provider '{}' model '{}' did not return token logprobs",
            result.provider, result.model
        )));
    }
    Ok(VmValue::Float(score_logprobs(&result.logprobs)?))
}

fn estimate_echo_tokens(text: &str) -> i64 {
    ((text.chars().count() as i64 + 2) / 3 + 8).clamp(1, 4096)
}

fn vm_logprobs(value: &VmValue) -> Result<Vec<serde_json::Value>, VmError> {
    match value {
        VmValue::List(items) => Ok(items.iter().map(super::helpers::vm_value_to_json).collect()),
        VmValue::Dict(dict) => {
            if let Some(logprobs) = dict.get("logprobs") {
                return vm_logprobs(logprobs);
            }
            Ok(vec![serde_json::Value::Object(
                dict.iter()
                    .map(|(key, value)| (key.clone(), super::helpers::vm_value_to_json(value)))
                    .collect(),
            )])
        }
        other => Err(VmError::Runtime(format!(
            "__llm_self_certainty: logprobs must be a list or dict, got {}",
            other.type_name()
        ))),
    }
}

fn score_logprobs(entries: &[serde_json::Value]) -> Result<f64, VmError> {
    let mut sum = 0.0;
    let mut count = 0usize;
    for entry in entries {
        if let Some(logprob) = logprob_value(entry) {
            if logprob.is_finite() {
                sum += logprob;
                count += 1;
            }
        }
    }
    if count == 0 {
        return Err(VmError::Runtime(
            "__llm_self_certainty: no finite token logprobs were available".to_string(),
        ));
    }
    let mean = sum / count as f64;
    Ok(mean.exp().clamp(0.0, 1.0))
}

fn logprob_value(entry: &serde_json::Value) -> Option<f64> {
    match entry {
        serde_json::Value::Number(number) => number.as_f64(),
        serde_json::Value::Object(object) => object
            .get("logprob")
            .or_else(|| object.get("token_logprob"))
            .and_then(|value| value.as_f64()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn score_logprobs_uses_length_normalized_probability() {
        let ln_half = -std::f64::consts::LN_2;
        let entries = vec![
            serde_json::json!({"token": "a", "logprob": ln_half}),
            serde_json::json!({"token": "b", "logprob": ln_half}),
        ];
        let score = score_logprobs(&entries).expect("score");
        assert!((score - 0.5).abs() < 0.000001);
    }
}
