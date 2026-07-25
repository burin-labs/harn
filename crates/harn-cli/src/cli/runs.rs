use clap::{Args, Subcommand};
use std::path::Path;
use std::process;

#[derive(Debug, Args)]
pub(crate) struct RunsArgs {
    #[command(subcommand)]
    pub command: RunsCommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum RunsCommand {
    /// Inspect a persisted run record and optionally diff it against another.
    Inspect(RunsInspectArgs),
    /// Print the stable harn.run_view.v1 / harn.session_view.v1 JSON projection.
    View(RunsViewArgs),
    /// Project one authoritative run into a harn.agent_training_example.v1 example.
    ExportTraining(RunsExportTrainingArgs),
}

#[derive(Debug, Args)]
pub(crate) struct RunsExportTrainingArgs {
    /// Path to a run record JSON file, or a directory holding run records.
    /// A directory with more than one record requires `--run-id`.
    pub path: String,
    /// Run id this export must project. Required to disambiguate a directory
    /// holding several runs, and otherwise asserted against the record.
    #[arg(long)]
    pub run_id: Option<String>,
    /// Session id the transcript must belong to.
    #[arg(long)]
    pub session_id: Option<String>,
    /// Write the example as one JSONL row to this path instead of stdout.
    #[arg(long)]
    pub out: Option<String>,
    /// Emit the structured report as JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct RunsInspectArgs {
    /// Path to the run record JSON file.
    pub path: String,
    /// Optional baseline run record to diff against.
    #[arg(long)]
    pub compare: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct RunsViewArgs {
    /// Path to a run record JSON file or a directory containing run records.
    pub path: String,
    /// Aggregate matching records into a harn.session_view.v1 projection.
    #[arg(long)]
    pub session: bool,
    /// Emit JSON. Accepted for consistency with other CLI surfaces.
    #[arg(long)]
    pub json: bool,
}

/// Dispatch a `harn runs` subcommand.
///
/// Lives beside the argument definitions rather than in the top-level command
/// match, so the surface's parsing and its wiring stay in one file.
pub(crate) async fn run_runs_command(args: RunsArgs) {
    match args.command {
        RunsCommand::Inspect(inspect) => {
            crate::inspect_run_record(&inspect.path, inspect.compare.as_deref());
        }
        RunsCommand::View(view) => print_view(&view.path, view.session, view.json),
        RunsCommand::ExportTraining(export) => {
            let code = crate::commands::runs_export_training::run(&export).await;
            if code != 0 {
                process::exit(code);
            }
        }
    }
}

pub(crate) fn print_view(path: &str, force_session: bool, _json: bool) {
    let paths = crate::collect_run_record_paths(path);
    if paths.is_empty() {
        eprintln!("No run records found at {path}");
        process::exit(1);
    }

    if force_session || paths.len() > 1 || Path::new(path).is_dir() {
        let views = paths
            .iter()
            .map(|path| {
                harn_vm::orchestration::build_run_view_with_path(
                    &crate::load_run_record_or_exit(path),
                    Some(path.display().to_string()),
                )
            })
            .collect();
        print_json(&harn_vm::orchestration::build_session_view_from_run_views(
            views,
            harn_vm::orchestration::SessionViewOptions::default(),
        ));
    } else {
        print_json(&harn_vm::orchestration::build_run_view_with_path(
            &crate::load_run_record_or_exit(&paths[0]),
            Some(paths[0].display().to_string()),
        ));
    }
}

fn print_json<T: serde::Serialize>(value: &T) {
    match serde_json::to_string_pretty(value) {
        Ok(rendered) => println!("{rendered}"),
        Err(error) => {
            eprintln!("Failed to render JSON: {error}");
            process::exit(1);
        }
    }
}

#[derive(Debug, Args)]
pub(crate) struct ReplayArgs {
    /// Path to the run record JSON file. Kept for compatibility with older `harn replay <path>` usage.
    #[arg(
        value_name = "PATH",
        required_unless_present_any = ["fixture", "session_id"],
        conflicts_with_all = ["fixture", "session_id"]
    )]
    pub path: Option<String>,
    /// Path to a run record or replay-oracle fixture.
    #[arg(long, value_name = "PATH", conflicts_with_all = ["path", "session_id"])]
    pub fixture: Option<String>,
    /// Reconstruct replay input from the agent-session events in `--events-db`.
    #[arg(long, requires = "events_db", conflicts_with_all = ["path", "fixture"])]
    pub session_id: Option<String>,
    /// SQLite EventLog database to read for `--session-id`.
    #[arg(long, value_name = "PATH", requires = "session_id")]
    pub events_db: Option<String>,
    /// Time-travel: rehydrate the session only up to (and including) this
    /// event id, replaying it as it stood at that point. Requires
    /// `--session-id`; omit to replay the whole session.
    #[arg(long, value_name = "EVENT_ID", requires = "session_id")]
    pub at: Option<u64>,
    /// Counterfactual: after rehydrating the session at `--at` (or its full
    /// state), evaluate one or more `.harn` plans and report how the workspace
    /// *would have* diverged — the set of files the chained edits would
    /// touch. The plans run through `edit.dry_run` against a throw-away
    /// staged-fs overlay (#1722), so the recorded session and the on-disk
    /// tree are never mutated. Requires `--session-id`.
    #[arg(long, value_name = "PLAN", requires = "session_id")]
    pub counterfactual: Vec<String>,
    /// Number of replay reads to compare for deterministic output.
    #[arg(long, default_value_t = 1)]
    pub runs: usize,
    /// Emit a structured `JsonEnvelope` replay summary instead of human-readable output.
    /// See `docs/src/cli-json-contract.md` for the envelope shape.
    #[arg(long)]
    pub json: bool,
}
