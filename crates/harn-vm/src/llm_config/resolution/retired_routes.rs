//! Provider routes whose advertised identity no longer names the served model.
//! This exact deny registry leaves private and newly released IDs open-world.

use super::ModelResolutionError;

struct RetiredModelRoute {
    provider: &'static str,
    catalog_id: &'static str,
    wire_model: &'static str,
    reason: &'static str,
}

const RETIRED_ROUTES: &[RetiredModelRoute] = &[RetiredModelRoute {
    provider: "deepinfra",
    catalog_id: "deepinfra/Qwen/Qwen3.8-2.4T-A95B",
    wire_model: "Qwen/Qwen3.8-2.4T-A95B",
    reason: "DeepInfra redirects this Qwen identity to GLM-5.3 on 2026-10-12 at 23:48:18 UTC; select a different model explicitly (https://status.deepinfra.com/models/qwen3-8-2-4t-a95b)",
}];

pub(super) fn check(
    provider: &str,
    model: &str,
    wire_model: Option<&str>,
) -> Result<(), ModelResolutionError> {
    if let Some(route) = RETIRED_ROUTES.iter().find(|route| {
        route.provider == provider
            && (route.catalog_id == model
                || route.wire_model == model
                || Some(route.wire_model) == wire_model)
    }) {
        return Err(ModelResolutionError::RetiredModel {
            provider: provider.to_string(),
            model: model.to_string(),
            reason: route.reason.to_string(),
            catalog_version: super::catalog_version(),
        });
    }
    Ok(())
}
