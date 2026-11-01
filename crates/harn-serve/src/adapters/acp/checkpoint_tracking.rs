//! Session-owned identities and capture failures for automatic boundaries.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

#[derive(Clone, Default)]
struct State {
    ids: HashSet<String>,
    failures: HashMap<String, String>,
    pending: HashSet<String>,
    closed: bool,
    unavailable: bool,
}

#[derive(Clone, Default)]
pub(super) struct CheckpointTracking(Arc<Mutex<State>>);

impl CheckpointTracking {
    pub(super) fn contains(&self, id: &str) -> Result<bool, &'static str> {
        let state = self
            .0
            .lock()
            .map_err(|_| "checkpoint tracking unavailable")?;
        if state.unavailable {
            return Err("checkpoint tracking unavailable");
        }
        Ok(state.ids.contains(id))
    }

    pub(super) fn reserve(&self, id: &str) -> Result<bool, &'static str> {
        let mut state = self
            .0
            .lock()
            .map_err(|_| "checkpoint tracking unavailable")?;
        if state.unavailable || state.closed {
            return Err("checkpoint tracking unavailable");
        }
        Ok(state.ids.insert(id.to_string()))
    }

    pub(super) fn fail(&self, id: &str, detail: String) {
        if let Ok(mut state) = self.0.lock() {
            state.failures.insert(id.to_string(), detail);
        }
    }

    #[cfg(any(test, feature = "hostlib"))]
    pub(super) fn failure(&self, id: &str) -> Result<Option<String>, &'static str> {
        let state = self
            .0
            .lock()
            .map_err(|_| "checkpoint tracking unavailable")?;
        if state.unavailable {
            return Err("checkpoint tracking unavailable");
        }
        if state.pending.contains(id) {
            return Ok(Some("message delivery has not finished".to_string()));
        }
        Ok(state.failures.get(id).cloned())
    }

    pub(super) fn fork(&self, inherit: bool) -> Self {
        let mut copy = match self.0.lock() {
            Ok(state) => state.clone(),
            Err(_) => State {
                unavailable: true,
                ..State::default()
            },
        };
        if !inherit {
            copy.failures.clear();
            copy.pending.clear();
        } else {
            for id in copy.pending.drain() {
                copy.failures.insert(
                    id,
                    "message delivery was incomplete when the session forked".to_string(),
                );
            }
        }
        copy.closed = false;
        Self(Arc::new(Mutex::new(copy)))
    }

    /// Claim boundary ids an earlier process recorded for this session, so a
    /// prompt cannot reuse one after a restart.
    #[cfg(feature = "hostlib")]
    pub(super) fn adopt(&self, ids: impl IntoIterator<Item = String>) {
        if let Ok(mut state) = self.0.lock() {
            state.ids.extend(ids);
        }
    }

    pub(super) fn close(&self) {
        if let Ok(mut state) = self.0.lock() {
            state.closed = true;
        }
    }

    #[cfg(feature = "hostlib")]
    pub(super) fn drop_checkpoint(
        &self,
        session_id: &str,
    ) -> Result<harn_hostlib::workspace_checkpoint::DropReceipt, &'static str> {
        let state = self
            .0
            .lock()
            .map_err(|_| "checkpoint tracking unavailable")?;
        if state.unavailable {
            return Err("checkpoint tracking unavailable");
        }
        Ok(harn_hostlib::workspace_checkpoint::drop_session(session_id))
    }

    #[cfg(feature = "hostlib")]
    pub(super) fn capture_boundary(
        &self,
        session_id: &str,
        message_id: &str,
        root: &std::path::Path,
        store_scope: &harn_vm::SessionStoreScope,
        already_reserved: bool,
    ) {
        let Ok(mut state) = self.0.lock() else { return };
        if state.closed || state.unavailable {
            return;
        }
        if already_reserved && !state.ids.contains(message_id) {
            state.failures.insert(
                message_id.to_string(),
                "checkpoint identity was not reserved".to_string(),
            );
            return;
        }
        if !already_reserved && !state.ids.insert(message_id.to_string()) {
            state.failures.insert(
                message_id.to_string(),
                "messageId was already used by this checkpoint lineage".to_string(),
            );
            return;
        }
        if !already_reserved {
            state.pending.insert(message_id.to_string());
        }
        let _scope = store_scope.enter();
        let capture =
            harn_hostlib::workspace_checkpoint::capture(session_id, message_id, Some(root));
        let failure = match capture {
            Ok(harn_hostlib::workspace_checkpoint::Outcome::Ready(_)) => None,
            Ok(harn_hostlib::workspace_checkpoint::Outcome::Unavailable(reason)) => {
                Some(format!("{}: {}", reason.reason, reason.detail))
            }
            Err(error) => Some(error.to_string()),
        };
        if let Some(failure) = failure {
            state.failures.insert(message_id.to_string(), failure);
        }
    }

    #[cfg(any(test, feature = "hostlib"))]
    pub(super) fn finish_injection(&self, message_id: &str, delivered: bool) {
        if let Ok(mut state) = self.0.lock() {
            state.pending.remove(message_id);
            if !delivered {
                state.failures.insert(
                    message_id.to_string(),
                    "message delivery failed".to_string(),
                );
            }
        }
    }
}

