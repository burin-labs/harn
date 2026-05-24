//! `harn models list` — list configured models grouped by provider.
//!
//! ## .harn dispatch (W9 — see harn#2309)
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
//! `HARN_CLI_IMPL=rust` keeps the legacy direct path for the
//! parity-snapshot harness (#2299) until the C1 ratchet (#2314) lands.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write as _;

use harn_vm::llm_config;
use serde_json::json;

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
/// race on the global env vars the shim sets. Same pattern as the
/// other partial-port commands (see harn#2305 / #2306).
static DISPATCH_LIST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

pub(crate) async fn run(args: ModelsListArgs) {
    if std::env::var("HARN_CLI_IMPL").as_deref() == Ok("rust") {
        run_legacy(args).await;
        return;
    }
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

/// Legacy direct-render path. Kept verbatim for the parity-snapshot
/// harness (#2299) until C1 (#2314) deletes it.
async fn run_legacy(args: ModelsListArgs) {
    let installed_ollama = detect_installed_ollama_models().await;

    // Build provider -> Vec<(id, tags, installed)>.
    let mut by_provider: BTreeMap<String, Vec<(String, Vec<String>, bool)>> = BTreeMap::new();

    for (id, model) in llm_config::model_catalog_entries() {
        let provider = model.provider.clone();
        let installed = provider == "ollama" && installed_ollama.contains(&id);
        if args.installed_only && !installed {
            continue;
        }
        if let Some(filter) = &args.provider {
            if &provider != filter {
                continue;
            }
        }
        by_provider
            .entry(provider)
            .or_default()
            .push((id, model.capabilities, installed));
    }

    // Synthesize installed Ollama models the catalog hasn't listed.
    if args
        .provider
        .as_deref()
        .map(|p| p == "ollama")
        .unwrap_or(true)
    {
        let known: BTreeSet<String> = by_provider
            .get("ollama")
            .into_iter()
            .flat_map(|v| v.iter().map(|(id, _, _)| id.clone()))
            .collect();
        for id in &installed_ollama {
            if !known.contains(id) {
                by_provider.entry("ollama".to_string()).or_default().push((
                    id.clone(),
                    vec!["local".to_string()],
                    true,
                ));
            }
        }
    }

    if args.json {
        let providers: Vec<serde_json::Value> = by_provider
            .iter()
            .map(|(name, models)| {
                let model_list: Vec<serde_json::Value> = models
                    .iter()
                    .map(|(id, tags, installed)| {
                        if name == "ollama" {
                            json!({"id": id, "tags": tags, "installed": installed})
                        } else {
                            json!({"id": id, "tags": tags})
                        }
                    })
                    .collect();
                json!({"name": name, "models": model_list})
            })
            .collect();
        let payload = json!({"providers": providers});
        match serde_json::to_string_pretty(&payload) {
            Ok(s) => println!("{s}"),
            Err(e) => eprintln!("failed to render JSON: {e}"),
        }
        return;
    }

    if by_provider.is_empty() {
        println!("(no models match)");
        return;
    }
    for (provider, mut models) in by_provider {
        println!("{provider}");
        models.sort_by(|a, b| a.0.cmp(&b.0));
        for (id, tags, installed) in models {
            let suffix = if installed { " [installed]" } else { "" };
            let tag_text = if tags.is_empty() {
                String::new()
            } else {
                format!("  ({})", tags.join(", "))
            };
            println!("  {id}{suffix}{tag_text}");
        }
        println!();
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    #[test]
    fn empty_payload_serializes() {
        let payload = json!({"providers": []});
        let parsed: Value = serde_json::from_str(&payload.to_string()).unwrap();
        assert_eq!(parsed["providers"].as_array().unwrap().len(), 0);
    }
}
