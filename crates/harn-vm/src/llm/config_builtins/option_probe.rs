//! One selected portable option through the ordinary admitted call path.
use crate::llm::capabilities::PortableOption;
use crate::stdlib::macros::harn_builtin;
use crate::value::{VmError, VmValue};

/// Suspend catalog shaping for one option and allow one physical request.
#[harn_builtin(
    exposure = "harness.llm.option_probe_call",
    effects = ["llm.write@arg2.provider", "llm.write@arg2.model"],
    sig = "llm_option_probe_call(prompt: string, option: string, options: dict<string, unknown>) -> dict<string, unknown>",
    kind = "async",
    category = "llm.config"
)]
async fn llm_option_probe_call_builtin(
    ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let [prompt @ VmValue::String(_), VmValue::String(option), options @ VmValue::Dict(_)] =
        args.as_slice()
    else {
        return Err(VmError::TypeError(
            "option_probe_call expects a prompt, portable option, and call options".into(),
        ));
    };
    let option: PortableOption =
        serde_json::from_value(serde_json::Value::String(option.to_string()))
            .map_err(|error| VmError::TypeError(format!("option_probe_call: {error}")))?;
    if !PortableOption::PRESENCE_DRIVEN.contains(&option) {
        return Err(VmError::TypeError(format!(
            "option_probe_call requires one of: {}",
            PortableOption::PRESENCE_DRIVEN
                .map(PortableOption::name)
                .join(", ")
        )));
    }
    crate::llm::with_portable_option_probe(
        option,
        crate::llm::llm_call_impl(
            Some(&ctx),
            vec![prompt.clone(), VmValue::Nil, options.clone()],
        ),
    )
    .await
}
