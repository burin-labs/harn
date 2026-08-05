//! One place that answers "which run?" for every `harn runs` subcommand.
//!
//! `inspect`, `view`, `report`, `review`, and `export-training` all open a run
//! record. Historically each took a path, which meant none of them could be
//! pointed at a headless agent run: nothing on the agent-session path writes a
//! run record, so a run could persist thousands of events and still be
//! unreadable by every tool built to read runs (issue #6120).
//!
//! `--session` closes that by projecting the record from the session Harn
//! already persisted. It lives here rather than in each subcommand so the flag,
//! its help text, its store lookup, and its failure messages have one owner and
//! cannot drift into five slightly different behaviors.

use std::path::PathBuf;
use std::process;

use clap::Args;

/// Name a run by the session it was persisted under, as an alternative to
/// naming the run-record file.
#[derive(Debug, Default, Args)]
pub(crate) struct SessionSourceArgs {
    /// Project the run record from this persisted agent session instead of
    /// reading one off disk. Use this for a run driven by a host — an IDE or a
    /// headless agent loop — which persists events but writes no run record.
    ///
    /// Named `--from-session` rather than `--session` because `harn runs view
    /// --session` already means "aggregate these records into a session view",
    /// which is a different question about a different input.
    #[arg(long, value_name = "ID")]
    pub from_session: Option<String>,
    /// Workspace root holding `.harn/session-store.sqlite`. Defaults to the
    /// current directory.
    #[arg(long, value_name = "PATH", requires = "from_session")]
    pub session_root: Option<PathBuf>,
}

impl SessionSourceArgs {
    /// Project and persist the named session's run record, returning its path.
    ///
    /// Returns `None` when no session was named, which is the signal for the
    /// caller to fall back to its positional path.
    pub(crate) async fn materialize(&self) -> Option<Result<String, String>> {
        let session = self.from_session.as_deref()?;
        let root = self
            .session_root
            .clone()
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
        Some(
            harn_vm::orchestration::materialize_session_run_record(&root, session, None)
                .await
                .map_err(|error| error.to_string()),
        )
    }
}

/// Resolve a run-record path from either a positional path or `--session`.
///
/// Clap already rejects the neither-nor case via `required_unless_present`, so
/// reaching that arm means the argument definitions and this resolver have
/// drifted; it exits rather than guessing.
pub(crate) async fn resolve_run_path_or_exit(
    path: Option<&str>,
    session: &SessionSourceArgs,
) -> String {
    match session.materialize().await {
        Some(Ok(materialized)) => materialized,
        Some(Err(error)) => {
            eprintln!("error: {error}");
            process::exit(1);
        }
        None => match path {
            Some(path) => path.to_string(),
            None => {
                eprintln!("error: a run record path or --session is required");
                process::exit(2);
            }
        },
    }
}
