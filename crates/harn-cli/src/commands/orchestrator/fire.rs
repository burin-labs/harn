use super::errors::OrchestratorError;
use crate::cli::OrchestratorFireArgs;

use super::common::{load_local_runtime, synthetic_event_for_binding, trigger_fire};

pub(super) async fn run(args: OrchestratorFireArgs) -> Result<(), OrchestratorError> {
    let mut ctx = load_local_runtime(&args.local).await?;
    let event = synthetic_event_for_binding(&ctx, &args.binding_id)?;
    let handle = trigger_fire(&mut ctx, &args.binding_id, event).await?;

    println!(
        "{}",
        crate::localization::formatted!("Synthetic event dispatched:")
    );
    println!(
        "{}",
        crate::localization::formatted!("- binding_id={}", handle.binding_id)
    );
    println!(
        "{}",
        crate::localization::formatted!("- binding_version={}", handle.binding_version)
    );
    println!(
        "{}",
        crate::localization::formatted!("- event_id={}", handle.event_id)
    );
    println!(
        "{}",
        crate::localization::formatted!("- status={}", handle.status)
    );
    println!(
        "{}",
        crate::localization::formatted!(
            "- replay_of_event_id={}",
            handle.replay_of_event_id.as_deref().unwrap_or("-")
        )
    );
    println!(
        "{}",
        crate::localization::formatted!(
            "- dlq_entry_id={}",
            handle.dlq_entry_id.as_deref().unwrap_or("-")
        )
    );
    println!(
        "{}",
        crate::localization::formatted!("- error={}", handle.error.as_deref().unwrap_or("-"))
    );
    println!(
        "{}",
        crate::localization::formatted!(
            "- result={}",
            handle
                .result
                .map(|result| result.to_string())
                .unwrap_or_else(|| "-".to_string())
        )
    );
    Ok(())
}
