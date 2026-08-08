//! Merge Captain mock-repos playground (#1020) — temp git repos plus a
//! fake GitHub HTTP server that the captain (and any external connector)
//! can sweep against. Companion to the JSONL replay backend in
//! `merge_captain_driver.rs`.
//!
//! Public surface:
//!
//! * [`ScenarioManifest`] — declarative seed manifest, JSON or YAML.
//! * [`init_playground_at`] / [`cleanup_playground_at`] — materialize and
//!   tear down a playground directory.
//! * [`load_playground`] — load `state.json` plus the canonicalized manifest.
//! * [`apply_step`] / [`run_named_step`] / [`apply_one_action`] — advance
//!   the playground state.
//! * [`fake_server`] — pure request handlers used by the axum-based serve
//!   command.
//! * [`synthesize_sweep`] — generate a canonical agent-event transcript
//!   reflecting the current playground state. Used by the `--backend mock
//!   <playground-dir>` driver path.
//! * [`load_builtin`] / [`builtin_scenario_names`] — embedded scenarios
//!   resolved by `--scenario <name>`.

mod fake_server;
mod git;
mod init;
mod manifest;
mod scenarios;
mod state;
mod step;
mod transcript;

#[cfg(test)]
mod integration_tests;

pub use fake_server::{
    create_issue_comment, get_issue, get_pull, list_check_runs, list_issue_comments,
    list_pull_files, list_pulls, merge_pull, merge_queue_enqueue, merge_queue_status, parse_query,
    patch_pull, set_labels, workflow_run_logs, CreateCommentBody, EnqueueMergeQueueBody,
    FakeResponse, ListPullsQuery, MergePullBody, SetLabelsBody, UpdatePullBody,
};
pub use init::{cleanup_playground_at, init_playground_at, load_playground, InitOptions};
pub use manifest::{
    ScenarioAction, ScenarioBranch, ScenarioCheck, ScenarioComment, ScenarioCommit,
    ScenarioManifest, ScenarioPullRequest, ScenarioRepo, ScenarioStep, SCENARIO_TYPE,
};
pub use scenarios::{builtin_scenario_names, load_builtin};
pub use state::{
    manifest_path, playground_marker_path, state_path, PlaygroundHistoryEntry, PlaygroundMarker,
    PlaygroundPullRequest, PlaygroundRepoState, PlaygroundState, PLAYGROUND_TYPE, STATE_TYPE,
};
pub use step::{apply_one_action, apply_step, run_named_step, StepReport};
pub use transcript::{synthesize_sweep, TranscriptOptions};

/// Locate a manifest given either a `--scenario <name>` (built-in lookup),
/// a `--manifest <path>` (file read), or a default that prefers a
/// `merge_captain.scenario.json` adjacent to the playground path. Useful
/// for `harn merge-captain mock init`.
pub fn resolve_init_manifest(
    scenario_name: Option<&str>,
    manifest_path: Option<&std::path::Path>,
) -> Result<ScenarioManifest, crate::value::VmError> {
    if let Some(path) = manifest_path {
        return ScenarioManifest::load(path);
    }
    let name = scenario_name.unwrap_or("three_repo_basic");
    load_builtin(name)
}
