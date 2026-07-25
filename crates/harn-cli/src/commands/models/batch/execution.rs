//! Durable orchestration for `harn models batch execute`.
//!
//! Provider policy and wire normalization stay in the embedded Harn modules.
//! This file owns the narrow host duties needed to sequence those modules:
//! exclusive file updates, atomic replacement, and process-crash boundaries.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest as _, Sha256};

use crate::cli::{ModelsBatchExecuteArgs, ModelsBatchExecuteCommand, ModelsBatchExecuteInitArgs};
use crate::commands::run::{RunOutcome, RunSandboxOptions};
use crate::env_guard::ScopedEnvVar;

const EXECUTION_FILE: &str = "execution.json";
const LOCK_DIR: &str = ".execution.lock";
const EXECUTION_KIND: &str = "harn.model_batch_execution_receipt";
const KILL_POINT_ENV: &str = "HARN_MODELS_BATCH_TEST_KILL_POINT";

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct ArtifactIdentity {
    role: String,
    path: PathBuf,
    sha256: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct ExecutionPaths {
    requests: PathBuf,
    manifest: PathBuf,
    prepare_receipt: PathBuf,
    submission: PathBuf,
    status: PathBuf,
    download_dir: PathBuf,
    download_receipt: PathBuf,
    rejoin_dir: PathBuf,
    rejoin_receipt: PathBuf,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProviderOperation {
    id: String,
    kind: String,
    status: String,
    idempotency_mode: String,
    receipt_path: PathBuf,
    receipt_sha256: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct ExecutionState {
    schema_version: u32,
    kind: String,
    execution_id: String,
    revision: u64,
    phase: String,
    dry_run: bool,
    consumable: bool,
    policy: Value,
    paths: ExecutionPaths,
    request_ids: Vec<String>,
    job_ids: Vec<String>,
    artifacts: Vec<ArtifactIdentity>,
    operation: Option<ProviderOperation>,
    history: Vec<Value>,
}

struct ExecutionLock(fs::File);

impl ExecutionLock {
    fn acquire(execution_dir: &Path) -> Result<Self, String> {
        let path = execution_dir.join(LOCK_DIR);
        let file = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)
            .map_err(|error| format!("failed to open batch execution lock: {error}"))?;
        match file.try_lock() {
            Ok(()) => Ok(Self(file)),
            Err(std::fs::TryLockError::WouldBlock) => Err(format!(
                "batch execution is already being advanced: {}",
                execution_dir.display()
            )),
            Err(error) => Err(format!("failed to acquire batch execution lock: {error}")),
        }
    }
}

impl Drop for ExecutionLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

pub(super) async fn run(args: ModelsBatchExecuteArgs) -> i32 {
    let json_mode = match &args.command {
        ModelsBatchExecuteCommand::Advance(args) => args.json,
        ModelsBatchExecuteCommand::Cancel(args) => args.json,
        ModelsBatchExecuteCommand::Init(args) => args.json,
        ModelsBatchExecuteCommand::Inspect(args) => args.json,
    };
    let result = match args.command {
        ModelsBatchExecuteCommand::Advance(args) => {
            advance(&absolute(&args.execution_dir), args.json).await
        }
        ModelsBatchExecuteCommand::Cancel(args) => {
            cancel(&absolute(&args.execution_dir), args.json).await
        }
        ModelsBatchExecuteCommand::Init(args) => {
            let json = args.json;
            init(args).await.map(|state| report(&state, json))
        }
        ModelsBatchExecuteCommand::Inspect(args) => {
            inspect(&absolute(&args.execution_dir), args.json)
        }
    };
    match result {
        Ok(()) => 0,
        Err(error) => {
            emit_error(&error, json_mode);
            1
        }
    }
}

async fn init(args: ModelsBatchExecuteInitArgs) -> Result<ExecutionState, String> {
    let execution_dir = absolute(&args.execution_dir);
    let requests = absolute(&args.requests);
    fs::create_dir_all(&execution_dir)
        .map_err(|error| format!("failed to create execution directory: {error}"))?;
    let _lock = ExecutionLock::acquire(&execution_dir)?;
    let state_path = execution_dir.join(EXECUTION_FILE);
    if state_path.exists() {
        return Err(format!(
            "batch execution already exists: {}",
            state_path.display()
        ));
    }
    if !requests.is_file() {
        return Err(format!("request JSONL not found: {}", requests.display()));
    }

    let durable_requests = execution_dir.join("requests.jsonl");
    fs::copy(&requests, &durable_requests)
        .map_err(|error| format!("failed to copy request ledger into execution: {error}"))?;
    let paths = execution_paths(&execution_dir, &durable_requests);
    let sandbox = sandbox_for(&execution_dir, None);
    let manifest_outcome = run_stage(
        "manifest",
        vec![
            ("HARN_MODELS_BATCH_REQUESTS", path_text(&durable_requests)),
            ("HARN_MODELS_BATCH_OUT", path_text(&paths.manifest)),
            (
                "HARN_MODELS_BATCH_PROVIDER",
                args.provider.as_deref().unwrap_or("").trim().to_string(),
            ),
            (
                "HARN_MODELS_BATCH_MODEL",
                args.model.as_deref().unwrap_or("").trim().to_string(),
            ),
            ("HARN_MODELS_BATCH_WORKLOAD", args.workload.clone()),
            ("HARN_MODELS_BATCH_TOOL_FORMAT", args.tool_format.clone()),
            ("HARN_MODELS_BATCH_ID_PREFIX", args.id_prefix.clone()),
            (
                "HARN_MODELS_BATCH_MIN_DISCOUNT_PERCENT",
                args.min_discount_percent
                    .map_or_else(String::new, |v| v.to_string()),
            ),
            (
                "HARN_MODELS_BATCH_MAX_TURNAROUND_HOURS",
                args.max_turnaround_hours
                    .map_or_else(String::new, |v| v.to_string()),
            ),
        ],
        sandbox.clone(),
    )
    .await;
    require_stage("manifest", &manifest_outcome)?;

    let prepare_outcome = run_stage(
        "prepare",
        vec![
            ("HARN_MODELS_BATCH_MANIFEST", path_text(&paths.manifest)),
            (
                "HARN_MODELS_BATCH_OUT_DIR",
                path_text(
                    paths
                        .prepare_receipt
                        .parent()
                        .ok_or("prepare receipt has no parent")?,
                ),
            ),
        ],
        sandbox,
    )
    .await;
    require_stage("prepare", &prepare_outcome)?;

    let manifest = read_object(&paths.manifest, "batch manifest")?;
    let prepare = read_object(&paths.prepare_receipt, "batch prepare receipt")?;
    validate_kind(&manifest, "harn.model_batch_manifest", "batch manifest")?;
    validate_kind(
        &prepare,
        "harn.model_batch_prepare_receipt",
        "batch prepare receipt",
    )?;
    verify_source(&prepare, "/manifest", &paths.manifest, "prepare manifest")?;

    let manifest_sha = hash_file(&paths.manifest)?;
    let execution_id = format!("batch-{}", &manifest_sha[..24]);
    let request_ids = manifest_request_ids(&manifest)?;
    let job_ids = receipt_job_ids(&prepare)?;
    let mut artifacts = vec![
        identity("requests", &durable_requests)?,
        identity("manifest", &paths.manifest)?,
        identity("prepare_receipt", &paths.prepare_receipt)?,
    ];
    for job in objects_at(&prepare, "/jobs")? {
        let request_file = string_at(job, "/request_file")?;
        artifacts.push(identity("prepared_request", Path::new(request_file))?);
    }
    let mut state = ExecutionState {
        schema_version: 1,
        kind: EXECUTION_KIND.to_string(),
        execution_id,
        revision: 1,
        phase: "prepared".to_string(),
        dry_run: args.dry_run,
        consumable: false,
        policy: json!({
            "provider": args.provider,
            "model": args.model,
            "workload": args.workload,
            "toolFormat": args.tool_format,
            "idPrefix": args.id_prefix,
            "minDiscountPercent": args.min_discount_percent,
            "maxTurnaroundHours": args.max_turnaround_hours,
        }),
        paths,
        request_ids,
        job_ids,
        artifacts,
        operation: None,
        history: vec![json!({"revision": 1, "from": null, "to": "prepared"})],
    };
    validate_state(&state)?;
    save_state(&mut state, None)?;
    Ok(state)
}

fn inspect(execution_dir: &Path, json_mode: bool) -> Result<(), String> {
    let (state, _) = load_state(execution_dir)?;
    validate_state(&state)?;
    report(&state, json_mode);
    Ok(())
}

async fn advance(execution_dir: &Path, json_mode: bool) -> Result<(), String> {
    let _guard = super::DISPATCH_BATCH_LOCK.lock().await;
    let lock = ExecutionLock::acquire(execution_dir)?;
    let (mut state, observed_sha) = load_state(execution_dir)?;
    validate_state(&state)?;
    if let Some(operation) = &state.operation {
        if operation.status == "dispatching" {
            if operation.receipt_path.is_file() {
                finish_operation(&mut state, &observed_sha)?;
                drop(lock);
                report(&state, json_mode);
                return Ok(());
            }
            if operation.kind == "submit"
                && operation.idempotency_mode == "deterministic_client_token"
            {
                let operation_id = operation.id.clone();
                state
                    .operation
                    .as_mut()
                    .expect("dispatching operation exists")
                    .status = "planned".to_string();
                let phase = state.phase.clone();
                bump(
                    &mut state,
                    phase,
                    json!({
                        "operation": "retry_planned",
                        "operationId": operation_id,
                        "reason": "deterministic_client_token",
                    }),
                );
                save_state(&mut state, Some(&observed_sha))?;
                let retry_sha = hash_file(&execution_dir.join(EXECUTION_FILE))?;
                drop(lock);
                return dispatch_planned(state, retry_sha, json_mode).await;
            }
            return Err(format!(
                "provider operation {} requires reconciliation; Harn will not retry it",
                operation.id
            ));
        }
        if operation.status == "planned" {
            drop(lock);
            return dispatch_planned(state, observed_sha, json_mode).await;
        }
    }
    if state.phase == "rejoined" || state.phase == "cancelled" {
        drop(lock);
        report(&state, json_mode);
        return Ok(());
    }
    plan_operation(&mut state, &observed_sha)?;
    let planned_sha = hash_file(&execution_dir.join(EXECUTION_FILE))?;
    drop(lock);
    maybe_kill("pre_call");
    dispatch_planned(state, planned_sha, json_mode).await
}

async fn dispatch_planned(
    mut state: ExecutionState,
    observed_sha: String,
    json_mode: bool,
) -> Result<(), String> {
    {
        let _lock = ExecutionLock::acquire(
            state
                .paths
                .manifest
                .parent()
                .ok_or("manifest path has no parent")?,
        )?;
        let operation = state
            .operation
            .as_mut()
            .ok_or("planned execution has no operation")?;
        operation.status = "dispatching".to_string();
        let dispatch_from = state.phase.clone();
        bump(
            &mut state,
            dispatch_from,
            json!({"operation": "dispatching"}),
        );
        save_state(&mut state, Some(&observed_sha))?;
    }
    let dispatch_sha = hash_file(
        &state
            .paths
            .manifest
            .parent()
            .ok_or("manifest path has no parent")?
            .join(EXECUTION_FILE),
    )?;
    let outcome = execute_operation(&state).await?;
    require_stage(
        state
            .operation
            .as_ref()
            .map_or("batch execution", |operation| operation.kind.as_str()),
        &outcome,
    )?;
    maybe_kill_accepted_middle(&state);
    {
        let execution_dir = state
            .paths
            .manifest
            .parent()
            .ok_or("manifest path has no parent")?;
        let _lock = ExecutionLock::acquire(execution_dir)?;
        let current_sha = hash_file(&execution_dir.join(EXECUTION_FILE))?;
        if current_sha != dispatch_sha {
            return Err(
                "stale batch execution writer rejected after provider operation".to_string(),
            );
        }
        finish_operation(&mut state, &current_sha)?;
    }
    maybe_kill("post_receipt");
    report(&state, json_mode);
    Ok(())
}

async fn cancel(execution_dir: &Path, json_mode: bool) -> Result<(), String> {
    let _guard = super::DISPATCH_BATCH_LOCK.lock().await;
    let lock = ExecutionLock::acquire(execution_dir)?;
    let (mut state, observed_sha) = load_state(execution_dir)?;
    validate_state(&state)?;
    if state.phase == "cancelled" || state.phase == "rejoined" {
        drop(lock);
        report(&state, json_mode);
        return Ok(());
    }
    if let Some(operation) = &state.operation {
        if operation.status == "dispatching" {
            if operation.kind != "cancel" {
                return Err(format!(
                    "provider operation {} requires reconciliation before cancellation",
                    operation.id
                ));
            }
            if operation.receipt_path.is_file() {
                finish_operation(&mut state, &observed_sha)?;
                drop(lock);
                report(&state, json_mode);
                return Ok(());
            }
            return Err(format!(
                "provider operation {} requires reconciliation; Harn will not retry it",
                operation.id
            ));
        }
        if operation.status == "planned" {
            if operation.kind != "cancel" {
                return Err(format!(
                    "planned {} operation must finish before cancellation",
                    operation.kind
                ));
            }
            drop(lock);
            return dispatch_planned(state, observed_sha, json_mode).await;
        }
    }
    if !state.paths.submission.is_file() {
        return Err("batch execution has not submitted provider jobs".to_string());
    }
    let receipt = execution_dir.join("cancel.json");
    state.operation = Some(ProviderOperation {
        id: operation_id(&state, "cancel"),
        kind: "cancel".to_string(),
        status: "planned".to_string(),
        idempotency_mode: "provider_status_reconciliation".to_string(),
        receipt_path: receipt,
        receipt_sha256: None,
    });
    let cancel_from = state.phase.clone();
    bump(&mut state, cancel_from, json!({"operation": "planned"}));
    save_state(&mut state, Some(&observed_sha))?;
    let planned_sha = hash_file(&execution_dir.join(EXECUTION_FILE))?;
    drop(lock);
    maybe_kill("pre_call");
    dispatch_planned(state, planned_sha, json_mode).await
}

fn plan_operation(state: &mut ExecutionState, observed_sha: &str) -> Result<(), String> {
    let (kind, receipt_path, idempotency_mode) = match state.phase.as_str() {
        "prepared" => (
            "submit",
            state.paths.submission.clone(),
            create_idempotency_mode(state),
        ),
        "submitted" | "running" => (
            "status",
            state.paths.status.clone(),
            "read_only".to_string(),
        ),
        "completed" => (
            "download",
            state.paths.download_receipt.clone(),
            "read_only".to_string(),
        ),
        "downloaded" => (
            "rejoin",
            state.paths.rejoin_receipt.clone(),
            "local_atomic".to_string(),
        ),
        phase => return Err(format!("batch execution cannot advance from phase {phase}")),
    };
    state.operation = Some(ProviderOperation {
        id: operation_id(state, kind),
        kind: kind.to_string(),
        status: "planned".to_string(),
        idempotency_mode,
        receipt_path,
        receipt_sha256: None,
    });
    bump(state, state.phase.clone(), json!({"operation": "planned"}));
    save_state(state, Some(observed_sha))
}

async fn execute_operation(state: &ExecutionState) -> Result<RunOutcome, String> {
    let operation = state
        .operation
        .as_ref()
        .ok_or("execution has no planned operation")?;
    let execution_dir = state
        .paths
        .manifest
        .parent()
        .ok_or("manifest path has no parent")?;
    let sandbox = sandbox_for(execution_dir, None);
    match operation.kind.as_str() {
        "submit" => Box::pin(run_stage(
            "submit",
            vec![
                (
                    "HARN_MODELS_BATCH_RECEIPT",
                    path_text(&state.paths.prepare_receipt),
                ),
                (
                    "HARN_MODELS_BATCH_SUBMIT_OUT",
                    path_text(&state.paths.submission),
                ),
                (
                    "HARN_MODELS_BATCH_DRY_RUN",
                    bool_env(state.dry_run).to_string(),
                ),
                ("HARN_MODELS_BATCH_OPERATION_ID", operation.id.clone()),
            ],
            sandbox,
        ))
        .await
        .pipe(Ok),
        "status" => {
            let outcome = Box::pin(run_stage(
                "status",
                vec![
                    (
                        "HARN_MODELS_BATCH_SUBMISSION",
                        path_text(&state.paths.submission),
                    ),
                    (
                        "HARN_MODELS_BATCH_STATUS_OUT",
                        path_text(&state.paths.status),
                    ),
                    (
                        "HARN_MODELS_BATCH_DRY_RUN",
                        bool_env(state.dry_run).to_string(),
                    ),
                ],
                sandbox,
            ))
            .await;
            if outcome.exit_code == 0 && state.dry_run {
                fixture_complete_status(&state.paths.status)?;
            }
            Ok(outcome)
        }
        "download" => {
            let outcome = Box::pin(run_stage(
                "download",
                vec![
                    ("HARN_MODELS_BATCH_STATUS", path_text(&state.paths.status)),
                    (
                        "HARN_MODELS_BATCH_RESULTS_OUT_DIR",
                        path_text(&state.paths.download_dir),
                    ),
                    (
                        "HARN_MODELS_BATCH_MAX_DOWNLOAD_BYTES",
                        "268435456".to_string(),
                    ),
                    (
                        "HARN_MODELS_BATCH_DRY_RUN",
                        bool_env(state.dry_run).to_string(),
                    ),
                ],
                sandbox,
            ))
            .await;
            if outcome.exit_code == 0 && state.dry_run {
                fixture_download_rows(state)?;
            }
            Ok(outcome)
        }
        "rejoin" => Box::pin(run_stage(
            "rejoin",
            vec![
                (
                    "HARN_MODELS_BATCH_MANIFEST",
                    path_text(&state.paths.manifest),
                ),
                (
                    "HARN_MODELS_BATCH_DOWNLOAD_RECEIPT",
                    path_text(&state.paths.download_receipt),
                ),
                (
                    "HARN_MODELS_BATCH_REJOIN_OUT_DIR",
                    path_text(&state.paths.rejoin_dir),
                ),
                (
                    "HARN_MODELS_BATCH_EXECUTION",
                    path_text(&execution_dir.join(EXECUTION_FILE)),
                ),
            ],
            sandbox,
        ))
        .await
        .pipe(Ok),
        "cancel" => Box::pin(run_stage(
            "cancel",
            vec![
                (
                    "HARN_MODELS_BATCH_CANCEL_RECEIPT",
                    path_text(if state.paths.status.is_file() {
                        &state.paths.status
                    } else {
                        &state.paths.submission
                    }),
                ),
                (
                    "HARN_MODELS_BATCH_CANCEL_OUT",
                    path_text(&operation.receipt_path),
                ),
                (
                    "HARN_MODELS_BATCH_DRY_RUN",
                    bool_env(state.dry_run).to_string(),
                ),
            ],
            sandbox,
        ))
        .await
        .pipe(Ok),
        kind => Err(format!("unsupported batch execution operation: {kind}")),
    }
}

fn finish_operation(state: &mut ExecutionState, observed_sha: &str) -> Result<(), String> {
    let operation = state
        .operation
        .as_ref()
        .ok_or("execution has no operation to finish")?
        .clone();
    let receipt = read_object(&operation.receipt_path, "operation receipt")?;
    let expected_kind = match operation.kind.as_str() {
        "submit" => "harn.model_batch_submission_receipt",
        "status" => "harn.model_batch_status_receipt",
        "download" => "harn.model_batch_results_receipt",
        "rejoin" => "harn.model_batch_rejoin_receipt",
        "cancel" => "harn.model_batch_cancel_receipt",
        kind => return Err(format!("unsupported operation kind: {kind}")),
    };
    validate_kind(&receipt, expected_kind, "operation receipt")?;
    if operation.kind != "rejoin" && operation.kind != "cancel" {
        let new_job_ids = receipt_job_ids(&receipt)?;
        if new_job_ids != state.job_ids {
            return Err("batch job identity changed across lifecycle transition".to_string());
        }
    }
    verify_operation_source(state, &operation.kind, &receipt)?;
    let receipt_identity = identity(
        &format!("{}_receipt", operation.kind),
        &operation.receipt_path,
    )?;
    state
        .artifacts
        .retain(|artifact| artifact.role != receipt_identity.role);
    state.artifacts.push(receipt_identity.clone());
    if operation.kind == "download" {
        for job in objects_at(&receipt, "/jobs")? {
            for artifact in objects_at(job, "/artifacts")? {
                let label = string_at(artifact, "/label")?;
                if !matches!(
                    label,
                    "output" | "results" | "responses" | "error" | "errors"
                ) {
                    continue;
                }
                let path = string_at(artifact, "/path")?;
                let expected = string_at(artifact, "/sha256")?;
                let actual = hash_file(Path::new(path))?;
                if actual != expected {
                    return Err(format!("download artifact hash mismatch: {path}"));
                }
                state.artifacts.push(ArtifactIdentity {
                    role: "raw_provider_result".to_string(),
                    path: PathBuf::from(path),
                    sha256: actual,
                });
            }
        }
    }
    if operation.kind == "rejoin" {
        let normalized = string_at(&receipt, "/normalized/path")?;
        state
            .artifacts
            .push(identity("normalized_results", Path::new(normalized))?);
        state.consumable = receipt
            .pointer("/consumable")
            .and_then(Value::as_bool)
            .unwrap_or(false);
    }
    let has_receipt_errors = receipt
        .pointer("/errors")
        .and_then(Value::as_array)
        .is_some_and(|errors| !errors.is_empty());
    let next_phase = match operation.kind.as_str() {
        "submit" if has_receipt_errors => "reconciliation_required",
        "submit" => "submitted",
        "status" => match string_at(&receipt, "/status")? {
            "completed" => "completed",
            "failed" | "cancelled" => "failed",
            _ => "running",
        },
        "download" if has_receipt_errors => "completed",
        "download" => "downloaded",
        "rejoin" => "rejoined",
        "cancel" if has_receipt_errors => "reconciliation_required",
        "cancel" => "cancelled",
        _ => unreachable!(),
    };
    let from = state.phase.clone();
    state.phase = next_phase.to_string();
    state.operation = Some(ProviderOperation {
        status: "committed".to_string(),
        receipt_sha256: Some(receipt_identity.sha256),
        ..operation
    });
    bump(state, from, json!({"operation": "committed"}));
    validate_state(state)?;
    save_state(state, Some(observed_sha))
}

fn validate_state(state: &ExecutionState) -> Result<(), String> {
    if state.kind != EXECUTION_KIND || state.schema_version != 1 {
        return Err("unsupported batch execution receipt".to_string());
    }
    if state.execution_id.is_empty() || state.request_ids.is_empty() || state.job_ids.is_empty() {
        return Err("batch execution identity is incomplete".to_string());
    }
    for artifact in &state.artifacts {
        let actual = hash_file(&artifact.path)?;
        if actual != artifact.sha256 {
            return Err(format!(
                "batch execution artifact changed ({}): {}",
                artifact.role,
                artifact.path.display()
            ));
        }
    }
    let manifest = read_object(&state.paths.manifest, "batch manifest")?;
    let expected_execution_id = format!("batch-{}", &hash_file(&state.paths.manifest)?[..24]);
    if state.execution_id != expected_execution_id {
        return Err("batch execution identity changed".to_string());
    }
    if manifest_request_ids(&manifest)? != state.request_ids {
        return Err("batch request identity changed".to_string());
    }
    let prepare = read_object(&state.paths.prepare_receipt, "prepare receipt")?;
    if receipt_job_ids(&prepare)? != state.job_ids {
        return Err("batch job identity changed".to_string());
    }
    verify_source(
        &prepare,
        "/manifest",
        &state.paths.manifest,
        "prepare manifest",
    )
}

fn verify_operation_source(
    state: &ExecutionState,
    kind: &str,
    receipt: &Value,
) -> Result<(), String> {
    match kind {
        "submit" => verify_source(
            receipt,
            "/source",
            &state.paths.prepare_receipt,
            "submission source",
        ),
        "status" => verify_source(receipt, "/source", &state.paths.submission, "status source"),
        "download" => verify_source(receipt, "/source", &state.paths.status, "download source"),
        "rejoin" => {
            let execution_id = string_at(receipt, "/execution/id")?;
            if execution_id != state.execution_id {
                return Err("rejoin receipt belongs to the wrong execution".to_string());
            }
            let execution_path = state
                .paths
                .manifest
                .parent()
                .ok_or("manifest path has no parent")?
                .join(EXECUTION_FILE);
            if string_at(receipt, "/execution/sha256")? != hash_file(&execution_path)? {
                return Err("rejoin receipt execution hash mismatch".to_string());
            }
            verify_source(
                receipt,
                "/source/manifest",
                &state.paths.manifest,
                "rejoin manifest source",
            )?;
            verify_source(
                receipt,
                "/source/download",
                &state.paths.download_receipt,
                "rejoin download source",
            )
        }
        "cancel" => Ok(()),
        _ => Err(format!("unsupported operation source: {kind}")),
    }
}

fn verify_source(receipt: &Value, pointer: &str, path: &Path, label: &str) -> Result<(), String> {
    let source = receipt
        .pointer(pointer)
        .ok_or_else(|| format!("{label} identity is missing"))?;
    let recorded_path = string_at(source, "/path")?;
    if absolute(Path::new(recorded_path)) != absolute(path) {
        return Err(format!("{label} path mismatch"));
    }
    let recorded_sha = string_at(source, "/sha256")?;
    if recorded_sha != hash_file(path)? {
        return Err(format!("{label} hash mismatch"));
    }
    Ok(())
}

fn save_state(state: &mut ExecutionState, expected_sha: Option<&str>) -> Result<(), String> {
    let state_path = state
        .paths
        .manifest
        .parent()
        .ok_or("manifest path has no parent")?
        .join(EXECUTION_FILE);
    if let Some(expected) = expected_sha {
        let actual = hash_file(&state_path)?;
        if actual != expected {
            return Err("stale batch execution writer rejected".to_string());
        }
    }
    let text = serde_json::to_string_pretty(state)
        .map_err(|error| format!("failed to serialize batch execution: {error}"))?
        + "\n";
    let temp = state_path.with_extension(format!("json.{}.tmp", std::process::id()));
    {
        let mut file = fs::File::create(&temp)
            .map_err(|error| format!("failed to create execution temp file: {error}"))?;
        file.write_all(text.as_bytes())
            .map_err(|error| format!("failed to write execution temp file: {error}"))?;
        file.sync_all()
            .map_err(|error| format!("failed to flush execution temp file: {error}"))?;
    }
    fs::rename(&temp, &state_path)
        .map_err(|error| format!("failed to replace execution receipt: {error}"))?;
    fs::File::open(
        state_path
            .parent()
            .ok_or("execution receipt has no parent directory")?,
    )
    .and_then(|directory| directory.sync_all())
    .map_err(|error| format!("failed to flush execution directory: {error}"))
}

fn load_state(execution_dir: &Path) -> Result<(ExecutionState, String), String> {
    let path = execution_dir.join(EXECUTION_FILE);
    let bytes =
        fs::read(&path).map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    let sha = sha256(&bytes);
    let state = serde_json::from_slice(&bytes)
        .map_err(|error| format!("invalid batch execution receipt: {error}"))?;
    Ok((state, sha))
}

async fn run_stage(
    mode: &'static str,
    vars: Vec<(&'static str, String)>,
    sandbox: RunSandboxOptions,
) -> RunOutcome {
    let _mode = ScopedEnvVar::set(super::BATCH_MODE_ENV, mode);
    let _vars: Vec<_> = vars
        .iter()
        .map(|(key, value)| ScopedEnvVar::set(key, value))
        .collect();
    super::capture_batch_script(true, Some(sandbox)).await
}

fn sandbox_for(execution_dir: &Path, read_path: Option<&Path>) -> RunSandboxOptions {
    let mut sandbox = RunSandboxOptions::default().with_workspace_root(execution_dir.to_path_buf());
    if let Some(path) = read_path {
        let root = path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();
        if root != execution_dir {
            sandbox = sandbox.with_read_only_roots(vec![root]);
        }
    }
    sandbox
}

fn require_stage(label: &str, outcome: &RunOutcome) -> Result<(), String> {
    if outcome.exit_code == 0 {
        return Ok(());
    }
    let detail = if outcome.stderr.trim().is_empty() {
        outcome.stdout.trim()
    } else {
        outcome.stderr.trim()
    };
    Err(format!("{label} stage failed: {detail}"))
}

fn fixture_complete_status(path: &Path) -> Result<(), String> {
    let mut receipt = read_object(path, "fixture status receipt")?;
    let jobs = receipt
        .pointer_mut("/jobs")
        .and_then(Value::as_array_mut)
        .ok_or("fixture status receipt has no jobs")?;
    for job in jobs.iter_mut() {
        let object = job
            .as_object_mut()
            .ok_or("fixture status job is not an object")?;
        let id = object
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("batch")
            .replace(|ch: char| !ch.is_ascii_alphanumeric(), "-");
        object.insert("status".to_string(), json!("completed"));
        object.insert("provider_status".to_string(), json!("completed"));
        object.insert(
            "provider_batch_id".to_string(),
            json!(format!("fixture-{id}-batch")),
        );
        object.insert(
            "output_file_id".to_string(),
            json!(format!("fixture-{id}-output")),
        );
        object.insert(
            "output_file".to_string(),
            json!(format!("fixture-{id}-output")),
        );
        object.insert(
            "results_url".to_string(),
            json!(format!("https://example.invalid/{id}/results.jsonl")),
        );
        object.insert(
            "responses_file".to_string(),
            json!(format!("fixture-{id}-responses")),
        );
        object.insert(
            "output_dataset_id".to_string(),
            json!(format!("fixture-{id}-dataset")),
        );
    }
    let count = jobs.len();
    let object = receipt
        .as_object_mut()
        .ok_or("fixture status receipt is not an object")?;
    object.insert("status".to_string(), json!("completed"));
    object.insert("completedCount".to_string(), json!(count));
    object.insert("readyCount".to_string(), json!(0));
    object.insert("runningCount".to_string(), json!(0));
    object.insert("failedCount".to_string(), json!(0));
    write_json_atomic(path, &receipt)
}

fn fixture_download_rows(state: &ExecutionState) -> Result<(), String> {
    let manifest = read_object(&state.paths.manifest, "fixture manifest")?;
    let mut ids_by_job = std::collections::BTreeMap::<String, Vec<String>>::new();
    for group in objects_at(&manifest, "/groups")? {
        let id = string_at(group, "/id")?.to_string();
        let ids = objects_at(group, "/requests")?
            .iter()
            .map(|request| string_at(request, "/custom_id").map(str::to_string))
            .collect::<Result<Vec<_>, _>>()?;
        ids_by_job.insert(id, ids);
    }
    let mut receipt = read_object(&state.paths.download_receipt, "fixture download receipt")?;
    for job in receipt
        .pointer_mut("/jobs")
        .and_then(Value::as_array_mut)
        .ok_or("fixture download receipt has no jobs")?
    {
        let job_object = job
            .as_object_mut()
            .ok_or("fixture download job is not an object")?;
        job_object.insert("status".to_string(), json!("downloaded"));
        let provider = string_at(job, "/provider")?.to_string();
        let job_id = string_at(job, "/id")?.to_string();
        let ids = ids_by_job
            .get(&job_id)
            .ok_or_else(|| format!("fixture result job missing from manifest: {job_id}"))?;
        let artifacts = job
            .pointer_mut("/artifacts")
            .and_then(Value::as_array_mut)
            .ok_or("fixture download job has no artifacts")?;
        if !artifacts.iter().any(|artifact| {
            artifact
                .pointer("/label")
                .and_then(Value::as_str)
                .is_some_and(|label| matches!(label, "output" | "results" | "responses"))
        }) {
            artifacts.push(json!({
                "label": "results",
                "handle": format!("fixture-{job_id}-results"),
                "path": state.paths.download_dir.join(format!("{job_id}.results.jsonl")),
                "dry_run": true,
            }));
        }
        for artifact in artifacts {
            let label = string_at(artifact, "/label")?;
            if !matches!(label, "output" | "results" | "responses") {
                continue;
            }
            let path = PathBuf::from(string_at(artifact, "/path")?);
            let rows = ids
                .iter()
                .map(|id| fixture_wire_row(&provider, id))
                .map(|row| serde_json::to_string(&row).expect("fixture row serializes"))
                .collect::<Vec<_>>();
            let text = rows.join("\n") + "\n";
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).map_err(|error| {
                    format!("failed to create fixture result directory: {error}")
                })?;
            }
            fs::write(&path, &text)
                .map_err(|error| format!("failed to write fixture result rows: {error}"))?;
            let object = artifact
                .as_object_mut()
                .ok_or("fixture artifact is not an object")?;
            object.insert("sha256".to_string(), json!(sha256(text.as_bytes())));
            object.insert("bytes".to_string(), json!(text.len()));
            object.insert("dry_run".to_string(), json!(false));
        }
    }
    write_json_atomic(&state.paths.download_receipt, &receipt)
}

fn fixture_wire_row(provider: &str, id: &str) -> Value {
    match provider {
        "anthropic" => json!({
            "custom_id": id,
            "result": {"type": "succeeded", "message": {"content": [{"type": "text", "text": "fixture"}]}}
        }),
        "gemini" | "vertex" => {
            json!({"key": id, "response": {"candidates": [{"content": {"parts": [{"text": "fixture"}]}}]}})
        }
        "bedrock" => json!({"recordId": id, "modelOutput": {"outputText": "fixture"}}),
        _ => {
            json!({"custom_id": id, "response": {"status_code": 200, "body": {"choices": [{"message": {"role": "assistant", "content": "fixture"}}]}}, "error": null})
        }
    }
}

fn write_json_atomic(path: &Path, value: &Value) -> Result<(), String> {
    let text = serde_json::to_string_pretty(value)
        .map_err(|error| format!("failed to serialize fixture receipt: {error}"))?
        + "\n";
    let temp = path.with_extension(format!("json.{}.tmp", std::process::id()));
    fs::write(&temp, text)
        .map_err(|error| format!("failed to write fixture receipt temp file: {error}"))?;
    fs::rename(&temp, path).map_err(|error| format!("failed to replace fixture receipt: {error}"))
}

fn execution_paths(execution_dir: &Path, requests: &Path) -> ExecutionPaths {
    ExecutionPaths {
        requests: requests.to_path_buf(),
        manifest: execution_dir.join("manifest.json"),
        prepare_receipt: execution_dir.join("prepared/receipt.json"),
        submission: execution_dir.join("submission.json"),
        status: execution_dir.join("status.json"),
        download_dir: execution_dir.join("results"),
        download_receipt: execution_dir.join("results/receipt.json"),
        rejoin_dir: execution_dir.join("rejoin"),
        rejoin_receipt: execution_dir.join("rejoin/receipt.json"),
    }
}

fn manifest_request_ids(manifest: &Value) -> Result<Vec<String>, String> {
    let mut ids = Vec::new();
    for group in objects_at(manifest, "/groups")? {
        for request in objects_at(group, "/requests")? {
            ids.push(string_at(request, "/custom_id")?.to_string());
        }
    }
    if ids.iter().any(String::is_empty) {
        return Err("batch manifest contains an empty custom_id".to_string());
    }
    let mut sorted = ids.clone();
    sorted.sort();
    sorted.dedup();
    if sorted.len() != ids.len() {
        return Err("batch manifest contains duplicate custom_id values".to_string());
    }
    Ok(ids)
}

fn receipt_job_ids(receipt: &Value) -> Result<Vec<String>, String> {
    objects_at(receipt, "/jobs")?
        .iter()
        .map(|job| string_at(job, "/id").map(str::to_string))
        .collect()
}

fn objects_at<'a>(value: &'a Value, pointer: &str) -> Result<Vec<&'a Value>, String> {
    value
        .pointer(pointer)
        .and_then(Value::as_array)
        .ok_or_else(|| format!("missing object collection at {pointer}"))?
        .iter()
        .map(|item| {
            if item.is_object() {
                Ok(item)
            } else {
                Err(format!("non-object entry at {pointer}"))
            }
        })
        .collect()
}

