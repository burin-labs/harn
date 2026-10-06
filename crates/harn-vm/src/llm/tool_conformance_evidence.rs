//! Provider observation and qualification share one evidence contract.

use serde::{Deserialize, Serialize};

use super::{
    report_satisfies_required_probe, ToolConformanceCase, ToolConformanceReport,
    TOOL_CONFORMANCE_SCHEMA_VERSION,
};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolProbeEvidenceSource {
    #[default]
    Unknown,
    LiveRequest,
    LiveRawEndpoint,
    SavedResponse,
}

pub(super) fn observed_source(
    cases: &[ToolConformanceCase],
    raw_endpoint: bool,
) -> ToolProbeEvidenceSource {
    if !cases
        .iter()
        .any(|case| case.usage.is_some() || case.http_status.is_some())
    {
        // Resolution, admission, request construction and transport failures
        // do not establish an observation of the provider's behavior.
        ToolProbeEvidenceSource::Unknown
    } else if raw_endpoint {
        ToolProbeEvidenceSource::LiveRawEndpoint
    } else {
        ToolProbeEvidenceSource::LiveRequest
    }
}

impl ToolConformanceReport {
    pub fn passed_probes(&self) -> Vec<String> {
        [
            "tool_probe",
            "tool_call_probe",
            "native_tool_probe",
            "streaming_tool_probe",
        ]
        .into_iter()
        .filter(|requirement| report_satisfies_required_probe(self, requirement))
        .map(str::to_owned)
        .collect()
    }

    /// Only a current report from a live provider-adapter request certifies its route.
    pub fn require_live_evidence(&self) -> Result<(), String> {
        if self.schema_version != TOOL_CONFORMANCE_SCHEMA_VERSION {
            return Err(format!(
                "unsupported tool-probe report schema_version {}; expected {}",
                self.schema_version, TOOL_CONFORMANCE_SCHEMA_VERSION
            ));
        }
        if self.evidence_source != ToolProbeEvidenceSource::LiveRequest {
            return Err("not live provider evidence".into());
        }
        Ok(())
    }
}
