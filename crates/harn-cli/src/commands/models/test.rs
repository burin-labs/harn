//! `harn models test` — round-trip a small prompt through a model.
//!
//! ## Harn renderer
//!
//! **The actual smoke test stays in Rust.** `run_model_smoke_test`
//! reaches into `vm_call_llm_full_streaming` with bespoke
//! `LlmCallOptions`, probes provider readiness, drives a streaming
//! callback for first-token-ms, and computes pricing — none of that is
//! reachable from script-land today without exposing a much wider VM
//! surface today. The shim runs the test and
//! captures either a success result or an error.
//!
//! The rendering layer delegates to
//! `crates/harn-stdlib/src/stdlib/cli/models/test.harn`, which owns
//! both the human-readable line and the JSON envelope (success +
//! failure shapes). That's the surface a user actually reads or parses,
//! so it stays in Harn.

use std::io::Write as _;
use std::process;

use serde::Serialize;

use crate::cli::ModelsTestArgs;
use crate::dispatch;
use crate::env_guard::ScopedEnvVar;

/// Env var carrying the smoke-test outcome (success result or error)
/// handed to the embedded `cli/models/test` script. The script picks
/// the rendering format based on `HARN_OUTPUT_JSON`.
const TEST_RESULT_ENV: &str = "HARN_MODELS_TEST_RESULT_JSON";

/// Serialises the dispatch path so concurrent in-process callers don't
/// race on the global env var.
static DISPATCH_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Envelope the Rust shim hands to the .harn script. Mirrors the
/// success / failure split rendered by the embedded script.
#[derive(Debug, Serialize)]
struct TestEnvelope<'a> {
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<&'a harn_vm::llm::ModelSmokeTestResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<&'a str>,
}

pub(crate) async fn run(args: &ModelsTestArgs) {
    let exit_code = run_dispatch(args).await;
    if exit_code != 0 {
        process::exit(exit_code);
    }
}

async fn run_dispatch(args: &ModelsTestArgs) -> i32 {
    let result = harn_vm::llm::run_model_smoke_test(harn_vm::llm::ModelSmokeTestOptions {
        model: args.model.clone(),
        provider: args.provider.clone(),
        prompt: args.prompt.clone(),
    })
    .await;

    let envelope = match &result {
        Ok(value) => TestEnvelope {
            ok: true,
            result: Some(value),
            error: None,
        },
        Err(error) => TestEnvelope {
            ok: false,
            result: None,
            error: Some(error.as_str()),
        },
    };
    let envelope_json = match serde_json::to_string(&envelope) {
        Ok(json) => json,
        Err(error) => {
            eprintln!("error: failed to serialise models-test envelope: {error}");
            return 1;
        }
    };

    let _guard = DISPATCH_TEST_LOCK.lock().await;
    let _payload = ScopedEnvVar::set(TEST_RESULT_ENV, &envelope_json);
    let outcome = dispatch::run_embedded_script("models/test", Vec::new(), args.json).await;
    if !outcome.stderr.is_empty() {
        let _ = std::io::stderr().write_all(outcome.stderr.as_bytes());
    }
    if !outcome.stdout.is_empty() {
        let _ = std::io::stdout().write_all(outcome.stdout.as_bytes());
    }
    outcome.exit_code
}
