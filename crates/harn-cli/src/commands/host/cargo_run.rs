//! Supervised Cargo execution behind a durable machine-resource lease.

use std::cell::Cell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use harn_vm::clock::{now_wall_ms, RealClock};

use crate::cli::{
    HostLeaseRunArgs, HostLeaseRunCargoArgs, HostLeaseRunCargoWorkerArgs, HostLeaseRunCommand,
};
use crate::format::format_duration_ms;

use super::{print_error, EX_TEMPFAIL};

const EX_CANCELLED: i32 = 130;
const EX_TIMEOUT: i32 = 124;
const CARGO_LEASE_CONTROL_ENV: [&str; 6] = [
    "HARN_CARGO_LEASE_RUNNER",
    "HARN_CARGO_LEASE_OWNER",
    "HARN_CARGO_LEASE_HOST",
    "HARN_CARGO_LEASE_WAIT_MS",
    "HARN_CARGO_LEASE_PRIORITY_CLASS",
    "HARN_CARGO_LEASE_WORKLOAD_TIMEOUT_MS",
];

pub(super) async fn run_supervised(
    store: &harn_hostlib::HostLeaseStore,
    args: HostLeaseRunArgs,
) -> i32 {
    match args.command {
        HostLeaseRunCommand::Cargo(args) => run_cargo(store, args).await,
    }
}

async fn run_cargo(store: &harn_hostlib::HostLeaseStore, args: HostLeaseRunCargoArgs) -> i32 {
    if args.workload_timeout_ms == Some(0) {
        return print_error(
            "host_lease_run_cargo",
            "--workload-timeout-ms must be greater than zero",
            false,
        );
    }
    let cargo = match normalized_cargo_paths(
        &args.workspace,
        &args.target_dir,
        args.build_dir.as_deref(),
    ) {
        Ok(cargo) => cargo,
        Err(error) => return print_error("host_lease_run_cargo", &error, false),
    };
    let executable = match std::env::current_exe().and_then(path_into_string) {
        Ok(executable) => executable,
        Err(error) => return print_error("host_lease_run_cargo", &error.to_string(), false),
    };
    let host = args
        .host
        .clone()
        .unwrap_or_else(harn_hostlib::HostLeaseStore::default_host);
    let context = harn_hostlib::HostLeaseExecutionContext::cargo(
        &cargo.workspace,
        &cargo.target_dir,
        cargo.build_dir.as_deref(),
    );
    let run = match store.begin_run(
        &args.owner,
        super::priority(args.priority_class),
        harn_hostlib::HostLeaseResourceKey {
            machine: host,
            resource_class: harn_hostlib::HostLeaseResourceClass::RustHeavy,
            domain: harn_hostlib::DEFAULT_HOST_LEASE_DOMAIN.to_string(),
        },
        context,
        args.wait_ms,
    ) {
        Ok(run) => run,
        Err(error) => {
            return print_error("host_lease_run_cargo_receipt", &error.to_string(), false);
        }
    };
    let worker_args = match cargo_worker_args(&args, &cargo, &run.run_id) {
        Ok(worker_args) => worker_args,
        Err(error) => {
            return fail_run_start(
                store,
                &run.run_id,
                harn_hostlib::HostLeaseRunStartFailure::WorkerArguments,
                &error,
            );
        }
    };
    let worker = match harn_hostlib::process::spawn_process(harn_hostlib::process::SpawnSpec {
        builtin: "harn_host_lease_run_cargo",
        program: executable,
        args: worker_args,
        cwd: Some(cargo.workspace.clone()),
        env: BTreeMap::new(),
        env_remove: cargo_lease_control_env(),
        env_mode: harn_hostlib::process::EnvMode::InheritClean,
        use_stdin: true,
        configure_process_group: true,
        owner_death: harn_hostlib::process::OwnerDeathPolicy::None,
        output_capture: harn_hostlib::process::OutputCapture::Inherit,
    }) {
        Ok(worker) => worker,
        Err(error) => {
            return fail_run_start(
                store,
                &run.run_id,
                harn_hostlib::HostLeaseRunStartFailure::WorkerSpawn,
                &error.to_string(),
            );
        }
    };
    let completion = match wait_for_worker(worker).await {
        Ok(completion) => completion,
        Err(error) => return print_error("host_lease_run_cargo", &error, false),
    };
    match finalize_run(store, &run.run_id, completion) {
        Ok(exit) => exit,
        Err(error) => print_error("host_lease_run_cargo_receipt", &error, false),
    }
}

enum WorkerCompletion {
    Exited(harn_hostlib::process::ExitStatus),
    Cancelled,
}

async fn wait_for_worker(
    mut worker: Box<dyn harn_hostlib::process::ProcessHandle>,
) -> Result<WorkerCompletion, String> {
    let killer = worker.killer();
    let interrupted = Arc::new(AtomicBool::new(false));
    let wait_interrupted = Arc::clone(&interrupted);
    let mut wait = tokio::task::spawn_blocking(move || {
        worker.wait_with_timeout(None, &|| wait_interrupted.load(Ordering::SeqCst))
    });

    tokio::select! {
        result = &mut wait => worker_completion(result, killer.as_ref()),
        signal = wait_for_shutdown_signal() => {
            if let Err(error) = signal {
                eprintln!("warning: host lease signal handler unavailable: {error}");
                return worker_completion(wait.await, killer.as_ref());
            }
            interrupted.store(true, Ordering::SeqCst);
            worker_completion(wait.await, killer.as_ref())
        }
    }
}

