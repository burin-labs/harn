//! Production-owned run/session view compatibility fixture engine.

use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::Value as JsonValue;
use thiserror::Error;

use super::{
    build_run_view_with_options, build_session_view_from_run_views, RunRecord, RunView,
    RunViewOptions, SessionView, SessionViewOptions, ViewProducer, RUN_VIEW_SCHEMA,
    SESSION_VIEW_SCHEMA,
};

const FIXTURE_ROOT: &str = "spec/run-view-fixtures";
const FIXTURE_PRODUCER_VERSION: &str = "fixture";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunViewFixtureMode {
    Check,
    Write,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RunViewFixtureSummary {
    pub case_count: usize,
    pub run_view_count: usize,
    pub snapshot_count: usize,
}

#[derive(Debug, Error)]
pub enum RunViewFixtureError {
    #[error("failed to {operation} {path}: {source}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("failed to decode {path} as a run record: {source}")]
    Decode {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("invalid run-view fixture `{case}`: {detail}")]
    Invalid { case: String, detail: String },
    #[error("{path} is stale; run `make gen-run-view-fixtures` to refresh")]
    Stale { path: PathBuf },
}

/// Check or rewrite every public run/session-view compatibility snapshot.
///
/// `repository_root` is explicit so the production CLI, Rust regressions, and
/// release audit all execute one engine without relying on the builder's
/// compile-time path or the caller's current directory.
pub fn sync_run_view_fixtures(
    repository_root: &Path,
    mode: RunViewFixtureMode,
) -> Result<RunViewFixtureSummary, RunViewFixtureError> {
    let fixture_root = repository_root.join(FIXTURE_ROOT);
    let cases_root = fixture_root.join("cases");
    let cases = read_sorted_dirs(&cases_root)?;
    if cases.is_empty() {
        return Err(invalid(
            FIXTURE_ROOT,
            format!("expected at least one case under {}", cases_root.display()),
        ));
    }

    let mut run_view_count = 0;
    let mut snapshot_count = 0;
    let mut snapshots = Vec::new();
    for case_dir in &cases {
        let case_name = utf8_file_name(case_dir)?;
        let records_root = case_dir.join("records");
        let record_paths = read_sorted_json_files(&records_root)?;
        if record_paths.is_empty() {
            return Err(invalid(
                &case_name,
                format!(
                    "expected at least one JSON record under {}",
                    records_root.display()
                ),
            ));
        }

        let mut run_views = Vec::with_capacity(record_paths.len());
        for record_path in record_paths {
            let record_name = utf8_file_stem(&record_path)?;
            let raw = read_text(&record_path)?;
            let run: RunRecord =
                serde_json::from_str(&raw).map_err(|source| RunViewFixtureError::Decode {
                    path: record_path.clone(),
                    source,
                })?;
            let run_path = repo_relative_path(repository_root, &record_path, &case_name)?;
            let run_view = build_run_view_with_options(
                &run,
                RunViewOptions {
                    producer: fixture_producer(),
                    run_path: Some(run_path),
                    ..RunViewOptions::default()
                },
            );
            let snapshot_path = case_dir
                .join("expected")
                .join("runs")
                .join(format!("{record_name}.run_view.json"));
            snapshots.push((
                snapshot_path,
                render_snapshot(&run_view, RUN_VIEW_SCHEMA, &case_name)?,
            ));
            run_views.push(run_view);
            run_view_count += 1;
            snapshot_count += 1;
        }

        let session_view = build_session_view_from_run_views(
            run_views.clone(),
            SessionViewOptions {
                producer: fixture_producer(),
                ..SessionViewOptions::default()
            },
        );
        snapshots.push((
            case_dir.join("expected").join("session_view.json"),
            render_snapshot(&session_view, SESSION_VIEW_SCHEMA, &case_name)?,
        ));
        assert_case_coverage(&case_name, &run_views, &session_view)?;
        snapshot_count += 1;
    }

    for (path, rendered) in snapshots {
        match mode {
            RunViewFixtureMode::Check => check_snapshot(&path, &rendered)?,
            RunViewFixtureMode::Write => write_snapshot(&path, rendered)?,
        }
    }

    Ok(RunViewFixtureSummary {
        case_count: cases.len(),
        run_view_count,
        snapshot_count,
    })
}

fn fixture_producer() -> ViewProducer {
    ViewProducer {
        name: "harn".to_string(),
        version: FIXTURE_PRODUCER_VERSION.to_string(),
    }
}

fn read_sorted_dirs(path: &Path) -> Result<Vec<PathBuf>, RunViewFixtureError> {
    let entries = fs::read_dir(path).map_err(|source| RunViewFixtureError::Io {
        operation: "read directory",
        path: path.to_path_buf(),
        source,
    })?;
    let mut dirs = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| RunViewFixtureError::Io {
            operation: "read directory entry in",
            path: path.to_path_buf(),
            source,
        })?;
        if entry.path().is_dir() {
            dirs.push(entry.path());
        }
    }
    dirs.sort();
    Ok(dirs)
}

