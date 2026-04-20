use crate::cli::OrchestratorInspectArgs;

use super::common::{load_local_runtime, print_json};
use super::inspect_data::collect_orchestrator_inspect_data;

pub(super) async fn run(args: OrchestratorInspectArgs) -> Result<(), String> {
    let mut ctx = load_local_runtime(&args.local).await?;
    let payload = collect_orchestrator_inspect_data(&mut ctx).await?;

    if args.json {
        return print_json(&payload);
    }

    println!("Triggers:");
    if payload.triggers.is_empty() {
        println!("- none");
    } else {
        for trigger in &payload.triggers {
            println!(
                "- {} provider={} kind={} state={} version={}",
                trigger.id,
                trigger.provider,
                trigger.kind,
                trigger.state.as_deref().unwrap_or("-"),
                trigger
                    .version
                    .map(|version| version.to_string())
                    .unwrap_or_else(|| "-".to_string())
            );
        }
    }

    println!();
    println!("Connectors:");
    if !payload.activations.is_empty() {
        for activation in &payload.activations {
            println!(
                "- {} bindings={}",
                activation.provider, activation.binding_count
            );
        }
    } else if !payload.connectors.is_empty() {
        for connector in &payload.connectors {
            println!("- {connector}");
        }
    } else {
        println!("- none");
    }

    if let Some(snapshot) = payload.snapshot.as_ref() {
        println!();
        println!("Snapshot:");
        println!("- status={}", snapshot.status);
        println!("- bind={}", snapshot.bind);
    }

    println!();
    println!("Recent dispatches:");
    if payload.recent_dispatches.is_empty() {
        println!("- none");
        return Ok(());
    }
    for dispatch in payload.recent_dispatches.iter().rev().take(5).rev() {
        println!(
            "- {} trigger={} event={} attempt={} replay_of={}",
            dispatch.kind,
            dispatch.trigger_id.as_deref().unwrap_or("-"),
            dispatch.event_id.as_deref().unwrap_or("-"),
            dispatch
                .attempt
                .map(|attempt| attempt.to_string())
                .as_deref()
                .unwrap_or("-"),
            dispatch.replay_of_event_id.as_deref().unwrap_or("-")
        );
    }

    Ok(())
}
