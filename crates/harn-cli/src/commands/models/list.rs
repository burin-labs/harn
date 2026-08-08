//! `harn models list` — query the authoritative configured-model catalog.
//!
//! ## Harn renderer
//!
//! The filter + render pipeline lives in
//! `crates/harn-stdlib/src/stdlib/cli/models/list.harn`. The catalog
//! itself comes from the read-only `harness.llm.catalog()` handle, so the
//! script owns the entire data-shape transformation end-to-end. The Rust
//! shim's only contribution is detecting installed Ollama models
//! out-of-process (sandboxed scripts can't run `ollama list`) and collecting
//! parsed command input into one typed JSON payload.
//!

use std::collections::BTreeSet;
use std::io::Write as _;

use serde::Serialize;

use crate::cli::{ModelsListArgs, ModelsListSort};
use crate::dispatch;
use crate::env_guard::ScopedEnvVar;

/// Env var carrying the complete parsed list query. Keeping this as one JSON
/// object prevents independent mutable env knobs from drifting as the Harn
/// query surface grows.
const LIST_QUERY_ENV: &str = "HARN_MODELS_LIST_QUERY_JSON";

/// Serialises the dispatch path so concurrent in-process callers don't
/// race on the global env vars the shim sets.
static DISPATCH_LIST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[derive(Debug, Serialize)]
struct ListDispatchPayload<'a> {
    provider: Option<&'a str>,
    #[serde(rename = "where")]
    where_filters: &'a [String],
    sort: Option<&'static str>,
    columns: Option<&'a str>,
    installed_only: bool,
    installed_ollama: &'a [String],
}

pub(crate) async fn run(args: ModelsListArgs) {
    let exit_code = run_dispatch(args).await;
    if exit_code != 0 {
        std::process::exit(exit_code);
    }
}

async fn run_dispatch(args: ModelsListArgs) -> i32 {
    let installed = detect_installed_ollama_models().await;
    let installed_vec: Vec<String> = installed.iter().cloned().collect();
    let payload = ListDispatchPayload {
        provider: args.provider.as_deref(),
        where_filters: &args.where_filters,
        sort: args.sort.map(ModelsListSort::catalog_field),
        columns: args.columns.as_deref(),
        installed_only: args.installed_only,
        installed_ollama: &installed_vec,
    };
    let payload_json = match serde_json::to_string(&payload) {
        Ok(json) => json,
        Err(error) => {
            eprintln!("error: failed to serialise models list query: {error}");
            return 1;
        }
    };

    let _guard = DISPATCH_LIST_LOCK.lock().await;
    let _payload = ScopedEnvVar::set(LIST_QUERY_ENV, &payload_json);

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
