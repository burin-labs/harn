//! `harn local status` — show the active local model plus a brief summary
//! of every other local runtime.

use std::path::Path;

use serde::Serialize;

use crate::cli::LocalStatusArgs;
use crate::commands::hardware::{bytes_to_gib_rounded, collect_hardware_snapshot};

use super::profile::defaults_for;
use super::runtime::{local_provider_ids, snapshot_provider, LocalProviderSnapshot};
use super::state::{read_selection, LocalSelection};

#[derive(Debug, Serialize)]
struct StatusPayload {
    selection: Option<LocalSelection>,
    profile: ProfileSummary,
    providers: Vec<LocalProviderSnapshot>,
}

#[derive(Debug, Serialize)]
struct ProfileSummary {
    bucket: String,
    default_ctx: u64,
    default_keep_alive: String,
    total_ram_gib: Option<u64>,
    gpu: String,
}

pub(crate) async fn run(args: LocalStatusArgs, base_dir: &Path) -> Result<(), String> {
    let selection = read_selection(base_dir)?;
    let hardware = collect_hardware_snapshot();
    let defaults = defaults_for(&hardware);
    let profile = ProfileSummary {
        bucket: defaults.bucket.as_str().to_string(),
        default_ctx: defaults.ctx,
        default_keep_alive: defaults.keep_alive.to_string(),
        total_ram_gib: hardware.ram.total_bytes.map(bytes_to_gib_rounded),
        gpu: format!("{:?}", hardware.gpu.kind).to_lowercase(),
    };

    let mut snapshots = Vec::new();
    for provider in local_provider_ids(None) {
        match snapshot_provider(&provider, base_dir).await {
            Ok(snapshot) => snapshots.push(snapshot),
            Err(error) => eprintln!("warning: failed to snapshot {provider}: {error}"),
        }
    }

    if args.json {
        let payload = StatusPayload {
            selection: selection.clone(),
            profile,
            providers: snapshots,
        };
        println!(
            "{}",
            serde_json::to_string_pretty(&payload)
                .map_err(|error| format!("failed to render status JSON: {error}"))?
        );
        return Ok(());
    }

    if let Some(selection) = selection.as_ref() {
        println!(
            "Active: {provider} :: {model}{alias}",
            provider = selection.provider,
            model = selection.model,
            alias = selection
                .alias
                .as_deref()
                .map(|alias| format!(" (alias {alias})"))
                .unwrap_or_default(),
        );
        println!("URL:    {}", selection.base_url);
        if let Some(ctx) = selection.ctx {
            println!("Ctx:    {ctx}");
        }
        if let Some(keep_alive) = selection.keep_alive.as_deref() {
            println!("Keep:   {keep_alive}");
        }
        println!("Since:  {}", selection.switched_at);
    } else {
        println!("Active: (no selection — run `harn local switch <alias>`)");
    }
    println!(
        "Profile: {} (default ctx {}, keep-alive {})",
        profile.bucket, profile.default_ctx, profile.default_keep_alive
    );
    println!();

    for snapshot in &snapshots {
        let badge = if snapshot.reachable { "up" } else { "down" };
        let loaded = if snapshot.loaded_models.is_empty() {
            String::new()
        } else {
            format!(
                "  loaded: {}",
                snapshot
                    .loaded_models
                    .iter()
                    .map(|model| model.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        println!(
            "  {:<10} [{}] {}{}",
            snapshot.provider, badge, snapshot.base_url, loaded
        );
    }
    Ok(())
}
