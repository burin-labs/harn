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
                "- {} provider={} kind={} state={} version={} metrics=received:{} dispatched:{} failed:{} in_flight:{}",
                trigger.id,
                trigger.provider,
                trigger.kind,
                trigger.state.as_deref().unwrap_or("-"),
                trigger
                    .version
                    .map(|version| version.to_string())
                    .unwrap_or_else(|| "-".to_string()),
                trigger.metrics.received,
                trigger.metrics.dispatched,
                trigger.metrics.failed,
                trigger.metrics.in_flight
            );
        }
    }

    if payload.budget.daily_limit_usd.is_some() || payload.budget.hourly_limit_usd.is_some() {
        println!();
        println!("Budget:");
        if let Some(limit) = payload.budget.daily_limit_usd {
            println!(
                "- daily used=${:.6} remaining=${:.6} limit=${:.6}",
                payload.budget.used_today_usd,
                payload.budget.remaining_today_usd.unwrap_or_default(),
                limit
            );
        }
        if let Some(limit) = payload.budget.hourly_limit_usd {
            println!(
                "- hourly used=${:.6} remaining=${:.6} limit=${:.6}",
                payload.budget.used_hour_usd,
                payload.budget.remaining_hour_usd.unwrap_or_default(),
                limit
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
