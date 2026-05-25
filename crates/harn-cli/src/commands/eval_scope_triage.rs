//! `harn eval scope_triage` — deterministic scope-triage measurement harness.

use std::io::Write as _;

use crate::cli::EvalScopeTriageArgs;
use crate::dispatch;
use crate::env_guard::ScopedEnvVar;

pub async fn run(args: EvalScopeTriageArgs) -> i32 {
    let dataset = args.dataset.to_string_lossy().to_string();
    let output = args
        .output
        .as_ref()
        .map(|path| path.to_string_lossy().to_string())
        .unwrap_or_default();
    let max_cases = args.max_cases.map(|n| n.to_string()).unwrap_or_default();
    let threshold = args.confidence_threshold.to_string();

    let _dataset = ScopedEnvVar::set("HARN_EVAL_SCOPE_TRIAGE_DATASET", &dataset);
    let _output = ScopedEnvVar::set("HARN_EVAL_SCOPE_TRIAGE_OUTPUT", &output);
    let _json = ScopedEnvVar::set(
        "HARN_EVAL_SCOPE_TRIAGE_JSON",
        if args.json { "1" } else { "0" },
    );
    let _live = ScopedEnvVar::set(
        "HARN_EVAL_SCOPE_TRIAGE_LIVE",
        if args.live { "1" } else { "0" },
    );
    let _model = ScopedEnvVar::set("HARN_EVAL_SCOPE_TRIAGE_MODEL", &args.model);
    let _threshold = ScopedEnvVar::set("HARN_EVAL_SCOPE_TRIAGE_THRESHOLD", &threshold);
    let _max_cases = ScopedEnvVar::set("HARN_EVAL_SCOPE_TRIAGE_MAX_CASES", &max_cases);

    let outcome = dispatch::run_embedded_script("eval/scope_triage", Vec::new(), false).await;
    if !outcome.stdout.is_empty() {
        let _ = std::io::stdout().write_all(outcome.stdout.as_bytes());
    }
    if !outcome.stderr.is_empty() {
        let _ = std::io::stderr().write_all(outcome.stderr.as_bytes());
    }
    outcome.exit_code
}
