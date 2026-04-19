use crate::cli::OrchestratorInspectArgs;

use serde::Serialize;

use super::common::{
    load_local_runtime, print_json, read_topic, trigger_list, ConnectorActivationSnapshot,
    PersistedStateSnapshot, TRIGGER_OUTBOX_TOPIC,
};

#[derive(Debug, Serialize)]
struct InspectPayload {
    bindings: Vec<harn_vm::TriggerBindingSnapshot>,
    connectors: Vec<String>,
    activations: Vec<ConnectorActivationSnapshot>,
    snapshot: Option<PersistedStateSnapshot>,
    recent_dispatches: Vec<RecentDispatchRecord>,
}

#[derive(Debug, Serialize)]
struct RecentDispatchRecord {
    kind: String,
    status: String,
    occurred_at_ms: i64,
    trigger_id: Option<String>,
    event_id: Option<String>,
    attempt: Option<u32>,
    replay_of_event_id: Option<String>,
    handler_kind: Option<String>,
    target_uri: Option<String>,
    error: Option<String>,
    result: Option<serde_json::Value>,
}

pub(super) async fn run(args: OrchestratorInspectArgs) -> Result<(), String> {
    let mut ctx = load_local_runtime(&args.local).await?;
    let bindings: Vec<_> = trigger_list(&mut ctx)
        .await?
        .into_iter()
        .filter(|binding| binding.source == harn_vm::TriggerBindingSource::Manifest)
        .collect();
    let dispatches = read_topic(&ctx.event_log, TRIGGER_OUTBOX_TOPIC).await?;
    let recent_dispatches = recent_dispatch_records(dispatches, 20);

    if args.json {
        return print_json(&InspectPayload {
            bindings,
            connectors: ctx
                .snapshot
                .as_ref()
                .map(|snapshot| snapshot.connectors.clone())
                .unwrap_or_default(),
            activations: ctx
                .snapshot
                .as_ref()
                .map(|snapshot| snapshot.activations.clone())
                .unwrap_or_default(),
            snapshot: ctx.snapshot.clone(),
            recent_dispatches,
        });
    }

    println!("Triggers:");
    if bindings.is_empty() {
        println!("- none");
    } else {
        for binding in bindings {
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
    if recent_dispatches.is_empty() {
        println!("- none");
        return Ok(());
    }
    for dispatch in recent_dispatches.into_iter().rev().take(5).rev() {
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

fn recent_dispatch_records(
    dispatches: Vec<(u64, harn_vm::event_log::LogEvent)>,
    limit: usize,
) -> Vec<RecentDispatchRecord> {
    let mut recent: Vec<_> = dispatches
        .into_iter()
        .filter_map(|(_, event)| {
            if !matches!(
                event.kind.as_str(),
                "dispatch_succeeded" | "dispatch_failed"
            ) {
                return None;
            }

            let kind = event.kind;
            let occurred_at_ms = event.occurred_at_ms;
            let headers = event.headers;
            let payload = event.payload;
            let payload = payload.as_object();
            Some(RecentDispatchRecord {
                status: kind.trim_start_matches("dispatch_").to_string(),
                kind,
                occurred_at_ms,
                trigger_id: headers.get("trigger_id").cloned(),
                event_id: headers.get("event_id").cloned(),
                attempt: headers
                    .get("attempt")
                    .and_then(|attempt| attempt.parse::<u32>().ok()),
                replay_of_event_id: headers.get("replay_of_event_id").cloned(),
                handler_kind: headers.get("handler_kind").cloned(),
                target_uri: payload.and_then(|payload| {
                    payload
                        .get("target_uri")
                        .and_then(|value| value.as_str())
                        .map(ToOwned::to_owned)
                }),
                error: payload.and_then(|payload| {
                    payload
                        .get("error")
                        .and_then(|value| value.as_str())
                        .map(ToOwned::to_owned)
                }),
                result: payload.and_then(|payload| payload.get("result").cloned()),
            })
        })
        .collect();

    recent.sort_by_key(|dispatch| dispatch.occurred_at_ms);
    if recent.len() > limit {
        recent.drain(0..recent.len() - limit);
    }
    recent
}
