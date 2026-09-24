use std::ffi::OsString;

use clap::{Args, Subcommand};

#[derive(Debug, Args)]
pub(crate) struct SelfArgs {
    #[command(subcommand)]
    pub command: SelfCommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum SelfCommand {
    /// Download and cache a checksum-verified release binary.
    Install { version: String },
    /// Run a command with a cached release binary, installing it if needed.
    Run {
        #[arg(long, value_name = "TAG")]
        version: String,
        #[arg(last = true, required = true, value_name = "HARN_ARGS")]
        args: Vec<OsString>,
    },
    /// List locally cached release binaries.
    List,
    /// Remove all but the newest N cached release binaries.
    Prune {
        #[arg(long, value_name = "N")]
        keep: usize,
    },
}
