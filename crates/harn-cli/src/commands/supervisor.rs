use std::fs::OpenOptions;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value as JsonValue};

use crate::cli::{
    OrchestratorLocalArgs, SupervisorArgs, SupervisorCommand, SupervisorDlqCommand,
    SupervisorDlqListArgs, SupervisorDlqReplayArgs, SupervisorFireArgs, SupervisorInspectArgs,
    SupervisorListArgs, SupervisorPauseArgs, SupervisorRecoverArgs, SupervisorReplayArgs,
    SupervisorResumeArgs, SupervisorStartArgs, SupervisorStopArgs,
};
use crate::commands::orchestrator::common::{
    absolutize_from_cwd, load_local_runtime, print_json, stranded_envelopes,
    synthetic_event_for_binding, trigger_fire, trigger_inspect_dlq, trigger_replay,
    DispatchHandleRecord, DlqEntryRecord, StrandedEnvelopeRecord,
};
use crate::commands::orchestrator::errors::{OrchestratorError, OrchestratorResult};
use crate::commands::orchestrator::inspect_data::{
    collect_orchestrator_inspect_data, OrchestratorInspectData, RecentDispatchRecord,
    TriggerInspectMetrics,
};
use crate::commands::orchestrator::supervisor_state::{
    now_rfc3339, read_supervisor_state, set_workflow_status, supervisor_state_path,
    workflow_notification_hint, workflow_override, write_supervisor_state,
    WorkflowSupervisorNotificationHint, WorkflowSupervisorProcess,
};

#[derive(Debug, Serialize)]
struct SupervisorSnapshot {
    schema_version: u32,
    process: Option<SupervisorProcessSnapshot>,
    workflows: Vec<SupervisorWorkflowSnapshot>,
    events: Vec<SupervisorEvent>,
    dlq: SupervisorDlqSummary,
    catchup_needed: Vec<StrandedEnvelopeRecord>,
    inspect: OrchestratorInspectData,
}

#[derive(Debug, Serialize)]
struct SupervisorProcessSnapshot {
    pid: u32,
    status: String,
    alive: bool,
    config_path: String,
    state_dir: String,
    bind: String,
    log_path: String,
    started_at: String,
    stopped_at: Option<String>,
}

#[derive(Debug, Serialize)]
struct SupervisorWorkflowSnapshot {
    workflow_id: String,
    status: String,
    provider: String,
    kind: String,
    handler: String,
    version: Option<u32>,
    metrics: TriggerInspectMetrics,
    reason: Option<String>,
    notification_hint: WorkflowSupervisorNotificationHint,
}

#[derive(Debug, Serialize)]
struct SupervisorEvent {
    name: &'static str,
    workflow_id: Option<String>,
    event_id: Option<String>,
    occurred_at_ms: Option<i64>,
    source: &'static str,
    detail: JsonValue,
}

#[derive(Debug, Serialize)]
struct SupervisorDlqSummary {
    pending_entries: usize,
    entries: Vec<DlqEntryRecord>,
}

#[derive(Debug, Serialize)]
struct SupervisorAction<T: Serialize> {
    status: String,
    workflow_id: Option<String>,
    process: Option<SupervisorProcessSnapshot>,
    result: T,
}

enum SupervisorStartWait {
    Ready,
    Exited,
    TimedOut,
}

#[derive(Debug, Deserialize)]
struct PersistedOrchestratorStatus {
    status: Option<String>,
}

pub(crate) async fn handle(args: SupervisorArgs) -> OrchestratorResult<()> {
    match args.command {
        SupervisorCommand::Start(args) => run_start(args).await,
        SupervisorCommand::Stop(args) => run_stop(args).await,
        SupervisorCommand::List(args) => run_list(args).await,
        SupervisorCommand::Inspect(args) => run_inspect(args).await,
        SupervisorCommand::Pause(args) => run_pause(args).await,
        SupervisorCommand::Resume(args) => run_resume(args).await,
        SupervisorCommand::Fire(args) => run_fire(args).await,
        SupervisorCommand::Replay(args) => run_replay(args).await,
        SupervisorCommand::Dlq(args) => match args.command {
            SupervisorDlqCommand::List(list) => run_dlq_list(list).await,
            SupervisorDlqCommand::Replay(replay) => run_dlq_replay(replay).await,
        },
        SupervisorCommand::Recover(args) => run_recover(args).await,
    }
}

