//! `harn models` — list and install models.
//!
//! `recommend` and `test` are deferred to follow-up issues
//! (see `crates/harn-cli/FOLLOWUPS.md`).

pub(crate) mod install;
pub(crate) mod list;

use crate::cli::{ModelsArgs, ModelsCommand};

pub(crate) async fn run(args: ModelsArgs) {
    match args.command {
        ModelsCommand::List(args) => list::run(args).await,
        ModelsCommand::Install(args) => install::run(args).await,
    }
}
