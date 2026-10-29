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
use crate::commands::generated_artifact::write_or_check;

/// Run one generator command.
///
/// Panics on any other variant: callers route only the generator commands
/// here, and the top-level match is exhaustive over the rest.
pub(crate) fn dispatch(command: Command) {
    match command {
        Command::DumpHighlightKeywords(args) => {
            commands::dump_highlight_keywords::run(
                &args.output,
                &args.json_output,
                &args.wasm_json_output,
                args.check,
            );
        }
        Command::DumpOperatorPrecedence(args) => {
            commands::dump_operator_precedence::run(&args.markdown, &args.tree_sitter, args.check);
        }
        Command::DumpPortableBenchmarkSchema(args) => {
            commands::dump_portable_benchmark_schema::run(&args.output, args.check);
        }
        Command::DumpPromptGrammar(args) => {
            commands::dump_prompt_grammar::run(&args.output, args.check);
        }
        Command::DumpBuiltinMethods(args) => {
            commands::dump_builtin_methods::run(&args.output, args.check);
        }
        Command::DumpPortalTypes(args) => {
            write_or_check(
                std::path::Path::new(&args.output),
                &commands::portal::render_ts_types(),
                args.check,
                "the portal DTOs",
                "gen-portal-types",
            );
        }
        Command::DumpTriggerQuickref(args) => {
            commands::dump_trigger_quickref::run(&args.output, args.check);
        }
        Command::DumpCliSurface(args) => {
            commands::argument_schema::dump_surface(&args.output, args.check);
        }
        Command::DumpConnectorMatrix(args) => {
            commands::check::connector_matrix::run_docs(&args.output, &args.sources, args.check);
        }
        Command::DumpProtocolArtifacts(args) => {
            commands::dump_protocol_artifacts::run(&args.output_dir, args.check);
        }
        Command::DumpHarnessMigrations(args) => {
            let code = commands::harness_migration_codegen::run(&args);
            if code != 0 {
                process::exit(code);
            }
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
