//! Supervised Cargo execution behind a durable machine-resource lease.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use harn_vm::clock::{now_wall_ms, RealClock};

use crate::cli::{
    HostLeaseRunArgs, HostLeaseRunCargoArgs, HostLeaseRunCargoWorkerArgs, HostLeaseRunCommand,
};

use super::{print_error, EX_TEMPFAIL};

const EX_CANCELLED: i32 = 130;

pub(super) async fn run_supervised(
    store: &harn_hostlib::HostLeaseStore,
    args: HostLeaseRunArgs,
) -> i32 {
    match args.command {
        HostLeaseRunCommand::Cargo(args) => run_cargo(store, args).await,
    }
}

async fn run_cargo(store: &harn_hostlib::HostLeaseStore, args: HostLeaseRunCargoArgs) -> i32 {
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
        harn_hostlib::HostLeaseResourceKey {
            machine: host,
            resource_class: harn_hostlib::HostLeaseResourceClass::RustHeavy,
        },
        context,
        args.wait_ms,
    ) {
        Ok(run) => run,
        Err(error) => {
            return print_error("host_lease_run_cargo_receipt", &error.to_string(), false)
        }
    };
    let worker_args = match cargo_worker_args(&args, &cargo, &run.run_id) {
        Ok(worker_args) => worker_args,
        Err(error) => {
            record_start_failure(
                store,
                &run.run_id,
                harn_hostlib::HostLeaseRunStartFailure::WorkerArguments,
            );
            return print_error("host_lease_run_cargo", &error, false);
        }
    };
    let worker = match harn_hostlib::process::spawn_process(harn_hostlib::process::SpawnSpec {
        builtin: "harn_host_lease_run_cargo",
        program: executable,
        args: worker_args,
        cwd: Some(cargo.workspace.clone()),
        env: BTreeMap::new(),
        env_remove: Vec::new(),
        env_mode: harn_hostlib::process::EnvMode::InheritClean,
        use_stdin: true,
        configure_process_group: true,
        output_capture: harn_hostlib::process::OutputCapture::Inherit,
    }) {
        Ok(worker) => worker,
        Err(error) => {
            record_start_failure(
                store,
                &run.run_id,
                harn_hostlib::HostLeaseRunStartFailure::WorkerSpawn,
            );
            return print_error("host_lease_run_cargo", &error.to_string(), false);
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
    let exit_code = match &completion {
        WorkerCompletion::Exited(status) => status_code(*status),
        WorkerCompletion::Cancelled => EX_CANCELLED,
    };
    let next = match current.status {
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
            return Err("run receipt was already finalized".to_string())
        }
    };
    if let Some(next) = next {
        store
            .transition_run(run_id, next)
            .map_err(|error| error.to_string())?;
    }
    let path = store
        .run_receipt_path(run_id)
        .map_err(|error| error.to_string())?;
    eprintln!("Cargo lease receipt: {}", path.display());
    Ok(exit_code)
}

fn completed_release_outcome(
    store: &harn_hostlib::HostLeaseStore,
    resource: &harn_hostlib::HostLeaseResourceKey,
    lease_id: &str,
) -> Result<harn_hostlib::HostLeaseRunReleaseOutcome, String> {
    let release = store
        .release_for_resource(&resource.machine, resource.resource_class, lease_id)
        .map_err(|error| error.to_string())?;
    if release.released {
        return Ok(harn_hostlib::HostLeaseRunReleaseOutcome::Released);
    }
    let state = store
        .status_for_resource(&resource.machine, resource.resource_class)
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
        return 1;
    }
    let resource = pending.resource;
    let request = harn_hostlib::HostLeaseRequest {
        host: resource.machine,
        resource_class: resource.resource_class,
        execution_context: Some(context),
        owner: pending.owner,
        priority_class: harn_hostlib::HostLeasePriorityClass::CiVerify,
        ttl_ms: None,
        owner_pid: Some(std::process::id()),
        reason: Some("supervised cargo workload".to_string()),
        metadata: BTreeMap::new(),
    };
    let acquisition = if pending.wait_limit_ms == 0 {
        store.try_acquire(request)
    } else {
        store.acquire_wait(request, Duration::from_millis(pending.wait_limit_ms))
    };
    let acquisition = match acquisition {
        Ok(receipt) if receipt.status == harn_hostlib::HostLeaseAcquireStatus::Acquired => receipt,
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
            return 1;
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
        return 1;
    };
    let Some(worker_pid) = handle.owner_pid else {
        fail_acquired_before_running(
            &store,
            &args.run_id,
            &acquisition,
            harn_hostlib::HostLeaseRunStartFailure::WorkerContract,
        );
        eprintln!("error: acquired lease omitted its worker PID");
        return 1;
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
        return 1;
    }
    run_cargo_workload(&cargo, &args.cargo_args, &store, &args.run_id, &acquisition)
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
    path.to_str()
        .map(ToOwned::to_owned)
        .ok_or_else(|| format!("path {} is not valid UTF-8", path.display()))
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
            )
        }
    };
    let error = match harn_hostlib::process::replace_current_process(spec) {
        Ok(never) => match never {},
        Err(error) => error,
    };
    fail_cargo_launch(
        store,
        run_id,
        acquisition,
        harn_hostlib::HostLeaseRunLaunchFailure::ProcessReplace,
        &format!("failed to exec Cargo: {error}"),
    )
}

#[cfg(target_os = "windows")]
fn run_cargo_workload(
    cargo: &NormalizedCargoPaths,
    args: &[String],
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
            )
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
            )
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
            )
        }
    };
    let status = match child.wait() {
        Ok(status) => status,
        Err(error) => {
            eprintln!("error: failed to wait for Cargo: {error}");
            return 1;
        }
    };
    if let Err(error) = job.disarm() {
        eprintln!("error: failed to close Cargo process-tree supervision: {error}");
        return 1;
    }
    status_code(status)
}

#[cfg(not(any(unix, target_os = "windows")))]
fn run_cargo_workload(
    _cargo: &NormalizedCargoPaths,
    _args: &[String],
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
        env_remove: Vec::new(),
        env_mode: harn_hostlib::process::EnvMode::InheritClean,
        use_stdin: true,
        configure_process_group: false,
        output_capture: harn_hostlib::process::OutputCapture::Inherit,
    })
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
    1
}

fn fail_acquired_before_running(
    store: &harn_hostlib::HostLeaseStore,
    run_id: &str,
    acquisition: &harn_hostlib::HostLeaseAcquireReceipt,
    error: harn_hostlib::HostLeaseRunStartFailure,
) {
    if let Some(handle) = acquisition.handle.as_ref() {
        let _ = store.release_for_resource(&handle.host, handle.resource_class, &handle.lease_id);
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
