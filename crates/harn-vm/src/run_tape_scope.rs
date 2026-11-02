//! Which execution a branch belongs to, and where its durable state goes.
//!
//! A branch is owned by the persistent-state scope its host enters, so
//! workers and subtasks of that execution inherit it and every other
//! session in the process stays outside it. While a branch is in scope,
//! every durable path resolves through [`durable_path`]: the branch's own
//! state root stays as it is, a path inside a tree the recording owns
//! remaps under that root, and any other path is an effect outside the
//! branch, refused in the prefix and reported once live.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use super::BranchState;
use crate::event_log::AnyEventLog;
use crate::value::VmError;

/// Branches alive in this process; zero lets hot paths skip the scope lookup.
static ARMED: AtomicUsize = AtomicUsize::new(0);
/// Branch handles not yet dropped, ended or not; zero lets path resolution
/// skip the scope lookup.
static HELD: AtomicUsize = AtomicUsize::new(0);

pub(crate) struct BranchHandle {
    state: Mutex<BranchState>,
    /// True while the prefix replays.
    prefix: AtomicBool,
    /// Set when the host's guard drops. A scope that outlives it serves
    /// nothing, but its durable writes still stay in the branch.
    ended: AtomicBool,
    state_root: PathBuf,
    /// Recording-owned trees, longest first, with where each lands under
    /// `state_root`.
    protected: Vec<(PathBuf, PathBuf)>,
    recorded_workspace: Option<PathBuf>,
    workspace: PathBuf,
    source_roots: Vec<super::ReplaySourceRoot>,
    event_log: OnceLock<Option<Arc<AnyEventLog>>>,
}

impl std::fmt::Debug for BranchHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BranchHandle")
            .field("state_root", &self.state_root)
            .finish_non_exhaustive()
    }
}

impl BranchHandle {
    pub(super) fn arm(
        state: BranchState,
        state_root: PathBuf,
        mut protected: Vec<(PathBuf, PathBuf)>,
        recorded_workspace: Option<PathBuf>,
        source_roots: Vec<super::ReplaySourceRoot>,
    ) -> Arc<Self> {
        protected.sort_by_key(|(tree, _)| std::cmp::Reverse(tree.components().count()));
        ARMED.fetch_add(1, Ordering::AcqRel);
        HELD.fetch_add(1, Ordering::AcqRel);
        Arc::new(Self {
            workspace: state.workspace_root.clone(),
            recorded_workspace,
            source_roots,
            state: Mutex::new(state),
            prefix: AtomicBool::new(true),
            ended: AtomicBool::new(false),
            state_root,
            protected,
            event_log: OnceLock::new(),
        })
    }

    pub(super) fn end(&self) {
        if !self.ended.swap(true, Ordering::AcqRel) {
            ARMED.fetch_sub(1, Ordering::AcqRel);
        }
    }

