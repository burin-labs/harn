//! `harn local stop` — unload local models and stop Harn-managed servers.
//!
//! For Ollama, "stop" means `keep_alive=0` over `/api/generate`, which
//! matches the semantics of `ollama stop <model>`. Managed-process runtimes
//! stop only the PID Harn stored when it launched the server. Externally
//! managed runtimes are explicitly left to the host.

use std::path::Path;

use harn_vm::llm_config::LocalRuntimeStop;
use serde::Serialize;

use crate::cli::LocalStopArgs;

use super::runtime::{
    local_provider_ids, local_runtime_lifecycle_for_provider, normalize_local_provider_id,
    ollama_unload_model, snapshot_provider, terminate_managed_pid,
};
use super::state::read_selection;

#[derive(Debug, Serialize)]
struct StopResult {
    providers: Vec<ProviderStopOutcome>,
}

#[derive(Debug, Serialize)]
struct ProviderStopOutcome {
    provider: String,
    actions: Vec<StopAction>,
}

#[derive(Debug, Serialize)]
struct StopAction {
    target: String,
    outcome: String,
}

pub(crate) async fn run(args: LocalStopArgs, base_dir: &Path) -> Result<(), String> {
    let targets = resolve_targets(&args, base_dir)?;
    let mut outcomes = Vec::with_capacity(targets.len());
    for provider in targets {
        let mut actions = Vec::new();
        let lifecycle = local_runtime_lifecycle_for_provider(&provider)?;
        match lifecycle.stop {
            LocalRuntimeStop::KeepAliveZero => stop_ollama(&provider, base_dir, &mut actions).await,
            LocalRuntimeStop::Pid => stop_managed_pid(&provider, base_dir, &mut actions),
            LocalRuntimeStop::External => actions.push(StopAction {
                target: "runtime".to_string(),
                outcome: "externally managed; Harn did not stop it".to_string(),
            }),
        }
        outcomes.push(ProviderStopOutcome { provider, actions });
    }

    let payload = StopResult {
        providers: outcomes,
    };
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&payload)
                .map_err(|error| format!("failed to render stop JSON: {error}"))?
        );
    } else if payload.providers.is_empty() {
        println!("(no local providers to stop)");
    } else {
        for outcome in &payload.providers {
            if outcome.actions.is_empty() {
                println!("{}: nothing to stop", outcome.provider);
            } else {
                println!("{}:", outcome.provider);
                for action in &outcome.actions {
                    println!("  - {} -> {}", action.target, action.outcome);
                }
            }
        }
    }
    Ok(())
}

fn resolve_targets(args: &LocalStopArgs, base_dir: &Path) -> Result<Vec<String>, String> {
    if let Some(provider) = args.provider.as_deref() {
        let provider = normalize_local_provider_id(provider);
        if !local_provider_ids(None).contains(&provider) {
            return Err(format!("'{provider}' is not a local provider Harn manages"));
        }
        return Ok(vec![provider]);
    }
    if args.all {
        return Ok(local_provider_ids(None));
    }
    // No explicit target: default to the currently-selected provider, falling
    // back to "every local provider" if the user has never run `switch`.
    match read_selection(base_dir)? {
        Some(selection) => Ok(vec![selection.provider]),
        None => Ok(local_provider_ids(None)),
    }
}

async fn stop_ollama(provider: &str, base_dir: &Path, actions: &mut Vec<StopAction>) {
    let snapshot = match snapshot_provider(provider, base_dir).await {
        Ok(snapshot) => snapshot,
        Err(error) => {
            actions.push(StopAction {
                target: "snapshot".to_string(),
                outcome: format!("error: {error}"),
            });
            return;
        }
    };
    if !snapshot.reachable || snapshot.loaded_models.is_empty() {
        return;
    }
    for loaded in snapshot.loaded_models {
        let outcome = match ollama_unload_model(&snapshot.base_url, &loaded.name).await {
            Ok(()) => "unloaded".to_string(),
            Err(error) => format!("error: {error}"),
        };
        actions.push(StopAction {
            target: loaded.name,
            outcome,
        });
    }
}

fn stop_managed_pid(provider: &str, base_dir: &Path, actions: &mut Vec<StopAction>) {
    let stopped = match terminate_managed_pid(provider, base_dir) {
        Ok(Some(stopped)) => stopped,
        Ok(None) => return,
        Err(error) => {
            actions.push(StopAction {
                target: "pid".to_string(),
                outcome: format!("error: {error}"),
            });
            return;
        }
    };
    let (target, outcome) = stopped.into_action();
    actions.push(StopAction { target, outcome });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::local::state::{read_pid_record, write_pid_record, PidRecord};

    #[test]
    fn failed_managed_stop_preserves_pid_record_for_retry() {
        let dir = tempfile::tempdir().expect("state directory");
        let record = PidRecord {
            provider: "llamacpp".to_string(),
            pid: u32::MAX,
            model: "test-model".to_string(),
            base_url: "http://127.0.0.1:8001".to_string(),
            command: "llama-server".to_string(),
            args: Vec::new(),
            started_at: "2026-08-07T00:00:00Z".to_string(),
        };
        write_pid_record(dir.path(), &record).expect("write PID record");

        let mut actions = Vec::new();
        stop_managed_pid(&record.provider, dir.path(), &mut actions);

        assert!(
            actions
                .iter()
                .any(|action| action.outcome.starts_with("error:")),
            "the invalid PID must exercise the failed-termination path"
        );
        assert_eq!(
            read_pid_record(dir.path(), &record.provider).expect("read PID record after failure"),
            Some(record),
            "a failed termination must retain ownership state for a retry"
        );
    }
}