/// Bracket one admitted tool execution with the session's checkpoint calls, so
/// a restore plan names the paths that call changed. A session with no captured
/// boundary, or a call id already in flight, opens no window.
#[cfg(feature = "hostlib")]
pub(super) fn attribute_tool_execution(
    session_id: &str,
    store_scope: &harn_vm::SessionStoreScope,
    tool_call_id: &str,
    side_effect: harn_vm::tool_annotations::SideEffectLevel,
    stage: harn_vm::bridge::ToolExecutionStage,
) -> harn_vm::bridge::ToolExecutionAdmission {
    use harn_hostlib::workspace_checkpoint::{self as checkpoint, Outcome};
    use harn_vm::bridge::ToolExecutionAdmission;
    let _scope = store_scope.enter();
    match stage {
        harn_vm::bridge::ToolExecutionStage::Begin => {
            let failure = match checkpoint::call_begin(session_id, tool_call_id) {
                Ok(Outcome::Ready(_)) => return ToolExecutionAdmission::Tracked,
                Ok(Outcome::Unavailable(reason)) if reason.reason == "no_checkpoint" => {
                    return ToolExecutionAdmission::Untracked
                }
                Ok(Outcome::Unavailable(reason)) => format!("{}: {}", reason.reason, reason.detail),
                // A rejected request (a call id already in flight, an invalid
                // id) is the caller's error, not a recording failure: refuse
                // it and leave the session's history alone.
                Err(error) => return ToolExecutionAdmission::Refused(error.to_string()),
            };
            // Tracking is optional; the person's tool call is not. A call that
            // runs untracked makes every earlier boundary unsafe to restore, so
            // drop them, on disk too, or a later load would resume them.
            // Boundaries captured after this call stay valid. Only when the old
            // record cannot be removed does the call wait, since running it
            // would leave a restorable boundary that misses its changes.
            let dropped = checkpoint::drop_session(session_id);
            if let Some(error) = dropped.error {
                tracing::warn!(session_id, tool_call_id, detail = %failure, "workspace checkpoint tracking failed");
                return ToolExecutionAdmission::Refused(error);
            }
            tracing::warn!(
                session_id,
                tool_call_id,
                detail = %failure,
                "workspace checkpoint tracking failed; the tool call runs untracked"
            );
            ToolExecutionAdmission::Untracked
        }
        harn_vm::bridge::ToolExecutionStage::End => {
            match checkpoint::call_end(session_id, tool_call_id, Some(side_effect)) {
                Ok(Outcome::Ready(receipt)) => match receipt.persistence_error {
                    Some(error) => ToolExecutionAdmission::Refused(error),
                    None => ToolExecutionAdmission::Tracked,
                },
                // Another call's tracking failure dropped the session while
                // this one ran; its history is gone, so this call is untracked.
                Ok(Outcome::Unavailable(reason)) if reason.reason == "no_checkpoint" => {
                    ToolExecutionAdmission::Untracked
                }
                Ok(Outcome::Unavailable(reason)) => {
                    ToolExecutionAdmission::Refused(format!("{}: {}", reason.reason, reason.detail))
                }
                Err(error) => ToolExecutionAdmission::Refused(error.to_string()),
            }
        }
        _ => {
            ToolExecutionAdmission::Refused(format!("unsupported tool execution stage: {stage:?}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(all(feature = "hostlib", unix))]
    fn find_index(dir: &std::path::Path) -> Option<std::path::PathBuf> {
        std::fs::read_dir(dir).ok()?.flatten().find_map(|entry| {
            let path = entry.path();
            if path.is_dir() {
                find_index(&path)
            } else {
                (path.file_name()? == "index.json").then_some(path)
            }
        })
    }

    #[cfg(all(feature = "hostlib", unix))]
    #[test]
    fn a_tool_call_runs_when_checkpoint_tracking_cannot_record_it() {
        use harn_hostlib::workspace_checkpoint::{self as checkpoint, Outcome};
        use harn_vm::bridge::{ToolExecutionAdmission, ToolExecutionStage};
        use harn_vm::tool_annotations::SideEffectLevel;
        use std::os::unix::fs::PermissionsExt;

        let root = tempfile::tempdir().unwrap();
        for args in [
            &["init", "-q"][..],
            &["commit", "-q", "--allow-empty", "-m", "base"],
        ] {
            let status = std::process::Command::new("git")
                .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
                .args(args)
                .current_dir(root.path())
                .status()
                .unwrap();
            assert!(status.success());
        }
        let scope = harn_vm::SessionStoreScope::resolve(
            root.path(),
            &harn_vm::security::LauncherEnvironment::from_snapshot(Default::default()),
        )
        .unwrap();
        let session = "tracking-fails-open";
        let _entered = scope.enter();
        let captured = checkpoint::capture(session, "message-1", Some(root.path())).unwrap();
        assert!(matches!(captured, Outcome::Ready(_)));
        let session_dir = find_index(root.path())
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf();
        let checkpoints = session_dir.parent().unwrap().to_path_buf();
        let mode = |path: &std::path::Path, bits| {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(bits)).unwrap();
        };
        let begin = |call: &str| {
            attribute_tool_execution(
                session,
                &scope,
                call,
                SideEffectLevel::WorkspaceWrite,
                ToolExecutionStage::Begin,
            )
        };

        // A call id already in flight is the caller's error: it is refused and
        // the session keeps its history.
        assert_eq!(begin("call-0"), ToolExecutionAdmission::Tracked);
        assert!(
            matches!(begin("call-0"), ToolExecutionAdmission::Refused(detail) if detail.contains("already in flight"))
        );
        assert!(checkpoint::root_matches(session, root.path()).is_some());

        // Neither delete nor move-aside can retire the old record, so running
        // the call would leave a restorable boundary that misses its edit.
        mode(&session_dir, 0o500);
        mode(&checkpoints, 0o500);
        let refused = [begin("call-1"), begin("call-1b")];
        mode(&checkpoints, 0o700);
        mode(&session_dir, 0o700);
        for admission in &refused {
            assert!(
                matches!(admission, ToolExecutionAdmission::Refused(detail) if detail.contains("later load could resume")),
                "a retry must not read as having no checkpoint: {admission:?}"
            );
        }

        // The index cannot be written, but its directory can be moved aside:
        // the call runs untracked and no load can bring the old boundary back.
        assert!(session_dir.join("index.json").exists() || find_index(&session_dir).is_some());
        mode(&session_dir, 0o500);
        assert_eq!(begin("call-2"), ToolExecutionAdmission::Untracked);
        std::fs::write(root.path().join("edited.txt"), "untracked edit").unwrap();
        for entry in std::fs::read_dir(&checkpoints).unwrap().flatten() {
            mode(&entry.path(), 0o700);
        }
        assert!(!session_dir.exists(), "the old record left the load path");
        // The call that was in flight when the session was dropped finishes
        // untracked, not as a tracking failure.
        assert_eq!(
            attribute_tool_execution(
                session,
                &scope,
                "call-0",
                SideEffectLevel::WorkspaceWrite,
                ToolExecutionStage::End,
            ),
            ToolExecutionAdmission::Untracked
        );
        let resumed = checkpoint::resume(session, root.path()).unwrap();
        assert!(
            matches!(&resumed, Outcome::Unavailable(reason) if reason.reason == "no_checkpoint"),
            "a load must not bring back a boundary from before an untracked call"
        );
        let plan = checkpoint::plan(session, "message-1").unwrap();
        assert!(
            matches!(&plan, Outcome::Unavailable(reason) if reason.reason == "no_checkpoint"),
            "a boundary from before an untracked call must not be restorable"
        );
    }

    #[test]
    fn fork_does_not_promote_an_incomplete_delivery_to_a_valid_boundary() {
        let tracking = CheckpointTracking::default();
        {
            let mut state = tracking.0.lock().unwrap();
            state.ids.insert("message".to_string());
            state.pending.insert("message".to_string());
        }
        assert!(tracking.failure("message").unwrap().is_some());
        let fork = tracking.fork(true);
        tracking.finish_injection("message", true);
        assert!(tracking.failure("message").unwrap().is_none());
        assert!(fork.failure("message").unwrap().is_some());
        assert!(fork.contains("message").unwrap());
    }
}
