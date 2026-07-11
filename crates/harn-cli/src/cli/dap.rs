use clap::Args;

/// Arguments for `harn dap`.
///
/// The debug adapter speaks the Debug Adapter Protocol over stdio and is
/// normally launched by an editor, so it takes no options today. The struct
/// exists so the subcommand shows up in `harn --help`, shell completions, and
/// the JSON-schema catalog alongside every other command.
#[derive(Debug, Args)]
pub(crate) struct DapArgs {}
