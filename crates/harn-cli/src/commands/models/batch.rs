//! `harn models batch` — plan provider Batch API use.
//!
//! The catalog filtering and workload guidance live in
//! `crates/harn-stdlib/src/stdlib/cli/models/batch_plan.harn`. This
//! shim only forwards parsed clap flags through env vars, because the
//! script already has the read-only `harness.llm.catalog()` capability
//! it needs.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use crate::cli::{
    ModelsBatchArgs, ModelsBatchCommand, ModelsBatchManifestArgs, ModelsBatchPlanArgs,
    ModelsBatchPrepareArgs,
};
use crate::commands::run::RunSandboxOptions;
use crate::dispatch;
use crate::env_guard::ScopedEnvVar;

const BATCH_MODE_ENV: &str = "HARN_MODELS_BATCH_MODE";
const BATCH_PROVIDER_ENV: &str = "HARN_MODELS_BATCH_PROVIDER";
const BATCH_MODEL_ENV: &str = "HARN_MODELS_BATCH_MODEL";
const BATCH_WORKLOAD_ENV: &str = "HARN_MODELS_BATCH_WORKLOAD";
const BATCH_MIN_DISCOUNT_ENV: &str = "HARN_MODELS_BATCH_MIN_DISCOUNT_PERCENT";
const BATCH_MAX_TURNAROUND_ENV: &str = "HARN_MODELS_BATCH_MAX_TURNAROUND_HOURS";
const BATCH_REQUESTS_ENV: &str = "HARN_MODELS_BATCH_REQUESTS";
const BATCH_OUT_ENV: &str = "HARN_MODELS_BATCH_OUT";
const BATCH_MANIFEST_ENV: &str = "HARN_MODELS_BATCH_MANIFEST";
const BATCH_OUT_DIR_ENV: &str = "HARN_MODELS_BATCH_OUT_DIR";
const BATCH_TOOL_FORMAT_ENV: &str = "HARN_MODELS_BATCH_TOOL_FORMAT";
const BATCH_ID_PREFIX_ENV: &str = "HARN_MODELS_BATCH_ID_PREFIX";

static DISPATCH_BATCH_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

pub(crate) async fn run(args: ModelsBatchArgs) {
    let exit_code = match args.command {
        ModelsBatchCommand::Manifest(args) => run_manifest(args).await,
        ModelsBatchCommand::Plan(args) => run_plan(args).await,
        ModelsBatchCommand::Prepare(args) => run_prepare(args).await,
    };
    if exit_code != 0 {
        std::process::exit(exit_code);
    }
}

async fn run_manifest(args: ModelsBatchManifestArgs) -> i32 {
    let _guard = DISPATCH_BATCH_LOCK.lock().await;
    let requests_path = absolutize_path(&args.requests);
    let out_path = absolutize_path(&args.out);
    let mut sandbox = RunSandboxOptions::default().with_workspace_root(parent_dir(&out_path));
    let request_root = parent_dir(&requests_path);
    if request_root != parent_dir(&out_path) {
        sandbox = sandbox.with_read_only_roots(vec![request_root]);
    }

    let _mode = ScopedEnvVar::set(BATCH_MODE_ENV, "manifest");
    let _provider = ScopedEnvVar::set(
        BATCH_PROVIDER_ENV,
        args.provider.as_deref().map(str::trim).unwrap_or(""),
    );
    let _model = ScopedEnvVar::set(
        BATCH_MODEL_ENV,
        args.model.as_deref().map(str::trim).unwrap_or(""),
    );
    let _workload = ScopedEnvVar::set(BATCH_WORKLOAD_ENV, args.workload.trim());
    let min_discount = args
        .min_discount_percent
        .map(|value| value.to_string())
        .unwrap_or_default();
    let max_turnaround = args
        .max_turnaround_hours
        .map(|value| value.to_string())
        .unwrap_or_default();
    let requests = requests_path.to_string_lossy().to_string();
    let out = out_path.to_string_lossy().to_string();
    let _min_discount = ScopedEnvVar::set(BATCH_MIN_DISCOUNT_ENV, &min_discount);
    let _max_turnaround = ScopedEnvVar::set(BATCH_MAX_TURNAROUND_ENV, &max_turnaround);
    let _requests = ScopedEnvVar::set(BATCH_REQUESTS_ENV, requests.trim());
    let _out = ScopedEnvVar::set(BATCH_OUT_ENV, out.trim());
    let _tool_format = ScopedEnvVar::set(BATCH_TOOL_FORMAT_ENV, args.tool_format.trim());
    let _id_prefix = ScopedEnvVar::set(BATCH_ID_PREFIX_ENV, args.id_prefix.trim());

    run_batch_script(args.json, Some(sandbox)).await
}

