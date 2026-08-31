use std::path::{Path, PathBuf};

use super::FlightRecorderOptions;

pub(super) struct ExecutionEvidencePersistFailure {
    pub(super) stage: &'static str,
    pub(super) summary: String,
    pub(super) diagnostics: Vec<String>,
}

#[allow(clippy::too_many_arguments)]
pub(super) fn persist_execution_evidence(
    vm: &harn_vm::Vm,
    options: &FlightRecorderOptions,
    source_path: &str,
    store_base: &Path,
    status: &str,
    started_at: String,
    finished_at: String,
) -> Result<
    Option<harn_vm::flight_recorder::FlightRecordingArtifact>,
    ExecutionEvidencePersistFailure,
> {
    let flight_recording = match persist_flight_recording(vm, options, store_base) {
        Ok(recording) => recording,
        Err(error) => {
            let mut diagnostics = vec![error.clone()];
            match persist_execution_run_record(
                vm,
                source_path,
                store_base,
                "failed",
                started_at,
                finished_at,
                None,
                vec![harn_vm::orchestration::RunEvidenceGapRecord {
                    component: "flight_recording".to_string(),
                    code: "persist_failed".to_string(),
                    message: error.clone(),
                }],
            ) {
                Ok(run_record_path) => emit_persisted(vm, &run_record_path, None),
                Err(record_error) => diagnostics.push(record_error),
            }
            return Err(ExecutionEvidencePersistFailure {
                stage: "flight_recording_persist",
                summary: error,
                diagnostics,
            });
        }
    };
    let run_record_path = persist_execution_run_record(
        vm,
        source_path,
        store_base,
        status,
        started_at,
        finished_at,
        flight_recording.as_ref(),
        Vec::new(),
    )
    .map_err(|error| ExecutionEvidencePersistFailure {
        stage: "run_record_persist",
        summary: error.clone(),
        diagnostics: vec![error],
    })?;
    emit_persisted(vm, &run_record_path, flight_recording.clone());
    Ok(flight_recording)
}

fn emit_persisted(
    vm: &harn_vm::Vm,
    run_record_path: &Path,
    flight_recording: Option<harn_vm::flight_recorder::FlightRecordingArtifact>,
) {
    harn_vm::run_events::emit(harn_vm::run_events::RunEvent::EvidencePersisted {
        execution_id: vm.execution_id().to_string(),
        run_record_path: run_record_path.to_string_lossy().into_owned(),
        flight_recording,
    });
}

pub(super) fn persist_flight_recording(
    vm: &harn_vm::Vm,
    options: &FlightRecorderOptions,
    store_base: &Path,
) -> Result<Option<harn_vm::flight_recorder::FlightRecordingArtifact>, String> {
    if !options.enabled {
        return Ok(None);
    }
    let recording = vm
        .flight_recording()
        .ok_or_else(|| "flight recorder was requested but not installed".to_string())?;
    let custom_path = options.out.is_some();
    let path = options.out.clone().unwrap_or_else(|| {
        harn_vm::runtime_paths::run_root(store_base)
            .join("flight-recordings")
            .join(format!("{}.json", recording.execution_id))
    });
    harn_vm::flight_recorder::persist_recording(
        &recording,
        &path,
        if custom_path {
            // A caller-supplied directory may contain unrelated JSON. Rotation
            // is safe only in Harn's dedicated default directory.
            usize::MAX
        } else {
            options.retain_files
        },
    )
    .map(Some)
}

pub(super) fn persist_execution_run_record(
    vm: &harn_vm::Vm,
    source_path: &str,
    store_base: &Path,
    status: &str,
    started_at: String,
    finished_at: String,
    flight_recording: Option<&harn_vm::flight_recorder::FlightRecordingArtifact>,
    gaps: Vec<harn_vm::orchestration::RunEvidenceGapRecord>,
) -> Result<PathBuf, String> {
    let execution_id = vm.execution_id().to_string();
    let source = Path::new(source_path);
    let run_path =
        harn_vm::runtime_paths::run_root(store_base).join(format!("{execution_id}.json"));
    let run = harn_vm::orchestration::RunRecord {
        type_name: "run_record".to_string(),
        id: execution_id.clone(),
        workflow_id: "harn.execution".to_string(),
        workflow_name: source
            .file_name()
            .and_then(|name| name.to_str())
            .map(str::to_string),
        task: source_path.to_string(),
        status: status.to_string(),
        started_at,
        finished_at: Some(finished_at),
        root_run_id: Some(execution_id.clone()),
        execution: Some(harn_vm::orchestration::RunExecutionRecord {
            cwd: std::env::current_dir()
                .ok()
                .map(|path| path.to_string_lossy().into_owned()),
            project_root: Some(store_base.to_string_lossy().into_owned()),
            source_dir: source
                .parent()
                .map(|path| path.to_string_lossy().into_owned()),
            adapter: Some("harn_cli".to_string()),
            ..harn_vm::orchestration::RunExecutionRecord::default()
        }),
        evidence: vm.execution_evidence(flight_recording.cloned(), gaps),
        ..harn_vm::orchestration::RunRecord::default()
    };
    harn_vm::orchestration::save_execution_run_record(&run, &run_path)
        .map_err(|error| format!("failed to persist execution run record: {error}"))?;
    Ok(run_path)
}
