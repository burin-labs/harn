//! One persistence operation for a terminal VM execution.
//!
//! Hosts describe the execution and choose whether to retain a flight
//! recording. Harn owns the record shape, validation, rotation, gap handling,
//! and the event projected after the durable record becomes readable.

use std::path::{Path, PathBuf};

use crate::flight_recorder::FlightRecordingArtifact;

use super::{RunEvidenceGapRecord, RunExecutionRecord, RunRecord};

/// Terminal status of an automatically persisted VM execution.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExecutionRecordStatus {
    /// The program reached a successful terminal outcome.
    Completed,
    /// Execution or evidence finalization failed.
    Failed,
}

impl ExecutionRecordStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Failed => "failed",
        }
    }
}

/// Storage policy for an enabled VM flight recorder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FlightRecordingStorage<'a> {
    /// Write into Harn's dedicated directory and retain the newest files.
    Managed {
        /// Maximum number of newest managed recordings to keep.
        retain_files: usize,
    },
    /// Write exactly to a caller-owned path without rotating sibling files.
    Custom {
        /// Exact caller-owned destination.
        output: &'a Path,
    },
}

/// Host facts needed to materialize one automatic execution record.
#[derive(Clone, Copy, Debug)]
pub struct ExecutionRecordRequest<'a> {
    /// Harn source file that was executed.
    pub source_path: &'a Path,
    /// Project or standalone root that owns Harn's run directory.
    pub store_base: &'a Path,
    /// Host adapter responsible for starting the execution.
    pub adapter: &'a str,
    /// Terminal program outcome.
    pub status: ExecutionRecordStatus,
    /// RFC 3339 execution start timestamp.
    pub started_at: &'a str,
    /// RFC 3339 execution finish timestamp.
    pub finished_at: &'a str,
    /// Optional flight-recorder storage policy.
    pub flight_recording: Option<FlightRecordingStorage<'a>>,
}

/// Durable artifacts produced for one execution.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PersistedExecutionRecord {
    /// Validated automatic run-record path.
    pub run_record_path: PathBuf,
    /// Flight artifact written for this execution, when enabled.
    pub flight_recording: Option<FlightRecordingArtifact>,
}

/// Closed persistence stage used by host error and exit projections.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExecutionRecordPersistStage {
    /// The optional flight artifact could not be written.
    FlightRecording,
    /// The canonical automatic run record could not be written.
    RunRecord,
}

impl ExecutionRecordPersistStage {
    /// Stable machine-readable stage code used by host adapters.
    pub fn code(self) -> &'static str {
        match self {
            Self::FlightRecording => "flight_recording_persist",
            Self::RunRecord => "run_record_persist",
        }
    }
}

/// Typed failure from the unified execution persistence operation.
#[derive(Debug, thiserror::Error)]
#[error("{summary}")]
pub struct ExecutionRecordPersistError {
    stage: ExecutionRecordPersistStage,
    summary: String,
    diagnostics: Vec<String>,
    flight_recording: Option<FlightRecordingArtifact>,
}

impl ExecutionRecordPersistError {
    /// Stage that failed.
    pub fn stage(&self) -> ExecutionRecordPersistStage {
        self.stage
    }

    /// Primary human-readable failure.
    pub fn summary(&self) -> &str {
        &self.summary
    }

    /// Complete ordered diagnostics, including fallback-record failures.
    pub fn diagnostics(&self) -> &[String] {
        &self.diagnostics
    }

    /// Flight artifact that became durable before a later stage failed.
    pub fn flight_recording(&self) -> Option<&FlightRecordingArtifact> {
        self.flight_recording.as_ref()
    }
}

