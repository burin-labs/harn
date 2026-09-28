//! Script access to the same admitted producer used by `harn provider tool-probe`.
use crate::llm::tool_conformance::{run_tool_conformance_probe, ToolConformanceProbeOptions};
use crate::stdlib::macros::harn_builtin;
use crate::value::{VmError, VmValue};

/// Run a provider tool probe inside the host's inference and durable budget scope.
#[harn_builtin(
    exposure = "harness.llm.tool_probe",
    effects = ["llm.write@arg0.provider", "llm.write@arg0.model"],
    sig = "llm_tool_probe(options: dict<string, unknown>) -> dict<string, unknown>",
    kind = "async",
    category = "llm.config"
)]
async fn llm_tool_probe_builtin(
    _ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let [options @ VmValue::Dict(_)] = args.as_slice() else {
        return Err(VmError::TypeError(
            "tool_probe expects one options dictionary".into(),
        ));
    };
    let value = crate::llm::vm_value_to_json_strict(options, "tool_probe.options")
        .map_err(VmError::TypeError)?;
    let options: ToolConformanceProbeOptions = serde_json::from_value(value)
        .map_err(|error| VmError::TypeError(format!("tool_probe: {error}")))?;
    if options.model.trim().is_empty() || options.repeat == 0 || options.timeout_secs == 0 {
        return Err(VmError::TypeError(
            "tool_probe requires a model, positive repeat, and positive timeout_secs".into(),
        ));
    }
    crate::llm_config::resolve_model_request(&options.model, Some(&options.provider))
        .map_err(|error| VmError::TypeError(format!("tool_probe: {error}")))?;
    let report = run_tool_conformance_probe(options).await;
    let value = serde_json::to_value(report)
        .map_err(|error| VmError::Runtime(format!("tool_probe: {error}")))?;
    Ok(crate::stdlib::json_to_vm_value(&value))
}