fn worker_completion(
    result: Result<std::io::Result<harn_hostlib::process::WaitOutcome>, tokio::task::JoinError>,
    killer: &dyn harn_hostlib::process::ProcessKiller,
) -> Result<WorkerCompletion, String> {
    let result = match result {
        Ok(result) => result,
        Err(error) => {
            let _ = killer.kill();
            return Err(format!("worker wait task failed: {error}"));
        }
    };
    match result {
        Ok(harn_hostlib::process::WaitOutcome::Exited(status)) => {
            Ok(WorkerCompletion::Exited(status))
        }
        Ok(harn_hostlib::process::WaitOutcome::Interrupted(_)) => Ok(WorkerCompletion::Cancelled),
        Ok(harn_hostlib::process::WaitOutcome::TimedOut(_)) => {
            Err("worker wait timed out without a configured deadline".to_string())
        }
        Err(error) => {
            let _ = killer.kill();
            Err(format!("worker wait failed: {error}"))
        }
    }
}

#[cfg(unix)]
async fn wait_for_shutdown_signal() -> Result<(), std::io::Error> {
    use tokio::signal::unix::{signal, SignalKind};

    let mut interrupt = signal(SignalKind::interrupt())?;
    let mut terminate = signal(SignalKind::terminate())?;
    tokio::select! {
        _ = interrupt.recv() => {}
        _ = terminate.recv() => {}
    }
    Ok(())
}

#[cfg(not(unix))]
async fn wait_for_shutdown_signal() -> Result<(), std::io::Error> {
    tokio::signal::ctrl_c().await
}

fn finalize_run(
    store: &harn_hostlib::HostLeaseStore,
    run_id: &str,
    completion: WorkerCompletion,
) -> Result<i32, String> {
    let current = store.load_run(run_id).map_err(|error| error.to_string())?;
    let worker_exit_code = match &completion {
        WorkerCompletion::Exited(status) => status_code(*status),
        WorkerCompletion::Cancelled => EX_CANCELLED,
    };
    let next = match current.status.clone() {
        harn_hostlib::HostLeaseRunState::Pending { .. } => Some(match completion {
            WorkerCompletion::Exited(_) => harn_hostlib::HostLeaseRunState::StartFailed {
                observed_at_ms: unix_now_ms(),
                error: harn_hostlib::HostLeaseRunStartFailure::WorkerExitedBeforeAcquire,
            },
            WorkerCompletion::Cancelled => harn_hostlib::HostLeaseRunState::CancelledBeforeStart {
                finished_at_ms: unix_now_ms(),
            },
        }),
        harn_hostlib::HostLeaseRunState::Running {
            lease_id,
            acquired_at_ms,
            acquire_wait_ms,
            worker_pid,
        } => {
            let release = completed_release_outcome(store, &current.resource, &lease_id)?;
            Some(match completion {
                WorkerCompletion::Exited(status) => harn_hostlib::HostLeaseRunState::Completed {
                    lease_id,
                    acquire_wait_ms,
                    hold_ms: elapsed_since_ms(acquired_at_ms),
                    worker_pid,
                    exit: process_exit(&status),
                    release,
                    finished_at_ms: unix_now_ms(),
                },
                WorkerCompletion::Cancelled => harn_hostlib::HostLeaseRunState::Cancelled {
                    lease_id,
                    acquire_wait_ms,
                    hold_ms: elapsed_since_ms(acquired_at_ms),
                    worker_pid,
                    release,
                    finished_at_ms: unix_now_ms(),
                },
            })
        }
        harn_hostlib::HostLeaseRunState::Deferred { .. }
        | harn_hostlib::HostLeaseRunState::StartFailed { .. }
        | harn_hostlib::HostLeaseRunState::CancelledBeforeStart { .. }
        | harn_hostlib::HostLeaseRunState::LaunchFailed { .. } => None,
        harn_hostlib::HostLeaseRunState::Completed { .. }
        | harn_hostlib::HostLeaseRunState::Cancelled { .. } => {
            return Err("run receipt was already finalized".to_string());
        }
    };
    let terminal = if let Some(next) = next {
        store
            .transition_run(run_id, next)
            .map_err(|error| error.to_string())?
    } else {
        current
    };
    let projection = terminal_projection(&terminal.status, worker_exit_code)?;
    report_terminal_run(store, run_id, projection)
}

struct TerminalProjection {
    exit_code: i32,
    diagnostic: Option<String>,
}

fn terminal_projection(
    state: &harn_hostlib::HostLeaseRunState,
    worker_exit_code: i32,
) -> Result<TerminalProjection, String> {
    let projection = match state {
        harn_hostlib::HostLeaseRunState::StartFailed { error, .. } => TerminalProjection {
            exit_code: EX_TEMPFAIL,
            diagnostic: Some(format!(
                "error: Cargo workload did not start (state=start-failed error={})",
                error.as_str()
            )),
        },
        harn_hostlib::HostLeaseRunState::Deferred { .. } => TerminalProjection {
            exit_code: EX_TEMPFAIL,
            diagnostic: None,
        },
        harn_hostlib::HostLeaseRunState::CancelledBeforeStart { .. }
        | harn_hostlib::HostLeaseRunState::Cancelled { .. } => TerminalProjection {
            exit_code: EX_CANCELLED,
            diagnostic: None,
        },
        harn_hostlib::HostLeaseRunState::Completed { exit, .. } => TerminalProjection {
            exit_code: exit.code.unwrap_or(worker_exit_code),
            diagnostic: None,
        },
        harn_hostlib::HostLeaseRunState::LaunchFailed { error, .. } => TerminalProjection {
            exit_code: EX_TEMPFAIL,
            diagnostic: Some(format!(
                "error: Cargo workload did not start (state=launch-failed error={})",
                error.as_str()
            )),
        },
        harn_hostlib::HostLeaseRunState::Pending { .. }
        | harn_hostlib::HostLeaseRunState::Running { .. } => {
            return Err("run receipt remained non-terminal after worker exit".to_string());
        }
    };
    Ok(projection)
}