fn string_at<'a>(value: &'a Value, pointer: &str) -> Result<&'a str, String> {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .ok_or_else(|| format!("missing string at {pointer}"))
}

fn read_object(path: &Path, label: &str) -> Result<Value, String> {
    let bytes = fs::read(path)
        .map_err(|error| format!("failed to read {label} {}: {error}", path.display()))?;
    let value: Value =
        serde_json::from_slice(&bytes).map_err(|error| format!("invalid {label} JSON: {error}"))?;
    if !value.is_object() {
        return Err(format!("{label} must be a JSON object"));
    }
    Ok(value)
}

fn validate_kind(value: &Value, expected: &str, label: &str) -> Result<(), String> {
    if value.pointer("/kind").and_then(Value::as_str) == Some(expected) {
        Ok(())
    } else {
        Err(format!("{label} has unsupported kind"))
    }
}

fn identity(role: &str, path: &Path) -> Result<ArtifactIdentity, String> {
    Ok(ArtifactIdentity {
        role: role.to_string(),
        path: absolute(path),
        sha256: hash_file(path)?,
    })
}

fn hash_file(path: &Path) -> Result<String, String> {
    let bytes =
        fs::read(path).map_err(|error| format!("failed to hash {}: {error}", path.display()))?;
    Ok(sha256(&bytes))
}

