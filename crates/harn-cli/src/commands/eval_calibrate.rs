//! `harn eval calibrate` — measure a classifier's confidence against labels.
//!
//! The arithmetic lives in `std/eval/calibration` so the stdlib, a pipeline,
//! and this command all read the same numbers from one implementation. This
//! file is the argument shim.

use std::io::Write as _;

use crate::cli::EvalCalibrateArgs;
use crate::dispatch;
use crate::env_guard::ScopedEnvVar;

pub async fn run(args: EvalCalibrateArgs) -> i32 {
    let corpus = args.corpus.to_string_lossy().to_string();
    let answers = args.answers.to_string_lossy().to_string();
    let thresholds = args
        .thresholds
        .iter()
        .map(|value| value.to_string())
        .collect::<Vec<_>>()
        .join(",");

    let _corpus = ScopedEnvVar::set("HARN_EVAL_CALIBRATE_CORPUS", &corpus);
    let _answers = ScopedEnvVar::set("HARN_EVAL_CALIBRATE_ANSWERS", &answers);
    let _thresholds = ScopedEnvVar::set("HARN_EVAL_CALIBRATE_THRESHOLDS", &thresholds);
    let _target_error = ScopedEnvVar::set(
        "HARN_EVAL_CALIBRATE_TARGET_ERROR",
        &args.target_error.to_string(),
    );
    let _model_revision = ScopedEnvVar::set(
        "HARN_EVAL_CALIBRATE_MODEL_REVISION",
        args.model_revision.as_deref().unwrap_or_default(),
    );
    let _served_model_id = ScopedEnvVar::set(
        "HARN_EVAL_CALIBRATE_SERVED_MODEL_ID",
        args.served_model_id.as_deref().unwrap_or_default(),
    );
    let _json = ScopedEnvVar::set(
        "HARN_EVAL_CALIBRATE_JSON",
        if args.json { "1" } else { "0" },
    );

    let outcome = dispatch::run_embedded_script("eval/calibrate", Vec::new(), args.json).await;
    if !outcome.stdout.is_empty() {
        let _ = std::io::stdout().write_all(outcome.stdout.as_bytes());
    }
    if !outcome.stderr.is_empty() {
        let _ = std::io::stderr().write_all(outcome.stderr.as_bytes());
    }
    outcome.exit_code
}
