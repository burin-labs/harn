//! `harn models` — list, install, and recommend models.

pub(crate) mod install;
pub(crate) mod list;
pub(crate) mod recommend;

use crate::cli::{ModelsArgs, ModelsCommand};

pub(crate) async fn run(args: ModelsArgs) {
    match args.command {
        ModelsCommand::List(args) => list::run(args).await,
        ModelsCommand::Install(args) => install::run(args).await,
        ModelsCommand::Recommend(args) => recommend::run(&args),
    }
}
