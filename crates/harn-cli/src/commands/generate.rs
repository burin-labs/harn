//! Dispatch for the hidden `dump-*` / codegen commands.
//!
//! These commands are dev-only: each regenerates a committed artifact from a
//! live source of truth and, with `--check`, fails when the committed copy has
//! drifted. They are invoked through `make gen-*` / `make check-*` rather than
//! by users, which is why they are hidden from `--help`.
//!
//! They live here rather than in the main `run` match so the top-level
//! dispatcher stays a map of the *user-facing* command surface.

use std::process;

use crate::cli::Command;
use crate::commands;

/// Run one generator command.
///
/// Panics on any other variant: callers route only the generator commands
/// here, and the top-level match is exhaustive over the rest.
pub(crate) fn dispatch(command: Command) {
    match command {
        Command::DumpHighlightKeywords(args) => {
            commands::dump_highlight_keywords::run(&args.output, args.check);
        }
        Command::DumpPromptGrammar(args) => {
            commands::dump_prompt_grammar::run(&args.output, args.check);
        }
        Command::DumpTriggerQuickref(args) => {
            commands::dump_trigger_quickref::run(&args.output, args.check);
        }
        Command::DumpConnectorMatrix(args) => {
            commands::check::connector_matrix::run_docs(&args.output, &args.sources, args.check);
        }
        Command::DumpProtocolArtifacts(args) => {
            commands::dump_protocol_artifacts::run(
                &args.output_dir,
                args.check,
                args.artifact_version.as_deref(),
            );
        }
        Command::ConnectorSchemaCodegen(args) => {
            let code = commands::connector_schema_codegen::run(&args);
            if code != 0 {
                process::exit(code);
            }
        }
        other => unreachable!("not a generator command: {other:?}"),
    }
}