async fn run_start(args: SupervisorStartArgs) -> OrchestratorResult<()> {
    let config_path = absolutize_from_cwd(&args.local.config)?;
    let state_dir = absolutize_from_cwd(&args.local.state_dir)?;
    std::fs::create_dir_all(&state_dir).map_err(|error| {
        format!(
            "failed to create supervisor state dir {}: {error}",
            state_dir.display()
        )
    })?;
    let log_path = args
        .log_file
        .map(|path| absolutize_from_cwd(&path))
        .transpose()?
        .unwrap_or_else(|| state_dir.join("workflow-supervisor.log"));
    if let Some(parent) = log_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("failed to create log dir {}: {error}", parent.display()))?;
    }

    let mut state = read_supervisor_state(&state_dir)?;
    if let Some(process) = state.process.as_ref() {
        if process_is_alive(process.pid) {
            return Err(format!(
                "workflow supervisor is already running as pid {}",
                process.pid
            )
            .into());
        }
    }

    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .map_err(|error| format!("failed to open {}: {error}", log_path.display()))?;
    let stderr = log
        .try_clone()
        .map_err(|error| format!("failed to clone log handle: {error}"))?;
    let mut command = Command::new(std::env::current_exe().map_err(|error| error.to_string())?);
    command
        .arg("orchestrator")
        .arg("serve")
        .arg("--config")
        .arg(&config_path)
        .arg("--state-dir")
        .arg(&state_dir)
        .arg("--bind")
        .arg(args.bind.to_string())
        .arg("--log-format")
        .arg("json")
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(stderr));
    if args.mcp {
        command.arg("--mcp");
    }
    let mut child = command
        .spawn()
        .map_err(|error| format!("failed to start workflow supervisor: {error}"))?;
    let started_at = now_rfc3339()?;
    state.schema_version = 1;
    state.updated_at = Some(started_at.clone());
    state.process = Some(WorkflowSupervisorProcess {
        pid: child.id(),
        status: "starting".to_string(),
        config_path: config_path.display().to_string(),
        state_dir: state_dir.display().to_string(),
        bind: args.bind.to_string(),
        log_path: log_path.display().to_string(),
        started_at,
        stopped_at: None,
    });
    write_supervisor_state(&state_dir, &state)?;

    let wait = wait_for_running_snapshot(&mut child, &state_dir, args.wait).await?;
    let ready = matches!(wait, SupervisorStartWait::Ready);
    let status = match wait {
        SupervisorStartWait::Ready => "running",
        SupervisorStartWait::Exited => "failed",
        SupervisorStartWait::TimedOut => "starting",
    };
    let mut state = read_supervisor_state(&state_dir)?;
    if let Some(process) = state.process.as_mut() {
        process.status = status.to_string();
    }
    state.updated_at = Some(now_rfc3339()?);
    write_supervisor_state(&state_dir, &state)?;

    let process = state.process.as_ref().map(process_snapshot);
    if status == "failed" {
        return Err(format!(
            "workflow supervisor exited before readiness; see {}",
            log_path.display()
        )
        .into());
    }
    if args.json {
        return print_json(&SupervisorAction {
            status: status.to_string(),
            workflow_id: None,
            process,
            result: json!({ "ready": ready }),
        });
    }
    if ready {
        println!(
            "workflow supervisor running pid={} state_dir={}",
            child.id(),
            state_dir.display()
        );
    } else {
        println!(
            "workflow supervisor starting pid={} state_dir={} log={}",
            child.id(),
            state_dir.display(),
            log_path.display()
        );
    }
    Ok(())
}

async fn run_stop(args: SupervisorStopArgs) -> OrchestratorResult<()> {
    let state_dir = absolutize_from_cwd(&args.local.state_dir)?;
    let mut state = read_supervisor_state(&state_dir)?;
    let Some(process) = state.process.as_mut() else {
        return Err("workflow supervisor has no recorded process"
            .to_string()
            .into());
    };
    let pid = process.pid;
    let was_alive = process_is_alive(pid);
    if was_alive {
        terminate_process(pid)?;
    }
    let stopped = wait_for_process_exit(pid, Duration::from_secs(10)).await;
    process.status = if stopped { "stopped" } else { "stopping" }.to_string();
    process.stopped_at = Some(now_rfc3339()?);
    state.updated_at = process.stopped_at.clone();
    write_supervisor_state(&state_dir, &state)?;
    let process = state.process.as_ref().map(process_snapshot);
    if args.json {
        return print_json(&SupervisorAction {
            status: if stopped { "stopped" } else { "stopping" }.to_string(),
            workflow_id: None,
            process,
            result: json!({ "was_alive": was_alive, "stopped": stopped }),
        });
    }
    println!(
        "workflow supervisor {} pid={pid}",
        if stopped { "stopped" } else { "stopping" }
    );
    Ok(())
}

