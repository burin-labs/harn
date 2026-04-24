use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::http::StatusCode;
use axum::Json;
use tokio::process::Command;

use super::dto::{
    MaterializedLaunchTarget, PortalLaunchJob, PortalLaunchRequest, PortalLaunchTarget,
};
use super::errors::{bad_request_error, internal_error};
use super::query::ErrorResponse;
use super::run_analysis::scan_runs;
use super::state::PortalState;
use super::util::portal_timestamp_id;

pub(super) async fn create_launch_job(
    state: &Arc<PortalState>,
    request: PortalLaunchRequest,
) -> Result<PortalLaunchJob, (StatusCode, Json<ErrorResponse>)> {
    validate_launch_request(&request)?;

    let job_id = portal_timestamp_id("job");
    let started_at = portal_timestamp_id("started");
    let before_paths = known_run_paths(&state.run_dir).map_err(internal_error)?;
    let launch_env = validated_env_overrides(request.env.as_ref())?;
    let materialized =
        materialize_launch_target(&state.run_dir, &state.workspace_root, &job_id, request)
            .map_err(internal_error)?;
    let transcript_path = materialized.workspace_dir.as_ref().map(|dir| {
        dir.join("run-llm")
            .join("llm_transcript.jsonl")
            .display()
            .to_string()
    });

    let job = PortalLaunchJob {
        id: job_id.clone(),
        mode: materialized.mode.clone(),
        target_label: materialized.target_label.clone(),
        status: "running".to_string(),
        started_at,
        finished_at: None,
        exit_code: None,
        logs: String::new(),
        discovered_run_paths: Vec::new(),
        workspace_dir: materialized
            .workspace_dir
            .as_ref()
            .map(|path| path.display().to_string()),
        transcript_path,
    };
    state
        .launch_jobs
        .lock()
        .await
        .insert(job_id.clone(), job.clone());

    let jobs = state.launch_jobs.clone();
    let launch_program = state.launch_program.clone();
    let run_dir = state.run_dir.clone();
    let workspace_root = state.workspace_root.clone();
    tokio::spawn(async move {
        let output = Command::new(&launch_program)
            .arg("run")
            .arg(&materialized.launch_file)
            .current_dir(&workspace_root)
            .envs(build_launch_env(
                materialized.workspace_dir.as_deref(),
                &launch_env,
            ))
            .output()
            .await;

        let mut jobs = jobs.lock().await;
        if let Some(job) = jobs.get_mut(&job_id) {
            match output {
                Ok(output) => {
                    let mut logs = String::new();
                    logs.push_str(&String::from_utf8_lossy(&output.stdout));
                    if !output.stderr.is_empty() {
                        if !logs.is_empty() {
                            logs.push('\n');
                        }
                        logs.push_str(&String::from_utf8_lossy(&output.stderr));
                    }
                    job.logs = logs;
                    job.exit_code = output.status.code();
                    job.status = if output.status.success() {
                        "completed".to_string()
                    } else {
                        "failed".to_string()
                    };
                    job.finished_at = Some(portal_timestamp_id("finished"));
                    job.discovered_run_paths =
                        discovered_run_paths(&run_dir, &before_paths).unwrap_or_default();
                }
                Err(error) => {
                    job.status = "failed".to_string();
                    job.finished_at = Some(portal_timestamp_id("finished"));
                    job.logs = format!("failed to start harn run: {error}");
                }
            }
        }
    });

    Ok(job)
}

pub(super) async fn create_trigger_replay_job(
    state: &Arc<PortalState>,
    event_id: &str,
) -> Result<PortalLaunchJob, (StatusCode, Json<ErrorResponse>)> {
    let event_id = event_id.trim();
    if event_id.is_empty() {
        return Err(bad_request_error("event_id is required"));
    }

    let job_id = portal_timestamp_id("job");
    let started_at = portal_timestamp_id("started");
    let before_paths = known_run_paths(&state.run_dir).map_err(internal_error)?;
    let job = PortalLaunchJob {
        id: job_id.clone(),
        mode: "trigger_replay".to_string(),
        target_label: format!("trigger replay {event_id}"),
        status: "running".to_string(),
        started_at,
        finished_at: None,
        exit_code: None,
        logs: String::new(),
        discovered_run_paths: Vec::new(),
        workspace_dir: None,
        transcript_path: None,
    };
    state
        .launch_jobs
        .lock()
        .await
        .insert(job_id.clone(), job.clone());

    let jobs = state.launch_jobs.clone();
    let launch_program = state.launch_program.clone();
    let run_dir = state.run_dir.clone();
    let workspace_root = state.workspace_root.clone();
    let event_id = event_id.to_string();
    tokio::spawn(async move {
        let output = Command::new(&launch_program)
            .arg("trigger")
            .arg("replay")
            .arg(&event_id)
            .current_dir(&workspace_root)
            .output()
            .await;

        let mut jobs = jobs.lock().await;
        if let Some(job) = jobs.get_mut(&job_id) {
            match output {
                Ok(output) => {
                    let mut logs = String::new();
                    logs.push_str(&String::from_utf8_lossy(&output.stdout));
                    if !output.stderr.is_empty() {
                        if !logs.is_empty() {
                            logs.push('\n');
                        }
                        logs.push_str(&String::from_utf8_lossy(&output.stderr));
                    }
                    job.logs = logs;
                    job.exit_code = output.status.code();
                    job.status = if output.status.success() {
                        "completed".to_string()
                    } else {
                        "failed".to_string()
                    };
                    job.finished_at = Some(portal_timestamp_id("finished"));
                    job.discovered_run_paths =
                        discovered_run_paths(&run_dir, &before_paths).unwrap_or_default();
                }
                Err(error) => {
                    job.status = "failed".to_string();
                    job.finished_at = Some(portal_timestamp_id("finished"));
                    job.logs = format!("failed to start trigger replay: {error}");
                }
            }
        }
    });

    Ok(job)
}