fn report_terminal_run(
    store: &harn_hostlib::HostLeaseStore,
    run_id: &str,
    projection: TerminalProjection,
) -> Result<i32, String> {
    if let Some(diagnostic) = projection.diagnostic {
        eprintln!("{diagnostic}");
    }
    let path = store
        .run_receipt_path(run_id)
        .map_err(|error| error.to_string())?;
    eprintln!("Cargo lease receipt: {}", path.display());
    Ok(projection.exit_code)
}

fn fail_run_start(
    store: &harn_hostlib::HostLeaseStore,
    run_id: &str,
    error: harn_hostlib::HostLeaseRunStartFailure,
    message: &str,
) -> i32 {
    eprintln!("error: {message}");
    let terminal = harn_hostlib::HostLeaseRunState::StartFailed {
        observed_at_ms: unix_now_ms(),
        error,
    };
    if let Err(transition_error) = store.transition_run(run_id, terminal.clone()) {
        eprintln!("error: failed to record Cargo start failure: {transition_error}");
    }
    match terminal_projection(&terminal, EX_TEMPFAIL)
        .and_then(|projection| report_terminal_run(store, run_id, projection))
    {
        Ok(exit_code) => exit_code,
        Err(report_error) => {
            eprintln!("error: failed to report Cargo start failure: {report_error}");
            EX_TEMPFAIL
        }
    }
}

fn completed_release_outcome(
    store: &harn_hostlib::HostLeaseStore,
    resource: &harn_hostlib::HostLeaseResourceKey,
    lease_id: &str,
) -> Result<harn_hostlib::HostLeaseRunReleaseOutcome, String> {
    let release = store
        .release_for_domain(
            &resource.machine,
            resource.resource_class,
            &resource.domain,
            lease_id,
        )
        .map_err(|error| error.to_string())?;
    if release.released {
        return Ok(harn_hostlib::HostLeaseRunReleaseOutcome::Released);
    }
    let state = store
        .status_for_domain(&resource.machine, resource.resource_class, &resource.domain)
        .map_err(|error| error.to_string())?;
    if state
        .active
        .as_ref()
        .is_some_and(|active| active.lease_id == lease_id)
    {
        return Err("worker lease remained active after its process exited".to_string());
    }
    Ok(harn_hostlib::HostLeaseRunReleaseOutcome::AlreadyRecovered)
}

pub(super) fn run_cargo_worker(args: HostLeaseRunCargoWorkerArgs) -> i32 {
    let cargo = match normalized_cargo_paths(
        &args.workspace,
        &args.target_dir,
        args.build_dir.as_deref(),
    ) {
        Ok(cargo) => cargo,
        Err(error) => {
            eprintln!("error: {error}");
            return 1;
        }
    };
    let store = match harn_hostlib::HostLeaseStore::from_env() {
        Ok(store) => store,
        Err(error) => {
            eprintln!("error: {error}");
            return 1;
        }
    };
    let context = harn_hostlib::HostLeaseExecutionContext::cargo(
        &cargo.workspace,
        &cargo.target_dir,
        cargo.build_dir.as_deref(),
    );
    let pending = match store.load_run(&args.run_id) {
        Ok(pending) => pending,
        Err(error) => {
            eprintln!("error: {error}");
            return 1;
        }
    };
    if pending.resource.resource_class != harn_hostlib::HostLeaseResourceClass::RustHeavy
        || pending.execution_context != context
    {
        record_start_failure(
            &store,
            &args.run_id,
            harn_hostlib::HostLeaseRunStartFailure::WorkerContextMismatch,
        );
        eprintln!("error: worker context does not match its durable run receipt");
        return EX_TEMPFAIL;
    }
    let saw_wait = Cell::new(false);
    let mut report_wait = |receipt: &harn_hostlib::HostLeaseAcquireReceipt| {
        saw_wait.set(true);
        eprintln!("{}", format_lease_wait(receipt));
    };
    let acquisition = store.acquire_wait_for_run_with_progress(
        &args.run_id,
        std::process::id(),
        &mut report_wait,
    );
    let acquisition = match acquisition {
        Ok(receipt) if receipt.status == harn_hostlib::HostLeaseAcquireStatus::Acquired => {
            if saw_wait.get() {
                eprintln!(
                    "Acquired rust-heavy lease after {}",
                    format_duration_ms(receipt.waited_ms)
                );
            }
            receipt
        }
        Ok(receipt) => {
            let deferred = receipt
                .defer
                .as_ref()
                .expect("deferred lease has a typed receipt");
            eprintln!(
                "rust-heavy lease on {} remains held; retry after its next receipt wake",
                deferred.host
            );
            let _ = store.transition_run(
                &args.run_id,
                harn_hostlib::HostLeaseRunState::Deferred {
                    observed_at_ms: deferred.observed_at_ms,
                    waited_ms: receipt.waited_ms,
                },
            );
            return EX_TEMPFAIL;
        }
        Err(error) => {
            let _ = store.transition_run(
                &args.run_id,
                harn_hostlib::HostLeaseRunState::StartFailed {
                    observed_at_ms: unix_now_ms(),
                    error: harn_hostlib::HostLeaseRunStartFailure::ResourceAcquire,
                },
            );
            eprintln!("error: {error}");
            return EX_TEMPFAIL;
        }
    };
    let Some(handle) = acquisition.handle.as_ref() else {
        fail_acquired_before_running(
            &store,
            &args.run_id,
            &acquisition,
            harn_hostlib::HostLeaseRunStartFailure::WorkerContract,
        );
        eprintln!("error: acquired lease omitted its handle");
        return EX_TEMPFAIL;
    };
    let Some(worker_pid) = handle.owner_pid else {
        fail_acquired_before_running(
            &store,
            &args.run_id,
            &acquisition,
            harn_hostlib::HostLeaseRunStartFailure::WorkerContract,
        );
        eprintln!("error: acquired lease omitted its worker PID");
        return EX_TEMPFAIL;
    };
    if let Err(error) = store.transition_run(
        &args.run_id,
        harn_hostlib::HostLeaseRunState::Running {
            lease_id: handle.lease_id.clone(),
            acquired_at_ms: handle.acquired_at_ms,
            acquire_wait_ms: acquisition.waited_ms,
            worker_pid,
        },
    ) {
        fail_acquired_before_running(
            &store,
            &args.run_id,
            &acquisition,
            harn_hostlib::HostLeaseRunStartFailure::ReceiptTransition,
        );
        eprintln!("error: {error}");
        return EX_TEMPFAIL;
    }
    run_cargo_workload(
        &cargo,
        &args.cargo_args,
        args.workload_timeout_ms,
        &store,
        &args.run_id,
        &acquisition,
    )
}

