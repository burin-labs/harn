use std::process::Command;

use super::TargetInfo;
use crate::commands::command_probe::{self, PROBE_TIMEOUT, TARGET_PROBE_TIMEOUT};

/// Target inventory and optional build diagnostics share one bounded probe path.
const CANONICAL_TARGETS: &[&str] = &[
    "x86_64-unknown-linux-gnu",
    "aarch64-unknown-linux-gnu",
    "x86_64-apple-darwin",
    "aarch64-apple-darwin",
    "x86_64-pc-windows-msvc",
    "aarch64-pc-windows-msvc",
    "wasm32-unknown-unknown",
    "wasm32-wasip1",
];

pub(super) async fn collect_targets(check_targets: bool) -> Result<Vec<TargetInfo>, String> {
    let installed = installed_rustup_targets()?;
    let mut triples: std::collections::BTreeSet<String> =
        CANONICAL_TARGETS.iter().map(|t| (*t).to_string()).collect();
    triples.extend(installed.iter().cloned());
    let mut targets = Vec::with_capacity(triples.len());
    for triple in triples {
        let is_installed = installed.contains(&triple);
        let mut reasons = Vec::new();
        let (buildable, checked) = if !check_targets {
            (None, false)
        } else if !is_installed {
            reasons.push(format!(
                "target not installed; run `rustup target add {triple}` to probe"
            ));
            (Some(false), true)
        } else {
            match cargo_check_target(&triple).await {
                Ok(()) => (Some(true), true),
                Err(detail) => {
                    reasons.push(detail);
                    (Some(false), true)
                }
            }
        };
        targets.push(TargetInfo {
            triple,
            installed: is_installed,
            buildable,
            reasons,
            checked,
        });
    }
    Ok(targets)
}

fn installed_rustup_targets() -> Result<Vec<String>, String> {
    let mut command = Command::new("rustup");
    command.args(["target", "list", "--installed"]);
    let out = command_probe::output(command, PROBE_TIMEOUT)
        .map_err(|error| format!("rustup target list could not be measured: {error}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(|line| line.trim().to_string())
            .filter(|line| !line.is_empty())
            .collect())
    } else {
        Err(format!("rustup target list exited {}", out.status))
    }
}

async fn cargo_check_target(triple: &str) -> Result<(), String> {
    let mut command = Command::new("cargo");
    command.args(["check", "--quiet", "--target", triple]);
    let output = command_probe::output_async(command, TARGET_PROBE_TIMEOUT)
        .await
        .map_err(|err| format!("cargo check probe failed: {err}"))?;
    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let summary = stderr
            .lines()
            .rev()
            .find(|line| line.contains("error"))
            .map(|line| line.trim().to_string())
            .unwrap_or_else(|| format!("cargo check --target {triple} failed"));
        Err(summary)
    }
}
