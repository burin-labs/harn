use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::{default_root, load_state, lock_sessions, manifest_path, HostlibError, SessionState};

type LockedSession = (
    std::sync::MutexGuard<'static, SessionCache>,
    SessionState,
    Vec<harn_vm::conditional_replace::ConditionalReplaceLock>,
);

pub(super) fn locked_state(
    session_id: &str,
    proposed_root: Option<PathBuf>,
    builtin: &'static str,
) -> Result<LockedSession, HostlibError> {
    let mut guard = lock_sessions();
    let current_root = guard
        .root(session_id)
        .map(Path::to_path_buf)
        .or_else(|| proposed_root.clone())
        .unwrap_or_else(default_root);
    let mut manifests = vec![manifest_path(&current_root, session_id)];
    if let Some(root) = &proposed_root {
        manifests.push(manifest_path(root, session_id));
    }
    let locks = match harn_vm::conditional_replace::acquire_locks(&manifests) {
        Ok(locks) => locks,
        Err(error) => {
            let message = format!("lock staged manifest: {error}");
            guard.unavailable(session_id, current_root, message.clone());
            return Err(HostlibError::Backend { builtin, message });
        }
    };
    let mut load = |root: PathBuf| {
        load_state(session_id, Some(root.clone())).map_err(|message| {
            guard.unavailable(session_id, root, message.clone());
            HostlibError::Backend { builtin, message }
        })
    };
    let mut state = load(current_root)?;
    if state.entries.is_empty() && state.atomic_groups.is_empty() {
        if let Some(root) = proposed_root.filter(|root| *root != state.root) {
            state = load(root)?;
        }
    }
    guard.insert(session_id.to_string(), state.clone());
    Ok((guard, state, locks))
}

enum CachedSession {
    Loaded(SessionState),
    Unavailable { root: PathBuf, reason: String },
}

#[derive(Default)]
pub(super) struct SessionCache(BTreeMap<String, CachedSession>);

impl SessionCache {
    pub(super) fn insert(&mut self, id: String, state: SessionState) {
        self.0.insert(id, CachedSession::Loaded(state));
    }

    fn unavailable(&mut self, id: &str, root: PathBuf, reason: String) {
        self.0
            .insert(id.to_string(), CachedSession::Unavailable { root, reason });
    }

    pub(super) fn remove(&mut self, id: &str) -> Result<Option<SessionState>, String> {
        if let Some(CachedSession::Unavailable { reason, .. }) = self.0.get(id) {
            return Err(reason.clone());
        }
        match self.0.remove(id) {
            Some(CachedSession::Loaded(state)) => Ok(Some(state)),
            Some(CachedSession::Unavailable { .. }) => unreachable!("unavailable state retained"),
            None => Ok(None),
        }
    }

    pub(super) fn root(&self, id: &str) -> Option<&Path> {
        match self.0.get(id)? {
            CachedSession::Loaded(state) => Some(&state.root),
            CachedSession::Unavailable { root, .. } => Some(root),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::{configure_session_root, manifest_path, read, read_dir};

    #[test]
    fn unavailable_manifest_does_not_fall_through_to_disk_reads() {
        let root = tempfile::tempdir().unwrap();
        let session = format!("invalid-manifest-{}", uuid::Uuid::new_v4());
        let path = root.path().join("document");
        std::fs::write(&path, b"physical disk is not the staged view").unwrap();
        let manifest = manifest_path(root.path(), &session);
        std::fs::create_dir_all(manifest.parent().unwrap()).unwrap();
        std::fs::write(manifest, b"invalid manifest").unwrap();
        configure_session_root(&session, root.path());
        assert!(read(&path, Some(&session))
            .expect("explicit failure, not disk fallback")
            .is_err());
        assert!(read_dir(root.path(), Some(&session))
            .expect("explicit directory failure")
            .is_err());
    }
}
