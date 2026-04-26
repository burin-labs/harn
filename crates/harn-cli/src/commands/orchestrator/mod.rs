pub(crate) mod common;
mod deploy;
mod dlq;
mod fire;
mod inspect;
pub(crate) mod inspect_data;
pub(crate) mod listener;
mod origin_guard;
mod queue;
mod recover;
mod reload;
mod replay;
mod resume;
pub(crate) mod role;
mod serve;
mod stats;
mod tenant;
mod tls;

use crate::cli::{OrchestratorArgs, OrchestratorCommand};

pub(crate) async fn handle(args: OrchestratorArgs) -> Result<(), String> {
    match args.command {
        OrchestratorCommand::Serve(serve_args) => serve::run(serve_args).await,
        OrchestratorCommand::Deploy(deploy_args) => deploy::run(*deploy_args).await,
        OrchestratorCommand::Reload(reload_args) => reload::run(reload_args).await,
        OrchestratorCommand::Inspect(inspect_args) => inspect::run(inspect_args).await,
        OrchestratorCommand::Stats(stats_args) => stats::run(stats_args).await,
        OrchestratorCommand::Fire(fire_args) => fire::run(fire_args).await,
        OrchestratorCommand::Replay(replay_args) => replay::run(replay_args).await,
        OrchestratorCommand::Resume(resume_args) => resume::run(resume_args).await,
        OrchestratorCommand::Dlq(dlq_args) => dlq::run(dlq_args).await,
        OrchestratorCommand::Queue(queue_args) => queue::run(queue_args).await,
        OrchestratorCommand::Recover(recover_args) => recover::run(recover_args).await,
        OrchestratorCommand::Tenant(tenant_args) => tenant::run(tenant_args).await,
    }
}