async fn run_list(args: SupervisorListArgs) -> OrchestratorResult<()> {
    let snapshot = supervisor_snapshot(&args.local).await?;
    if args.json {
        return print_json(&snapshot);
    }
    println!("Workflow supervisor:");
    if let Some(process) = snapshot.process.as_ref() {
        println!(
            "- process pid={} status={} alive={}",
            process.pid, process.status, process.alive
        );
    } else {
        println!("- process none");
    }
    println!("Workflows:");
    if snapshot.workflows.is_empty() {
        println!("- none");
    } else {
        for workflow in &snapshot.workflows {
            println!(
                "- {} status={} provider={} kind={}",
                workflow.workflow_id, workflow.status, workflow.provider, workflow.kind
            );
        }
    }
    println!("- dlq_pending={}", snapshot.dlq.pending_entries);
    println!("- catchup_needed={}", snapshot.catchup_needed.len());
    Ok(())
}

async fn run_inspect(args: SupervisorInspectArgs) -> OrchestratorResult<()> {
    let mut snapshot = supervisor_snapshot(&args.local).await?;
    if let Some(workflow_id) = args.workflow_id.as_deref() {
        snapshot
            .workflows
            .retain(|workflow| workflow.workflow_id == workflow_id);
        if snapshot.workflows.is_empty() {
            return Err(format!("unknown supervised workflow '{workflow_id}'").into());
        }
        snapshot
            .events
            .retain(|event| event.workflow_id.as_deref() == Some(workflow_id));
        snapshot
            .dlq
            .entries
            .retain(|entry| entry.binding_id == workflow_id);
        snapshot.dlq.pending_entries = snapshot.dlq.entries.len();
        snapshot
            .catchup_needed
            .retain(|entry| entry.trigger_id.as_deref() == Some(workflow_id));
    }
    if args.json {
        return print_json(&snapshot);
    }
    for workflow in &snapshot.workflows {
        println!(
            "{} status={} version={}",
            workflow.workflow_id,
            workflow.status,
            workflow
                .version
                .map(|version| version.to_string())
                .unwrap_or_else(|| "-".to_string())
        );
    }
    Ok(())
}

async fn run_pause(args: SupervisorPauseArgs) -> OrchestratorResult<()> {
    set_status_and_apply(
        &args.local,
        &args.workflow_id,
        "paused",
        args.reason,
        args.json,
    )
    .await
}

async fn run_resume(args: SupervisorResumeArgs) -> OrchestratorResult<()> {
    set_status_and_apply(
        &args.local,
        &args.workflow_id,
        "running",
        args.reason,
        args.json,
    )
    .await
}

async fn set_status_and_apply(
    local: &OrchestratorLocalArgs,
    workflow_id: &str,
    status: &str,
    reason: Option<String>,
    json_output: bool,
) -> OrchestratorResult<()> {
    let state_dir = absolutize_from_cwd(&local.state_dir)?;
    let config_path = absolutize_from_cwd(&local.config)?;
    let mut ctx = load_local_runtime(local).await?;
    if !ctx
        .collected_triggers
        .iter()
        .any(|trigger| trigger.config.id == workflow_id)
    {
        return Err(format!("unknown supervised workflow '{workflow_id}'").into());
    }
    let mut state = read_supervisor_state(&state_dir)?;
    let workflow = set_workflow_status(
        &mut state,
        Some(&config_path),
        &state_dir,
        workflow_id,
        status,
        reason,
    )?;
    write_supervisor_state(&state_dir, &state)?;
    let signaled_pid = signal_process_reload(state.process.as_ref())?;
    match status {
        "paused" => harn_vm::pause(workflow_id)
            .await
            .map_err(|error| format!("failed to pause workflow '{workflow_id}': {error}"))?,
        "running" => harn_vm::resume(workflow_id)
            .await
            .map_err(|error| format!("failed to resume workflow '{workflow_id}': {error}"))?,
        _ => {}
    }
    let inspect = collect_orchestrator_inspect_data(&mut ctx).await?;
    if json_output {
        return print_json(&SupervisorAction {
            status: status.to_string(),
            workflow_id: Some(workflow_id.to_string()),
            process: state.process.as_ref().map(process_snapshot),
            result: json!({
                "workflow": workflow,
                "inspect": inspect,
                "state_file": supervisor_state_path(&state_dir),
                "signaled_pid": signaled_pid,
            }),
        });
    }
    println!("{workflow_id} {status}");
    Ok(())
}