async fn run_prepare(args: ModelsBatchPrepareArgs) -> i32 {
    let _guard = DISPATCH_BATCH_LOCK.lock().await;
    let manifest_path = absolutize_path(&args.manifest);
    let out_dir = absolutize_path(&args.out_dir);
    let mut sandbox = RunSandboxOptions::default().with_workspace_root(parent_dir(&out_dir));
    let manifest_root = parent_dir(&manifest_path);
    if manifest_root != parent_dir(&out_dir) {
        sandbox = sandbox.with_read_only_roots(vec![manifest_root]);
    }

    let manifest = manifest_path.to_string_lossy().to_string();
    let out = out_dir.to_string_lossy().to_string();
    let _mode = ScopedEnvVar::set(BATCH_MODE_ENV, "prepare");
    let _manifest = ScopedEnvVar::set(BATCH_MANIFEST_ENV, manifest.trim());
    let _out = ScopedEnvVar::set(BATCH_OUT_DIR_ENV, out.trim());

    run_batch_script(args.json, Some(sandbox)).await
}

async fn run_plan(args: ModelsBatchPlanArgs) -> i32 {
    let _guard = DISPATCH_BATCH_LOCK.lock().await;
    let _mode = ScopedEnvVar::set(BATCH_MODE_ENV, "plan");
    let _provider = ScopedEnvVar::set(
        BATCH_PROVIDER_ENV,
        args.provider.as_deref().map(str::trim).unwrap_or(""),
    );
    let _model = ScopedEnvVar::set(
        BATCH_MODEL_ENV,
        args.model.as_deref().map(str::trim).unwrap_or(""),
    );
    let _workload = ScopedEnvVar::set(BATCH_WORKLOAD_ENV, args.workload.trim());
    let min_discount = args
        .min_discount_percent
        .map(|value| value.to_string())
        .unwrap_or_default();
    let max_turnaround = args
        .max_turnaround_hours
        .map(|value| value.to_string())
        .unwrap_or_default();
    let _min_discount = ScopedEnvVar::set(BATCH_MIN_DISCOUNT_ENV, &min_discount);
    let _max_turnaround = ScopedEnvVar::set(BATCH_MAX_TURNAROUND_ENV, &max_turnaround);

    run_batch_script(args.json, None).await
}

async fn run_batch_script(json: bool, sandbox: Option<RunSandboxOptions>) -> i32 {
    let outcome = match sandbox {
        Some(sandbox) => {
            dispatch::run_embedded_script_with_sandbox(
                "models/batch_plan",
                Vec::new(),
                json,
                sandbox,
            )
            .await
        }
        None => dispatch::run_embedded_script("models/batch_plan", Vec::new(), json).await,
    };
    if !outcome.stderr.is_empty() {
        let _ = std::io::stderr().write_all(outcome.stderr.as_bytes());
    }
    if !outcome.stdout.is_empty() {
        let _ = std::io::stdout().write_all(outcome.stdout.as_bytes());
    }
    outcome.exit_code
}

fn absolutize_path(path: &Path) -> PathBuf {
    if path.is_absolute() {
        return path.to_path_buf();
    }
    std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(path)
}

fn parent_dir(path: &Path) -> PathBuf {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf()
}
