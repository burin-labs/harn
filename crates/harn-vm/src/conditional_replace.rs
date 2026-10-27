//! Native state namespace and mutation evidence for file replacement.

use std::{io, path::Path};

pub use harn_vm_core::conditional_replace::{
    require_parent, scope_conditional_replace_lock_root, ConditionalReplaceOptions,
    ConditionalReplaceReceipt, ConditionalReplaceStatus, ScopedConditionalReplaceLockRoot,
    CONDITIONAL_REPLACE_LOCK_DIRECTORY,
};

pub(crate) fn scope_native_lock_root() -> Option<ScopedConditionalReplaceLockRoot> {
    if harn_vm_core::conditional_replace::scoped_conditional_replace_lock_root().is_some() {
        return None;
    }
    let runtime_root = crate::stdlib::process::runtime_root_base();
    Some(scope_conditional_replace_lock_root(
        crate::runtime_paths::state_root(&runtime_root).join(CONDITIONAL_REPLACE_LOCK_DIRECTORY),
    ))
}

pub(crate) fn acquire_lock(
    path: &Path,
) -> io::Result<harn_vm_core::conditional_replace::ConditionalReplaceLock> {
    let _scope = scope_native_lock_root();
    harn_vm_core::conditional_replace::acquire_lock(path)
}

pub fn conditional_replace(
    path: &Path,
    contents: &[u8],
    options: &ConditionalReplaceOptions,
) -> io::Result<ConditionalReplaceReceipt> {
    conditional_replace_with_hook(path, contents, options, || {})
}

/// Snapshot the pre-image while locked and record successful native mutations.
pub fn conditional_replace_with_hook<F: FnOnce()>(
    path: &Path,
    contents: &[u8],
    options: &ConditionalReplaceOptions,
    before_write: F,
) -> io::Result<ConditionalReplaceReceipt> {
    let _scope = scope_native_lock_root();
    let receipt = harn_vm_core::conditional_replace::conditional_replace_with_hook(
        path,
        contents,
        options,
        before_write,
    )?;
    if matches!(
        receipt.status,
        ConditionalReplaceStatus::Created | ConditionalReplaceStatus::Replaced
    ) {
        crate::agent_sessions::record_tool_call_mutation(&path.to_string_lossy());
    }
    Ok(receipt)
}

pub(crate) fn conditional_replace_with_io<R, W, F>(
    path: &Path,
    contents: &[u8],
    options: &ConditionalReplaceOptions,
    read: R,
    write: W,
    before_write: F,
) -> io::Result<ConditionalReplaceReceipt>
where
    R: FnOnce(&Path) -> io::Result<Vec<u8>>,
    W: FnOnce(
        &Path,
        &[u8],
        crate::atomic_io::AtomicWriteDurability,
        bool,
    ) -> io::Result<crate::atomic_io::AtomicWriteReceipt>,
    F: FnOnce(),
{
    let _scope = scope_native_lock_root();
    harn_vm_core::conditional_replace::conditional_replace_with_io(
        path,
        contents,
        options,
        read,
        write,
        before_write,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_lock_namespace_uses_the_scoped_persistent_store() {
        let directory = tempfile::tempdir().unwrap();
        let state = directory.path().join("state");
        let _store = crate::persistent_state::scope_persistent_state_root(
            crate::persistent_state::PersistentStateRoot::new(&state),
        );
        assert!(
            harn_vm_core::conditional_replace::scoped_conditional_replace_lock_root().is_none()
        );
        {
            let _scope = scope_native_lock_root().expect("native state scope installed");
            assert_eq!(
                harn_vm_core::conditional_replace::scoped_conditional_replace_lock_root(),
                Some(state.join("fs-cas-locks"))
            );
            let _holder = acquire_lock(&directory.path().join("document")).unwrap();
            assert_eq!(
                std::fs::read_dir(state.join("fs-cas-locks"))
                    .unwrap()
                    .count(),
                1
            );
        }
        assert!(
            harn_vm_core::conditional_replace::scoped_conditional_replace_lock_root().is_none()
        );
    }

    #[test]
    fn native_and_core_replacements_share_the_explicit_lock_namespace() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("locks");
        let destination = directory.path().join("state.json");
        let _scope = scope_conditional_replace_lock_root(&root);
        let holder = acquire_lock(&destination).unwrap();
        let entries = std::fs::read_dir(&root)
            .unwrap()
            .collect::<io::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(entries.len(), 1);
        let contender = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(entries[0].path())
            .unwrap();
        assert!(matches!(
            contender.try_lock(),
            Err(std::fs::TryLockError::WouldBlock)
        ));
        drop(holder);
        harn_flock::assert_released(&contender);
        contender.unlock().unwrap();
        let receipt = harn_vm_core::conditional_replace::conditional_replace(
            &destination,
            b"from core",
            &ConditionalReplaceOptions::default(),
        )
        .unwrap();
        assert_eq!(receipt.status, ConditionalReplaceStatus::Created);
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1);
        assert_eq!(std::fs::read(destination).unwrap(), b"from core");
    }
}