fn format_lease_wait(receipt: &harn_hostlib::HostLeaseAcquireReceipt) -> String {
    let queue_position = receipt
        .queue
        .as_ref()
        .map(|queue| queue.position.to_string())
        .unwrap_or_else(|| "unknown".to_string());
    let elapsed = format_duration_ms(receipt.waited_ms);
    match receipt.defer.as_ref().map(|defer| defer.deferred_reason) {
        Some(harn_hostlib::HostLeaseDeferReason::Contended) => receipt
            .defer
            .as_ref()
            .and_then(|defer| defer.active.as_ref())
            .map(|active| {
                format!(
                    "Waiting for rust-heavy lease held by {} (queue position {queue_position}, elapsed {elapsed})",
                    active.owner
                )
            })
            .unwrap_or_else(|| {
                format!(
                    "Waiting for contended rust-heavy lease (queue position {queue_position}, elapsed {elapsed})"
                )
            }),
        Some(harn_hostlib::HostLeaseDeferReason::Queued) => format!(
            "Waiting for rust-heavy lease admission (queue position {queue_position}, elapsed {elapsed})"
        ),
        Some(harn_hostlib::HostLeaseDeferReason::RegistryBusy) => format!(
            "Waiting for rust-heavy lease registry (queue position {queue_position}, elapsed {elapsed})"
        ),
        None => format!(
            "Waiting for rust-heavy lease (queue position {queue_position}, elapsed {elapsed})"
        ),
    }
}

struct NormalizedCargoPaths {
    workspace: PathBuf,
    target_dir: PathBuf,
    build_dir: Option<PathBuf>,
}

fn cargo_worker_args(
    args: &HostLeaseRunCargoArgs,
    cargo: &NormalizedCargoPaths,
    run_id: &str,
) -> Result<Vec<String>, String> {
    let mut worker_args = vec![
        "host".to_string(),
        "lease".to_string(),
        "run-cargo-worker".to_string(),
        "--workspace".to_string(),
        path_argument(&cargo.workspace)?,
        "--target-dir".to_string(),
        path_argument(&cargo.target_dir)?,
    ];
    if let Some(build_dir) = cargo.build_dir.as_ref() {
        worker_args.push("--build-dir".to_string());
        worker_args.push(path_argument(build_dir)?);
    }
    if let Some(timeout_ms) = args.workload_timeout_ms {
        worker_args.extend(["--workload-timeout-ms".to_string(), timeout_ms.to_string()]);
    }
    worker_args.extend(["--run-id".to_string(), run_id.to_string(), "--".to_string()]);
    worker_args.extend(args.cargo_args.iter().cloned());
    Ok(worker_args)
}

fn normalized_cargo_paths(
    workspace: &Path,
    target_dir: &Path,
    build_dir: Option<&Path>,
) -> Result<NormalizedCargoPaths, String> {
    let workspace = workspace
        .canonicalize()
        .map_err(|error| format!("workspace {} is unavailable: {error}", workspace.display()))?;
    if !workspace.is_dir() {
        return Err(format!(
            "workspace {} is not a directory",
            workspace.display()
        ));
    }
    let target_dir = normalized_output_directory("target directory", target_dir)?;
    require_matching_cargo_environment("CARGO_TARGET_DIR", &target_dir)?;
    let build_dir = match build_dir {
        Some(build_dir) => {
            let build_dir = normalized_output_directory("build directory", build_dir)?;
            require_matching_cargo_environment("CARGO_BUILD_BUILD_DIR", &build_dir)?;
            Some(build_dir)
        }
        None => std::env::var_os("CARGO_BUILD_BUILD_DIR")
            .map(|path| normalized_output_directory("build directory", Path::new(&path)))
            .transpose()?,
    };
    Ok(NormalizedCargoPaths {
        workspace,
        target_dir,
        build_dir,
    })
}

