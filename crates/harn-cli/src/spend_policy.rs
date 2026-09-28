//! Host-owned CLI projection of the shared durable provider allowance.
use std::future::Future;
use std::path::{Path, PathBuf};

use harn_vm::llm::{MachineSpendPolicy, MachineSpendQuota};

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct PolicyFile {
    ledger_path: PathBuf,
    scope: String,
    limits: MachineSpendPolicy,
}

pub(crate) async fn run<F: Future<Output = ()>>(
    path: Option<&Path>,
    command: F,
) -> Result<(), String> {
    let Some(path) = path else {
        command.await;
        return Ok(());
    };
    let source = std::fs::read_to_string(path)
        .map_err(|error| format!("cannot read spend policy {}: {error}", path.display()))?;
    let policy: PolicyFile = toml::from_str(&source)
        .map_err(|error| format!("invalid spend policy {}: {error}", path.display()))?;
    let quota = MachineSpendQuota::open(policy.ledger_path, policy.scope, policy.limits)
        .map_err(|error| error.to_string())?;
    quota
        .scope(command)
        .await
        .map_err(|error| error.to_string())
}
