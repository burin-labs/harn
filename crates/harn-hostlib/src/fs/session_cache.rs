use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::{default_root, load_state, lock_sessions, manifest_path, HostlibError, SessionState};

type LockedMutation = (
    std::sync::MutexGuard<'static, SessionCache>,
    SessionState,
    Vec<harn_vm::conditional_replace::ConditionalReplaceLock>,
);

pub(super) fn mutation_state(
    session_id: &str,
    proposed_root: Option<PathBuf>,
    builtin: &'static str,
) -> Result<LockedMutation, HostlibError> {
    let guard = lock_sessions();
    let current = guard
        .get(session_id)
        .map_err(|message| HostlibError::Backend { builtin, message })?;
    let current_root = current
        .map(|state| state.root.clone())
        .or_else(|| proposed_root.clone())
        .unwrap_or_else(default_root);
    let mut manifests = vec![manifest_path(&current_root, session_id)];
    if let Some(root) = &proposed_root {
        manifests.push(manifest_path(root, session_id));
    }
    let locks = harn_vm::conditional_replace::acquire_locks(&manifests).map_err(|error| {
        HostlibError::Backend {
            builtin,
            message: format!("lock staged manifest: {error}"),
        }
    })?;
    let mut state = load_state(session_id, Some(current_root))
        .map_err(|message| HostlibError::Backend { builtin, message })?;
    if state.entries.is_empty() && state.atomic_groups.is_empty() {
        if let Some(root) = proposed_root.filter(|root| *root != state.root) {
            state = load_state(session_id, Some(root))
                .map_err(|message| HostlibError::Backend { builtin, message })?;
        }
    }
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

    pub(super) fn get(&self, id: &str) -> Result<Option<&SessionState>, String> {
        match self.0.get(id) {
            Some(CachedSession::Loaded(state)) => Ok(Some(state)),
            Some(CachedSession::Unavailable { reason, .. }) => Err(reason.clone()),
            None => Ok(None),
        }
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

    pub(super) fn configure(&mut self, id: &str, root: PathBuf) {
        match self.0.get_mut(id) {
            Some(CachedSession::Loaded(state)) => {
                if state.entries.is_empty() && state.atomic_groups.is_empty() {
                    state.root = root;
                }
            }
            Some(CachedSession::Unavailable { .. }) | None => {
                let slot = match load_state(id, Some(root.clone())) {
                    Ok(state) => CachedSession::Loaded(state),
                    Err(reason) => CachedSession::Unavailable { root, reason },
                };
                self.0.insert(id.to_string(), slot);
            }
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
