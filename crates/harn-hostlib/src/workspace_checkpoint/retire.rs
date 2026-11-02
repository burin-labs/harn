//! Retiring a session: the gate that serializes it with every checkpoint
//! operation, and the drop that takes its record off disk.

use super::*;

/// Serializes retiring a session with every operation that reads or installs
/// session state. [`drop_session`] holds it exclusively from the moment it
/// takes the session until its record is off disk, so no operation can see an
/// empty registry slot while the old index is still loadable, and none can act
/// on a session that is being retired. Operations hold it shared, so they still
/// run concurrently with each other.
static RETIREMENT: std::sync::RwLock<()> = std::sync::RwLock::new(());

thread_local! {
    /// The shared hold this thread already has, so an operation that runs
    /// inside another (a test hook, a fork's pre-publish callback) does not
    /// queue behind a waiting retirement it would then deadlock.
    static OPERATION_HOLD: std::cell::RefCell<(usize, Option<std::sync::RwLockReadGuard<'static, ()>>)> =
        const { std::cell::RefCell::new((0, None)) };
}

/// A shared hold on [`RETIREMENT`] for the length of one operation.
pub(super) struct OperationGuard;

pub(super) fn operation() -> OperationGuard {
    OPERATION_HOLD.with(|hold| {
        let mut hold = hold.borrow_mut();
        if hold.0 == 0 {
            let shared = match RETIREMENT.try_read() {
                Ok(shared) => shared,
                Err(std::sync::TryLockError::Poisoned(e)) => e.into_inner(),
                Err(std::sync::TryLockError::WouldBlock) => {
                    #[cfg(all(test, unix))]
                    super::tests::gate_wait_hook();
                    RETIREMENT.read().unwrap_or_else(|e| e.into_inner())
                }
            };
            hold.1 = Some(shared);
        }
        hold.0 += 1;
    });
    OperationGuard
}

impl Drop for OperationGuard {
    fn drop(&mut self) {
        OPERATION_HOLD.with(|hold| {
            let mut hold = hold.borrow_mut();
            hold.0 -= 1;
            if hold.0 == 0 {
                hold.1 = None;
            }
        });
    }
}

/// Drop a session's checkpoint state and stored blobs.
///
/// The session stays registered until its record is off disk, and the whole
/// retirement holds [`RETIREMENT`] exclusively: a concurrent resume, capture or
/// call admission waits for the outcome instead of finding the slot empty and
/// loading or replacing the history being retired.
pub fn drop_session(session_id: &str) -> DropReceipt {
    if OPERATION_HOLD.with(|hold| hold.borrow().0 > 0) {
        return DropReceipt {
            dropped: false,
            error: Some(
                "a checkpoint session cannot be dropped from inside a checkpoint operation"
                    .to_string(),
            ),
        };
    }
    let _retiring = RETIREMENT.write().unwrap_or_else(|e| e.into_inner());
    let Some(session) = find_session(session_id) else {
        return DropReceipt {
            dropped: false,
            error: None,
        };
    };
    let receipt = retire_record(session_id, &session);
    if receipt.dropped {
        sessions()
            .lock()
            .expect("workspace checkpoint registry poisoned")
            .remove(session_id);
    }
    receipt
}

/// Take `session`'s record off disk, or mark it interrupted when that fails.
fn retire_record(session_id: &str, session: &SharedSession) -> DropReceipt {
    #[cfg(all(test, unix))]
    tests::retirement_hook();
    let root = lock(session).root.clone();
    let dir = session_dir(&root, session_id);
    let removal = match stdfs::remove_dir_all(&dir) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => e,
        _ => {
            return DropReceipt {
                dropped: true,
                error: None,
            }
        }
    };
    // A partial delete can leave the index behind, and a later load would
    // resume it. Moving the directory aside needs only its parent writable, and
    // the index is then gone from the path every load reads.
    let aside = dir.with_file_name(format!(
        ".dropped-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos())
    ));
    match stdfs::rename(&dir, &aside) {
        Ok(()) => {
            let _ = stdfs::remove_dir_all(&aside);
            DropReceipt {
                dropped: true,
                error: None,
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => DropReceipt {
            dropped: true,
            error: None,
        },
        Err(rename) => {
            let error = format!(
                "the checkpoint record could not be removed ({removal}) or moved aside ({rename}), so a later load could resume it"
            );
            // The session stays loaded and interrupted: with it gone from
            // memory, the next call would read as having no checkpoint and run
            // while the record it should have retired is still on disk.
            lock(session).interrupted = Some(error.clone());
            DropReceipt {
                dropped: false,
                error: Some(error),
            }
        }
    }
}