async fn run_fire(args: SupervisorFireArgs) -> OrchestratorResult<()> {
    let mut ctx = load_local_runtime(&args.local).await?;
    let mut event = synthetic_event_for_binding(&ctx, &args.workflow_id)?;
    let payload: JsonValue = serde_json::from_str(&args.payload_json)
        .map_err(|error| format!("failed to parse --payload-json: {error}"))?;
    merge_json_object(&mut event, payload);
    let handle = trigger_fire(&mut ctx, &args.workflow_id, event).await?;
    if args.json {
        return print_json(&handle);
    }
    print_dispatch_handle(&handle);
    Ok(())
}

async fn run_replay(args: SupervisorReplayArgs) -> OrchestratorResult<()> {
    let mut ctx = load_local_runtime(&args.local).await?;
    let handle = trigger_replay(&mut ctx, &args.event_id).await?;
    if args.json {
        return print_json(&handle);
    }
    print_dispatch_handle(&handle);
    Ok(())
}

async fn run_dlq_list(args: SupervisorDlqListArgs) -> OrchestratorResult<()> {
    let mut ctx = load_local_runtime(&args.local).await?;
    let entries = trigger_inspect_dlq(&mut ctx).await?;
    let payload = SupervisorDlqSummary {
        pending_entries: entries.len(),
        entries,
    };
    if args.json {
        return print_json(&payload);
    }
    println!("DLQ pending_entries={}", payload.pending_entries);
    for entry in &payload.entries {
        println!(
            "- {} workflow={} event={}",
            entry.id, entry.binding_id, entry.event_id
        );
    }
    Ok(())
}

async fn run_dlq_replay(args: SupervisorDlqReplayArgs) -> OrchestratorResult<()> {
    let mut ctx = load_local_runtime(&args.local).await?;
    let entries = trigger_inspect_dlq(&mut ctx).await?;
    let entry = entries
        .iter()
        .find(|entry| entry.id == args.entry_id)
        .ok_or_else(|| format!("unknown pending DLQ entry '{}'", args.entry_id))?;
    let handle = trigger_replay(&mut ctx, &entry.event_id).await?;
    if args.json {
        return print_json(&SupervisorAction {
            status: handle.status.clone(),
            workflow_id: Some(entry.binding_id.clone()),
            process: None,
            result: json!({ "entry_id": entry.id, "handle": handle }),
        });
    }
    print_dispatch_handle(&handle);
    Ok(())
}

async fn run_recover(args: SupervisorRecoverArgs) -> OrchestratorResult<()> {
    if !args.dry_run && !args.yes {
        return Err(
            "refusing to replay stranded events without --yes; pass --dry-run to inspect first"
                .to_string()
                .into(),
        );
    }
    let ctx = load_local_runtime(&args.local).await?;
    let stranded = stranded_envelopes(&ctx.event_log, args.envelope_age).await?;
    let mut replayed = Vec::new();
    if !args.dry_run {
        let mut replay_ctx = ctx;
        for envelope in &stranded {
            replayed.push(trigger_replay(&mut replay_ctx, &envelope.event_id).await?);
        }
    }
    let payload = json!({
        "dry_run": args.dry_run,
        "stranded_envelopes": stranded,
        "replayed": replayed,
    });
    if args.json {
        return print_json(&payload);
    }
    println!(
        "Recovery stranded_envelopes={} replayed={}",
        payload["stranded_envelopes"]
            .as_array()
            .map(Vec::len)
            .unwrap_or(0),
        payload["replayed"].as_array().map(Vec::len).unwrap_or(0)
    );
    Ok(())
}

