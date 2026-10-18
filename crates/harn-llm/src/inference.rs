//! Typed inference ceilings and the catalog evidence used to evaluate them.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum InferenceReach {
    LocalOnly,
    HostedOpenWeight,
    AnyHosted,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InferenceBoundary {
    pub reach: InferenceReach,
    pub allow_training_discounts: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InferenceCatalogEvidence {
    pub local_runtime: bool,
    pub open_weight: Option<bool>,
}

/// Trusted launcher's optional ceiling, captured before accepting client policy.
pub const HOST_BOUNDARY_ENV: &str = "HARN_INFERENCE_BOUNDARY_JSON";

pub fn parse_host_boundary(raw: &str) -> Result<InferenceBoundary, String> {
    serde_json::from_str(raw).map_err(|_| "inference_boundary.host_boundary_malformed".to_string())
}

impl InferenceBoundary {
    /// Capture the trusted launcher's ceiling once, before accepting client
    /// session policies. An absent ceiling preserves standalone behavior.
    pub fn capture_process() -> Result<Option<Self>, String> {
        match std::env::var(HOST_BOUNDARY_ENV) {
            Ok(raw) => parse_host_boundary(&raw).map(Some),
            Err(std::env::VarError::NotPresent) => Ok(None),
            Err(std::env::VarError::NotUnicode(_)) => {
                Err("inference_boundary.host_boundary_malformed".into())
            }
        }
    }
}