fn normalized_output_directory(name: &str, path: &Path) -> Result<PathBuf, String> {
    let path = absolute_path(path)?;
    std::fs::create_dir_all(&path)
        .map_err(|error| format!("cannot create {name} {}: {error}", path.display()))?;
    path.canonicalize()
        .map_err(|error| format!("cannot normalize {name} {}: {error}", path.display()))
}

fn absolute_path(path: &Path) -> Result<PathBuf, String> {
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    std::env::current_dir()
        .map(|cwd| cwd.join(path))
        .map_err(|error| {
            format!(
                "cannot resolve {} from the current directory: {error}",
                path.display()
            )
        })
}

fn path_into_string(path: PathBuf) -> Result<String, std::io::Error> {
    path.into_os_string().into_string().map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "supervised process paths must be valid UTF-8",
        )
    })
}

fn path_argument(path: &Path) -> Result<String, String> {
    let text = path
        .to_str()
        .ok_or_else(|| format!("path {} is not valid UTF-8", path.display()))?;
    // Canonical paths remain authoritative for lease identity and equality,
    // but Windows canonicalization adds a `\\?\` prefix that child tools do
    // not uniformly accept in argv or environment values. In particular,
    // cc-rs forwards Cargo's verbatim OUT_DIR to cl.exe, which can reinterpret
    // the source path as rooted at `\\`. Normalize only at the external
    // process boundary through the shared Windows-path owner.
    Ok(harn_vm::windows_path::strip_windows_verbatim_prefix(text).into_owned())
}

fn require_matching_cargo_environment(name: &str, expected: &Path) -> Result<(), String> {
    let Some(actual) = std::env::var_os(name) else {
        return Ok(());
    };
    let actual = Path::new(&actual).canonicalize().map_err(|error| {
        format!(
            "cannot normalize {name} value {}: {error}",
            Path::new(&actual).display()
        )
    })?;
    if actual == expected {
        return Ok(());
    }
    Err(format!(
        "{name} disagrees with the supervised Cargo context: {} != {}",
        actual.display(),
        expected.display()
    ))
}

#[cfg(unix)]
fn run_cargo_workload(
    cargo: &NormalizedCargoPaths,
    args: &[String],
    workload_timeout_ms: Option<u64>,
    store: &harn_hostlib::HostLeaseStore,
    run_id: &str,
    acquisition: &harn_hostlib::HostLeaseAcquireReceipt,
) -> i32 {
    let spec = match cargo_spawn_spec(cargo, args) {
        Ok(spec) => spec,
        Err(error) => {
            return fail_cargo_launch(
                store,
                run_id,
                acquisition,
                harn_hostlib::HostLeaseRunLaunchFailure::ArgumentEncoding,
                &error,
            );
        }
    };
    let mut child = match harn_hostlib::process::spawn_process(spec) {
        Ok(child) => child,
        Err(error) => {
            return fail_cargo_launch(
                store,
                run_id,
                acquisition,
                harn_hostlib::HostLeaseRunLaunchFailure::ProcessSpawn,
                &format!("failed to spawn Cargo: {error}"),
            );
        }
    };
    if let Err(error) = adopt_workload_pid(store, run_id, acquisition, child.pid()) {
        let _ = child.killer().kill();
        return error;
    }
    wait_for_cargo_workload(child.as_mut(), workload_timeout_ms)
}

#[cfg(target_os = "windows")]
fn run_cargo_workload(
    cargo: &NormalizedCargoPaths,
    args: &[String],
    workload_timeout_ms: Option<u64>,
    store: &harn_hostlib::HostLeaseStore,
    run_id: &str,
    acquisition: &harn_hostlib::HostLeaseAcquireReceipt,
) -> i32 {
    let job = match harn_hostlib::process::KillOnCloseJob::enroll_current_process() {
        Ok(job) => job,
        Err(error) => {
            return fail_cargo_launch(
                store,
                run_id,
                acquisition,
                harn_hostlib::HostLeaseRunLaunchFailure::ProcessSupervision,
                &format!("failed to supervise the Cargo process tree: {error}"),
            );
        }
    };
    let spec = match cargo_spawn_spec(cargo, args) {
        Ok(spec) => spec,
        Err(error) => {
            return fail_cargo_launch(
                store,
                run_id,
                acquisition,
                harn_hostlib::HostLeaseRunLaunchFailure::ArgumentEncoding,
                &error,
            );
        }
    };
    let mut child = match harn_hostlib::process::spawn_process(spec) {
        Ok(child) => child,
        Err(error) => {
            return fail_cargo_launch(
                store,
                run_id,
                acquisition,
                harn_hostlib::HostLeaseRunLaunchFailure::ProcessSpawn,
                &format!("failed to spawn Cargo: {error}"),
            );
        }
    };
    if let Err(error) = adopt_workload_pid(store, run_id, acquisition, child.pid()) {
        let _ = child.killer().kill();
        return error;
    }
    let status = wait_for_cargo_workload(child.as_mut(), workload_timeout_ms);
    if let Err(error) = job.disarm() {
        eprintln!("error: failed to close Cargo process-tree supervision: {error}");
        return 1;
    }
    status
}

