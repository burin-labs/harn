use crate::cli::OrchestratorDlqArgs;

use super::common::{
    append_dlq_entry, discard_dlq_entry, load_local_runtime, trigger_inspect_dlq, trigger_replay,
};

pub(super) async fn run(args: OrchestratorDlqArgs) -> Result<(), String> {
    let mut ctx = load_local_runtime(&args.local).await?;
    let entries = trigger_inspect_dlq(&mut ctx).await?;

    if let Some(entry_id) = args.replay.as_deref() {
        let entry = entries
            .iter()
            .find(|entry| entry.id == entry_id)
            .ok_or_else(|| format!("unknown pending DLQ entry '{entry_id}'"))?;
        let handle = trigger_replay(&mut ctx, &entry.event_id).await?;
        println!("DLQ replay:");
        println!("- entry_id={}", entry.id);
        println!("- event_id={}", handle.event_id);
        println!("- status={}", handle.status);
        println!("- error={}", handle.error.as_deref().unwrap_or("-"));
        println!(
            "- replay_of_event_id={}",
            handle.replay_of_event_id.as_deref().unwrap_or("-")
        );
        return Ok(());
    }

    if let Some(entry_id) = args.discard.as_deref() {
        let entry = entries
            .iter()
            .find(|entry| entry.id == entry_id)
            .ok_or_else(|| format!("unknown pending DLQ entry '{entry_id}'"))?;
        let discarded = discard_dlq_entry(entry)?;
        append_dlq_entry(&ctx.event_log, &discarded).await?;
        println!("DLQ entry discarded:");
        println!("- entry_id={}", discarded.id);
        println!("- event_id={}", discarded.event_id);
        println!("- state={}", discarded.state);
        return Ok(());
    }

    let stats = harn_vm::snapshot_dispatcher_stats();
    println!("DLQ:");
    println!("- dispatcher_dlq_depth={}", stats.dlq_depth);
    println!("- pending_entries={}", entries.len());
    if entries.is_empty() {
        println!("- none");
        return Ok(());
    }
    for entry in entries {
        println!(
            "- {} binding={} event={} error={}",
            entry.id, entry.binding_id, entry.event_id, entry.error
        );
    }
    Ok(())
}