fn read_sorted_json_files(path: &Path) -> Result<Vec<PathBuf>, RunViewFixtureError> {
    let entries = fs::read_dir(path).map_err(|source| RunViewFixtureError::Io {
        operation: "read directory",
        path: path.to_path_buf(),
        source,
    })?;
    let mut files = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| RunViewFixtureError::Io {
            operation: "read directory entry in",
            path: path.to_path_buf(),
            source,
        })?;
        if entry.path().is_file() && entry.path().extension() == Some(OsStr::new("json")) {
            files.push(entry.path());
        }
    }
    files.sort();
    Ok(files)
}

fn read_text(path: &Path) -> Result<String, RunViewFixtureError> {
    fs::read_to_string(path).map_err(|source| RunViewFixtureError::Io {
        operation: "read",
        path: path.to_path_buf(),
        source,
    })
}

fn utf8_file_name(path: &Path) -> Result<String, RunViewFixtureError> {
    path.file_name()
        .and_then(OsStr::to_str)
        .map(str::to_owned)
        .ok_or_else(|| invalid(path.display().to_string(), "path has no UTF-8 file name"))
}

fn utf8_file_stem(path: &Path) -> Result<String, RunViewFixtureError> {
    path.file_stem()
        .and_then(OsStr::to_str)
        .map(str::to_owned)
        .ok_or_else(|| invalid(path.display().to_string(), "path has no UTF-8 file stem"))
}

