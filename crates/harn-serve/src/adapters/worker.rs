//! Worker / job execution surface for `.harn` programs (#3171).
//!
//! `harn-serve` is HTTP-first: every other adapter answers a request over
//! a transport. Long-running, scheduled, and operator-batch programs need
//! a different shape — read a JSON request, do work, emit a JSON report —
//! which is exactly what `harn-cloud`'s factory workers (scan / repair /
//! launch) do today as bespoke Rust binaries (#500 / E.10).
//!
//! Crucially this is **not** a second execution engine. A `@job` function
//! is lowered into a `harn_vm` [`TriggerBindingSpec`] whose handler is the
//! function's own closure, registered in the trigger registry, and
//! dispatched through the trigger [`Dispatcher`] — the same machinery that
//! already powers webhook / cron / queue triggers. Retry,
//! dead-letter-queue, per-dispatch budget, cancellation, and the
//! action-graph audit trail therefore come *for free* from the
//! dispatcher; the dispatcher needs zero changes to host jobs.
//!
//! ```text
//!   request.json ──▶ TriggerEvent (webhook payload `raw` = request)
//!                         │
//!   @job fn closure ──▶ TriggerBindingSpec{ handler: Local{closure} }
//!                         │  dynamic_register + resolve_live_trigger_binding
//!                         ▼
//!                   Dispatcher::dispatch(&binding, event)
//!                         │  retry / DLQ / budget / cancel (unchanged)
//!                         ▼
//!                   DispatchOutcome.result ──▶ report.json
//! ```
//!
//! ## Phase 2 (TODO, #3171): `harn serve worker <file.harn>` daemon
//!
//! Phase 1 (this module) ships the one-shot driver. The follow-up daemon
//! mode is not yet wired:
//!
//! - add a `Worker` variant to `ServeCommand` in `harn-cli`;
//! - for each `@schedule(...)` job, activate `harn_vm`'s `CronConnector`
//!   so a cron tick enqueues a job event (the [`JobSpec::schedule`] field
//!   is already parsed and carried for exactly this);
//! - for each `@queue(...)` job, run the [`harn_vm::WorkerQueue`] consumer
//!   loop (claim / ack / lease);
//! - drive `Dispatcher::run()` with graceful shutdown
//!   (`Dispatcher::shutdown` + `drain`).
//!
//! The dispatcher already owns the run loop, cron connector, and worker
//! queue, so the daemon is wiring — no new execution engine — just like
//! the one-shot path here.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use harn_vm::event_log::install_default_for_base_dir;
use harn_vm::triggers::event::{GenericWebhookPayload, KnownProviderPayload};
use harn_vm::{
    dynamic_register, resolve_live_trigger_binding, DispatchOutcome, DispatchStatus, Dispatcher,
    ProviderId, ProviderPayload, RetryPolicy, SignatureStatus, TriggerBindingSource,
    TriggerBindingSpec, TriggerEvent, TriggerHandlerSpec, TriggerRetryConfig, Vm,
    WorkerQueuePriority,
};

use crate::limits::BudgetSpec;
use crate::{DispatchError, ExportCatalog, ExportedFunction, JobSpec, RetryBackoff, RetrySpec};

/// Provider id stamped on synthetic job events. Reuses the generic
/// `webhook` payload variant so the request JSON rides in
/// `provider_payload.raw`, the idiomatic place `.harn` handlers read a
/// request body from (matching every other trigger handler).
const JOB_PROVIDER: &str = "webhook";

/// Outcome of running one job dispatch. Thin wrapper over the trigger
/// [`DispatchOutcome`] so the CLI / factory worker can render the report
/// and pick an exit code without depending on `harn_vm` internals.
#[derive(Clone, Debug)]
pub struct JobRunOutcome {
    /// Job name (the `@job("name")` argument or the function name).
    pub job: String,
    /// Terminal dispatch status — `succeeded`, `dlq`, `failed`, …
    pub status: DispatchStatus,
    /// Number of attempts the dispatcher made (≥ 1 on success, up to the
    /// retry ceiling before a DLQ).
    pub attempt_count: u32,
    /// The value the `@job` function returned, JSON-encoded. `None` when
    /// the job failed before producing a result.
    pub result: Option<serde_json::Value>,
    /// Terminal error message when the job did not succeed.
    pub error: Option<String>,
}

