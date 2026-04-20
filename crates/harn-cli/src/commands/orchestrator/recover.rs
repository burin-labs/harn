use crate::cli::OrchestratorRecoverArgs;

use super::common::{
    format_duration, format_timestamp, load_local_runtime, stranded_envelopes, trigger_replay,
};

pub(super) async fn run(args: OrchestratorRecoverArgs) -> Result<(), String> {
    if !args.dry_run && !args.yes {
        return Err(
            "refusing to replay stranded envelopes without --yes; pass --dry-run to inspect first"
                .to_string(),
        );
    }

    let mut ctx = load_local_runtime(&args.local).await?;
    let stranded = stranded_envelopes(&ctx.event_log, args.envelope_age).await?;

    println!("Recovery:");
    println!("- dry_run={}", args.dry_run);
    println!("- min_envelope_age={}", format_duration(args.envelope_age));
    println!("- stranded_envelopes={}", stranded.len());

    println!();
    println!("Candidates:");
    if stranded.is_empty() {
        println!("- none");
        return Ok(());
    }

    for envelope in &stranded {
        println!(
            "- event_id={} trigger_id={} binding_version={} provider={} kind={} age={} received_at={} inbox_offset={}",
            envelope.event_id,
            envelope.trigger_id.as_deref().unwrap_or("-"),
            envelope
                .binding_version
                .map(|value| value.to_string())
                .unwrap_or_else(|| "-".to_string()),
            envelope.provider,
            envelope.kind,
            format_duration(envelope.age),
            format_timestamp(envelope.received_at),
            envelope.inbox_offset,
        );
    }

    if args.dry_run {
        return Ok(());
    }

    println!();
    println!("Replay results:");
    for envelope in stranded {
        let handle = trigger_replay(&mut ctx, &envelope.event_id).await?;
        println!(
            "- event_id={} status={} replay_of_event_id={} binding_id={} binding_version={} dlq_entry_id={} error={}",
            handle.event_id,
            handle.status,
            handle.replay_of_event_id.as_deref().unwrap_or("-"),
            handle.binding_id,
            handle.binding_version,
            handle.dlq_entry_id.as_deref().unwrap_or("-"),
            handle.error.as_deref().unwrap_or("-"),
        );
    }

    Ok(())
}