fn repo_relative_path(
    repository_root: &Path,
    path: &Path,
    case_name: &str,
) -> Result<String, RunViewFixtureError> {
    let relative = path.strip_prefix(repository_root).map_err(|error| {
        invalid(
            case_name,
            format!(
                "{} is not under repository root {}: {error}",
                path.display(),
                repository_root.display()
            ),
        )
    })?;
    Ok(relative
        .components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/"))
}

fn check_snapshot(path: &Path, rendered: &str) -> Result<(), RunViewFixtureError> {
    let expected = read_text(path)?;
    if expected.replace("\r\n", "\n") != rendered {
        return Err(RunViewFixtureError::Stale {
            path: path.to_path_buf(),
        });
    }
    Ok(())
}

fn write_snapshot(path: &Path, rendered: String) -> Result<(), RunViewFixtureError> {
    let parent = path.parent().ok_or_else(|| {
        invalid(
            path.display().to_string(),
            format!("{} has no parent", path.display()),
        )
    })?;
    fs::create_dir_all(parent).map_err(|source| RunViewFixtureError::Io {
        operation: "create directory",
        path: parent.to_path_buf(),
        source,
    })?;
    fs::write(path, rendered).map_err(|source| RunViewFixtureError::Io {
        operation: "write",
        path: path.to_path_buf(),
        source,
    })
}

fn render_snapshot<T: Serialize>(
    value: &T,
    schema: &str,
    case_name: &str,
) -> Result<String, RunViewFixtureError> {
    let value = serde_json::to_value(value).map_err(|error| {
        invalid(
            case_name,
            format!("failed to serialize projection: {error}"),
        )
    })?;
    assert_projection_metadata(&value, schema, case_name)?;
    serde_json::to_string_pretty(&value)
        .map(|json| format!("{json}\n"))
        .map_err(|error| invalid(case_name, format!("failed to render projection: {error}")))
}

fn assert_projection_metadata(
    value: &JsonValue,
    schema: &str,
    case_name: &str,
) -> Result<(), RunViewFixtureError> {
    ensure_case(case_name, value["schema"] == schema, "schema drifted")?;
    ensure_case(
        case_name,
        value["schema_version"] == 1,
        "schema_version drifted",
    )?;
    ensure_case(
        case_name,
        value["producer"]["name"] == "harn",
        "producer.name drifted",
    )?;
    ensure_case(
        case_name,
        value["producer"]["version"] == FIXTURE_PRODUCER_VERSION,
        "producer.version drifted",
    )
}

fn assert_case_coverage(
    case_name: &str,
    runs: &[RunView],
    session: &SessionView,
) -> Result<(), RunViewFixtureError> {
    match case_name {
        "legacy-sparse" => {
            let run = only_run(case_name, runs)?;
            ensure_case(case_name, run.run.run_id == "run_legacy_sparse", "run id")?;
            ensure_case(case_name, run.run.session_id.is_none(), "session id")?;
            ensure_case(case_name, run.run.workflow_id.is_empty(), "workflow id")?;
            ensure_case(
                case_name,
                run.failure.as_ref().map(|failure| failure.status.as_str()) == Some("failed"),
                "failure status",
            )
        }
        "root-transcript" => {
            let run = only_run(case_name, runs)?;
            ensure_case(case_name, run.transcript.present, "transcript presence")?;
            ensure_case(
                case_name,
                run.transcript.source.as_deref() == Some("run_root"),
                "transcript source",
            )?;
            ensure_case(
                case_name,
                run.transcript.message_count == 2,
                "message count",
            )?;
            ensure_case(case_name, run.providers.len() == 1, "provider count")
        }
        "session-lineage-failure-approval" => {
            ensure_case(
                case_name,
                session.session.session_id.as_deref() == Some("session_lineage"),
                "session id",
            )?;
            ensure_case(
                case_name,
                session.session.status == "failed",
                "session status",
            )?;
            ensure_case(case_name, session.session.run_count == 2, "run count")?;
            ensure_case(
                case_name,
                session.pending.approvals.len() == 1,
                "pending approval count",
            )?;
            ensure_case(
                case_name,
                runs[0].run.child_runs.len() == 1,
                "child run count",
            )?;
            ensure_case(
                case_name,
                runs.iter().any(|run| run.failure.is_some()),
                "failure coverage",
            )
        }
        "stage-transcript-active-auth" => {
            let run = only_run(case_name, runs)?;
            ensure_case(case_name, run.run.status == "running", "run status")?;
            ensure_case(
                case_name,
                run.transcript.source.as_deref() == Some("stages"),
                "transcript source",
            )?;
            ensure_case(
                case_name,
                run.transcript.message_count == 2,
                "message count",
            )?;
            ensure_case(case_name, run.pending.auth.len() == 2, "pending auth count")?;
            ensure_case(
                case_name,
                session.session.status == "active",
                "session status",
            )
        }
        other => Err(invalid(other, "unrecognized fixture case")),
    }
}

fn only_run<'a>(case_name: &str, runs: &'a [RunView]) -> Result<&'a RunView, RunViewFixtureError> {
    ensure_case(case_name, runs.len() == 1, "expected exactly one run")?;
    Ok(&runs[0])
}

fn ensure_case(
    case_name: &str,
    condition: bool,
    detail: impl Into<String>,
) -> Result<(), RunViewFixtureError> {
    if condition {
        Ok(())
    } else {
        Err(invalid(case_name, detail))
    }
}

fn invalid(case: impl Into<String>, detail: impl Into<String>) -> RunViewFixtureError {
    RunViewFixtureError::Invalid {
        case: case.into(),
        detail: detail.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_snapshot_fails_closed() {
        let temp = tempfile::tempdir().expect("temporary fixture root");
        let path = temp.path().join("stale.json");
        fs::write(&path, "{}\n").expect("seed stale snapshot");
        let error = check_snapshot(&path, "{\"current\":true}\n")
            .expect_err("stale fixture must fail closed");
        assert!(matches!(error, RunViewFixtureError::Stale { path: stale } if stale == path));
    }
}
