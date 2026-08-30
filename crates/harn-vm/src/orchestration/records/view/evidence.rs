use serde_json::Value;

use crate::redact::RedactionPolicy;

use super::{redact_bounded, RunRecord, PREVIEW_LIMIT};

/// Whether a run projection may expose host-local artifact locators.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ArtifactPathVisibility {
    #[default]
    Hidden,
    Local,
}

pub(super) fn project_evidence(
    run: &RunRecord,
    policy: &RedactionPolicy,
    artifact_paths: ArtifactPathVisibility,
) -> super::super::ExecutionEvidenceRecord {
    let mut evidence = run.evidence.clone();
    for span in &mut evidence.trace_spans {
        span.name = redact_bounded(&span.name, policy, PREVIEW_LIMIT);
        let mut metadata = Value::Object(std::mem::take(&mut span.metadata).into_iter().collect());
        policy.redact_json_in_place(&mut metadata);
        if let Value::Object(metadata) = metadata {
            span.metadata = metadata.into_iter().collect();
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
        gap.message = redact_bounded(&gap.message, policy, PREVIEW_LIMIT);
    }
    evidence
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
        let mut run = RunRecord::default();
        run.evidence.flight_recording = Some(FlightRecordingArtifact {
            schema_version: FLIGHT_RECORDING_SCHEMA_VERSION,
            execution_id: "hxe-view".to_string(),
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
}