impl JobRunOutcome {
    /// `true` when the dispatcher reported a successful terminal outcome.
    pub fn succeeded(&self) -> bool {
        matches!(self.status, DispatchStatus::Succeeded)
    }

    /// The report JSON to emit. Successful jobs render their returned
    /// value; failed jobs render a `{status, error}` envelope so the
    /// consumer always gets a JSON object on stdout.
    pub fn report_json(&self) -> serde_json::Value {
        match (&self.result, self.succeeded()) {
            (Some(value), true) => value.clone(),
            _ => serde_json::json!({
                "status": self.status.as_str(),
                "error": self.error.clone().unwrap_or_default(),
                "attempt_count": self.attempt_count,
            }),
        }
    }
}

/// Run one `@job` function against a single JSON request and return its
/// outcome. This is the one-shot driver behind `harn run --as-job`.
///
/// Mirrors [`crate::core::DispatchCore::invoke_function`] for the base-VM
/// build (stdlib + store/metadata builtins + real harness), then hands
/// the rest of the lifecycle to the trigger dispatcher.
pub async fn run_job_once(
    script_path: &Path,
    job_name: &str,
    request: serde_json::Value,
) -> Result<JobRunOutcome, DispatchError> {
    run_job_once_with(script_path, job_name, request, |_vm| {}).await
}

/// Like [`run_job_once`], but lets the embedder inject extra VM state via a
/// `configure` closure that runs on the fully-built job VM.
///
/// The closure receives `&mut Vm` *after* the standard registration
/// (`register_vm_stdlib` + `register_store_builtins` +
/// `register_metadata_builtins` + source-dir/harness wiring) and *before*
/// the job module is loaded and the entrypoint executes. This lets an
/// embedder register host-defined builtins (e.g. a `sandbox_exec` that
/// bridges to a cloud-sandbox adapter) that coexist with the standard
/// ones, so the `@job` closure can call them.
///
/// Ordering guarantees:
/// - Standard stdlib + store/metadata builtins are registered first, so
///   embedder builtins may *extend* the surface the job sees.
/// - Embedder builtins are registered last, so a name collision *overrides*
///   the standard builtin (`register_builtin` replaces by name).
/// - The closure runs before `load_module_exports`, so the job module's
///   captured globals resolve against the embedder-augmented VM.
pub async fn run_job_once_with(
    script_path: &Path,
    job_name: &str,
    request: serde_json::Value,
    configure: impl FnOnce(&mut Vm),
) -> Result<JobRunOutcome, DispatchError> {
    // A one-shot process owns its trigger registry / dispatcher state, so
    // start from a clean slate. (No-op the first time; defends against a
    // second call in the same process — e.g. tests.)
    harn_vm::reset_thread_local_state();
    harn_vm::clear_trigger_registry();
    harn_vm::clear_dispatcher_state();

    // Canonicalize up front: `vm.load_module_exports` resolves the path
    // relative to the source dir, so a *relative* script path combined
    // with a `set_source_dir` of its parent would double-prefix. An
    // absolute path sidesteps that (matching how the HTTP dispatch core
    // builds an absolute `DispatchCoreConfig::script_path`).
    let script_path = std::fs::canonicalize(script_path).map_err(|error| {
        DispatchError::Io(format!(
            "failed to resolve job script {}: {error}",
            script_path.display()
        ))
    })?;
    let script_path = script_path.as_path();

    let catalog = ExportCatalog::from_path(script_path)?;
    crate::emit_export_diagnostics(catalog.diagnostics());

    let function = catalog.function(job_name).ok_or_else(|| {
        DispatchError::MissingExport(format!(
            "no `pub fn {job_name}` exported by {}",
            script_path.display()
        ))
    })?;
    let job = function.job.clone().ok_or_else(|| {
        DispatchError::Validation(format!(
            "`{job_name}` in {} is not a `@job`; add a `@job(\"{job_name}\")` attribute",
            script_path.display()
        ))
    })?;

    let base_dir = script_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();
    let event_log = install_default_for_base_dir(&base_dir).map_err(|error| {
        DispatchError::Io(format!(
            "failed to initialize event log for {}: {error}",
            base_dir.display()
        ))
    })?;

    // Per-dispatch resource budget caps declared via `@budget(...)`. The
    // dispatcher runs the closure on this thread, so installing the guard
    // here (held across the dispatch) is enough — the same pattern the
    // HTTP dispatch core uses.
    let _budget_guard = function.budget.as_ref().and_then(BudgetSpec::install);

    // Build the base VM the dispatcher clones a child from for each
    // attempt. It must be the VM that loaded the module so the `@job`
    // closure's captured globals resolve in the child.
    let mut vm = Vm::new();
    harn_vm::register_vm_stdlib(&mut vm);
    harn_vm::register_store_builtins(&mut vm, &base_dir);
    harn_vm::register_metadata_builtins(&mut vm, &base_dir);
    vm.set_source_dir(&base_dir);
    vm.set_harness(harn_vm::Harness::real());

    // Let the embedder register host-defined builtins on the fully-built VM
    // before the job module loads. See `run_job_once_with` docs for ordering.
    configure(&mut vm);

    let exports = vm
        .load_module_exports(script_path)
        .await
        .map_err(|error| DispatchError::Execution(error.to_string()))?;
    let closure = exports.get(job_name).cloned().ok_or_else(|| {
        DispatchError::MissingExport(format!(
            "function '{job_name}' is not exported by {}",
            script_path.display()
        ))
    })?;

    let spec = job_binding_spec(&job, function, closure);
    let id = dynamic_register(spec)
        .await
        .map_err(|error| DispatchError::Execution(error.to_string()))?;
    let binding = resolve_live_trigger_binding(id.as_str(), None)
        .map_err(|error| DispatchError::Execution(error.to_string()))?;

    let event = job_event(&job.name, request)?;
    let dispatcher = Dispatcher::with_event_log(vm, event_log);
    let outcome = dispatcher
        .dispatch(&binding, event)
        .await
        .map_err(|error| DispatchError::Execution(error.to_string()))?;

    Ok(job_run_outcome(&job.name, outcome))
}