#[cfg(not(any(unix, target_os = "windows")))]
fn run_cargo_workload(
    _cargo: &NormalizedCargoPaths,
    _args: &[String],
    _workload_timeout_ms: Option<u64>,
    store: &harn_hostlib::HostLeaseStore,
    run_id: &str,
    acquisition: &harn_hostlib::HostLeaseAcquireReceipt,
) -> i32 {
    fail_cargo_launch(
        store,
        run_id,
        acquisition,
        harn_hostlib::HostLeaseRunLaunchFailure::UnsupportedPlatform,
        "supervised Cargo workloads are unavailable on this platform",
    )
}

fn adopt_workload_pid(
    store: &harn_hostlib::HostLeaseStore,
    run_id: &str,
    acquisition: &harn_hostlib::HostLeaseAcquireReceipt,
    cargo_pid: Option<u32>,
) -> Result<(), i32> {
    let Some(handle) = acquisition.handle.as_ref() else {
        return Err(fail_cargo_launch(
            store,
            run_id,
            acquisition,
            harn_hostlib::HostLeaseRunLaunchFailure::ProcessSpawn,
            "spawned Cargo omitted its PID",
        ));
    };
    let Some(cargo_pid) = cargo_pid else {
        return Err(fail_cargo_launch(
            store,
            run_id,
            acquisition,
            harn_hostlib::HostLeaseRunLaunchFailure::ProcessSpawn,
            "spawned Cargo omitted its PID",
        ));
    };
    if let Err(error) = store.rebind_owner_pid(
        &handle.host,
        handle.resource_class,
        &handle.domain,
        &handle.lease_id,
        cargo_pid,
    ) {
        return Err(fail_cargo_launch(
            store,
            run_id,
            acquisition,
            harn_hostlib::HostLeaseRunLaunchFailure::ProcessSpawn,
            &format!("failed to transfer the lease to Cargo: {error}"),
        ));
    }
    let current = match store.load_run(run_id) {
        Ok(current) => current,
        Err(error) => {
            return Err(fail_cargo_launch(
                store,
                run_id,
                acquisition,
                harn_hostlib::HostLeaseRunLaunchFailure::ProcessSpawn,
                &format!("failed to load the run receipt after Cargo spawn: {error}"),
            ));
        }
    };
    let harn_hostlib::HostLeaseRunState::Running {
        lease_id,
        acquired_at_ms,
        acquire_wait_ms,
        ..
    } = current.status
    else {
        return Err(fail_cargo_launch(
            store,
            run_id,
            acquisition,
            harn_hostlib::HostLeaseRunLaunchFailure::ProcessSpawn,
            "run receipt was not running when Cargo started",
        ));
    };
    if let Err(error) = store.transition_run(
        run_id,
        harn_hostlib::HostLeaseRunState::Running {
            lease_id,
            acquired_at_ms,
            acquire_wait_ms,
            worker_pid: cargo_pid,
        },
    ) {
        return Err(fail_cargo_launch(
            store,
            run_id,
            acquisition,
            harn_hostlib::HostLeaseRunLaunchFailure::ProcessSpawn,
            &format!("failed to record the Cargo PID on the run receipt: {error}"),
        ));
    }
    Ok(())
}

fn wait_for_cargo_workload(
    child: &mut dyn harn_hostlib::process::ProcessHandle,
    workload_timeout_ms: Option<u64>,
) -> i32 {
    let timeout = workload_timeout_ms.map(std::time::Duration::from_millis);
    match child.wait_with_timeout(timeout, &|| false) {
        Ok(harn_hostlib::process::WaitOutcome::Exited(status)) => status_code(status),
        Ok(harn_hostlib::process::WaitOutcome::TimedOut(_)) => {
            let timeout_ms = workload_timeout_ms.expect("timeout outcome requires a deadline");
            eprintln!("error: Cargo workload timed out after {timeout_ms}ms after lease admission");
            EX_TIMEOUT
        }
        Ok(harn_hostlib::process::WaitOutcome::Interrupted(_)) => EX_CANCELLED,
        Err(error) => {
            eprintln!("error: failed to wait for Cargo: {error}");
            1
        }
    }
}

fn cargo_spawn_spec(
    cargo: &NormalizedCargoPaths,
    args: &[String],
) -> Result<harn_hostlib::process::SpawnSpec, String> {
    let target_dir = path_argument(&cargo.target_dir)?;
    let mut env = BTreeMap::from([("CARGO_TARGET_DIR".to_string(), target_dir)]);
    if let Some(build_dir) = cargo.build_dir.as_ref() {
        let build_dir = path_argument(build_dir)?;
        env.insert("CARGO_BUILD_BUILD_DIR".to_string(), build_dir);
    }
    Ok(harn_hostlib::process::SpawnSpec {
        builtin: "harn_host_lease_run_cargo_worker",
        program: "cargo".to_string(),
        args: args.to_vec(),
        cwd: Some(cargo.workspace.clone()),
        env,
        env_remove: cargo_lease_control_env(),
        env_mode: harn_hostlib::process::EnvMode::InheritClean,
        use_stdin: true,
        configure_process_group: false,
        owner_death: harn_hostlib::process::OwnerDeathPolicy::None,
        output_capture: harn_hostlib::process::OutputCapture::Inherit,
    })
}