pub(super) fn validate_launch_request(
    request: &PortalLaunchRequest,
) -> Result<(), (StatusCode, Json<ErrorResponse>)> {
    let has_file = request
        .file_path
        .as_ref()
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false);
    let has_source = request
        .source
        .as_ref()
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false);
    let has_playground = request
        .task
        .as_ref()
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false);
    let count = [has_file, has_source, has_playground]
        .into_iter()
        .filter(|value| *value)
        .count();
    if count != 1 {
        return Err(bad_request_error(
            "launch requires exactly one of file_path, source, or task",
        ));
    }
    Ok(())
}

pub(super) fn validated_env_overrides(
    env: Option<&BTreeMap<String, String>>,
) -> Result<BTreeMap<String, String>, (StatusCode, Json<ErrorResponse>)> {
    let mut validated = BTreeMap::new();
    if let Some(env) = env {
        for (key, value) in env {
            if key.is_empty()
                || !key
                    .chars()
                    .all(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit() || ch == '_')
            {
                return Err(bad_request_error(format!(
                    "invalid env key '{key}'; use uppercase shell-style names only"
                )));
            }
            if value.len() > 16_384 {
                return Err(bad_request_error(format!(
                    "env value for '{key}' is too large"
                )));
            }
            validated.insert(key.clone(), value.clone());
        }
    }
    Ok(validated)
}

pub(super) fn build_launch_env(
    workspace_dir: Option<&Path>,
    overrides: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    let mut env = overrides.clone();
    if let Some(workspace_dir) = workspace_dir {
        env.insert(
            "HARN_LLM_TRANSCRIPT_DIR".to_string(),
            workspace_dir.join("run-llm").display().to_string(),
        );
    }
    env
}

pub(super) fn materialize_launch_target(
    run_dir: &Path,
    workspace_root: &Path,
    job_id: &str,
    request: PortalLaunchRequest,
) -> Result<MaterializedLaunchTarget, String> {
    if let Some(file_path) = request.file_path.filter(|value| !value.trim().is_empty()) {
        let path = resolve_workspace_file(workspace_root, &file_path)?;
        return Ok(MaterializedLaunchTarget {
            mode: "file".to_string(),
            target_label: file_path,
            launch_file: path,
            workspace_dir: None,
        });
    }

    if let Some(source) = request.source.filter(|value| !value.trim().is_empty()) {
        let workspace_dir = launch_workspace_dir(run_dir, job_id)?;
        let launch_file = workspace_dir.join("workflow.harn");
        fs::write(&launch_file, source)
            .map_err(|error| format!("failed to write temp source file: {error}"))?;
        write_launch_metadata(
            &workspace_dir,
            &serde_json::json!({
                "mode": "source",
                "source_path": launch_file.display().to_string(),
            }),
        )?;
        return Ok(MaterializedLaunchTarget {
            mode: "source".to_string(),
            target_label: launch_file.display().to_string(),
            launch_file,
            workspace_dir: Some(workspace_dir),
        });
    }

    let task = request
        .task
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| "playground task is required".to_string())?;
    let workspace_dir = launch_workspace_dir(run_dir, job_id)?;
    let source = build_playground_source(
        &workspace_dir,
        &task,
        request.provider.as_deref(),
        request.model.as_deref(),
    );
    let launch_file = workspace_dir.join("workflow.harn");
    fs::write(&launch_file, source)
        .map_err(|error| format!("failed to write playground source file: {error}"))?;
    fs::write(workspace_dir.join("task.txt"), &task)
        .map_err(|error| format!("failed to write playground task file: {error}"))?;
    write_launch_metadata(
        &workspace_dir,
        &serde_json::json!({
            "mode": "playground",
            "task": task,
            "provider": request.provider.as_deref(),
            "model": request.model.as_deref(),
            "workflow_path": launch_file.display().to_string(),
            "run_path": workspace_dir.join("run.json").display().to_string(),
            "transcript_path": workspace_dir.join("run-llm").join("llm_transcript.jsonl").display().to_string(),
        }),
    )?;
    Ok(MaterializedLaunchTarget {
        mode: "playground".to_string(),
        target_label: format!("playground: {task}"),
        launch_file,
        workspace_dir: Some(workspace_dir),
    })
}

