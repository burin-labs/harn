//! Language projection of the canonical prompt-cache conformance report.

use crate::llm::cache_conformance::classify_cache_conformance_fixture;
use crate::stdlib::macros::harn_builtin;
use crate::value::{VmError, VmValue};

#[harn_builtin(
    exposure = "harness.llm.cache_conformance",
    effects = ["llm.read@dynamic"],
    sig = "llm_cache_conformance(provider: string, model: string, runs: list<unknown>) -> dict<string, unknown>",
    category = "llm.config"
)]
fn llm_cache_conformance_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let [VmValue::String(provider), VmValue::String(model), runs @ VmValue::List(_)] = args else {
        return Err(VmError::TypeError(
            "llm_cache_conformance expects provider, model, and a runs list".to_string(),
        ));
    };
    let raw = serde_json::to_string(&crate::llm::vm_value_to_json(runs))
        .map_err(|error| VmError::Runtime(format!("llm_cache_conformance: {error}")))?;
    let report = classify_cache_conformance_fixture(provider.as_str(), model.as_str(), &raw)
        .map_err(|error| VmError::Runtime(format!("llm_cache_conformance: {error}")))?;
    let value = serde_json::to_value(report)
        .map_err(|error| VmError::Runtime(format!("llm_cache_conformance: {error}")))?;
    Ok(crate::stdlib::json_to_vm_value(&value))
}
