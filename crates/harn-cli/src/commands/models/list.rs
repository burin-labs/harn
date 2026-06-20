//! `harn models list` — list configured models grouped by provider.
//!
//! ## Harn renderer
//!
//! The filter + render pipeline lives in
//! `crates/harn-stdlib/src/stdlib/cli/models/list.harn`. The catalog
//! itself comes from the read-only `harness.llm.catalog()` handle, so the
//! script owns the entire data-shape transformation end-to-end. The Rust
//! shim's only contribution is detecting installed Ollama models
//! out-of-process (sandboxed scripts can't run `ollama list`) and threading
//! the parsed `--provider` /
//! `--installed-only` flags through env vars.
//!

use std::collections::BTreeSet;
use std::io::Write as _;

use crate::cli::ModelsListArgs;
use crate::dispatch;
use crate::env_guard::ScopedEnvVar;

/// Env var the embedded `cli/models/list` script reads to pick up the
/// `--provider` filter. Empty when no filter was passed.
const LIST_PROVIDER_ENV: &str = "HARN_MODELS_LIST_PROVIDER";

/// Env var for `--installed-only`. `"1"` iff the flag was set.
const LIST_INSTALLED_ONLY_ENV: &str = "HARN_MODELS_LIST_INSTALLED_ONLY";

/// Env var carrying the JSON list of installed Ollama model ids. The
/// Rust shim resolves these via `ollama list` (which scripts can't run
/// because the sandbox would block the subprocess) and hands them
/// across as a single payload so the script doesn't have to.
const LIST_INSTALLED_OLLAMA_ENV: &str = "HARN_MODELS_INSTALLED_OLLAMA";

/// Serialises the dispatch path so concurrent in-process callers don't
/// race on the global env vars the shim sets.
static DISPATCH_LIST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

pub(crate) async fn run(args: ModelsListArgs) {
    let exit_code = run_dispatch(args).await;
    if exit_code != 0 {
        std::process::exit(exit_code);
    }
}

async fn run_dispatch(args: ModelsListArgs) -> i32 {
    let installed = detect_installed_ollama_models().await;
    let installed_vec: Vec<String> = installed.iter().cloned().collect();
    let installed_json = match serde_json::to_string(&installed_vec) {
        Ok(json) => json,
        Err(error) => {
            eprintln!("error: failed to serialise installed ollama set: {error}");
            return 1;
        }
    };

    let _guard = DISPATCH_LIST_LOCK.lock().await;
    let _installed = ScopedEnvVar::set(LIST_INSTALLED_OLLAMA_ENV, &installed_json);
    let _installed_only = ScopedEnvVar::set(
        LIST_INSTALLED_ONLY_ENV,
        if args.installed_only { "1" } else { "0" },
    );
    let _provider = ScopedEnvVar::set(LIST_PROVIDER_ENV, args.provider.as_deref().unwrap_or(""));

    let outcome = dispatch::run_embedded_script("models/list", Vec::new(), args.json).await;
    if !outcome.stderr.is_empty() {
        let _ = std::io::stderr().write_all(outcome.stderr.as_bytes());
    }
    if !outcome.stdout.is_empty() {
        let _ = std::io::stdout().write_all(outcome.stdout.as_bytes());
    }
    outcome.exit_code
}

async fn detect_installed_ollama_models() -> BTreeSet<String> {
    let mut set = BTreeSet::new();
    if which::which("ollama").is_err() {
        return set;
    }
    let Ok(output) = tokio::process::Command::new("ollama")
        .arg("list")
        .output()
        .await
    else {
        return set;
    };
    if !output.status.success() {
        return set;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    for line in text.lines().skip(1) {
        if let Some(name) = line.split_whitespace().next() {
            set.insert(name.to_string());
        }
    }
    set
}
