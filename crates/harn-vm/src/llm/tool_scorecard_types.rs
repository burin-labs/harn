use std::collections::BTreeMap;

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct ToolScorecardReport {
    pub schema_version: u32,
    pub route_count: usize,
    pub summary: ToolScorecardSummary,
    pub routes: Vec<ToolScorecardRoute>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolScorecardSummary {
    pub pass: usize,
    pub warn: usize,
    pub fail: usize,
    pub trusted: usize,
    pub needs_review: usize,
    pub quarantined: usize,
    pub best_route: Option<ToolScorecardRouteKey>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolScorecardRouteKey {
    pub provider: String,
    pub model: String,
    pub quality_score: u16,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolScorecardRoute {
    pub provider: String,
    pub model: String,
    pub catalog_claim: Option<ToolScorecardCatalogClaim>,
    pub report_count: usize,
    pub case_count: usize,
    pub successful_cases: usize,
    pub parseable_tool_call_cases: usize,
    pub native_tool_call_cases: usize,
    pub text_tool_call_cases: usize,
    pub actionless_cases: usize,
    pub empty_completion_cases: usize,
    pub malformed_argument_cases: usize,
    pub http_error_cases: usize,
    pub transport_error_cases: usize,
    pub rate_limited_cases: usize,
    pub observed_latency_case_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latency_p50_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latency_p95_ms: Option<u64>,
    pub observed_usage_case_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<i64>,
    pub observed_cost_case_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    pub pass_rate: f64,
    pub parseable_tool_call_rate: f64,
    pub empty_completion_rate: f64,
    pub actionless_rate: f64,
    pub quality_score: u16,
    pub status: &'static str,
    pub trust_status: &'static str,
    pub trust_reasons: Vec<&'static str>,
    pub evidence_status: &'static str,
    pub probe_evidence_status: &'static str,
    pub request_evidence_status: &'static str,
    pub recommended_tool_mode: &'static str,
    pub observed_probe_cases: Vec<&'static str>,
    pub missing_required_cases: Vec<&'static str>,
    pub observed_probe_evidence: Vec<ToolScorecardProbeEvidence>,
    pub missing_required_probe_evidence: Vec<ToolScorecardProbeEvidence>,
    pub missing_required_request_cases: Vec<&'static str>,
    pub observed_wire_dialects: Vec<&'static str>,
    pub classification_counts: BTreeMap<&'static str, usize>,
    pub mode_evidence: Vec<ToolScorecardModeEvidence>,
    pub issues: Vec<&'static str>,
    pub catalog_mismatches: Vec<ToolScorecardCatalogMismatch>,
    pub suggested_catalog_updates: Vec<ToolScorecardCatalogUpdate>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolScorecardModeEvidence {
    pub mode: &'static str,
    pub case_count: usize,
    pub successful_cases: usize,
    pub parseable_tool_call_cases: usize,
    pub native_tool_call_cases: usize,
    pub text_tool_call_cases: usize,
    pub actionless_cases: usize,
    pub empty_completion_cases: usize,
    pub malformed_argument_cases: usize,
    pub http_error_cases: usize,
    pub transport_error_cases: usize,
    pub rate_limited_cases: usize,
    pub observed_latency_case_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latency_p50_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latency_p95_ms: Option<u64>,
    pub observed_usage_case_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<i64>,
    pub observed_cost_case_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    pub pass_rate: f64,
    pub recommended_tool_mode: &'static str,
    pub status: &'static str,
    pub classification_counts: BTreeMap<&'static str, usize>,
    pub issues: Vec<&'static str>,
}

#[derive(Debug, Clone, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct ToolScorecardProbeEvidence {
    pub case_id: &'static str,
    pub mode: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolScorecardPlan {
    pub schema_version: u32,
    pub kind: &'static str,
    pub catalog: ToolScorecardCatalogProvenance,
    pub route_count: usize,
    pub readiness_command_count: usize,
    pub unscorecardable_provider_count: usize,
    pub case_count: usize,
    pub required_case_count: usize,
    pub executable_case_count: usize,
    pub live_tool_probe_case_count: usize,
    pub offline_request_case_count: usize,
    pub not_applicable_case_count: usize,
    pub provider_summaries: Vec<ToolScorecardPlanProviderSummary>,
    pub batch_manifest_request_count: usize,
    pub routes: Vec<ToolScorecardPlanRoute>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub unscorecardable_providers: Vec<ToolScorecardUnscorecardableProvider>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub batch_manifest_requests: Vec<ToolScorecardBatchManifestRequest>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolScorecardCatalogProvenance {
    pub schema_version: u32,
    pub generated_by: String,
    pub hash_blake3: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolScorecardPlanProviderSummary {
    pub provider: String,
    pub route_count: usize,
    pub case_count: usize,
    pub required_case_count: usize,
    pub executable_case_count: usize,
    pub live_tool_probe_case_count: usize,
    pub offline_request_case_count: usize,
    pub readiness_command_count: usize,
    pub batch_manifest_request_count: usize,
    pub not_applicable_case_count: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolScorecardPlanRoute {
    pub provider: String,
    pub model: String,
    pub trust_status: &'static str,
    pub trust_reasons: Vec<&'static str>,
    pub readiness: ToolScorecardReadinessPlan,
    pub catalog_claim: ToolScorecardCatalogClaim,
    pub cases: Vec<ToolScorecardPlanCase>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolScorecardReadinessPlan {
    pub status: &'static str,
    pub runner: &'static str,
    pub reason: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact_hint: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolScorecardUnscorecardableProvider {
    pub provider: String,
    pub reason: &'static str,
    pub model_count: usize,
    pub active_model_count: usize,
    pub route_count: usize,
    pub local_runtime: bool,
    pub auth_required: bool,
    pub credential_env_names: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolScorecardCatalogClaim {
    pub preferred_tool_format: Option<String>,
    pub tool_mode_parity: Option<String>,
    pub native_tools: bool,
    pub text_tools: bool,
    pub text_tool_wire_format_supported: bool,
    pub max_tools: Option<u32>,
    pub supports_parallel_tool_calls: bool,
    pub server_parser: String,
    pub tool_search: Vec<String>,
    pub batch_api: bool,
    pub batch_wire_format: Option<String>,
    pub batch_input_mode: Option<String>,
    pub batch_discount_percent: Option<u32>,
    pub provider_rate_limits: bool,
    pub model_rate_limits: bool,
    pub provider_rpm: Option<u32>,
    pub pricing: bool,
    pub provider_latency_p50_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolScorecardCatalogMismatch {
    pub code: &'static str,
    pub field: &'static str,
    pub observed: String,
    pub catalog: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolScorecardCatalogUpdate {
    pub field: &'static str,
    pub operation: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    pub reason: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolScorecardPlanCase {
    pub id: &'static str,
    pub description: &'static str,
    pub requirement: &'static str,
    pub requirement_reason: &'static str,
    pub turn_count: u8,
    pub batch_eligible: bool,
    pub probe_focus: Vec<&'static str>,
    pub execution: ToolScorecardPlanCaseExecution,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolScorecardPlanCaseExecution {
    pub status: &'static str,
    pub runner: &'static str,
    pub reason: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact_hint: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolScorecardBatchManifestRequest {
    pub request_id: String,
    pub provider: String,
    pub model: String,
    pub case_id: &'static str,
    pub batch_wire_format: Option<String>,
    pub batch_input_mode: Option<String>,
}