fn launch_workspace_dir(run_dir: &Path, job_id: &str) -> Result<PathBuf, String> {
    let dir = run_dir.join("playground").join(job_id);
    fs::create_dir_all(&dir).map_err(|error| {
        format!(
            "failed to create launch workspace {}: {error}",
            dir.display()
        )
    })?;
    Ok(dir)
}

fn write_launch_metadata(workspace_dir: &Path, payload: &serde_json::Value) -> Result<(), String> {
    let content = serde_json::to_string_pretty(payload)
        .map_err(|error| format!("failed to encode launch metadata: {error}"))?;
    fs::write(workspace_dir.join("launch.json"), content)
        .map_err(|error| format!("failed to write launch metadata: {error}"))
}

fn resolve_workspace_file(workspace_root: &Path, relative_path: &str) -> Result<PathBuf, String> {
    let path = Path::new(relative_path);
    if path.is_absolute() {
        return Err("file_path must be relative to the current workspace".to_string());
    }
    if path.components().any(|component| {
        matches!(
            component,
            std::path::Component::ParentDir
                | std::path::Component::RootDir
                | std::path::Component::Prefix(_)
        )
    }) {
        return Err("file_path must stay inside the current workspace".to_string());
    }
    let resolved = workspace_root.join(path);
    if !resolved.exists() {
        return Err(format!("file not found: {relative_path}"));
    }
    Ok(resolved)
}

fn build_playground_source(
    workspace_dir: &Path,
    task: &str,
    provider: Option<&str>,
    model: Option<&str>,
) -> String {
    let persist_path = workspace_dir.join("run.json");
    let provider_line = provider
        .filter(|value| !value.trim().is_empty())
        .map(|value| format!("provider: {:?},", value))
        .unwrap_or_default();
    let model_line = model
        .filter(|value| !value.trim().is_empty())
        .map(|value| format!("model: {:?},", value))
        .unwrap_or_default();

    format!(
        r#"pipeline main() {{
  let flow = workflow_graph(
    {{
    name: "portal_playground",
    entry: "act",
    nodes: {{
    act: {{
    kind: "stage",
    mode: "llm",
    model_policy: {{{provider_line}{model_line}}},
    output_contract: {{output_kinds: ["summary"]}},
  }},
  }},
    edges: [],
  }},
  )
  let seed = artifact({{kind: "summary", text: "Playground seed context", relevance: 0.5}})
  let workspace_note = artifact({{
    kind: "workspace_file",
    title: "task.txt",
    text: {task:?},
    relevance: 0.9,
  }})
  let result = workflow_execute(
    {task:?},
    flow,
    [seed, workspace_note],
    {{max_steps: 4, persist_path: {:?}}},
  )
  println(result?.status)
  println(result?.run?.persisted_path)
}}"#,
        persist_path.display().to_string()
    )
}

fn known_run_paths(run_dir: &Path) -> Result<HashSet<String>, String> {
    Ok(scan_runs(run_dir)?
        .into_iter()
        .map(|run| run.path)
        .collect::<HashSet<_>>())
}

fn discovered_run_paths(
    run_dir: &Path,
    before_paths: &HashSet<String>,
) -> Result<Vec<String>, String> {
    let mut paths = scan_runs(run_dir)?
        .into_iter()
        .map(|run| run.path)
        .filter(|path| !before_paths.contains(path))
        .collect::<Vec<_>>();
    paths.sort();
    Ok(paths)
}

pub(super) fn scan_launch_targets(
    workspace_root: &Path,
) -> Result<Vec<PortalLaunchTarget>, String> {
    let groups = [
        ("examples", workspace_root.join("examples")),
        ("conformance", workspace_root.join("conformance/tests")),
    ];
    let mut targets = Vec::new();
    for (group, root) in groups {
        collect_launch_targets(workspace_root, &root, group, &mut targets)?;
    }
    targets.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(targets)
}

fn collect_launch_targets(
    workspace_root: &Path,
    current: &Path,
    group: &str,
    out: &mut Vec<PortalLaunchTarget>,
) -> Result<(), String> {
    if !current.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(current)
        .map_err(|error| format!("failed to read {}: {error}", current.display()))?
    {
        let entry =
            entry.map_err(|error| format!("failed to iterate {}: {error}", current.display()))?;
        let path = entry.path();
        if path.is_dir() {
            collect_launch_targets(workspace_root, &path, group, out)?;
        } else if path.extension().is_some_and(|ext| ext == "harn") {
            let relative = path
                .strip_prefix(workspace_root)
                .map_err(|error| format!("failed to relativize {}: {error}", path.display()))?
                .display()
                .to_string();
            out.push(PortalLaunchTarget {
                path: relative,
                group: group.to_string(),
            });
        }
    }
    Ok(())
}
