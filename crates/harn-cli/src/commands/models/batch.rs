//! `harn models batch` — plan provider Batch API use.
//!
//! The catalog filtering and workload guidance live in
//! `crates/harn-stdlib/src/stdlib/cli/models/batch_plan.harn`. This
//! shim only forwards parsed clap flags through env vars, because the
//! script already has the read-only `harness.llm.catalog()` capability
//! it needs.

use std::io::Write as _;

use crate::cli::{ModelsBatchArgs, ModelsBatchCommand, ModelsBatchPlanArgs};
use crate::dispatch;
use crate::env_guard::ScopedEnvVar;

const BATCH_PROVIDER_ENV: &str = "HARN_MODELS_BATCH_PROVIDER";
const BATCH_MODEL_ENV: &str = "HARN_MODELS_BATCH_MODEL";
const BATCH_WORKLOAD_ENV: &str = "HARN_MODELS_BATCH_WORKLOAD";
const BATCH_MIN_DISCOUNT_ENV: &str = "HARN_MODELS_BATCH_MIN_DISCOUNT_PERCENT";
const BATCH_MAX_TURNAROUND_ENV: &str = "HARN_MODELS_BATCH_MAX_TURNAROUND_HOURS";

static DISPATCH_BATCH_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

pub(crate) async fn run(args: ModelsBatchArgs) {
    let exit_code = match args.command {
        ModelsBatchCommand::Plan(args) => run_plan(args).await,
    };
    if exit_code != 0 {
        std::process::exit(exit_code);
    }
}

async fn run_plan(args: ModelsBatchPlanArgs) -> i32 {
    let _guard = DISPATCH_BATCH_LOCK.lock().await;
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

    let outcome = dispatch::run_embedded_script("models/batch_plan", Vec::new(), args.json).await;
    if !outcome.stderr.is_empty() {
        let _ = std::io::stderr().write_all(outcome.stderr.as_bytes());
    }
    if !outcome.stdout.is_empty() {
        let _ = std::io::stdout().write_all(outcome.stdout.as_bytes());
    }
    outcome.exit_code
}
