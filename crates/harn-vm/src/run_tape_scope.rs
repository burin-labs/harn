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

pub(crate) struct BranchHandle {
    state: Mutex<BranchState>,
    /// True while the prefix replays.
    prefix: AtomicBool,
    /// Set when the host's guard drops; a scope that outlives it is inert.
    ended: AtomicBool,
    state_root: PathBuf,
    /// Recording-owned trees, longest first, with where each lands under
    /// `state_root`.
    protected: Vec<(PathBuf, PathBuf)>,
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
    ) -> Arc<Self> {
        protected.sort_by_key(|(tree, _)| std::cmp::Reverse(tree.components().count()));
        ARMED.fetch_add(1, Ordering::AcqRel);
        Arc::new(Self {
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

    /// Where `path` lands for this branch, or `None` when it is outside every
    /// tree the branch owns or remaps.
    fn remap(&self, path: &Path) -> Option<PathBuf> {
        let path = crate::workspace_path::canonicalize_existing_ancestor(path)
            .unwrap_or_else(|_| path.to_path_buf());
        if path.starts_with(&self.state_root) {
            return Some(path);
        }
        self.protected.iter().find_map(|(tree, mount)| {
            path.strip_prefix(tree)
                .ok()
                .map(|rest| self.state_root.join(mount).join(rest))
        })
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

/// The branch the current execution belongs to, if any.
pub(crate) fn current_branch() -> Option<Arc<BranchHandle>> {
    if ARMED.load(Ordering::Acquire) == 0 {
        return None;
    }
    crate::persistent_state::current_branch().filter(|branch| !branch.ended.load(Ordering::Acquire))
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
    let Some(branch) = current_branch() else {
        return Ok(path.to_path_buf());
    };
    if let Some(mapped) = branch.remap(path) {
        return Ok(mapped);
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

/// [`durable_path`] for a path resolver that cannot fail. A refused path is
/// still returned, but the branch has diverged and fails closed at its next
/// model call.
pub(crate) fn branch_path(path: PathBuf, writer: &str) -> PathBuf {
    durable_path(&path, writer).unwrap_or(path)
}

/// The state root a scope that names `root` resolves to: inside a branch the
/// branch's own root, so a host scope entered within it cannot point back at
/// the recording's state.
pub(crate) fn scoped_state_root(branch: Option<&BranchHandle>, root: &Path) -> PathBuf {
    match branch {
        Some(branch) if !branch.ended.load(Ordering::Acquire) => branch.state_root.clone(),
        _ => root.to_path_buf(),
    }
}

/// The event log a branch writes instead of a durable `active` one. An
/// in-memory log is process-local and stays shared.
pub(crate) fn branch_event_log(active: Option<&Arc<AnyEventLog>>) -> Option<Arc<AnyEventLog>> {
    if matches!(active.map(AsRef::as_ref), Some(AnyEventLog::Memory(_))) {
        return None;
    }
    current_branch()?.event_log()
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
