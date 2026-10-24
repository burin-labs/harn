//! Host-owned CLI projection of the shared durable provider allowance.
use std::future::Future;
use std::path::Path;

use harn_vm::llm::MachineSpendQuota;

pub(crate) async fn run<F: Future<Output = ()>>(
    path: Option<&Path>,
    command: F,
) -> Result<(), String> {
    let Some(path) = path else {
        command.await;
        return Ok(());
    };
    let quota = MachineSpendQuota::from_policy_file(path).map_err(|error| error.to_string())?;
    quota
        .scope(command)
        .await
        .map_err(|error| error.to_string())
}
