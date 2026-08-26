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

use std::process;

use crate::cli::ModelsTestArgs;

mod rendering;

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

    rendering::render(&result, args.json).await
}