/// Lower a parsed [`JobSpec`] (+ its `@budget`/`@scopes`) into the trigger
/// binding the dispatcher consumes. The handler is the function's own
/// closure, so dispatch executes the user's `.harn` code directly.
fn job_binding_spec(
    job: &JobSpec,
    function: &ExportedFunction,
    closure: Arc<harn_vm::VmClosure>,
) -> TriggerBindingSpec {
    let retry = job.retry.as_ref().map(retry_config).unwrap_or_default();

    // A scheduled job is cron-provided; a queue-only job still dispatches
    // locally in one-shot mode (the daemon, Phase 2, owns the actual
    // queue consumer). The provider stays `webhook` so the request rides
    // in `provider_payload.raw` regardless.
    let kind = if job.schedule.is_some() {
        "cron".to_string()
    } else {
        "job".to_string()
    };

    TriggerBindingSpec {
        id: format!("job:{}", job.name),
        source: TriggerBindingSource::Dynamic,
        kind,
        provider: ProviderId::from(JOB_PROVIDER),
        autonomy_tier: harn_vm::AutonomyTier::ActAuto,
        handler: TriggerHandlerSpec::Local {
            raw: function.name.clone(),
            closure,
        },
        dispatch_priority: WorkerQueuePriority::Normal,
        when: None,
        when_budget: None,
        retry,
        match_events: Vec::new(),
        dedupe_key: None,
        dedupe_retention_days: harn_vm::DEFAULT_INBOX_RETENTION_DAYS,
        filter: None,
        daily_cost_usd: None,
        hourly_cost_usd: None,
        max_autonomous_decisions_per_hour: None,
        max_autonomous_decisions_per_day: None,
        on_budget_exhausted: harn_vm::TriggerBudgetExhaustionStrategy::False,
        max_concurrent: None,
        flow_control: harn_vm::TriggerFlowControlConfig::default(),
        aggregation: None,
        manifest_path: None,
        package_name: None,
        definition_fingerprint: format!("job:{}:v1", job.name),
    }
}