/// Persist the canonical run record and optional flight artifact for one VM
/// execution. A flight-artifact failure still leaves a failed run record with
/// an explicit evidence gap whenever the record store remains writable.
pub fn persist_execution_record(
    vm: &crate::Vm,
    request: ExecutionRecordRequest<'_>,
) -> Result<PersistedExecutionRecord, ExecutionRecordPersistError> {
    let flight_recording = match persist_flight_recording(vm, &request) {
        Ok(recording) => recording,
        Err(error) => {
            let mut diagnostics = vec![error.clone()];
            match persist_run_record(
                vm,
                &request,
                ExecutionRecordStatus::Failed,
                None,
                vec![RunEvidenceGapRecord {
                    component: "flight_recording".to_string(),
                    code: "persist_failed".to_string(),
                    message: error.clone(),
                }],
            ) {
                Ok(run_record_path) => emit_persisted(vm, &run_record_path, None),
                Err(record_error) => diagnostics.push(record_error),
            }
            return Err(ExecutionRecordPersistError {
                stage: ExecutionRecordPersistStage::FlightRecording,
                summary: error,
                diagnostics,
                flight_recording: None,
            });
        }
    };

    let run_record_path = persist_run_record(
        vm,
        &request,
        request.status,
        flight_recording.as_ref(),
        Vec::new(),
    )
    .map_err(|error| ExecutionRecordPersistError {
        stage: ExecutionRecordPersistStage::RunRecord,
        summary: error.clone(),
        diagnostics: vec![error],
        flight_recording: flight_recording.clone(),
    })?;
    emit_persisted(vm, &run_record_path, flight_recording.clone());
    Ok(PersistedExecutionRecord {
        run_record_path,
        flight_recording,
    })
}

fn persist_flight_recording(
    vm: &crate::Vm,
    request: &ExecutionRecordRequest<'_>,
) -> Result<Option<FlightRecordingArtifact>, String> {
    let Some(options) = request.flight_recording else {
        return Ok(None);
    };
    let recording = vm
        .flight_recording()
        .ok_or_else(|| "flight recorder was requested but not installed".to_string())?;
    let (path, retain_files) = match options {
        FlightRecordingStorage::Managed { retain_files } => (
            crate::runtime_paths::run_root(request.store_base)
                .join("flight-recordings")
                .join(format!("{}.json", recording.execution_id)),
            retain_files,
        ),
        // A caller-supplied directory may contain unrelated JSON. Rotation is
        // safe only in Harn's dedicated managed directory.
        FlightRecordingStorage::Custom { output } => (output.to_path_buf(), usize::MAX),
    };
    crate::flight_recorder::persist_recording(&recording, &path, retain_files).map(Some)
}

fn persist_run_record(
    vm: &crate::Vm,
    request: &ExecutionRecordRequest<'_>,
    status: ExecutionRecordStatus,
    flight_recording: Option<&FlightRecordingArtifact>,
    gaps: Vec<RunEvidenceGapRecord>,
) -> Result<PathBuf, String> {
    let execution_id = vm.execution_id().to_string();
    let run_path =
        crate::runtime_paths::run_root(request.store_base).join(format!("{execution_id}.json"));
    let run = RunRecord {
        type_name: "run_record".to_string(),
        id: execution_id.clone(),
        workflow_id: "harn.execution".to_string(),
        workflow_name: request
            .source_path
            .file_name()
            .and_then(|name| name.to_str())
            .map(str::to_string),
        task: request.source_path.to_string_lossy().into_owned(),
        status: status.as_str().to_string(),
        started_at: request.started_at.to_string(),
        finished_at: Some(request.finished_at.to_string()),
        root_run_id: Some(execution_id),
        execution: Some(RunExecutionRecord {
            cwd: std::env::current_dir()
                .ok()
                .map(|path| path.to_string_lossy().into_owned()),
            project_root: Some(request.store_base.to_string_lossy().into_owned()),
            source_dir: request
                .source_path
                .parent()
                .map(|path| path.to_string_lossy().into_owned()),
            adapter: Some(request.adapter.to_string()),
            ..RunExecutionRecord::default()
        }),
        evidence: vm.execution_evidence(flight_recording.cloned(), gaps),
        ..RunRecord::default()
    };
    super::save_execution_run_record(&run, &run_path)
        .map_err(|error| format!("failed to persist run record: {error}"))?;
    Ok(run_path)
}

