//! `harn models` — list, install, recommend, and test models.

pub(crate) mod batch;
pub(crate) mod install;
pub(crate) mod list;
pub(crate) mod lora;
pub(crate) mod recommend;
mod recommend_sources;
pub(crate) mod test;

use crate::cli::{ModelsArgs, ModelsCommand};

pub(crate) async fn run(args: ModelsArgs) {
    match args.command {
        ModelsCommand::Batch(args) => batch::run(args).await,
        ModelsCommand::Info(args) => {
            if !crate::print_model_info(&args).await {
                std::process::exit(1);
            }
        }
        ModelsCommand::Lora(args) => lora::run(*args).await,
        ModelsCommand::List(args) => list::run(args).await,
        ModelsCommand::Install(args) => install::run(args).await,
        ModelsCommand::Recommend(args) => recommend::run(&args).await,
        ModelsCommand::Test(args) => test::run(&args).await,
    }
}