/// Map a parsed [`RetrySpec`] onto the dispatcher's retry config. Linear /
/// exponential pick conservative defaults the author can later tune via
/// the full trigger DSL; the keyword is what `@retry(backoff:)` exposes.
fn retry_config(spec: &RetrySpec) -> TriggerRetryConfig {
    let policy = match spec.backoff {
        RetryBackoff::Svix => RetryPolicy::Svix,
        RetryBackoff::Linear => RetryPolicy::Linear { delay_ms: 1_000 },
        RetryBackoff::Exponential => RetryPolicy::Exponential {
            base_ms: 1_000,
            cap_ms: 60_000,
        },
    };
    // `max_attempts == 0` means "defer to the dispatcher default", which
    // `TriggerRetryConfig::max_attempts()` already honours.
    TriggerRetryConfig::new(spec.max_attempts, policy)
}

/// Wrap a request JSON object as a synthetic [`TriggerEvent`]. The request
/// rides in the generic-webhook payload's `raw` field, so the `@job`
/// handler reads it as `event.provider_payload.raw` — the same place
/// every other webhook-shaped trigger handler reads its body.
fn job_event(job_name: &str, request: serde_json::Value) -> Result<TriggerEvent, DispatchError> {
    Ok(TriggerEvent::new(
        ProviderId::from(JOB_PROVIDER),
        "job",
        None,
        format!("job:{job_name}:{}", uuid_like()),
        None,
        std::collections::BTreeMap::new(),
        ProviderPayload::Known(KnownProviderPayload::Webhook(GenericWebhookPayload {
            source: Some(format!("job:{job_name}")),
            content_type: Some("application/json".to_string()),
            raw: request,
        })),
        SignatureStatus::Verified,
    ))
}

/// A cheap unique-ish suffix for the event dedupe key. We avoid pulling a
/// uuid dep into harn-serve for the one-shot path; nanos is unique enough
/// for a single-process driver where each invocation runs one job.
fn uuid_like() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

fn job_run_outcome(job_name: &str, outcome: DispatchOutcome) -> JobRunOutcome {
    JobRunOutcome {
        job: job_name.to_string(),
        status: outcome.status,
        attempt_count: outcome.attempt_count,
        result: outcome.result,
        error: outcome.error,
    }
}

/// Read a JSON request from `request_path`, run the named `@job` in
/// `script_path`, and (optionally) write the report JSON to
/// `result_out`. Always returns the rendered report string for the CLI to
/// print, plus the outcome for exit-code selection.
///
/// This is the drop-in replacement for the factory worker binaries'
/// `--request file.json → do work → println!(report_json)` contract.
pub async fn run_job_from_files(
    script_path: &Path,
    job_name: &str,
    request_path: &Path,
    result_out: Option<&Path>,
    pretty: bool,
) -> Result<(JobRunOutcome, String), DispatchError> {
    let raw = std::fs::read_to_string(request_path).map_err(|error| {
        DispatchError::Io(format!(
            "failed to read request {}: {error}",
            request_path.display()
        ))
    })?;
    let request: serde_json::Value = serde_json::from_str(&raw).map_err(|error| {
        DispatchError::Validation(format!(
            "request {} is not valid JSON: {error}",
            request_path.display()
        ))
    })?;

    let outcome = run_job_once(script_path, job_name, request).await?;
    let report = outcome.report_json();
    let rendered = if pretty {
        serde_json::to_string_pretty(&report)
    } else {
        serde_json::to_string(&report)
    }
    .map_err(|error| DispatchError::Execution(format!("failed to render report JSON: {error}")))?;

    if let Some(out) = result_out {
        std::fs::write(out, &rendered).map_err(|error| {
            DispatchError::Io(format!("failed to write report {}: {error}", out.display()))
        })?;
    }

    Ok((outcome, rendered))
}

