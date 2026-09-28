//! Memo of the normalized root sets a filesystem scope check compares against.
//!
//! [`super::check_fs_path_scope`] runs on every `harness.fs.*` call. Deriving
//! its roots canonicalizes each workspace root, the git-topology and runtime
//! extensions, every read-only root, and about thirty credential deny paths,
//! most of which do not exist and so also walk up to an existing parent. A
//! host step that stats a few thousand workspace entries paid that thousands
//! of times per turn (harn#8963). The inputs change far less often than the
//! calls arrive, so the derived sets are reused while the inputs are equal.
//!
//! A derived set can go stale when a path component it resolved is created or
//! replaced, for example a credential directory re-created as a symlink. Two
//! bounds keep that window short: a write or delete check always derives
//! fresh and clears the memo, so the next read after a scoped mutation sees
//! the tree the mutation left; and a memo older than [`MEMO_TTL`] is never
//! reused, which bounds changes made by child processes or other programs.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

use super::FsAccess;
use crate::orchestration::CapabilityPolicy;

/// Longest a derived root set is reused.
pub(super) const MEMO_TTL: Duration = Duration::from_millis(500);

/// The root sets one scope check compares a candidate path against.
pub(super) struct ScopeRoots {
    pub(super) workspace: Vec<PathBuf>,
    pub(super) read_only: Vec<PathBuf>,
    pub(super) read_deny: Vec<PathBuf>,
}

/// Every input the derivation reads besides the filesystem itself.
#[derive(Clone, PartialEq, Eq)]
struct MemoKey {
    workspace_roots: Vec<String>,
    read_only_roots: Vec<String>,
    read_deny_roots: Vec<String>,
    anchor_roots: Option<Vec<PathBuf>>,
    project_root: Option<PathBuf>,
    execution_root: PathBuf,
    home: Option<PathBuf>,
}

impl MemoKey {
    fn current(policy: &CapabilityPolicy) -> Self {
        Self {
            workspace_roots: policy.workspace_roots.clone(),
            read_only_roots: policy.read_only_roots.clone(),
            read_deny_roots: policy.process_sandbox.read_deny_roots.clone(),
            anchor_roots: super::current_session_anchor_workspace_roots(),
            project_root: super::project_root_workspace_root(),
            execution_root: crate::stdlib::process::execution_root_path(),
            home: crate::user_dirs::home_dir(),
        }
    }
}

thread_local! {
    static MEMO: RefCell<Option<(MemoKey, Instant, Rc<ScopeRoots>)>> = const { RefCell::new(None) };
    #[cfg(test)]
    static DERIVATIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn derive(policy: &CapabilityPolicy) -> Rc<ScopeRoots> {
    #[cfg(test)]
    DERIVATIONS.with(|count| count.set(count.get() + 1));
    Rc::new(ScopeRoots {
        workspace: super::normalized_workspace_roots(policy),
        read_only: super::normalized_read_only_roots(policy),
        read_deny: super::refusal::process_sandbox_read_deny_roots(policy),
    })
}

/// The root sets for a check of `access` under `policy`.
///
/// A read reuses a memo whose inputs match and that is younger than
/// [`MEMO_TTL`]. A write or delete derives fresh and leaves no memo behind.
pub(super) fn scope_roots(policy: &CapabilityPolicy, access: FsAccess) -> Rc<ScopeRoots> {
    if access != FsAccess::Read {
        MEMO.with(|memo| memo.borrow_mut().take());
        return derive(policy);
    }
    let key = MemoKey::current(policy);
    let reused = MEMO.with(|memo| {
        memo.borrow()
            .as_ref()
            .filter(|(memo_key, at, _)| *memo_key == key && at.elapsed() < MEMO_TTL)
            .map(|(_, _, roots)| Rc::clone(roots))
    });
    if let Some(roots) = reused {
        return roots;
    }
    let roots = derive(policy);
    MEMO.with(|memo| *memo.borrow_mut() = Some((key, Instant::now(), Rc::clone(&roots))));
    roots
}

#[cfg(test)]
pub(super) fn derivations() -> usize {
    DERIVATIONS.with(std::cell::Cell::get)
}

/// Backdate the memo by `by`, so a test can cross [`MEMO_TTL`] without sleeping.
#[cfg(test)]
pub(super) fn age(by: Duration) {
    MEMO.with(|memo| {
        if let Some((_, at, _)) = memo.borrow_mut().as_mut() {
            *at = at.checked_sub(by).expect("backdated instant");
        }
    });
}

#[cfg(test)]
pub(super) fn clear() {
    MEMO.with(|memo| memo.borrow_mut().take());
}
