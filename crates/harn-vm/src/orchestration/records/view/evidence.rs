use std::collections::BTreeMap;

use serde_json::Value;

use crate::redact::RedactionPolicy;

use super::{redact_bounded, RunRecord, PREVIEW_LIMIT};
use crate::orchestration::{
    ExecutionEvidenceRecord, ExecutionEvidenceValidationError, RunEvidenceGapRecord,
};

/// Whether a run projection may expose host-local artifact locators.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ArtifactPathVisibility {
    #[default]
    Hidden,
    Local,
}

/// Produce a redacted, defensively validated copy of execution evidence.
///
/// This is the owning boundary for evidence exposed outside the Harn runtime or
/// persisted by a host. The source record is never mutated. Host-local flight
/// recording paths are included only when `artifact_paths` permits them.
pub fn project_execution_evidence(
    evidence: &ExecutionEvidenceRecord,
    artifact_paths: ArtifactPathVisibility,
) -> ExecutionEvidenceRecord {
    project_execution_evidence_with_policy(
        evidence,
        &crate::redact::current_policy(),
        artifact_paths,
    )
}

pub(super) fn project_evidence(
    run: &RunRecord,
    policy: &RedactionPolicy,
    artifact_paths: ArtifactPathVisibility,
) -> ExecutionEvidenceRecord {
    project_execution_evidence_with_policy(&run.evidence, policy, artifact_paths)
}

fn project_execution_evidence_with_policy(
    evidence: &ExecutionEvidenceRecord,
    policy: &RedactionPolicy,
    artifact_paths: ArtifactPathVisibility,
) -> ExecutionEvidenceRecord {
    let mut evidence = evidence.clone();
    sanitize_untrusted_evidence(&mut evidence);
    let execution_id = evidence.execution_id.clone();
    for span in &mut evidence.trace_spans {
        span.metadata.remove(crate::tracing::meta::EXECUTION_ID);
        if let Some(execution_id) = execution_id.as_ref() {
            span.metadata.insert(
                crate::tracing::meta::EXECUTION_ID.to_string(),
                serde_json::json!(execution_id),
            );
        }
        span.trace_id = redact_bounded(&span.trace_id, policy, PREVIEW_LIMIT);
        span.kind = redact_bounded(&span.kind, policy, PREVIEW_LIMIT);
        span.name = redact_bounded(&span.name, policy, PREVIEW_LIMIT);
        redact_json_map(&mut span.metadata, policy);
        for link in &mut span.links {
            link.trace_id = redact_bounded(&link.trace_id, policy, PREVIEW_LIMIT);
            link.span_id = redact_bounded(&link.span_id, policy, PREVIEW_LIMIT);
            redact_string_map(&mut link.attributes, policy);
        }
        for event in &mut span.events {
            event.name = redact_bounded(&event.name, policy, PREVIEW_LIMIT);
            redact_json_map(&mut event.attributes, policy);
        }
    }
    if let Some(recording) = &mut evidence.flight_recording {
        if artifact_paths == ArtifactPathVisibility::Hidden {
            recording.path = None;
        } else if let Some(path) = &mut recording.path {
            *path = redact_bounded(path, policy, PREVIEW_LIMIT);
        }
    }
    for gap in &mut evidence.gaps {
        gap.component = redact_bounded(&gap.component, policy, PREVIEW_LIMIT);
        gap.code = redact_bounded(&gap.code, policy, PREVIEW_LIMIT);
        gap.message = redact_bounded(&gap.message, policy, PREVIEW_LIMIT);
    }
    evidence
}

fn redact_json_map(values: &mut BTreeMap<String, Value>, policy: &RedactionPolicy) {
    let mut object = Value::Object(std::mem::take(values).into_iter().collect());
    policy.redact_json_in_place(&mut object);
    if let Value::Object(redacted) = object {
        *values = redacted.into_iter().collect();
    }
}

fn redact_string_map(values: &mut BTreeMap<String, String>, policy: &RedactionPolicy) {
    let mut object = serde_json::to_value(std::mem::take(values)).unwrap_or(Value::Null);
    policy.redact_json_in_place(&mut object);
    *values = serde_json::from_value(object).unwrap_or_default();
}