fn sha256(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn absolute(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    }
}

fn path_text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn bool_env(value: bool) -> &'static str {
    if value {
        "1"
    } else {
        "0"
    }
}

fn operation_id(state: &ExecutionState, kind: &str) -> String {
    let material = format!("{}:{}:{}", state.execution_id, state.revision + 1, kind);
    format!("op-{}", &sha256(material.as_bytes())[..24])
}

fn create_idempotency_mode(state: &ExecutionState) -> String {
    let modes = read_object(&state.paths.prepare_receipt, "prepare receipt")
        .ok()
        .and_then(|receipt| {
            objects_at(&receipt, "/jobs").ok().map(|jobs| {
                jobs.iter()
                    .filter_map(|job| {
                        job.pointer("/create_recovery/mode")
                            .and_then(Value::as_str)
                            .map(str::to_string)
                    })
                    .collect::<Vec<_>>()
            })
        })
        .unwrap_or_default();
    if !modes.is_empty() && modes.iter().all(|mode| mode == "deterministic_token") {
        "deterministic_client_token".to_string()
    } else {
        "reconciliation_required".to_string()
    }
}

fn bump(state: &mut ExecutionState, from: String, detail: Value) {
    state.revision += 1;
    state.history.push(json!({
        "revision": state.revision,
        "from": from,
        "to": state.phase,
        "detail": detail,
    }));
}

