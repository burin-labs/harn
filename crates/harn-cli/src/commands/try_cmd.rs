//! `harn try "<prompt>"` — dispatches to the embedded
//! `cli/try.harn` script (see harn#2302 / W2). The shim is
//! intentionally tiny because all the agent_loop wiring lives in
//! the .harn impl now.
//!
//! `HARN_CLI_IMPL=rust` is reserved for the parity-snapshot harness
//! to keep both impls comparable until the harn impl is the default
//! everywhere (see C1 ratchet, #2314).

use harn_vm::llm_config;

use crate::cli::TryArgs;
use crate::dispatch;
use crate::env_guard::ScopedEnvVar;

pub(crate) async fn run(args: TryArgs) {
    if !mock_provider_active() && llm_config::available_provider_names().is_empty() {
        eprintln!("{}", crate::commands::doctor::no_credentials_hint());
        std::process::exit(1);
    }

    if std::env::var("HARN_CLI_IMPL").as_deref() == Ok("rust") {
        run_via_legacy_synth(args).await;
        return;
    }

    let _max = ScopedEnvVar::set("HARN_TRY_MAX_ITERS", &args.max_iterations.to_string());
    let exit =
        dispatch::dispatch_to_embedded_script("try", vec![args.prompt], /* json_mode */ false)
            .await;
    if exit != 0 {
        std::process::exit(exit);
    }
}

fn mock_provider_active() -> bool {
    std::env::var("HARN_LLM_PROVIDER")
        .map(|v| v == "mock")
        .unwrap_or(false)
}

/// Legacy synth-into-tempfile-then-`execute_run` path, retained so the
/// parity-snapshot harness can pin the new dispatch against it. The
/// C1 ratchet (#2314) removes this once the .harn impl is the
/// production default everywhere.
async fn run_via_legacy_synth(args: TryArgs) {
    use std::collections::HashSet;
    use std::fs;

    use crate::commands::run::{execute_run, CliLlmMockMode, RunOutcome, RunProfileOptions};

    let escaped = escape_for_harn_string(&args.prompt);
    let max_iters = args.max_iterations;
    let script = format!(
        "let result = agent_loop(\"{escaped}\", nil, {{\n    max_iterations: {max_iters},\n    llm_retries: 2\n}})\n__io_println(result.text)\n"
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
