use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{ToolProbeCase, ToolProbeMode, ToolProbeRequestProfile};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolConformanceRequestReport {
    pub schema_version: u32,
    pub provider: String,
    pub model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(default)]
    pub probe_case: ToolProbeCase,
    #[serde(default)]
    pub request_profile: ToolProbeRequestProfile,
    pub tool_name: String,
    pub marker: String,
    pub expected_value: String,
    pub requests: Vec<ToolConformanceRequestCase>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolConformanceRequestCase {
    pub mode: ToolProbeMode,
    pub request_body: Value,
    pub validation: ToolConformanceRequestValidation,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolConformanceRequestValidation {
    pub dialect: String,
    pub status: ToolConformanceRequestValidationStatus,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<ToolConformanceRequestWarning>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub issues: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum ToolConformanceRequestWarning {
    SamplingParamsOmitted {
        dialect: String,
        params: Vec<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolConformanceRequestAuditReport {
    pub schema_version: u32,
    pub catalog_model_count: usize,
    pub route_count: usize,
    pub probe_cases: Vec<String>,
    pub request_profiles: Vec<String>,
    pub modes: Vec<String>,
    pub request_count: usize,
    pub validation_pass_count: usize,
    pub validation_fail_count: usize,
    pub warning_count: usize,
    pub not_applicable_count: usize,
    pub dialect_counts: BTreeMap<String, usize>,
    pub provider_counts: BTreeMap<String, usize>,
    pub routes: Vec<ToolConformanceRequestAuditRoute>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failures: Vec<ToolConformanceRequestAuditFailure>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<ToolConformanceRequestAuditWarning>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub not_applicable: Vec<ToolConformanceRequestAuditNotApplicable>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolConformanceRequestAuditRoute {
    pub provider: String,
    pub model: String,
    pub request_count: usize,
    pub validation_pass_count: usize,
    pub validation_fail_count: usize,
    pub not_applicable_count: usize,
    pub dialect_counts: BTreeMap<String, usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolConformanceRequestAuditFailure {
    pub provider: String,
    pub model: String,
    pub probe_case: String,
    pub request_profile: String,
    pub mode: String,
    pub dialect: String,
    pub issues: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolConformanceRequestAuditWarning {
    pub provider: String,
    pub model: String,
    pub probe_case: String,
    pub request_profile: String,
    pub mode: String,
    pub dialect: String,
    pub warnings: Vec<ToolConformanceRequestWarning>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolConformanceRequestAuditNotApplicable {
    pub provider: String,
    pub model: String,
    pub probe_case: String,
    pub request_profile: String,
    pub mode: String,
    pub dialect: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolConformanceRequestValidationStatus {
    Pass,
    Fail,
    NotApplicable,
}
