use super::ExecutionEvidenceRecord;
use crate::flight_recorder::{FLIGHT_RECORDING_FORMAT, FLIGHT_RECORDING_SCHEMA_VERSION};
use crate::observability::execution_scope::ExecutionId;

pub const EXECUTION_EVIDENCE_SCHEMA_VERSION: u32 = 1;

/// A structural violation at an execution-evidence trust boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ExecutionEvidenceValidationError {
    #[error("execution evidence must use schema version {EXECUTION_EVIDENCE_SCHEMA_VERSION}")]
    UnsupportedSchema,
    #[error("execution evidence is missing its Harn execution identity")]
    MissingExecutionId,
    #[error("execution evidence has an invalid Harn execution identity")]
    InvalidExecutionId,
    #[error("execution evidence span cost must be finite and non-negative")]
    InvalidSpanCost,
    #[error("flight recording must use schema version {FLIGHT_RECORDING_SCHEMA_VERSION}")]
    UnsupportedFlightRecordingSchema,
    #[error("flight recording identity does not match its execution evidence")]
    FlightRecordingIdentityMismatch,
    #[error("flight recording has an unsupported format")]
    UnsupportedFlightRecordingFormat,
    #[error("flight recording has an invalid BLAKE3 content hash")]
    InvalidFlightRecordingHash,
}

/// Validate the producer-owned execution-evidence envelope before a host or
/// service persists or projects it.
pub fn validate_execution_evidence(
    evidence: &ExecutionEvidenceRecord,
) -> Result<(), ExecutionEvidenceValidationError> {
    if evidence.schema_version != EXECUTION_EVIDENCE_SCHEMA_VERSION {
        return Err(ExecutionEvidenceValidationError::UnsupportedSchema);
    }
    let Some(execution_id) = evidence.execution_id.as_deref() else {
        return Err(ExecutionEvidenceValidationError::MissingExecutionId);
    };
    validate_execution_id(execution_id)?;
    if evidence.trace_spans.iter().any(span_cost_is_invalid) {
        return Err(ExecutionEvidenceValidationError::InvalidSpanCost);
    }
    let Some(recording) = evidence.flight_recording.as_ref() else {
        return Ok(());
    };
    if recording.schema_version != FLIGHT_RECORDING_SCHEMA_VERSION {
        return Err(ExecutionEvidenceValidationError::UnsupportedFlightRecordingSchema);
    }
    if recording.execution_id != execution_id {
        return Err(ExecutionEvidenceValidationError::FlightRecordingIdentityMismatch);
    }
    if recording.format != FLIGHT_RECORDING_FORMAT {
        return Err(ExecutionEvidenceValidationError::UnsupportedFlightRecordingFormat);
    }
    if !is_blake3_hash(&recording.content_hash) {
        return Err(ExecutionEvidenceValidationError::InvalidFlightRecordingHash);
    }
    Ok(())
}

pub(super) fn span_cost_is_invalid(span: &super::RunTraceSpanRecord) -> bool {
    match span.cost_usd {
        Some(cost) => cost_is_invalid(cost),
        None => span
            .metadata
            .get(crate::tracing::meta::COST_USD)
            .is_some_and(legacy_span_cost_is_invalid),
    }
}

pub(super) fn cost_is_invalid(cost: f64) -> bool {
    !cost.is_finite() || cost < 0.0
}

pub(super) fn legacy_span_cost_is_invalid(value: &serde_json::Value) -> bool {
    value.as_f64().is_none_or(cost_is_invalid)
}

/// Validate one claimed Harn-owned execution identity.
pub fn validate_execution_id(candidate: &str) -> Result<(), ExecutionEvidenceValidationError> {
    ExecutionId::parse(candidate)
        .map(|_| ())
        .map_err(|_| ExecutionEvidenceValidationError::InvalidExecutionId)
}