async fn supervisor_snapshot(
    local: &OrchestratorLocalArgs,
) -> OrchestratorResult<SupervisorSnapshot> {
    let state_dir = absolutize_from_cwd(&local.state_dir)?;
    let config_path = absolutize_from_cwd(&local.config)?;
    let mut ctx = load_local_runtime(local).await?;
    let state = read_supervisor_state(&state_dir)?;
    let inspect = collect_orchestrator_inspect_data(&mut ctx).await?;
    let dlq_entries = trigger_inspect_dlq(&mut ctx).await?;
    let catchup_needed = stranded_envelopes(&ctx.event_log, Duration::ZERO).await?;

    let workflows = inspect
        .triggers
        .iter()
        .map(|trigger| {
            let override_state = workflow_override(&state, &trigger.id);
            SupervisorWorkflowSnapshot {
                workflow_id: trigger.id.clone(),
                status: override_state
                    .map(|workflow| workflow.status.clone())
                    .or_else(|| trigger.state.clone())
                    .unwrap_or_else(|| "unknown".to_string()),
                provider: trigger.provider.clone(),
                kind: trigger.kind.clone(),
                handler: trigger.handler.clone(),
                version: trigger.version,
                metrics: trigger.metrics.clone(),
                reason: override_state.and_then(|workflow| workflow.reason.clone()),
                notification_hint: override_state
                    .map(|workflow| workflow.notification_hint.clone())
                    .unwrap_or_else(|| {
                        workflow_notification_hint(Some(&config_path), &state_dir, &trigger.id)
                    }),
            }
        })
        .collect::<Vec<_>>();

    let mut events = inspect
        .recent_dispatches
        .iter()
        .map(supervisor_event_from_dispatch)
        .collect::<Vec<_>>();
    events.extend(dlq_entries.iter().map(supervisor_event_from_dlq));
    events.extend(catchup_needed.iter().map(supervisor_event_from_stranded));

    Ok(SupervisorSnapshot {
        schema_version: 1,
        process: state.process.as_ref().map(process_snapshot),
        workflows,
        events,
        dlq: SupervisorDlqSummary {
            pending_entries: dlq_entries.len(),
            entries: dlq_entries,
        },
        catchup_needed,
        inspect,
    })
}

fn supervisor_event_from_dispatch(dispatch: &RecentDispatchRecord) -> SupervisorEvent {
    let name = stable_event_name(&dispatch.kind, &dispatch.status);
    SupervisorEvent {
        name,
        workflow_id: dispatch.trigger_id.clone(),
        event_id: dispatch.event_id.clone(),
        occurred_at_ms: Some(dispatch.occurred_at_ms),
        source: "trigger.outbox",
        detail: serde_json::to_value(dispatch).unwrap_or_else(|_| json!({})),
    }
}

fn supervisor_event_from_dlq(entry: &DlqEntryRecord) -> SupervisorEvent {
    SupervisorEvent {
        name: "workflow.dlq",
        workflow_id: Some(entry.binding_id.clone()),
        event_id: Some(entry.event_id.clone()),
        occurred_at_ms: None,
        source: "trigger.dlq",
        detail: serde_json::to_value(entry).unwrap_or_else(|_| json!({})),
    }
}

fn supervisor_event_from_stranded(entry: &StrandedEnvelopeRecord) -> SupervisorEvent {
    SupervisorEvent {
        name: "workflow.catchup_needed",
        workflow_id: entry.trigger_id.clone(),
        event_id: Some(entry.event_id.clone()),
        occurred_at_ms: None,
        source: "trigger.inbox",
        detail: serde_json::to_value(entry).unwrap_or_else(|_| json!({})),
    }
}

fn stable_event_name(kind: &str, status: &str) -> &'static str {
    let text = format!("{kind} {status}");
    if text.contains("dlq") {
        "workflow.dlq"
    } else if text.contains("wait") || text.contains("blocked") {
        "workflow.blocked"
    } else if text.contains("failed") || text.contains("error") {
        "workflow.failed"
    } else if text.contains("succeeded")
        || text.contains("completed")
        || text.contains("dispatched")
        || status == "skipped"
    {
        "workflow.completed"
    } else if text.contains("started") || text.contains("running") {
        "workflow.running"
    } else {
        "workflow.scheduled"
    }
}

