//! `harn models` — list, install, recommend, and test models.

pub(crate) mod install;
pub(crate) mod list;
pub(crate) mod recommend;
pub(crate) mod test;

use crate::cli::{ModelsArgs, ModelsCommand};

pub(crate) async fn run(args: ModelsArgs) {
    match args.command {
        ModelsCommand::List(args) => list::run(args).await,
        ModelsCommand::Install(args) => install::run(args).await,
        ModelsCommand::Recommend(args) => recommend::run(&args),
        ModelsCommand::Test(args) => test::run(&args).await,
    }
}