fn cargo_lease_control_env() -> Vec<String> {
    CARGO_LEASE_CONTROL_ENV
        .iter()
        .map(|name| (*name).to_string())
        .collect()
}

fn fail_cargo_launch(
    store: &harn_hostlib::HostLeaseStore,
    run_id: &str,
    acquisition: &harn_hostlib::HostLeaseAcquireReceipt,
    error: harn_hostlib::HostLeaseRunLaunchFailure,
    message: &str,
) -> i32 {
    let Some(handle) = acquisition.handle.as_ref() else {
        eprintln!("error: {message}");
        return 1;
    };
    match completed_release_outcome(
        store,
        &harn_hostlib::HostLeaseResourceKey {
            machine: handle.host.clone(),
            resource_class: handle.resource_class,
            domain: handle.domain.clone(),
        },
        &handle.lease_id,
    ) {
        Ok(release) => {
            let _ = store.transition_run(
                run_id,
                harn_hostlib::HostLeaseRunState::LaunchFailed {
                    lease_id: handle.lease_id.clone(),
                    release,
                    observed_at_ms: unix_now_ms(),
                    error,
                },
            );
        }
        Err(error) => eprintln!("error: failed to release launch lease: {error}"),
    }
    eprintln!("error: {message}");
    EX_TEMPFAIL
}

fn fail_acquired_before_running(
    store: &harn_hostlib::HostLeaseStore,
    run_id: &str,
    acquisition: &harn_hostlib::HostLeaseAcquireReceipt,
    error: harn_hostlib::HostLeaseRunStartFailure,
) {
    if let Some(handle) = acquisition.handle.as_ref() {
        let _ = store.release_for_domain(
            &handle.host,
            handle.resource_class,
            &handle.domain,
            &handle.lease_id,
        );
    }
    let _ = store.transition_run(
        run_id,
        harn_hostlib::HostLeaseRunState::StartFailed {
            observed_at_ms: unix_now_ms(),
            error,
        },
    );
}

fn record_start_failure(
    store: &harn_hostlib::HostLeaseStore,
    run_id: &str,
    error: harn_hostlib::HostLeaseRunStartFailure,
) {
    let _ = store.transition_run(
        run_id,
        harn_hostlib::HostLeaseRunState::StartFailed {
            observed_at_ms: unix_now_ms(),
            error,
        },
    );
}

fn unix_now_ms() -> i64 {
    now_wall_ms(&RealClock::new())
}

fn elapsed_since_ms(started_at_ms: i64) -> u64 {
    (unix_now_ms() as u64).saturating_sub(started_at_ms.max(0) as u64)
}

fn process_exit(status: &harn_hostlib::process::ExitStatus) -> harn_hostlib::HostLeaseProcessExit {
    harn_hostlib::HostLeaseProcessExit {
        code: status.code,
        signal: status.signal,
    }
}

fn status_code(status: harn_hostlib::process::ExitStatus) -> i32 {
    status.code.unwrap_or(1)
}

#[cfg(test)]
mod tests {
    use super::{
        finalize_run, format_lease_wait, path_argument, terminal_projection,
        wait_for_cargo_workload, WorkerCompletion, EX_TEMPFAIL, EX_TIMEOUT,
    };
    use harn_hostlib::process::{
        EnvMode, MockProcessConfig, MockSpawner, OutputCapture, OwnerDeathPolicy, ProcessSpawner,
        SpawnSpec,
    };
    use std::collections::BTreeMap;
    use std::path::Path;
    use tempfile::TempDir;

    #[test]
    fn pending_worker_exit_is_persisted_as_start_failed_not_cargo_failure() {
        let temp = TempDir::new().unwrap();
        let store = harn_hostlib::HostLeaseStore::for_root(temp.path()).unwrap();
        let run = store
            .begin_run(
                "waiting-lane",
                harn_hostlib::HostLeasePriorityClass::Measurement,
                harn_hostlib::HostLeaseResourceKey {
                    machine: "fixture".to_string(),
                    resource_class: harn_hostlib::HostLeaseResourceClass::RustHeavy,
                    domain: harn_hostlib::DEFAULT_HOST_LEASE_DOMAIN.to_string(),
                },
                harn_hostlib::HostLeaseExecutionContext::cargo(
                    Path::new("/workspace/project"),
                    Path::new("/tmp/target"),
                    None,
                ),
                5_000,
            )
            .unwrap();

        let exit = finalize_run(
            &store,
            &run.run_id,
            WorkerCompletion::Exited(harn_hostlib::process::ExitStatus::from_code(101)),
        )
        .unwrap();

        assert_eq!(exit, EX_TEMPFAIL);
        assert!(matches!(
            store.load_run(&run.run_id).unwrap().status,
            harn_hostlib::HostLeaseRunState::StartFailed {
                error: harn_hostlib::HostLeaseRunStartFailure::WorkerExitedBeforeAcquire,
                ..
            }
        ));
    }

    #[test]
    fn start_failed_uses_a_reserved_supervisor_status_and_stable_diagnostic() {
        let projection = terminal_projection(
            &harn_hostlib::HostLeaseRunState::StartFailed {
                observed_at_ms: 1,
                error: harn_hostlib::HostLeaseRunStartFailure::WorkerExitedBeforeAcquire,
            },
            101,
        )
        .unwrap();

        assert_eq!(projection.exit_code, EX_TEMPFAIL);
        assert_eq!(
            projection.diagnostic.as_deref(),
            Some(
                "error: Cargo workload did not start (state=start-failed error=worker-exited-before-acquire)"
            )
        );
    }