fn process_snapshot(process: &WorkflowSupervisorProcess) -> SupervisorProcessSnapshot {
    SupervisorProcessSnapshot {
        pid: process.pid,
        status: process.status.clone(),
        alive: process_is_alive(process.pid),
        config_path: process.config_path.clone(),
        state_dir: process.state_dir.clone(),
        bind: process.bind.clone(),
        log_path: process.log_path.clone(),
        started_at: process.started_at.clone(),
        stopped_at: process.stopped_at.clone(),
    }
}

async fn wait_for_running_snapshot(
    child: &mut Child,
    state_dir: &Path,
    timeout: Duration,
) -> Result<SupervisorStartWait, OrchestratorError> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Ok(content) = std::fs::read_to_string(state_dir.join("orchestrator-state.json")) {
            if serde_json::from_str::<PersistedOrchestratorStatus>(&content)
                .map(|snapshot| snapshot.status.as_deref() == Some("running"))
                .unwrap_or(false)
            {
                return Ok(SupervisorStartWait::Ready);
            }
        }
        if child
            .try_wait()
            .map_err(|error| format!("failed to inspect supervisor process: {error}"))?
            .is_some()
        {
            return Ok(SupervisorStartWait::Exited);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Ok(SupervisorStartWait::TimedOut)
}

/// Whether `pid` is still a live process. Uses `sysinfo` (already a
/// dependency) rather than shelling out to `kill -0`, so it works on every
/// platform and avoids spawning a subprocess on the liveness-polling hot path
/// in `wait_for_process_exit`.
fn process_is_alive(pid: u32) -> bool {
    let pid = sysinfo::Pid::from_u32(pid);
    let mut sys = sysinfo::System::new();
    sys.refresh_processes_specifics(
        sysinfo::ProcessesToUpdate::Some(&[pid]),
        true,
        sysinfo::ProcessRefreshKind::nothing(),
    );
    sys.process(pid).is_some()
}

#[cfg(unix)]
fn terminate_process(pid: u32) -> Result<(), OrchestratorError> {
    let status = Command::new("kill")
        .arg("-TERM")
        .arg(pid.to_string())
        .status()
        .map_err(|error| format!("failed to signal pid {pid}: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("failed to signal pid {pid}: kill exited with {status}").into())
    }
}

#[cfg(not(unix))]
fn terminate_process(pid: u32) -> Result<(), OrchestratorError> {
    Err(format!("stopping supervisor pid {pid} is unsupported on this platform").into())
}

#[cfg(unix)]
fn signal_process_reload(
    process: Option<&WorkflowSupervisorProcess>,
) -> Result<Option<u32>, OrchestratorError> {
    let Some(process) = process else {
        return Ok(None);
    };
    if !process_is_alive(process.pid) {
        return Ok(None);
    }
    let status = Command::new("kill")
        .arg("-HUP")
        .arg(process.pid.to_string())
        .status()
        .map_err(|error| format!("failed to signal supervisor reload: {error}"))?;
    if status.success() {
        Ok(Some(process.pid))
    } else {
        Err(format!(
            "failed to signal supervisor pid {} for reload: kill exited with {status}",
            process.pid
        )
        .into())
    }
}

#[cfg(not(unix))]
fn signal_process_reload(
    _process: Option<&WorkflowSupervisorProcess>,
) -> Result<Option<u32>, OrchestratorError> {
    Ok(None)
}

async fn wait_for_process_exit(pid: u32, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if !process_is_alive(pid) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    !process_is_alive(pid)
}

fn merge_json_object(target: &mut JsonValue, patch: JsonValue) {
    let Some(target) = target.as_object_mut() else {
        return;
    };
    if let Some(patch) = patch.as_object() {
        for (key, value) in patch {
            target.insert(key.clone(), value.clone());
        }
    }
}

fn print_dispatch_handle(handle: &DispatchHandleRecord) {
    println!("workflow dispatch:");
    println!("- workflow_id={}", handle.binding_id);
    println!("- binding_version={}", handle.binding_version);
    println!("- event_id={}", handle.event_id);
    println!("- status={}", handle.status);
    println!(
        "- replay_of_event_id={}",
        handle.replay_of_event_id.as_deref().unwrap_or("-")
    );
    println!(
        "- dlq_entry_id={}",
        handle.dlq_entry_id.as_deref().unwrap_or("-")
    );
    println!("- error={}", handle.error.as_deref().unwrap_or("-"));
}