fn sanitize_untrusted_evidence(evidence: &mut ExecutionEvidenceRecord) {
    use ExecutionEvidenceValidationError as ValidationError;

    if evidence.schema_version == 0
        && evidence.execution_id.is_none()
        && evidence.flight_recording.is_none()
    {
        return;
    }

    match crate::orchestration::validate_execution_evidence(evidence) {
        Ok(()) => (),
        Err(ValidationError::MissingExecutionId) => {
            evidence.flight_recording = None;
            push_gap_once(
                evidence,
                "execution_identity",
                "projection_invalid",
                "The persisted run contained evidence without a Harn execution identity.",
            );
        }
        Err(ValidationError::UnsupportedSchema | ValidationError::InvalidExecutionId) => {
            evidence.execution_id = None;
            evidence.flight_recording = None;
            push_gap_once(
                evidence,
                "execution_identity",
                "projection_invalid",
                "The persisted run contained invalid Harn execution evidence.",
            );
        }
        Err(
            ValidationError::UnsupportedFlightRecordingSchema
            | ValidationError::FlightRecordingIdentityMismatch
            | ValidationError::UnsupportedFlightRecordingFormat
            | ValidationError::InvalidFlightRecordingHash,
        ) => {
            evidence.flight_recording = None;
            push_gap_once(
                evidence,
                "flight_recording",
                "projection_invalid",
                "The persisted run contained invalid flight recording metadata.",
            );
        }
    }
}