    pub(super) fn state(&self) -> MutexGuard<'_, BranchState> {
        self.state.lock().expect("branch lock poisoned")
    }

    pub(super) fn in_prefix(&self) -> bool {
        self.prefix.load(Ordering::Acquire)
    }

    pub(super) fn go_live(&self) {
        self.prefix.store(false, Ordering::Release);
    }

    pub(crate) fn state_root(&self) -> &Path {
        &self.state_root
    }

    pub(crate) fn runtime_store(&self, path: PathBuf) -> crate::store::SharedStore {
        self.state().runtime_stores.at(path)
    }

    /// Resolve a physical branch asset back to the recorded source identity.
    pub(super) fn prompt_uri(&self, uri: &str) -> String {
        let path = Path::new(uri);
        for root in &self.source_roots {
            if let Some(relative) = workspace_relative(path, &root.restored) {
                return root.recorded.join(relative).display().to_string();
            }
        }
        match (
            &self.recorded_workspace,
            workspace_relative(path, &self.workspace),
        ) {
            (Some(recorded), Some(relative)) => recorded.join(relative).display().to_string(),
            _ => uri.to_string(),
        }
    }

    /// Where `path` lands for this branch, or `None` when it is outside every
    /// tree the branch owns or remaps.
    fn remap(&self, path: &Path) -> Option<PathBuf> {
        let path = crate::workspace_path::canonicalize_existing_ancestor(path)
            .unwrap_or_else(|_| path.to_path_buf());
        if path.starts_with(&self.state_root) {
            return Some(path);
        }
        if let Some(mapped) = self.protected.iter().find_map(|(tree, mount)| {
            path.strip_prefix(tree)
                .ok()
                .map(|rest| self.state_root.join(mount).join(rest))
        }) {
            return Some(mapped);
        }
        // The session store the host names now, which it may set only after
        // the branch starts, such as a fresh home per run.
        let named = crate::stdlib::process::read_env_value("HARN_SESSION_STORE_ROOT")
            .filter(|root| !root.trim().is_empty())?;
        let named = PathBuf::from(named);
        let named = crate::workspace_path::canonicalize_existing_ancestor(&named).unwrap_or(named);
        path.strip_prefix(&named)
            .ok()
            .map(|rest| self.state_root.join(rest))
    }

    /// The branch's own event log, opened on first use at its state root.
    fn event_log(&self) -> Option<Arc<AnyEventLog>> {
        self.event_log
            .get_or_init(|| {
                crate::event_log::EventLogConfig::for_state_root(&self.state_root)
                    .and_then(|config| crate::event_log::open_event_log(&config))
                    .map_err(|error| {
                        crate::events::log_warn("event_log.branch", &error.to_string());
                    })
                    .ok()
            })
            .clone()
    }
}

impl Drop for BranchHandle {
    fn drop(&mut self) {
        HELD.fetch_sub(1, Ordering::AcqRel);
    }
}

/// The branch the current execution belongs to, if any.
pub(crate) fn current_branch() -> Option<Arc<BranchHandle>> {
    if ARMED.load(Ordering::Acquire) == 0 {
        return None;
    }
    crate::persistent_state::current_branch().filter(|branch| !branch.ended.load(Ordering::Acquire))
}

/// The branch whose state the current execution writes, ended or not: work
/// that finishes after the host's guard drops, such as lifecycle cleanup,
/// still writes into the branch and never into the recording.
pub(crate) fn state_branch() -> Option<Arc<BranchHandle>> {
    if HELD.load(Ordering::Acquire) == 0 {
        return None;
    }
    crate::persistent_state::current_branch()
}

/// Resolve a recorded workspace path before the normal filesystem permission
/// check. Do not rewrite text or canonicalize against the original file tree:
/// a symlink there may have changed since recording.
pub(crate) fn workspace_path(path: PathBuf) -> Result<PathBuf, VmError> {
    let Some(branch) = state_branch() else {
        return Ok(path);
    };
    for root in &branch.source_roots {
        if let Some(relative) = workspace_relative(&path, &root.recorded) {
            return confined_mapping(&path, &root.restored, relative);
        }
    }
    let Some(recorded) = &branch.recorded_workspace else {
        return Ok(path);
    };
    if recorded == &branch.workspace {
        return Ok(path);
    }
    let Some(relative) = workspace_relative(&path, recorded) else {
        return Ok(path);
    };
    confined_mapping(&path, &branch.workspace, relative)
}

fn confined_mapping(path: &Path, root: &Path, relative: &Path) -> Result<PathBuf, VmError> {
    let mapped = root.join(relative);
    let resolved = crate::workspace_path::canonicalize_existing_ancestor(&mapped)
        .map_err(|error| VmError::Runtime(format!("branch workspace path: {error}")))?;
    if !resolved.starts_with(root) {
        return Err(VmError::Runtime(format!(
            "recorded workspace path {} escapes the isolated workspace",
            path.display()
        )));
    }
    Ok(mapped)
}

