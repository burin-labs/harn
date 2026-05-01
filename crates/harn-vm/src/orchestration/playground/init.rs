//! Materialize a Merge Captain mock-repos playground (#1020).
//!
//! `init_playground_at(dir, manifest)` creates:
//!
//! ```text
//! <dir>/
//!   playground.json   (marker — versioned, idempotent rejection key)
//!   manifest.json     (canonicalized seed manifest)
//!   state.json        (mutable playground state — what the fake server reads)
//!   remotes/<repo>.git/   (bare remotes)
//!   working/<repo>/       (working clones)
//! ```
//!
//! The captain (and, in the harn-github-connector repo, real connector code)
//! exercises `file://` remotes so every `git fetch / push / rebase /
//! force-with-lease` runs against real git.

use std::path::Path;

use serde_json::json;

use crate::value::VmError;

use super::git::{bare_file_url, GitOps};
use super::manifest::{ScenarioBranch, ScenarioManifest, ScenarioRepo};
use super::state::{
    manifest_path, playground_marker_path, PlaygroundMarker, PlaygroundState, PLAYGROUND_TYPE,
};

/// Default contents written for a repo's seed README when the manifest
/// doesn't supply any files. Keeps the bare clone non-empty so the first
/// `git push` lands on a real commit.
fn default_seed_files(repo: &ScenarioRepo) -> std::collections::BTreeMap<String, String> {
    if !repo.files.is_empty() {
        return repo.files.clone();
    }
    let mut files = std::collections::BTreeMap::new();
    files.insert(
        "README.md".to_string(),
        format!("# {}\n\nMerge Captain playground seed.\n", repo.name),
    );
    files
}

pub struct InitOptions<'a> {
    pub dir: &'a Path,
    pub manifest: &'a ScenarioManifest,
    /// When true, rejects re-init of an existing playground. Default for the
    /// CLI; tests typically clear the dir first.
    pub allow_existing: bool,
}

pub fn init_playground_at(options: InitOptions<'_>) -> Result<PlaygroundState, VmError> {
    let dir = options.dir;
    let manifest = options.manifest;
    let marker = playground_marker_path(dir);
    if marker.exists() && !options.allow_existing {
        return Err(VmError::Runtime(format!(
            "playground already initialized at {} (run `harn merge-captain mock cleanup {0}` first)",
            dir.display()
        )));
    }
    std::fs::create_dir_all(dir).map_err(|error| {
        VmError::Runtime(format!(
            "failed to create playground dir {}: {error}",
            dir.display()
        ))
    })?;
    std::fs::create_dir_all(dir.join("remotes"))
        .map_err(|error| VmError::Runtime(format!("failed to create remotes dir: {error}")))?;
    std::fs::create_dir_all(dir.join("working"))
        .map_err(|error| VmError::Runtime(format!("failed to create working dir: {error}")))?;

    let git = GitOps::default();
    let mut state = PlaygroundState::from_manifest(manifest);

    for repo in &manifest.repos {
        materialize_repo(&git, dir, repo, &mut state)?;
    }

    // Persist a canonicalized manifest copy so subsequent `step` and `serve`
    // commands work without the original file path.
    let manifest_bytes = serde_json::to_vec_pretty(manifest)
        .map_err(|error| VmError::Runtime(format!("failed to serialize manifest copy: {error}")))?;
    let mut manifest_with_newline = manifest_bytes;
    manifest_with_newline.push(b'\n');
    std::fs::write(manifest_path(dir), manifest_with_newline)
        .map_err(|error| VmError::Runtime(format!("failed to write manifest copy: {error}")))?;

    let marker_value = PlaygroundMarker::new(&manifest.scenario, state.now_ms);
    let mut marker_bytes = serde_json::to_vec_pretty(&marker_value).map_err(|error| {
        VmError::Runtime(format!("failed to serialize playground marker: {error}"))
    })?;
    marker_bytes.push(b'\n');
    std::fs::write(playground_marker_path(dir), marker_bytes)
        .map_err(|error| VmError::Runtime(format!("failed to write playground marker: {error}")))?;

    state.record(
        "init",
        json!({
            "scenario": manifest.scenario,
            "repos": manifest.repos.iter().map(|r| r.name.clone()).collect::<Vec<_>>(),
            "pull_requests": manifest.pull_requests.len(),
        }),
    );
    state.save(dir)?;

    Ok(state)
}

