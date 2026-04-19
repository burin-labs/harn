use crate::cli::OrchestratorInspectArgs;

use super::common::{load_local_runtime, read_topic, trigger_list, TRIGGER_OUTBOX_TOPIC};

pub(super) async fn run(args: OrchestratorInspectArgs) -> Result<(), String> {
    let mut ctx = load_local_runtime(&args.local).await?;
    let bindings = trigger_list(&mut ctx).await?;
    let dispatches = read_topic(&ctx.event_log, TRIGGER_OUTBOX_TOPIC).await?;

    println!("Triggers:");
    if bindings.is_empty() {
        println!("- none");
    } else {
        for binding in bindings
            .into_iter()
            .filter(|binding| binding.source == harn_vm::TriggerBindingSource::Manifest)
        {
            println!(
                "- {} provider={} kind={} state={} version={}",
                binding.id,
                binding.provider,
                binding.kind,
                binding.state.as_str(),
                binding.version
            );
        }
    }

    println!();
    println!("Connectors:");
    match ctx.snapshot.as_ref() {
        Some(snapshot) if !snapshot.activations.is_empty() => {
            for activation in &snapshot.activations {
                println!(
                    "- {} bindings={}",
                    activation.provider, activation.binding_count
                );
            }
        }
        Some(snapshot) if !snapshot.connectors.is_empty() => {
            for connector in &snapshot.connectors {
                println!("- {connector}");
            }
        }
        Some(_) | None => println!("- none"),
    }

    if let Some(snapshot) = ctx.snapshot.as_ref() {
        println!();
        println!("Snapshot:");
        println!("- status={}", snapshot.status);
        println!("- bind={}", snapshot.bind);
    }

    println!();
    println!("Recent dispatches:");
    let mut recent: Vec<_> = dispatches
        .into_iter()
        .filter(|(_, event)| {
            matches!(
                event.kind.as_str(),
                "dispatch_succeeded" | "dispatch_failed"
            )
        })
        .collect();
    if recent.is_empty() {
        println!("- none");
        return Ok(());
    }
    let keep_from = recent.len().saturating_sub(5);
    recent.drain(0..keep_from);
    for (_, event) in recent {
        let trigger_id = event
            .headers
            .get("trigger_id")
            .cloned()
            .unwrap_or_else(|| "-".to_string());
        let event_id = event
            .headers
            .get("event_id")
            .cloned()
            .unwrap_or_else(|| "-".to_string());
        let attempt = event
            .headers
            .get("attempt")
            .cloned()
            .unwrap_or_else(|| "-".to_string());
        let replay = event
            .headers
            .get("replay_of_event_id")
            .cloned()
            .unwrap_or_else(|| "-".to_string());
        println!(
            "- {} trigger={} event={} attempt={} replay_of={}",
            event.kind, trigger_id, event_id, attempt, replay
        );
    }

    Ok(())
}
