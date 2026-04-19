mod listener;
mod origin_guard;
pub(crate) mod role;
mod serve;
mod tls;

use crate::cli::{OrchestratorArgs, OrchestratorCommand};

const O08_NOT_IMPLEMENTED: &str = "not yet implemented (see O-08 #185)";

pub(crate) async fn handle(args: OrchestratorArgs) -> Result<(), String> {
    match args.command {
        OrchestratorCommand::Serve(serve_args) => serve::run(serve_args).await,
        OrchestratorCommand::Inspect => Err(format!(
            "`harn orchestrator inspect` is {O08_NOT_IMPLEMENTED}"
        )),
        OrchestratorCommand::Replay => Err(format!(
            "`harn orchestrator replay` is {O08_NOT_IMPLEMENTED}"
        )),
        OrchestratorCommand::Dlq => {
            Err(format!("`harn orchestrator dlq` is {O08_NOT_IMPLEMENTED}"))
        }
        OrchestratorCommand::Queue => Err(format!(
            "`harn orchestrator queue` is {O08_NOT_IMPLEMENTED}"
        )),
    }
}