pub(super) fn validate_source_roots(
    mut roots: Vec<super::ReplaySourceRoot>,
    workspace: &Path,
    tape: &super::EventTape,
) -> Result<Vec<super::ReplaySourceRoot>, String> {
    let recorded_workspace = tape
        .header
        .run
        .as_ref()
        .and_then(|run| run.workspace_root.as_deref());
    let overlaps = |a: &Path, b: &Path| a.starts_with(b) || b.starts_with(a);
    for root in &mut roots {
        if !root.recorded.is_absolute()
            || root
                .recorded
                .components()
                .any(|part| matches!(part, std::path::Component::ParentDir))
        {
            return Err("recorded source root must be an absolute normalized path".into());
        }
        root.recorded = crate::workspace_path::canonicalize_existing_ancestor(&root.recorded)
            .map_err(|error| format!("recorded source root: {error}"))?;
        root.restored = root
            .restored
            .canonicalize()
            .map_err(|error| format!("restored source root must exist: {error}"))?;
        if !root.restored.is_dir()
            || overlaps(&root.recorded, &root.restored)
            || overlaps(&root.restored, workspace)
            || overlaps(&root.recorded, workspace)
            || recorded_workspace.is_some_and(|recorded| {
                overlaps(&root.recorded, recorded) || overlaps(&root.restored, recorded)
            })
        {
            return Err(
                "replay source roots must be separate from the original and replay workspaces"
                    .into(),
            );
        }
    }
    for (index, root) in roots.iter().enumerate() {
        if roots[..index].iter().any(|prior| {
            overlaps(&root.recorded, &prior.recorded)
                || overlaps(&root.restored, &prior.restored)
                || overlaps(&root.recorded, &prior.restored)
                || overlaps(&root.restored, &prior.recorded)
        }) {
            return Err("replay source roots must not overlap".into());
        }
    }
    Ok(roots)
}

pub(super) fn workspace_relative<'a>(path: &'a Path, root: &Path) -> Option<&'a Path> {
    if let Ok(relative) = path.strip_prefix(root) {
        return Some(relative);
    }
    // Resolve only root aliases. Descendant links belong to the isolated copy.
    for ancestor in path.ancestors() {
        let Ok(canonical) = ancestor.canonicalize() else {
            continue;
        };
        let Ok(suffix) = root.strip_prefix(&canonical) else {
            continue;
        };
        if let Ok(relative) = path.strip_prefix(ancestor.join(suffix)) {
            return Some(relative);
        }
    }
    None
}

/// A shell's path literals do not pass through the VM filesystem resolver.
/// Until a host supplies a filesystem namespace, do not execute them against
/// a moved workspace where recorded absolute paths could reach the original.
pub(crate) fn check_live_command_workspace() -> Result<(), VmError> {
    if state_branch().is_some_and(|branch| {
        branch
            .recorded_workspace
            .as_ref()
            .is_some_and(|root| root != &branch.workspace)
    }) {
        return Err(VmError::Runtime(
            "live commands in a relocated replay workspace require a filesystem namespace; recorded command paths cannot be safely rewritten".into(),
        ));
    }
    Ok(())
}

/// The scope a host enters around the execution that branches. It is `Send`,
/// so the host can carry it onto the thread or task that runs the VM.
#[derive(Clone)]
pub struct BranchScope {
    pub(super) handle: Arc<BranchHandle>,
}

impl BranchScope {
    /// Bind the branch for a synchronous stretch on this thread. Use
    /// [`BranchScope::run`] across async suspensions.
    pub fn enter(&self) -> crate::persistent_state::ScopedPersistentStateRoot {
        crate::persistent_state::enter_branch(self.handle.clone())
    }

    /// Run `future`, and every worker it spawns, inside the branch.
    pub async fn run<F: std::future::Future>(&self, future: F) -> F::Output {
        crate::persistent_state::run_in_branch(self.handle.clone(), future).await
    }
}