fn is_blake3_hash(candidate: &str) -> bool {
    candidate.strip_prefix("blake3:").is_some_and(|digest| {
        digest.len() == 64
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flight_recorder::{FlightRecordingArtifact, FlightValuePolicy};

    const EXECUTION_ID: &str = "hxe-019c13e0-8080-7000-8000-000000000001";

    fn evidence() -> ExecutionEvidenceRecord {
        ExecutionEvidenceRecord {
            schema_version: EXECUTION_EVIDENCE_SCHEMA_VERSION,
            execution_id: Some(EXECUTION_ID.to_string()),
            ..ExecutionEvidenceRecord::default()
        }
    }

    fn artifact() -> FlightRecordingArtifact {
        FlightRecordingArtifact {
            schema_version: FLIGHT_RECORDING_SCHEMA_VERSION,
            execution_id: EXECUTION_ID.to_string(),
            format: FLIGHT_RECORDING_FORMAT.to_string(),
            path: Some(".harn/receipts/run.flight.json".to_string()),
            content_hash: format!("blake3:{}", "a".repeat(64)),
            byte_length: 10,
            retained_events: 1,
            dropped_events: 0,
            value_policy: FlightValuePolicy::Omitted,
        }
    }

    #[test]
    fn accepts_harn_owned_identity_and_matching_flight_artifact() {
        let mut evidence = evidence();
        evidence.flight_recording = Some(artifact());
        assert_eq!(validate_execution_id(EXECUTION_ID), Ok(()));
        assert_eq!(validate_execution_evidence(&evidence), Ok(()));
    }

    #[test]
    fn rejects_non_v7_and_non_rfc_execution_ids() {
        for invalid in [
            "cloud-run-id",
            "hxe-019C13E0-8080-7000-8000-000000000001",
            "hxe-019c13e0808070008000000000000001",
            "hxe-019c13e0-8080-4000-8000-000000000001",
            "hxe-019c13e0-8080-7000-c000-000000000001",
        ] {
            assert_eq!(
                validate_execution_id(invalid),
                Err(ExecutionEvidenceValidationError::InvalidExecutionId)
            );
            let mut evidence = evidence();
            evidence.execution_id = Some(invalid.to_string());
            assert_eq!(
                validate_execution_evidence(&evidence),
                Err(ExecutionEvidenceValidationError::InvalidExecutionId)
            );
        }
    }

    #[test]
    fn rejects_mismatched_or_forged_flight_artifacts() {
        let cases = [
            (
                {
                    let mut artifact = artifact();
                    artifact.execution_id = "hxe-019c13e0-8080-7000-8000-000000000002".to_string();
                    artifact
                },
                ExecutionEvidenceValidationError::FlightRecordingIdentityMismatch,
            ),
            (
                {
                    let mut artifact = artifact();
                    artifact.format = "application/json".to_string();
                    artifact
                },
                ExecutionEvidenceValidationError::UnsupportedFlightRecordingFormat,
            ),
            (
                {
                    let mut artifact = artifact();
                    artifact.content_hash = "blake3:not-a-digest".to_string();
                    artifact
                },
                ExecutionEvidenceValidationError::InvalidFlightRecordingHash,
            ),
            (
                {
                    let mut artifact = artifact();
                    artifact.content_hash = format!("blake3:{}", "A".repeat(64));
                    artifact
                },
                ExecutionEvidenceValidationError::InvalidFlightRecordingHash,
            ),
        ];
        for (artifact, expected) in cases {
            let mut evidence = evidence();
            evidence.flight_recording = Some(artifact);
            assert_eq!(validate_execution_evidence(&evidence), Err(expected));
        }
    }

    #[test]
    fn rejects_non_finite_or_negative_span_costs() {
        for invalid in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -0.01] {
            let mut evidence = evidence();
            evidence.trace_spans.push(super::super::RunTraceSpanRecord {
                cost_usd: Some(invalid),
                ..super::super::RunTraceSpanRecord::default()
            });
            assert_eq!(
                validate_execution_evidence(&evidence),
                Err(ExecutionEvidenceValidationError::InvalidSpanCost)
            );
        }

        let mut evidence = evidence();
        evidence.trace_spans.push(super::super::RunTraceSpanRecord {
            metadata: std::collections::BTreeMap::from([(
                crate::tracing::meta::COST_USD.to_string(),
                serde_json::json!(-0.01),
            )]),
            ..super::super::RunTraceSpanRecord::default()
        });
        assert_eq!(
            validate_execution_evidence(&evidence),
            Err(ExecutionEvidenceValidationError::InvalidSpanCost)
        );

        evidence.trace_spans[0].metadata.insert(
            crate::tracing::meta::COST_USD.to_string(),
            serde_json::json!("not-a-cost"),
        );
        assert_eq!(
            validate_execution_evidence(&evidence),
            Err(ExecutionEvidenceValidationError::InvalidSpanCost)
        );

        evidence.trace_spans[0].cost_usd = Some(0.25);
        assert_eq!(validate_execution_evidence(&evidence), Ok(()));
    }
}
