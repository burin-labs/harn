//! `harn guard` — manage downloadable on-device injection-detection models.
//!
//! Network I/O (the actual download from the catalog's upstream URLs) lives
//! here; the pure catalog/store/verify logic lives in the `harn-guard` crate.
//! Nothing is hosted by Harn — `install` fetches from already-hosted upstream
//! repositories on the user's machine, at the user's request.

use std::path::PathBuf;

use harn_guard::{catalog, GuardStore};

use crate::cli::{GuardInstallArgs, GuardListArgs, GuardRemoveArgs, GuardStatusArgs};

fn store() -> GuardStore {
    let home = std::env::var("HOME")
        .ok()
        .filter(|home| !home.trim().is_empty())
        .map_or_else(|| PathBuf::from("."), PathBuf::from);
    GuardStore::new(&home)
}

fn short_sha(sha: &str) -> String {
    sha.chars().take(12).collect()
}

/// Download one file from `url` (optionally HF-authenticated) into memory.
/// Returns a human-readable error string; the caller maps it to a CLI error.
async fn fetch_file(
    client: &reqwest::Client,
    url: &str,
    token: Option<&str>,
    dest: &str,
) -> Result<Vec<u8>, String> {
    let mut request = client.get(url);
    if let Some(token) = token {
        request = request.bearer_auth(token);
    }
    let response = request
        .send()
        .await
        .map_err(|error| format!("download failed for {dest}: {error}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "download failed for {dest} (HTTP {})",
            response.status()
        ));
    }
    response
        .bytes()
        .await
        .map(|bytes| bytes.to_vec())
        .map_err(|error| format!("read failed for {dest}: {error}"))
}

pub(crate) fn run_list(args: &GuardListArgs) {
    let store = store();
    let installed = store.installed();
    println!("Installed models ({}):", installed.len());
    if installed.is_empty() {
        println!("  (none) — run `harn guard install` to add the recommended model");
    }
    for manifest in &installed {
        println!(
            "  {:32} {}  [{}]",
            manifest.name, manifest.display_name, manifest.license_id
        );
    }

    if args.catalog {
        println!("\nAvailable to install:");
        for model in catalog::all() {
            let gated = if model.gated {
                "  (gated — needs HF_TOKEN)"
            } else {
                ""
            };
            let default = if model.name == catalog::DEFAULT_MODEL {
                "  *default"
            } else {
                ""
            };
            println!(
                "  {:32} {}  [{}]{}{}",
                model.name, model.display_name, model.license_id, gated, default
            );
            println!("      {}", model.description);
        }
    }
}

pub(crate) fn run_status(args: &GuardStatusArgs) {
    let store = store();
    let names: Vec<String> = match &args.model {
        Some(name) => vec![name.clone()],
        None => store.installed().into_iter().map(|m| m.name).collect(),
    };
    if names.is_empty() {
        println!("No models installed.");
        return;
    }
    for name in names {
        match store.read_manifest(&name) {
            Ok(manifest) => {
                let integrity = match store.verify_installed(&name) {
                    Ok(true) => "ok",
                    Ok(false) => "FAILED — re-install",
                    Err(_) => "unknown",
                };
                println!(
                    "{} — {} [{}]",
                    manifest.name, manifest.display_name, manifest.license_id
                );
                println!("  integrity: {integrity}");
                for file in &manifest.files {
                    println!(
                        "  {:20} {:>12} bytes  sha256:{}",
                        file.name,
                        file.size,
                        short_sha(&file.sha256)
                    );
                }
            }
            Err(_) => println!("{name} — not installed"),
        }
    }
}

pub(crate) fn run_remove(args: &GuardRemoveArgs) {
    let store = store();
    match store.remove(&args.model) {
        Ok(true) => println!("Removed `{}`.", args.model),
        Ok(false) => println!("`{}` is not installed.", args.model),
        Err(error) => crate::command_error(&format!("failed to remove `{}`: {error}", args.model)),
    }
}

pub(crate) async fn run_install(args: &GuardInstallArgs) {
    let store = store();
    let name = args
        .model
        .clone()
        .unwrap_or_else(|| catalog::DEFAULT_MODEL.to_owned());
    let Some(model) = catalog::find(&name) else {
        crate::command_error(&format!(
            "unknown model `{name}` — run `harn guard list --catalog` to see available models"
        ));
    };

    // Explicit, reviewable license acceptance — nothing downloads without it.
    if !args.accept_license {
        println!(
            "`{}` is licensed under {} — review the terms at {}",
            model.name, model.license_id, model.license_url
        );
        println!("Re-run with --accept-license to download and install.");
        return;
    }

    if store.is_installed(model.name) && !args.force {
        println!(
            "`{}` is already installed (use --force to re-download).",
            model.name
        );
        return;
    }

    // Gated upstreams (e.g. Meta's license-gated Hugging Face repos) require the
    // user's own token + their acceptance on the upstream — never ours.
    let token = std::env::var("HF_TOKEN")
        .ok()
        .filter(|token| !token.trim().is_empty());
    if model.gated && token.is_none() {
        crate::command_error(&format!(
            "`{}` is gated: accept the license at {} and set HF_TOKEN, then re-run",
            model.name, model.license_url
        ));
    }

    if let Some(total) = model.total_size() {
        println!(
            "Downloading `{}` (~{} MB) from {}",
            model.name,
            total / 1_000_000,
            model.repo
        );
    }

    let client = reqwest::Client::new();
    let mut payload: Vec<(String, Vec<u8>)> = Vec::with_capacity(model.files.len());
    for file in model.files {
        println!("  fetching {}", file.dest);
        let url = model.url_for(file);
        let bytes = match fetch_file(&client, &url, token.as_deref(), file.dest).await {
            Ok(bytes) => bytes,
            Err(error) => crate::command_error(&error),
        };
        payload.push((file.dest.to_owned(), bytes));
    }

    match store.install(model, &payload, true) {
        Ok(manifest) => println!(
            "Installed `{}` ({} files) to {}",
            manifest.name,
            manifest.files.len(),
            store.model_dir(&manifest.name).display()
        ),
        Err(error) => crate::command_error(&format!("install failed: {error}")),
    }
}
