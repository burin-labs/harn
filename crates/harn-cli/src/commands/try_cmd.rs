//! `harn try "<prompt>"` — minimal agent_loop convenience wrapper.
//!
//! Internally synthesizes a one-line Harn script and routes through the
//! existing run pipeline. Uses the `llm_retries` agent_loop option
//! directly because the synthesized script is meant to stay readable as
//! a single line; users who want composable middleware should write a
//! script with `with_retry(default_llm_caller(), {...})` from
//! `std/llm/handlers` instead.

use std::collections::HashSet;
use std::fs;

use harn_vm::llm_config;

use crate::cli::TryArgs;
use crate::commands::run::{execute_run, CliLlmMockMode, RunOutcome, RunProfileOptions};

pub(crate) async fn run(args: TryArgs) {
    let mock_active = std::env::var("HARN_LLM_PROVIDER")
        .map(|v| v == "mock")
        .unwrap_or(false);
    if !mock_active && llm_config::available_provider_names().is_empty() {
        eprintln!("{}", crate::commands::doctor::no_credentials_hint());
        std::process::exit(1);
    }

    let escaped = escape_for_harn_string(&args.prompt);
    let max_iters = args.max_iterations;
    // No `tools` option: the registered-tool contract requires schemas,
    // not bare identifiers, and `harn try` is meant to be a zero-config
    // smoke test. Users can drop into `harn run` for tool-augmented loops.
    let script = format!(
        "let result = agent_loop(\"{escaped}\", nil, {{\n    max_iterations: {max_iters},\n    llm_retries: 2\n}})\nprintln(result.text)\n"
    );

    let tmp = match tempfile::Builder::new()
        .prefix("harn-try-")
        .suffix(".harn")
        .tempfile()
    {
        Ok(t) => t,
        Err(error) => {
            eprintln!("failed to create temp file: {error}");
            std::process::exit(1);
        }
    };
    let path = tmp.path().to_path_buf();
    let wrapped = format!("pipeline main(task) {{\n{script}}}\n");
    if let Err(error) = fs::write(&path, &wrapped) {
        eprintln!("failed to write temp file: {error}");
        std::process::exit(1);
    }

    let outcome: RunOutcome = execute_run(
        &path.to_string_lossy(),
        false,
        HashSet::new(),
        Vec::new(),
        Vec::new(),
        CliLlmMockMode::Off,
        None,
        RunProfileOptions::default(),
    )
    .await;

    if !outcome.stderr.is_empty() {
        eprint!("{}", outcome.stderr);
    }
    if !outcome.stdout.is_empty() {
        print!("{}", outcome.stdout);
    }

    drop(tmp);
    if outcome.exit_code != 0 {
        std::process::exit(outcome.exit_code);
    }
}

fn escape_for_harn_string(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}