fn push_gap_once(
    evidence: &mut ExecutionEvidenceRecord,
    component: &str,
    code: &str,
    message: &str,
) {
    if evidence
        .gaps
        .iter()
        .any(|gap| gap.component == component && gap.code == code)
    {
        return;
    }
    evidence.gaps.push(RunEvidenceGapRecord {
        component: component.to_string(),
        code: code.to_string(),
        message: message.to_string(),
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flight_recorder::{
        FlightRecordingArtifact, FlightValuePolicy, FLIGHT_RECORDING_FORMAT,
        FLIGHT_RECORDING_SCHEMA_VERSION,
    };
    use crate::orchestration::RunRecord;
    use crate::redact::current_policy;

    fn run_with_local_recording() -> RunRecord {
        const EXECUTION_ID: &str = "hxe-019c13e0-8080-7000-8000-000000000001";
        let mut run = RunRecord::default();
        run.evidence.schema_version = crate::orchestration::EXECUTION_EVIDENCE_SCHEMA_VERSION;
        run.evidence.execution_id = Some(EXECUTION_ID.to_string());
        run.evidence.flight_recording = Some(FlightRecordingArtifact {
            schema_version: FLIGHT_RECORDING_SCHEMA_VERSION,
            execution_id: EXECUTION_ID.to_string(),
            format: FLIGHT_RECORDING_FORMAT.to_string(),
            path: Some("/private/workspace/.harn/flight.json".to_string()),
            content_hash: format!("blake3:{}", "a".repeat(64)),
            byte_length: 10,
            retained_events: 1,
            dropped_events: 0,
            value_policy: FlightValuePolicy::Omitted,
        });
        run
    }

    #[test]
    fn public_projection_hides_local_path_without_mutating_source() {
        let run = run_with_local_recording();
        let projected = project_evidence(&run, &current_policy(), ArtifactPathVisibility::Hidden);

        assert_eq!(
            projected
                .flight_recording
                .as_ref()
                .and_then(|recording| recording.path.as_deref()),
            None
        );
        assert_eq!(
            run.evidence
                .flight_recording
                .as_ref()
                .and_then(|recording| recording.path.as_deref()),
            Some("/private/workspace/.harn/flight.json")
        );
    }

    #[test]
    fn local_projection_retains_openable_path() {
        let run = run_with_local_recording();
        let projected = project_evidence(&run, &current_policy(), ArtifactPathVisibility::Local);

        assert_eq!(
            projected
                .flight_recording
                .as_ref()
                .and_then(|recording| recording.path.as_deref()),
            Some("/private/workspace/.harn/flight.json")
        );
    }

    #[test]
    fn public_projection_redacts_every_nested_span_and_gap_text_field() {
        const SECRET: &str = "projection-secret";
        let secret_url = format!("https://api.example.com/v1?api_key={SECRET}");
        let mut run = run_with_local_recording();
        run.evidence
            .trace_spans
            .push(crate::orchestration::RunTraceSpanRecord {
                trace_id: secret_url.clone(),
                kind: secret_url.clone(),
                name: secret_url.clone(),
                metadata: BTreeMap::from([("api_key".to_string(), serde_json::json!(SECRET))]),
                links: vec![crate::tracing::SpanLink {
                    trace_id: secret_url.clone(),
                    span_id: secret_url.clone(),
                    attributes: BTreeMap::from([("api_key".to_string(), SECRET.to_string())]),
                }],
                events: vec![crate::tracing::SpanEvent {
                    name: secret_url.clone(),
                    attributes: BTreeMap::from([(
                        "api_key".to_string(),
                        serde_json::json!(SECRET),
                    )]),
                    ..crate::tracing::SpanEvent::default()
                }],
                ..crate::orchestration::RunTraceSpanRecord::default()
            });
        run.evidence
            .gaps
            .push(crate::orchestration::RunEvidenceGapRecord {
                component: secret_url.clone(),
                code: secret_url.clone(),
                message: secret_url,
            });

        let projected = crate::orchestration::project_execution_evidence(
            &run.evidence,
            crate::orchestration::ArtifactPathVisibility::Hidden,
        );
        let rendered = serde_json::to_string(&projected).unwrap();

        assert!(!rendered.contains(SECRET), "projected evidence: {rendered}");
        assert_eq!(projected.trace_spans[0].events.len(), 1);
        assert_eq!(run.evidence.trace_spans[0].metadata["api_key"], SECRET);
        crate::orchestration::validate_execution_evidence(&projected).unwrap();
    }

    #[test]
    fn public_projection_does_not_trust_an_invalid_execution_identity() {
        let mut run = run_with_local_recording();
        run.evidence.execution_id = Some("host-run-id".to_string());
        run.evidence
            .trace_spans
            .push(crate::orchestration::RunTraceSpanRecord {
                metadata: std::collections::BTreeMap::from([(
                    crate::tracing::meta::EXECUTION_ID.to_string(),
                    serde_json::json!("host-run-id"),
                )]),
                ..crate::orchestration::RunTraceSpanRecord::default()
            });

        let projected = project_evidence(&run, &current_policy(), ArtifactPathVisibility::Hidden);

        assert_eq!(projected.execution_id, None);
        assert_eq!(projected.flight_recording, None);
        assert!(!projected.trace_spans[0]
            .metadata
            .contains_key(crate::tracing::meta::EXECUTION_ID));
        assert!(projected.gaps.iter().any(|gap| {
            gap.component == "execution_identity" && gap.code == "projection_invalid"
        }));
    }

    #[test]
    fn public_projection_marks_unexplained_missing_execution_identity() {
        let mut run = RunRecord::default();
        run.evidence.schema_version = crate::orchestration::EXECUTION_EVIDENCE_SCHEMA_VERSION;
        run.evidence
            .trace_spans
            .push(crate::orchestration::RunTraceSpanRecord {
                metadata: std::collections::BTreeMap::from([(
                    crate::tracing::meta::EXECUTION_ID.to_string(),
                    serde_json::json!("host-run-id"),
                )]),
                ..crate::orchestration::RunTraceSpanRecord::default()
            });

        let projected = project_evidence(&run, &current_policy(), ArtifactPathVisibility::Hidden);

        assert!(!projected.trace_spans[0]
            .metadata
            .contains_key(crate::tracing::meta::EXECUTION_ID));
        assert!(projected.gaps.iter().any(|gap| {
            gap.component == "execution_identity" && gap.code == "projection_invalid"
        }));
    }

    #[test]
    fn public_projection_keeps_valid_identity_when_recording_metadata_is_invalid() {
        let mut run = run_with_local_recording();
        run.evidence.flight_recording.as_mut().unwrap().content_hash = "blake3:invalid".to_string();
        run.evidence
            .trace_spans
            .push(crate::orchestration::RunTraceSpanRecord {
                metadata: std::collections::BTreeMap::from([(
                    crate::tracing::meta::EXECUTION_ID.to_string(),
                    serde_json::json!("hxe-019c13e0-8080-7000-8000-000000000099"),
                )]),
                ..crate::orchestration::RunTraceSpanRecord::default()
            });

        let projected = project_evidence(&run, &current_policy(), ArtifactPathVisibility::Hidden);

        assert_eq!(projected.execution_id, run.evidence.execution_id);
        assert_eq!(projected.flight_recording, None);
        assert_eq!(
            projected.trace_spans[0].metadata[crate::tracing::meta::EXECUTION_ID],
            serde_json::json!(run.evidence.execution_id.as_deref().unwrap())
        );
        assert!(projected.gaps.iter().any(|gap| {
            gap.component == "flight_recording" && gap.code == "projection_invalid"
        }));
    }
}
