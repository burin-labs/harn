//! Boundary-owned model/provider resolution for one LLM call.

use crate::value::{DictMap, VmDictExt, VmError, VmValue};

pub(super) fn resolve_model_selection(
    options: &Option<DictMap>,
) -> Result<(String, String, crate::llm_config::ModelResolution), VmError> {
    let requested_model = options
        .as_ref()
        .and_then(|resolved| resolved.get("model"))
        .filter(|value| !matches!(value, VmValue::Nil))
        .map(VmValue::display);
    let requested_provider = options
        .as_ref()
        .and_then(|resolved| resolved.get("provider"))
        .filter(|value| !matches!(value, VmValue::Nil))
        .map(VmValue::display)
        .filter(|provider| !provider.eq_ignore_ascii_case("auto"));

    if let Some(requested_model) = requested_model {
        let resolution = crate::llm_config::resolve_model_request(
            &requested_model,
            requested_provider.as_deref(),
        )
        .map_err(model_resolution_error)?;
        return Ok((
            resolution.resolved_provider.clone(),
            resolution.resolved_model.clone(),
            resolution,
        ));
    }

    let provider = super::vm_resolve_provider(options);
    let selector = super::vm_resolve_model_selector(options, &provider);
    let resolution = crate::llm_config::resolve_model_request(&selector, Some(&provider))
        .map_err(model_resolution_error)?;
    Ok((provider, resolution.resolved_model.clone(), resolution))
}

pub(super) fn model_resolution_error(error: crate::llm_config::ModelResolutionError) -> VmError {
    let mut fields = DictMap::default();
    fields.put_str("origin", "local");
    fields.put_str("category", "invalid_request");
    fields.put_str("code", "model_resolution_failed");
    fields.put_str("catalog_version", error.catalog_version());
    fields.put_str("message", error.to_string());
    VmError::Thrown(VmValue::Dict(fields.into()))
}
