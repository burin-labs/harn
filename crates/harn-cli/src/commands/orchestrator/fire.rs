use crate::cli::OrchestratorFireArgs;

use super::common::{load_local_runtime, synthetic_event_for_binding, trigger_fire};

pub(super) async fn run(args: OrchestratorFireArgs) -> Result<(), String> {
    let mut ctx = load_local_runtime(&args.local).await?;
    let event = synthetic_event_for_binding(&ctx, &args.binding_id)?;
    let handle = trigger_fire(&mut ctx, &args.binding_id, event).await?;

    println!("Synthetic event dispatched:");
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
