//! `harn local list` — survey every local provider Harn knows about.

use std::path::Path;

use serde::Serialize;

use crate::cli::LocalListArgs;

use super::runtime::{local_provider_ids, snapshot_provider, LocalProviderSnapshot};
use super::state::local_state_dir;

#[derive(Debug, Serialize)]
struct ListPayload {
    state_dir: String,
    providers: Vec<LocalProviderSnapshot>,
}

pub(crate) async fn run(args: LocalListArgs, base_dir: &Path) -> Result<(), String> {
    let providers = local_provider_ids(args.provider.as_deref());
    if providers.is_empty() {
        return Err(format!(
            "unknown local provider: {}",
            args.provider.as_deref().unwrap_or("(none)"),
        ));
    }
    let state_dir = local_state_dir(base_dir);
    let mut snapshots = Vec::with_capacity(providers.len());
    for provider in providers {
        match snapshot_provider(&provider, base_dir).await {
            Ok(snapshot) => snapshots.push(snapshot),
            Err(error) => eprintln!("warning: failed to snapshot {provider}: {error}"),
        }
    }
    if args.json {
        let payload = ListPayload {
            state_dir: state_dir.display().to_string(),
            providers: snapshots,
        };
        println!(
            "{}",
            serde_json::to_string_pretty(&payload)
                .map_err(|error| format!("failed to render list JSON: {error}"))?
        );
    } else {
        render_table(&snapshots);
    }
    Ok(())
}

fn render_table(snapshots: &[LocalProviderSnapshot]) {
    if snapshots.is_empty() {
        println!("(no local providers known)");
        return;
    }
    println!(
        "{:<10} {:<28} {:<8} {:<14} LOADED",
        "PROVIDER", "URL", "PORT", "STATUS"
    );
    for snapshot in snapshots {
        let port = snapshot
            .port
            .map(|p| p.to_string())
            .unwrap_or_else(|| "-".to_string());
        let status = if snapshot.reachable {
            "up".to_string()
        } else {
            format!("down ({})", snapshot.readiness_status)
        };
        let loaded = if snapshot.loaded_models.is_empty() {
            if snapshot.served_models.is_empty() {
                "-".to_string()
            } else {
                format!("served:{}", snapshot.served_models.len())
            }
        } else {
            snapshot
                .loaded_models
                .iter()
                .map(|model| match model.size_vram_bytes.or(model.size_bytes) {
                    Some(bytes) => format!("{} ({})", model.name, format_size(bytes)),
                    None => model.name.clone(),
                })
                .collect::<Vec<_>>()
                .join(", ")
        };
        println!(
            "{:<10} {:<28} {:<8} {:<14} {}",
            snapshot.provider, snapshot.base_url, port, status, loaded
        );
    }
}

fn format_size(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = KIB * 1024;
    const GIB: u64 = MIB * 1024;
    if bytes >= GIB {
        format!("{:.1} GiB", bytes as f64 / GIB as f64)
    } else if bytes >= MIB {
        format!("{:.0} MiB", bytes as f64 / MIB as f64)
    } else {
        format!("{} B", bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_size_picks_gib_for_multi_gb_models() {
        assert_eq!(format_size(8 * 1024 * 1024 * 1024), "8.0 GiB");
        assert_eq!(format_size(512 * 1024 * 1024), "512 MiB");
        assert_eq!(format_size(2048), "2048 B");
    }
}
