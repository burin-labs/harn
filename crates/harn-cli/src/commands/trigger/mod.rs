pub(crate) mod cancel;
pub(crate) mod ops;
pub(crate) mod replay;

use crate::cli::{TriggerArgs, TriggerCommand};

pub(crate) async fn handle(args: TriggerArgs) -> Result<(), String> {
    match args.command {
        TriggerCommand::Replay(args) => replay::run(args).await,
        TriggerCommand::Cancel(args) => cancel::run(args).await,
    }
}
