//! `harn runs export-training` — project one authoritative run into one
//! `harn.agent_training_example.v1` training example.
//!
//! The projection itself lives in `harn_vm::orchestration`; this command owns
//! only the surface: resolving which run the caller means, writing the example
//! out, and rendering the report. Selection is by explicit identity — a
//! directory holding more than one run record demands `--run-id` rather than
//! picking a winner by size, recency, or transcript content.

use std::path::{Path, PathBuf};

use serde::Serialize;

use harn_vm::orchestration::{
    project_agent_training_example, AgentTrainingExample, TrainingExampleError,
    TrainingExampleRequest,
};

use crate::cli::RunsExportTrainingArgs;

const EXPORT_PAYLOAD_ENV: &str = "HARN_RUNS_EXPORT_TRAINING_PAYLOAD_JSON";
const EXPORT_PAYLOAD_PRETTY_ENV: &str = "HARN_RUNS_EXPORT_TRAINING_PAYLOAD_PRETTY";
const EXPORT_SCRIPT: &str = "runs/export_training";

#[derive(Debug, Serialize)]
pub(crate) struct ExportTrainingReport {
    pub ok: bool,
    pub run_record_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub example: Option<AgentTrainingExample>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<TrainingExampleError>,
}

/// The renderer owns the exit code: it returns 1 for a refused projection and
/// 0 for a successful one, so `report.ok` is not re-interpreted here.
pub(crate) async fn run(args: &RunsExportTrainingArgs) -> i32 {
    crate::commands::embedded_report::render_embedded_report(
        &build_report(args),
        EXPORT_PAYLOAD_ENV,
        EXPORT_PAYLOAD_PRETTY_ENV,
        EXPORT_SCRIPT,
        args.json,
        "training example export",
    )
    .await
}

fn build_report(args: &RunsExportTrainingArgs) -> ExportTrainingReport {
    let run_record_path = match resolve_run_record_path(args) {
        Ok(path) => path,
        Err(error) => {
            return ExportTrainingReport {
                ok: false,
                run_record_path: args.path.clone(),
                output_path: None,
                example: None,
                error: Some(error),
            }
        }
    };
    let request = TrainingExampleRequest {
        run_record_path: run_record_path.clone(),
        run_id: args.run_id.clone(),
        session_id: args.session_id.clone(),
    };
    let run_record_path = run_record_path.display().to_string();
    match project_agent_training_example(&request) {
        Ok(example) => match write_example(args.out.as_deref(), &example) {
            Ok(output_path) => ExportTrainingReport {
                ok: true,
                run_record_path,
                output_path,
                example: Some(example),
                error: None,
            },
            Err(error) => ExportTrainingReport {
                ok: false,
                run_record_path,
                output_path: None,
                example: None,
                error: Some(error),
            },
        },
        Err(error) => ExportTrainingReport {
            ok: false,
            run_record_path,
            output_path: None,
            example: None,
            error: Some(error),
        },
    }
}

/// Pick the one run record this export projects.
///
/// A file is itself the answer. A directory is only unambiguous when it holds
/// exactly one record, or when `--run-id` names which record to read; anything
/// else fails so the caller states their intent instead of inheriting a
/// silent tie-break.
fn resolve_run_record_path(args: &RunsExportTrainingArgs) -> Result<PathBuf, TrainingExampleError> {
    let path = Path::new(&args.path);
    if path.is_file() {
        return Ok(path.to_path_buf());
    }
    if !path.is_dir() {
        return Err(error(
            "run_record_unreadable",
            format!("{} does not exist", path.display()),
        ));
    }
    let mut records: Vec<PathBuf> = std::fs::read_dir(path)
        .map_err(|io| {
            error(
                "run_record_unreadable",
                format!("failed to read {}: {io}", path.display()),
            )
        })?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|entry| entry.extension().and_then(|ext| ext.to_str()) == Some("json"))
        .collect();
    records.sort();
    if records.is_empty() {
        return Err(error(
            "run_record_unreadable",
            format!("{} holds no run records", path.display()),
        ));
    }
    let Some(run_id) = args.run_id.as_deref() else {
        if records.len() == 1 {
            return Ok(records.remove(0));
        }
        return Err(error(
            "ambiguous_authority",
            format!(
                "{} holds {} run records; pass --run-id to name the one to project",
                path.display(),
                records.len()
            ),
        ));
    };
    let matches: Vec<PathBuf> = records
        .into_iter()
        .filter(|record| record_id(record).as_deref() == Some(run_id))
        .collect();
    match matches.as_slice() {
        [single] => Ok(single.clone()),
        [] => Err(error(
            "run_id_mismatch",
            format!("{} holds no run record with id {run_id}", path.display()),
        )),
        _ => Err(error(
            "ambiguous_authority",
            format!(
                "{} holds {} run records with id {run_id}",
                path.display(),
                matches.len()
            ),
        )),
    }
}

fn record_id(path: &Path) -> Option<String> {
    let content = std::fs::read_to_string(path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&content).ok()?;
    value
        .get("id")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
}

fn write_example(
    out: Option<&str>,
    example: &AgentTrainingExample,
) -> Result<Option<String>, TrainingExampleError> {
    let Some(out) = out else { return Ok(None) };
    let path = PathBuf::from(out);
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent).map_err(|io| {
            error(
                "output_unwritable",
                format!("failed to create {}: {io}", parent.display()),
            )
        })?;
    }
    let mut row = serde_json::to_string(example).map_err(|encode| {
        error(
            "output_unwritable",
            format!("failed to encode the example: {encode}"),
        )
    })?;
    row.push('\n');
    std::fs::write(&path, row).map_err(|io| {
        error(
            "output_unwritable",
            format!("failed to write {}: {io}", path.display()),
        )
    })?;
    Ok(Some(path.display().to_string()))
}

fn error(kind: &str, message: String) -> TrainingExampleError {
    TrainingExampleError {
        kind: kind.to_string(),
        message,
        event_index: None,
    }
}