fn maybe_kill(point: &str) {
    if std::env::var(KILL_POINT_ENV).as_deref() == Ok(point) {
        std::process::exit(86);
    }
}

fn maybe_kill_accepted_middle(state: &ExecutionState) {
    if std::env::var(KILL_POINT_ENV).as_deref() != Ok("accepted_middle") {
        return;
    }
    if let Some(operation) = &state.operation {
        let _ = fs::remove_file(&operation.receipt_path);
    }
    std::process::exit(86);
}

fn report(state: &ExecutionState, json_mode: bool) {
    if json_mode {
        let data = serde_json::to_value(state).expect("execution state serializes");
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "schema_version": 1,
                "ok": true,
                "data": data,
            }))
            .expect("execution report serializes")
        );
    } else {
        println!("Batch execution {}", state.execution_id);
        println!("  phase: {}", state.phase);
        println!("  revision: {}", state.revision);
        println!(
            "  directory: {}",
            state
                .paths
                .manifest
                .parent()
                .unwrap_or(Path::new("."))
                .display()
        );
        if state
            .operation
            .as_ref()
            .is_some_and(|operation| operation.status == "dispatching")
        {
            println!("  reconciliation required: true");
        }
    }
}

fn emit_error(error: &str, json_mode: bool) {
    if json_mode {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "schema_version": 1,
                "ok": false,
                "error": {
                    "code": "batch_execution_failed",
                    "message": error,
                },
            }))
            .expect("execution error serializes")
        );
    } else {
        eprintln!("Batch execution failed: {error}");
    }
}

trait Pipe: Sized {
    fn pipe<T>(self, f: impl FnOnce(Self) -> T) -> T {
        f(self)
    }
}

impl<T> Pipe for T {}
