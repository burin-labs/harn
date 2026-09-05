pub(crate) mod common;
mod deploy;
mod dlq;
pub(crate) mod errors;
mod fire;
pub mod harness;
mod inspect;
pub(crate) mod inspect_data;
pub(crate) mod listener;
mod origin_guard;
mod queue;
mod recover;
mod reload;
mod replay;
mod replay_oracle;
mod resume;
pub(crate) mod role;
mod serve;
mod stats;
pub(crate) mod supervisor_state;
mod tenant;
pub mod tls;

#[allow(unused_imports)]
pub(crate) use errors::{OrchestratorError, OrchestratorResult};

use std::future::Future;
use std::pin::Pin;

use crate::cli::{OrchestratorArgs, OrchestratorCommand};

/// Dispatch one orchestrator subcommand.
///
/// Each arm is boxed before it is awaited. An `async fn` call materializes its
/// whole future in the caller's frame, and a `match` that awaits thirteen of
/// them inline holds every one of those states at once: this frame measured
/// within five percent of the stack ceiling that aborts a tokio worker, which
/// is one nested descent away from a crashed run with no diagnosis (harn#7931).
/// Boxing moves each subcommand's state to the heap and leaves a pointer here,
/// so the frame no longer grows with the number of subcommands.
pub(crate) async fn handle(args: OrchestratorArgs) -> OrchestratorResult<()> {
    let command: Pin<Box<dyn Future<Output = OrchestratorResult<()>> + Send>> = match args.command {
        OrchestratorCommand::Serve(serve_args) => Box::pin(serve::run(serve_args)),
        OrchestratorCommand::Deploy(deploy_args) => Box::pin(deploy::run(*deploy_args)),
        OrchestratorCommand::Reload(reload_args) => Box::pin(reload::run(reload_args)),
        OrchestratorCommand::Inspect(inspect_args) => Box::pin(inspect::run(inspect_args)),
        OrchestratorCommand::Stats(stats_args) => Box::pin(stats::run(stats_args)),
        OrchestratorCommand::Fire(fire_args) => Box::pin(fire::run(fire_args)),
        OrchestratorCommand::Replay(replay_args) => Box::pin(replay::run(replay_args)),
        OrchestratorCommand::ReplayOracle(replay_oracle_args) => {
            Box::pin(replay_oracle::run(replay_oracle_args))
        }
        OrchestratorCommand::Resume(resume_args) => Box::pin(resume::run(resume_args)),
        OrchestratorCommand::Dlq(dlq_args) => Box::pin(dlq::run(dlq_args)),
        OrchestratorCommand::Queue(queue_args) => Box::pin(queue::run(queue_args)),
        OrchestratorCommand::Recover(recover_args) => Box::pin(recover::run(recover_args)),
        OrchestratorCommand::Tenant(tenant_args) => Box::pin(tenant::run(tenant_args)),
    };
    command.await
}