/// Where a durable writer puts `path`. Outside a branch, `path` itself. In a
/// branch, a path the branch owns or remaps lands under its state root; any
/// other path is outside the branch, refused in the prefix and reported live.
pub(crate) fn durable_path(path: &Path, writer: &str) -> Result<PathBuf, VmError> {
    let Some(branch) = state_branch() else {
        return Ok(path.to_path_buf());
    };
    if let Some(mapped) = branch.remap(path) {
        return Ok(mapped);
    }
    if branch.ended.load(Ordering::Acquire) {
        return Err(VmError::Runtime(format!(
            "{writer} would write {} after its branch ended, outside the branch",
            path.display()
        )));
    }
    let mut state = branch.state();
    if !state.live {
        let call = state.report.served_calls + 1;
        return Err(state.diverge(
            call,
            "durable state outside the branch",
            format!(
                "{writer} would write {}, which is neither the branch's state root nor a tree \
                 the recording owns",
                path.display()
            ),
        ));
    }
    let entry = format!("{writer}: {}", path.display());
    if !state.report.external_state_writes.contains(&entry) {
        state.report.external_state_writes.push(entry);
    }
    Ok(path.to_path_buf())
}

/// [`durable_path`] for a path resolver that cannot fail. A refused path
/// diverges the branch, and the write lands under the branch's state root
/// instead of the recording's.
pub(crate) fn branch_path(path: PathBuf, writer: &str) -> PathBuf {
    match durable_path(&path, writer) {
        Ok(mapped) => mapped,
        Err(_) => match state_branch() {
            Some(branch) => branch
                .state_root
                .join("refused")
                .join(writer.replace(' ', "-")),
            None => path,
        },
    }
}

/// The state root a scope that names `root` resolves to: inside a branch the
/// branch's own root, so a host scope entered within it cannot point back at
/// the recording's state.
pub(crate) fn scoped_state_root(branch: Option<&BranchHandle>, root: &Path) -> PathBuf {
    match branch {
        Some(branch) => branch.state_root.clone(),
        None => root.to_path_buf(),
    }
}

/// The event log a branch writes instead of a durable `active` one: `None`
/// outside a branch or for an in-memory log, which is process-local and stays
/// shared. `Some(None)` when the branch's own log cannot open: the branch then
/// writes no log rather than the host's.
#[allow(clippy::option_option)] // Outer: in a branch. Inner: its log opened.
pub(crate) fn branch_event_log(
    active: Option<&Arc<AnyEventLog>>,
) -> Option<Option<Arc<AnyEventLog>>> {
    if matches!(active.map(AsRef::as_ref), Some(AnyEventLog::Memory(_))) {
        return None;
    }
    Some(state_branch()?.event_log())
}

/// Refuse a state root that is, sits inside, or holds a tree the recording
/// owns, and one that already holds state; create it and return it canonical.
pub(super) fn fresh_state_root(root: &Path, protected: &[PathBuf]) -> Result<PathBuf, String> {
    let describe = |err: std::io::Error| format!("branch state root {}: {err}", root.display());
    let canonical =
        crate::workspace_path::canonicalize_existing_ancestor(root).map_err(describe)?;
    if let Some(tree) = protected
        .iter()
        .find(|tree| canonical.starts_with(tree) || tree.starts_with(&canonical))
    {
        return Err(format!(
            "branch state root {} overlaps the recording's {}; a branch keeps its state in a \
             tree of its own",
            root.display(),
            tree.display()
        ));
    }
    if root.exists() && std::fs::read_dir(root).map_err(describe)?.next().is_some() {
        return Err(format!(
            "branch state root {} is not empty; a branch keeps its state apart from the \
             recording's, so give it a new directory",
            root.display()
        ));
    }
    std::fs::create_dir_all(root).map_err(describe)?;
    root.canonicalize().map_err(describe)
}
