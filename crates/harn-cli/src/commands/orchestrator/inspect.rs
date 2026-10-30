use super::errors::OrchestratorError;
use crate::cli::OrchestratorInspectArgs;

use super::common::{load_local_runtime, print_json};
use super::inspect_data::collect_orchestrator_inspect_data;

pub(super) async fn run(args: OrchestratorInspectArgs) -> Result<(), OrchestratorError> {
    let mut ctx = load_local_runtime(&args.local).await?;
    let payload = collect_orchestrator_inspect_data(&mut ctx).await?;

    if args.json {
        return print_json(&payload);
    }

    println!("{}", crate::localization::formatted!("Triggers:"));
    if payload.triggers.is_empty() {
        println!("{}", crate::localization::formatted!("- none"));
    } else {
        for trigger in &payload.triggers {
            println!("{}", crate::localization::formatted!("- {} provider={} kind={} state={} version={} metrics=received:{} dispatched:{} failed:{} in_flight:{}",
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
                trigger.metrics.in_flight));
        }
    }

    if payload.budget.daily_limit_usd.is_some() || payload.budget.hourly_limit_usd.is_some() {
        println!();
        println!("{}", crate::localization::formatted!("Budget:"));
        if let Some(limit) = payload.budget.daily_limit_usd {
            println!(
                "{}",
                crate::localization::formatted!(
                    "- daily used=${:.6} remaining=${:.6} limit=${:.6}",
                    payload.budget.used_today_usd,
                    payload.budget.remaining_today_usd.unwrap_or_default(),
                    limit
                )
            );
        }
        if let Some(limit) = payload.budget.hourly_limit_usd {
            println!(
                "{}",
                crate::localization::formatted!(
                    "- hourly used=${:.6} remaining=${:.6} limit=${:.6}",
                    payload.budget.used_hour_usd,
                    payload.budget.remaining_hour_usd.unwrap_or_default(),
                    limit
                )
            );
        }
    }

    println!();
    println!("{}", crate::localization::formatted!("Connectors:"));
    if !payload.activations.is_empty() {
        for activation in &payload.activations {
            println!(
                "{}",
                crate::localization::formatted!(
                    "- {} bindings={}",
                    activation.provider,
                    activation.binding_count
                )
            );
        }
    } else if !payload.connectors.is_empty() {
        for connector in &payload.connectors {
            println!("- {connector}");
        }
    } else {
        println!("{}", crate::localization::formatted!("- none"));
    }

    if let Some(snapshot) = payload.snapshot.as_ref() {
        println!();
        println!("{}", crate::localization::formatted!("Snapshot:"));
        println!(
            "{}",
            crate::localization::formatted!("- status={}", snapshot.status)
        );
        println!(
            "{}",
            crate::localization::formatted!("- bind={}", snapshot.bind)
        );
    }

    println!();
    println!("{}", crate::localization::formatted!("Recent dispatches:"));
    if payload.recent_dispatches.is_empty() {
        println!("{}", crate::localization::formatted!("- none"));
        return Ok(());
    }
    for dispatch in payload.recent_dispatches.iter().rev().take(5).rev() {
        println!(
            "{}",
            crate::localization::formatted!(
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
            )
        );
    }

    Ok(())
}