/// Convenience for callers that only have a script path string.
pub fn script_path_buf(path: &str) -> PathBuf {
    PathBuf::from(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn write_script(dir: &Path, body: &str) -> PathBuf {
        let path = dir.join("worker.harn");
        tokio::fs::write(&path, body).await.expect("write script");
        path
    }

    #[tokio::test(flavor = "current_thread")]
    async fn one_shot_job_echoes_request_and_succeeds() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let dir = tempfile::tempdir().expect("tempdir");
                let script = write_script(
                    dir.path(),
                    r#"
import "std/triggers"

@job("scan")
pub fn scan(event: TriggerEvent) -> dict {
  let req = event.provider_payload.raw
  return {status: "ok", echo: req}
}
"#,
                )
                .await;

                let request = serde_json::json!({"repo": "burin-labs/harn", "n": 7});
                let outcome = run_job_once(&script, "scan", request.clone())
                    .await
                    .expect("run job");

                assert_eq!(outcome.status, DispatchStatus::Succeeded);
                assert_eq!(outcome.attempt_count, 1);
                let result = outcome.result.expect("result");
                assert_eq!(result["status"], serde_json::json!("ok"));
                assert_eq!(result["echo"], request);
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn configure_hook_registers_callable_host_builtin() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let dir = tempfile::tempdir().expect("tempdir");
                let script = write_script(
                    dir.path(),
                    r#"
import "std/triggers"

@job("scan")
pub fn scan(event: TriggerEvent) -> dict {
  let req = event.provider_payload.raw
  return {status: "ok", host: host_echo(req.repo)}
}
"#,
                )
                .await;

                let request = serde_json::json!({"repo": "burin-labs/harn"});
                let outcome = run_job_once_with(&script, "scan", request, |vm| {
                    // An embedder-defined builtin, injected via the configure
                    // hook on the fully-built job VM. The `@job` closure calls
                    // it by bare name, exactly like the stdlib builtins.
                    vm.register_builtin("host_echo", |args, _out| {
                        let x = args.first().map(|a| a.display()).unwrap_or_default();
                        Ok(harn_vm::VmValue::String(std::sync::Arc::from(
                            format!("host:{x}").as_str(),
                        )))
                    });
                })
                .await
                .expect("run job");

                assert_eq!(outcome.status, DispatchStatus::Succeeded);
                let result = outcome.result.expect("result");
                assert_eq!(result["status"], serde_json::json!("ok"));
                assert_eq!(result["host"], serde_json::json!("host:burin-labs/harn"));
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn handler_error_retries_then_dlqs() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let dir = tempfile::tempdir().expect("tempdir");
                let script = write_script(
                    dir.path(),
                    r#"
import "std/triggers"

@job("scan", retry: { max: 2, backoff: "linear" })
pub fn scan(event: TriggerEvent) -> dict {
  throw "boom"
}
"#,
                )
                .await;

                let outcome = run_job_once(&script, "scan", serde_json::json!({}))
                    .await
                    .expect("run job returns terminal outcome");

                assert_eq!(outcome.status, DispatchStatus::Dlq);
                assert_eq!(outcome.attempt_count, 2);
                assert!(!outcome.succeeded());
                // The rendered report is a JSON object even on failure.
                let report = outcome.report_json();
                assert_eq!(report["status"], serde_json::json!("dlq"));
                assert!(report["error"].as_str().is_some_and(|e| e.contains("boom")));
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn run_from_files_writes_result_out() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let dir = tempfile::tempdir().expect("tempdir");
                let script = write_script(
                    dir.path(),
                    r#"
import "std/triggers"

@job("scan")
pub fn scan(event: TriggerEvent) -> dict {
  return {status: "ok", echo: event.provider_payload.raw}
}
"#,
                )
                .await;
                let request_path = dir.path().join("req.json");
                tokio::fs::write(&request_path, r#"{"k": "v"}"#)
                    .await
                    .expect("write request");
                let out_path = dir.path().join("out.json");

                let (outcome, rendered) =
                    run_job_from_files(&script, "scan", &request_path, Some(&out_path), false)
                        .await
                        .expect("run job from files");

                assert!(outcome.succeeded());
                let written = tokio::fs::read_to_string(&out_path)
                    .await
                    .expect("read out");
                assert_eq!(written, rendered);
                let parsed: serde_json::Value =
                    serde_json::from_str(&written).expect("parse report");
                assert_eq!(parsed["echo"], serde_json::json!({"k": "v"}));
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn missing_job_attribute_is_an_error() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let dir = tempfile::tempdir().expect("tempdir");
                let script = write_script(
                    dir.path(),
                    r"
pub fn scan(req: dict) -> dict { return req }
",
                )
                .await;
                let error = run_job_once(&script, "scan", serde_json::json!({}))
                    .await
                    .expect_err("not a job");
                assert!(error.message().contains("is not a `@job`"));
            })
            .await;
    }
}
