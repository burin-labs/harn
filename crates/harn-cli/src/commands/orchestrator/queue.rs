use std::collections::BTreeSet;
use std::sync::Arc;

use crate::cli::OrchestratorQueueArgs;

use super::common::{
    load_local_runtime, read_topic, TRIGGER_ATTEMPTS_TOPIC, TRIGGER_INBOX_CLAIMS_TOPIC,
    TRIGGER_INBOX_ENVELOPES_TOPIC, TRIGGER_INBOX_LEGACY_TOPIC, TRIGGER_OUTBOX_TOPIC,
};

pub(super) async fn run(args: OrchestratorQueueArgs) -> Result<(), String> {
    let ctx = load_local_runtime(&args.local).await?;
    let dispatcher = harn_vm::snapshot_dispatcher_stats();
    let outbox = read_topic(&ctx.event_log, TRIGGER_OUTBOX_TOPIC).await?;
    let attempts = read_topic(&ctx.event_log, TRIGGER_ATTEMPTS_TOPIC).await?;
    let claim_events = read_topic(&ctx.event_log, TRIGGER_INBOX_CLAIMS_TOPIC).await?;
    let envelope_events = read_topic(&ctx.event_log, TRIGGER_INBOX_ENVELOPES_TOPIC).await?;
    let legacy_inbox_events = read_topic(&ctx.event_log, TRIGGER_INBOX_LEGACY_TOPIC).await?;

    let mut started = BTreeSet::new();
    let mut finished = BTreeSet::new();
    for (_, event) in &outbox {
        let Some(key) = dispatch_key(&event.headers) else {
            continue;
        };
        match event.kind.as_str() {
            "dispatch_started" => {
                started.insert(key);
            }
            "dispatch_succeeded" | "dispatch_failed" => {
                finished.insert(key);
            }
            _ => {}
        }
    }
    let in_flight: Vec<_> = started.difference(&finished).cloned().collect();

    let mut scheduled = BTreeSet::new();
    for (_, event) in &attempts {
        if event.kind != "retry_scheduled" {
            continue;
        }
        if let Some(key) = dispatch_key(&event.headers) {
            scheduled.insert(key);
        }
    }
    let pending_retries: Vec<_> = scheduled.difference(&started).cloned().collect();

    let inbox_metrics = Arc::new(harn_vm::MetricsRegistry::default());
    let _inbox = harn_vm::triggers::InboxIndex::new(ctx.event_log.clone(), inbox_metrics.clone())
        .await
        .map_err(|error| error.to_string())?;
    let inbox_snapshot = inbox_metrics.snapshot();
    let inbox_claims_written = claim_events
        .iter()
        .chain(legacy_inbox_events.iter())
        .filter(|(_, event)| event.kind == "dedupe_claim")
        .count();
    let inbox_envelopes_written = envelope_events
        .iter()
        .chain(legacy_inbox_events.iter())
        .filter(|(_, event)| event.kind == "event_ingested")
        .count();

    println!("Queue:");
    println!("- dispatcher_in_flight={}", dispatcher.in_flight);
    println!(
        "- dispatcher_retry_queue_depth={}",
        dispatcher.retry_queue_depth
    );
    println!("- inferred_in_flight={}", in_flight.len());
    println!("- inferred_pending_retries={}", pending_retries.len());
    println!("- inbox_claims_written={}", inbox_claims_written);
    println!("- inbox_envelopes_written={}", inbox_envelopes_written);
    println!(
        "- inbox_duplicates_rejected={}",
        inbox_snapshot.inbox_duplicates_rejected
    );
    println!(
        "- inbox_fast_path_hits={}",
        inbox_snapshot.inbox_fast_path_hits
    );
    println!("- inbox_durable_hits={}", inbox_snapshot.inbox_durable_hits);
    println!(
        "- inbox_expired_entries={}",
        inbox_snapshot.inbox_expired_entries
    );
    println!(
        "- inbox_active_entries={}",
        inbox_snapshot.inbox_active_entries
    );

    println!();
    println!("In-flight dispatches:");
    if in_flight.is_empty() {
        println!("- none");
    } else {
        for key in in_flight {
            println!("- {key}");
        }
    }

    println!();
    println!("Pending retries:");
    if pending_retries.is_empty() {
        println!("- none");
    } else {
        for key in pending_retries {
            println!("- {key}");
        }
    }

    Ok(())
}

fn dispatch_key(headers: &std::collections::BTreeMap<String, String>) -> Option<String> {
    let binding_key = headers.get("binding_key")?;
    let event_id = headers.get("event_id")?;
    let attempt = headers.get("attempt")?;
    Some(format!("{binding_key}:{event_id}:{attempt}"))
}
