use crate::orchestration::{CrystallizationTrace, CrystallizeOptions};
use crate::value::VmError;

pub(super) fn parse_skill_induce_traces(
    payload: &serde_json::Map<String, serde_json::Value>,
) -> Result<Vec<CrystallizationTrace>, VmError> {
    let value = payload
        .get("traces")
        .or_else(|| payload.get("trajectory"))
        .or_else(|| payload.get("trace"))
        .ok_or_else(|| {
            VmError::Runtime(
                "skill_induce: payload requires `traces`, `trajectory`, or `trace`".to_string(),
            )
        })?;
    let traces = match value {
        serde_json::Value::Array(_) => {
            serde_json::from_value::<Vec<CrystallizationTrace>>(value.clone())
        }
        _ => serde_json::from_value::<CrystallizationTrace>(value.clone()).map(|trace| vec![trace]),
    }
    .map_err(|error| VmError::Runtime(format!("skill_induce: invalid trace payload: {error}")))?;
    if traces.is_empty() {
        return Err(VmError::Runtime(
            "skill_induce: at least one trace is required".to_string(),
        ));
    }
    Ok(traces)
}

pub(super) fn parse_skill_induce_options(
    payload: &serde_json::Map<String, serde_json::Value>,
) -> Result<CrystallizeOptions, VmError> {
    let mut options = CrystallizeOptions::default();
    let empty = serde_json::Map::new();
    let opts = payload
        .get("options")
        .and_then(serde_json::Value::as_object)
        .unwrap_or(&empty);
    options.min_examples = json_usize(
        payload
            .get("min_examples")
            .or_else(|| opts.get("min_examples")),
    )
    .unwrap_or_default();
    options.promotion_min_confidence = json_f64(
        payload
            .get("promotion_min_confidence")
            .or_else(|| opts.get("promotion_min_confidence")),
    )
    .unwrap_or_default();
    options.workflow_name = json_string(
        payload
            .get("workflow_name")
            .or_else(|| opts.get("workflow_name")),
    );
    options.package_name = json_string(
        payload
            .get("package_name")
            .or_else(|| opts.get("package_name")),
    );
    options.author = json_string(payload.get("author").or_else(|| opts.get("author")));
    options.approver = json_string(payload.get("approver").or_else(|| opts.get("approver")));
    options.eval_pack_link = json_string(
        payload
            .get("eval_pack_link")
            .or_else(|| opts.get("eval_pack_link")),
    );
    let heldout = payload
        .get("heldout_traces")
        .or_else(|| payload.get("shadow_traces"))
        .or_else(|| opts.get("heldout_traces"))
        .or_else(|| opts.get("shadow_traces"));
    if let Some(value) = heldout {
        options.shadow_traces = serde_json::from_value::<Vec<CrystallizationTrace>>(value.clone())
            .map_err(|error| {
                VmError::Runtime(format!("skill_induce: invalid heldout_traces: {error}"))
            })?;
    }
    Ok(options)
}

fn json_string(value: Option<&serde_json::Value>) -> Option<String> {
    value
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn json_usize(value: Option<&serde_json::Value>) -> Option<usize> {
    value
        .and_then(serde_json::Value::as_u64)
        .map(|value| value as usize)
}

fn json_f64(value: Option<&serde_json::Value>) -> Option<f64> {
    value.and_then(serde_json::Value::as_f64)
}
