use super::errors::OrchestratorError;
use crate::cli::OrchestratorReplayArgs;

use super::common::{load_local_runtime, print_json, trigger_replay};

pub(super) async fn run(args: OrchestratorReplayArgs) -> Result<(), OrchestratorError> {
    let mut ctx = load_local_runtime(&args.local).await?;
    let handle = trigger_replay(&mut ctx, &args.event_id).await?;
    if args.json {
        return print_json(&handle);
    }

    println!("{}", crate::localization::formatted!("Replay result:"));
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
