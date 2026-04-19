use crate::cli::OrchestratorReplayArgs;

use super::common::{load_local_runtime, trigger_replay};

pub(super) async fn run(args: OrchestratorReplayArgs) -> Result<(), String> {
    let mut ctx = load_local_runtime(&args.local).await?;
    let handle = trigger_replay(&mut ctx, &args.event_id).await?;

    println!("Replay result:");
    println!("- binding_id={}", handle.binding_id);
    println!("- binding_version={}", handle.binding_version);
    println!("- event_id={}", handle.event_id);
    println!("- status={}", handle.status);
    println!(
        "- replay_of_event_id={}",
        handle.replay_of_event_id.as_deref().unwrap_or("-")
    );
    println!(
        "- dlq_entry_id={}",
        handle.dlq_entry_id.as_deref().unwrap_or("-")
    );
    println!("- error={}", handle.error.as_deref().unwrap_or("-"));
    println!(
        "- result={}",
        handle
            .result
            .map(|result| result.to_string())
            .unwrap_or_else(|| "-".to_string())
    );
    Ok(())
}