    #[test]
    fn completed_workload_preserves_the_real_cargo_status() {
        let projection = terminal_projection(
            &harn_hostlib::HostLeaseRunState::Completed {
                lease_id: "lease".to_string(),
                acquire_wait_ms: 0,
                hold_ms: 1,
                worker_pid: 7,
                exit: harn_hostlib::HostLeaseProcessExit {
                    code: Some(101),
                    signal: None,
                },
                release: harn_hostlib::HostLeaseRunReleaseOutcome::Released,
                finished_at_ms: 2,
            },
            101,
        )
        .unwrap();

        assert_eq!(projection.exit_code, 101);
        assert!(projection.diagnostic.is_none());
    }

    #[test]
    fn wait_progress_projects_holder_queue_and_elapsed_time() {
        let receipt = harn_hostlib::HostLeaseAcquireReceipt {
            schema_version: 4,
            status: harn_hostlib::HostLeaseAcquireStatus::Deferred,
            observed_at_ms: 31_000,
            waited_ms: 31_000,
            handle: None,
            defer: Some(harn_hostlib::HostLeaseDeferReceipt {
                host: "fixture".to_string(),
                resource_class: harn_hostlib::HostLeaseResourceClass::RustHeavy,
                domain: harn_hostlib::DEFAULT_HOST_LEASE_DOMAIN.to_string(),
                deferred_reason: harn_hostlib::HostLeaseDeferReason::Contended,
                observed_at_ms: 31_000,
                next_wake_at_ms: None,
                deadline_at_ms: Some(90_000),
                active: Some(harn_hostlib::HostLeaseHandle {
                    schema_version: 4,
                    host: "fixture".to_string(),
                    resource_class: harn_hostlib::HostLeaseResourceClass::RustHeavy,
                    domain: harn_hostlib::DEFAULT_HOST_LEASE_DOMAIN.to_string(),
                    execution_context: None,
                    lease_id: "opaque".to_string(),
                    owner: "compile-lane".to_string(),
                    priority_class: harn_hostlib::HostLeasePriorityClass::Measurement,
                    acquired_at_ms: 0,
                    updated_at_ms: 0,
                    expires_at_ms: None,
                    owner_pid: None,
                    owner_process_identity: None,
                    reason: None,
                    metadata: BTreeMap::new(),
                }),
            }),
            recovered_stale_lease: false,
            recovered: None,
            queue: Some(harn_hostlib::HostLeaseQueueEvidence {
                waiter_id: "waiter".to_string(),
                requested_at_ms: 0,
                position: 2,
                predecessor_waiter_id: Some("first".to_string()),
            }),
        };

        assert_eq!(
            format_lease_wait(&receipt),
            "Waiting for rust-heavy lease held by compile-lane (queue position 2, elapsed 31.0s)"
        );

        let mut queued = receipt;
        let queued_defer = queued.defer.as_mut().unwrap();
        queued_defer.deferred_reason = harn_hostlib::HostLeaseDeferReason::Queued;
        queued_defer.active = None;
        assert_eq!(
            format_lease_wait(&queued),
            "Waiting for rust-heavy lease admission (queue position 2, elapsed 31.0s)"
        );

        let mut registry_busy = queued;
        registry_busy.defer.as_mut().unwrap().deferred_reason =
            harn_hostlib::HostLeaseDeferReason::RegistryBusy;
        assert_eq!(
            format_lease_wait(&registry_busy),
            "Waiting for rust-heavy lease registry (queue position 2, elapsed 31.0s)"
        );
    }

    #[test]
    fn child_process_paths_strip_windows_drive_verbatim_prefixes() {
        assert_eq!(
            path_argument(Path::new(r"\\?\E:\target\debug")).unwrap(),
            r"E:\target\debug"
        );
    }

    #[test]
    fn child_process_paths_strip_windows_unc_verbatim_prefixes() {
        assert_eq!(
            path_argument(Path::new(r"\\?\UNC\server\share\target")).unwrap(),
            r"\\server\share\target"
        );
    }

    #[test]
    fn child_process_paths_preserve_non_verbatim_spelling() {
        for path in [r"E:\target\debug", r"\\server\share\target", "/tmp/target"] {
            assert_eq!(path_argument(Path::new(path)).unwrap(), path);
        }
    }

    #[test]
    fn cargo_workload_timeout_starts_at_the_worker_boundary() {
        let spawner = MockSpawner::new();
        spawner.enqueue(MockProcessConfig {
            force_timeout: true,
            ..MockProcessConfig::default()
        });
        let mut child = spawner
            .spawn(SpawnSpec {
                builtin: "test",
                program: "cargo".to_string(),
                args: vec!["check".to_string()],
                cwd: None,
                env: BTreeMap::new(),
                env_remove: Vec::new(),
                env_mode: EnvMode::InheritClean,
                use_stdin: true,
                configure_process_group: false,
                owner_death: OwnerDeathPolicy::None,
                output_capture: OutputCapture::Inherit,
            })
            .unwrap();

        assert_eq!(
            wait_for_cargo_workload(child.as_mut(), Some(600_000)),
            EX_TIMEOUT
        );
    }
}
