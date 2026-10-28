//! Independent checkpoint history for a conversation fork.

use super::*;

/// Result of copying a session's checkpoint history to a fork.
#[derive(Debug, Serialize)]
pub struct ForkReceipt {
    /// Always `true`.
    pub available: bool,
    /// Number of copied turn boundaries.
    pub boundaries: usize,
    /// Number of copied call records.
    pub calls: usize,
}

/// Copy checkpoint history to a fork without sharing mutable state or blobs.
pub fn fork_session(
    source_id: &str,
    target_id: &str,
    target_root: &Path,
) -> Result<Outcome<ForkReceipt>, HostlibError> {
    fork_session_checked(source_id, target_id, target_root, || {})
}

pub(super) fn fork_session_checked(
    source_id: &str,
    target_id: &str,
    target_root: &Path,
    before_publish: impl FnOnce(),
) -> Result<Outcome<ForkReceipt>, HostlibError> {
    const FORK: &str = "workspace_checkpoint_fork";
    validate_session_id(FORK, source_id)?;
    validate_session_id(FORK, target_id)?;
    if source_id == target_id {
        return Err(HostlibError::InvalidParameter {
            builtin: FORK,
            param: "target_id",
            message: "must differ from the source session".to_string(),
        });
    }
    if find_session(target_id).is_some() {
        return Err(HostlibError::InvalidParameter {
            builtin: FORK,
            param: "target_id",
            message: "already has checkpoint history".to_string(),
        });
    }
    let Some(source) = find_session(source_id) else {
        return Ok(no_checkpoint(source_id));
    };
    let (root, writer_policy, read_parent_ignores, source_blobs, boundaries, calls, revision) = {
        let state = lock(&source);
        if state.root != resolve_root(Some(target_root)) {
            return Ok(unavailable(
                "workspace_root_changed",
                "the checkpoint belongs to a different workspace root",
            ));
        }
        if let Some(detail) = pending_calls_detail(&state) {
            return Ok(unavailable("calls_in_flight", detail));
        }
        (
            state.root.clone(),
            state.writer_policy,
            state.read_parent_ignores,
            state.blob_dir.clone(),
            state.boundaries.clone(),
            state.calls.clone(),
            state.revision,
        )
    };
    let target_dir = session_dir(&root, target_id);
    let parent_dir = target_dir.parent().expect("session directory has a parent");
    stdfs::create_dir_all(parent_dir)
        .map_err(|error| backend(FORK)(format!("create checkpoint directory: {error}")))?;
    let staging = tempfile::Builder::new()
        .prefix(".fork-")
        .tempdir_in(parent_dir)
        .map_err(|error| backend(FORK)(format!("create checkpoint staging directory: {error}")))?;
    let staging_blobs = staging.path().join("blobs");
    let target_blobs = target_dir.join("blobs");
    let hashes: BTreeSet<&str> = boundaries
        .iter()
        .flat_map(|boundary| boundary.stored.values().map(|file| file.hash.as_str()))
        .collect();
    for hash in &hashes {
        let bytes = stdfs::read(source_blobs.join(hash))
            .map_err(|error| backend(FORK)(format!("read checkpoint blob: {error}")))?;
        if sha256_hex(&bytes) != *hash {
            return Err(backend(FORK)(
                "source checkpoint blob hash mismatch".to_string(),
            ));
        }
        store_blob(&staging_blobs, hash, &bytes).map_err(backend(FORK))?;
        let copied = stdfs::read(staging_blobs.join(hash))
            .map_err(|error| backend(FORK)(format!("read copied checkpoint blob: {error}")))?;
        if sha256_hex(&copied) != *hash {
            return Err(backend(FORK)(
                "copied checkpoint blob hash mismatch".to_string(),
            ));
        }
    }
    let receipt = ForkReceipt {
        available: true,
        boundaries: boundaries.len(),
        calls: calls.len(),
    };
    before_publish();
    let mut registry = sessions()
        .lock()
        .expect("workspace checkpoint registry poisoned");
    if registry.contains_key(target_id) {
        return Err(HostlibError::InvalidParameter {
            builtin: FORK,
            param: "target_id",
            message: "already has checkpoint history".to_string(),
        });
    }
    if !registry
        .get(source_id)
        .is_some_and(|current| Arc::ptr_eq(current, &source))
    {
        return Ok(unavailable(
            "source_changed",
            "the source checkpoint session was replaced",
        ));
    }
    let state = lock(&source);
    if let Some(detail) = pending_calls_detail(&state) {
        return Ok(unavailable("calls_in_flight", detail));
    }
    if state.revision != revision {
        return Ok(unavailable(
            "source_changed",
            "the source checkpoint changed while it was being copied",
        ));
    }
    stdfs::create_dir(&target_dir)
        .map_err(|error| backend(FORK)(format!("create fork checkpoint directory: {error}")))?;
    let publish = (|| -> Result<(), String> {
        if !hashes.is_empty() {
            stdfs::rename(&staging_blobs, &target_blobs)
                .map_err(|error| format!("publish checkpoint blobs: {error}"))?;
        }
        Ok(())
    })();
    if let Err(error) = publish {
        if let Err(cleanup) = stdfs::remove_dir_all(&target_dir) {
            return Err(backend(FORK)(format!(
                "{error}; remove {}: {cleanup}",
                target_dir.display()
            )));
        }
        return Err(backend(FORK)(error));
    }
    registry.insert(
        target_id.to_string(),
        Arc::new(Mutex::new(Session {
            root,
            writer_policy,
            read_parent_ignores,
            blob_dir: target_blobs,
            boundaries,
            calls,
            pending: HashMap::new(),
            revision: 0,
        })),
    );
    Ok(Outcome::Ready(receipt))
}