fn materialize_repo(
    git: &GitOps,
    dir: &Path,
    repo: &ScenarioRepo,
    state: &mut PlaygroundState,
) -> Result<(), VmError> {
    let bare = dir.join("remotes").join(format!("{}.git", repo.name));
    let working = dir.join("working").join(&repo.name);
    git.init_bare(&bare, &repo.default_branch)?;
    git.clone(&bare, &working)?;

    // Seed the default branch — first commit of the bare remote.
    let seed_files = default_seed_files(repo);
    let seed_message = format!("Initial seed for {}", repo.name);
    let seed_sha = git.commit_overlay(
        &working,
        &seed_files,
        &[],
        &seed_message,
        Some(&repo.default_branch),
    )?;

    // Optional extra commits *before* feature branches are forked. We keep
    // `seed_sha` so any branch that wants `fork_before_extra_commits == true`
    // can reset to it instead of `origin/<default>`.
    for extra in &repo.default_branch_extra_commits {
        git.commit_overlay(
            &working,
            &extra.files_set,
            &extra.files_delete,
            &extra.message,
            Some(&repo.default_branch),
        )?;
    }

    // Feature branches.
    for branch in &repo.branches {
        materialize_branch(git, &working, repo, branch, &seed_sha)?;
    }

    // Update state for this repo.
    let url = bare_file_url(&bare);
    if let Some(repo_state) = state.repos.get_mut(&repo.name) {
        repo_state.remote_url = url;
        repo_state.remote_path = path_relative_to(&bare, dir);
        repo_state.working_path = path_relative_to(&working, dir);
    }

    // Resolve head_sha for any PRs that reference branches in this repo.
    for pr in state
        .pull_requests
        .values_mut()
        .filter(|pr| pr.repo == repo.name)
    {
        if pr.head_sha.is_some() {
            continue;
        }
        let sha = git
            .rev_parse(&working, &format!("origin/{}", pr.head_branch))
            .ok();
        pr.head_sha = sha;
    }

    Ok(())
}

fn materialize_branch(
    git: &GitOps,
    working: &Path,
    repo: &ScenarioRepo,
    branch: &ScenarioBranch,
    seed_sha: &str,
) -> Result<String, VmError> {
    let base_branch = branch
        .base
        .clone()
        .unwrap_or_else(|| repo.default_branch.clone());
    let from_ref = if branch.fork_before_extra_commits {
        seed_sha.to_string()
    } else {
        format!("origin/{base_branch}")
    };
    git.create_branch(working, &branch.name, &from_ref)?;
    let message = branch
        .commit_message
        .clone()
        .unwrap_or_else(|| format!("{} on {}", branch.name, repo.name));
    let sha = git.commit_overlay(
        working,
        &branch.files_set,
        &branch.files_delete,
        &message,
        Some(&branch.name),
    )?;
    // Return to the default branch so subsequent operations don't accumulate
    // on a feature branch.
    git.checkout(working, &repo.default_branch)?;
    Ok(sha)
}

fn path_relative_to(target: &Path, base: &Path) -> String {
    target
        .strip_prefix(base)
        .map(|rel| rel.to_string_lossy().to_string())
        .unwrap_or_else(|_| target.to_string_lossy().to_string())
}

/// Idempotent cleanup. Removes the playground directory entirely if it
/// looks like one (i.e. has a `playground.json` marker with the right
/// `_type`). Refuses to delete arbitrary directories — returns an error
/// instead of removing anything.
pub fn cleanup_playground_at(dir: &Path) -> Result<bool, VmError> {
    let marker = playground_marker_path(dir);
    if !marker.exists() {
        // Idempotent: cleanup of a non-playground or already-cleaned dir is OK.
        if !dir.exists() {
            return Ok(false);
        }
        // If the dir exists but has no marker, refuse — don't `rm -rf`
        // arbitrary user content.
        if dir
            .read_dir()
            .map(|mut r| r.next().is_none())
            .unwrap_or(true)
        {
            // empty dir is fine to leave alone (and idempotent).
            return Ok(false);
        }
        return Err(VmError::Runtime(format!(
            "{} does not look like a Merge Captain playground (missing playground.json marker); refusing to remove",
            dir.display()
        )));
    }
    // Verify the marker before removing.
    let bytes = std::fs::read(&marker).map_err(|error| {
        VmError::Runtime(format!(
            "failed to read playground marker {}: {error}",
            marker.display()
        ))
    })?;
    let parsed: PlaygroundMarker = serde_json::from_slice(&bytes).map_err(|error| {
        VmError::Runtime(format!(
            "playground marker {} is malformed: {error}",
            marker.display()
        ))
    })?;
    if parsed.type_name != PLAYGROUND_TYPE {
        return Err(VmError::Runtime(format!(
            "playground marker {} has wrong _type {:?}; refusing to remove",
            marker.display(),
            parsed.type_name
        )));
    }
    std::fs::remove_dir_all(dir).map_err(|error| {
        VmError::Runtime(format!(
            "failed to remove playground dir {}: {error}",
            dir.display()
        ))
    })?;
    Ok(true)
}

/// Verify that a directory hosts a playground; load and return the
/// state and manifest if so.
pub fn load_playground(dir: &Path) -> Result<(PlaygroundState, ScenarioManifest), VmError> {
    let marker = playground_marker_path(dir);
    if !marker.exists() {
        return Err(VmError::Runtime(format!(
            "{} is not a Merge Captain playground (missing playground.json)",
            dir.display()
        )));
    }
    let manifest_bytes = std::fs::read(manifest_path(dir)).map_err(|error| {
        VmError::Runtime(format!(
            "failed to read playground manifest {}: {error}",
            manifest_path(dir).display()
        ))
    })?;
    let manifest: ScenarioManifest = serde_json::from_slice(&manifest_bytes).map_err(|error| {
        VmError::Runtime(format!("failed to parse playground manifest copy: {error}"))
    })?;
    let state = PlaygroundState::load(dir)?;
    Ok((state, manifest))
}