fn emit_persisted(
    vm: &crate::Vm,
    run_record_path: &Path,
    flight_recording: Option<FlightRecordingArtifact>,
) {
    crate::run_events::emit(crate::run_events::RunEvent::EvidencePersisted {
        execution_id: vm.execution_id().to_string(),
        run_record_path: run_record_path.to_string_lossy().into_owned(),
        flight_recording,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request<'a>(source_path: &'a Path, store_base: &'a Path) -> ExecutionRecordRequest<'a> {
        ExecutionRecordRequest {
            source_path,
            store_base,
            adapter: "test",
            status: ExecutionRecordStatus::Completed,
            started_at: "2026-01-02T00:00:00Z",
            finished_at: "2026-01-02T00:00:01Z",
            flight_recording: None,
        }
    }

    #[test]
    fn interface_persists_a_valid_reloadable_execution_record() {
        let dir = tempfile::tempdir().unwrap();
        let _locks = crate::conditional_replace::scope_conditional_replace_lock_root(
            dir.path().join("locks"),
        );
        let source = dir.path().join("main.harn");
        let mut vm = crate::Vm::new();
        vm.enable_flight_recorder(8);
        let mut request = request(&source, dir.path());
        request.flight_recording = Some(FlightRecordingStorage::Managed { retain_files: 1 });

        let persisted = persist_execution_record(&vm, request).unwrap();
        let run = super::super::load_run_record(&persisted.run_record_path).unwrap();

        assert_eq!(run.id, vm.execution_id().as_str());
        assert_eq!(run.status, "completed");
        assert_eq!(run.execution.unwrap().adapter.as_deref(), Some("test"));
        assert_eq!(
            super::super::validate_execution_evidence(&run.evidence),
            Ok(())
        );
        let flight = persisted.flight_recording.unwrap();
        assert_eq!(flight.execution_id, run.id);
        let flight_path = Path::new(flight.path.as_deref().unwrap());
        assert_eq!(
            flight_path
                .parent()
                .and_then(Path::file_name)
                .and_then(|name| name.to_str()),
            Some("flight-recordings")
        );
        assert!(flight_path.is_file());
    }

    #[test]
    fn flight_failure_persists_one_failed_record_with_an_explicit_gap() {
        let dir = tempfile::tempdir().unwrap();
        let _locks = crate::conditional_replace::scope_conditional_replace_lock_root(
            dir.path().join("locks"),
        );
        let source = dir.path().join("main.harn");
        let output_directory = dir.path().join("not-a-flight-file");
        std::fs::create_dir(&output_directory).unwrap();
        let mut vm = crate::Vm::new();
        vm.enable_flight_recorder(8);
        let mut request = request(&source, dir.path());
        request.flight_recording = Some(FlightRecordingStorage::Custom {
            output: &output_directory,
        });

        let error = persist_execution_record(&vm, request).unwrap_err();

        assert_eq!(error.stage(), ExecutionRecordPersistStage::FlightRecording);
        assert_eq!(error.diagnostics().len(), 1);
        let run_path =
            crate::runtime_paths::run_root(dir.path()).join(format!("{}.json", vm.execution_id()));
        let run = super::super::load_run_record(&run_path).unwrap();
        assert_eq!(run.status, "failed");
        assert_eq!(run.evidence.gaps.len(), 1);
        assert_eq!(run.evidence.gaps[0].code, "persist_failed");
        assert!(run.evidence.flight_recording.is_none());
    }

    #[test]
    fn run_record_failure_preserves_the_durable_flight_artifact_receipt() {
        let dir = tempfile::tempdir().unwrap();
        let _locks = crate::conditional_replace::scope_conditional_replace_lock_root(
            dir.path().join("locks"),
        );
        let source = dir.path().join("main.harn");
        let run_root = crate::runtime_paths::run_root(dir.path());
        std::fs::create_dir_all(run_root.parent().unwrap()).unwrap();
        std::fs::write(run_root, "not a directory").unwrap();
        let flight_path = dir.path().join("flight.json");
        let mut vm = crate::Vm::new();
        vm.enable_flight_recorder(8);
        let mut request = request(&source, dir.path());
        request.flight_recording = Some(FlightRecordingStorage::Custom {
            output: &flight_path,
        });

        let error = persist_execution_record(&vm, request).unwrap_err();

        assert_eq!(error.stage(), ExecutionRecordPersistStage::RunRecord);
        let flight = error.flight_recording().expect("durable partial artifact");
        assert_eq!(flight.execution_id, vm.execution_id().as_str());
        assert_eq!(flight.path.as_deref(), flight_path.to_str());
        assert!(flight_path.is_file());
    }
}
