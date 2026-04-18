use std::path::PathBuf;
use std::process::Command;

use super::{WorkerExecutionProfile, WorkerWorktreeSpec};
use crate::value::VmError;

pub(super) fn infer_worktree_path(
    worker_id: &str,
    spec: &WorkerWorktreeSpec,
) -> Result<String, VmError> {
    if let Some(path) = &spec.path {
        return Ok(path.clone());
    }
    let repo_name = PathBuf::from(&spec.repo)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("repo")
        .to_string();
    let base_dir = crate::stdlib::process::current_execution_context()
        .and_then(|context| context.cwd.map(PathBuf::from))
        .or_else(|| crate::stdlib::process::VM_SOURCE_DIR.with(|sd| sd.borrow().clone()))
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));
    Ok(crate::runtime_paths::worktree_root(&base_dir)
        .join(repo_name)
        .join(worker_id)
        .display()
        .to_string())
}

pub(super) fn ensure_worker_worktree(
    worker_id: &str,
    profile: &mut WorkerExecutionProfile,
) -> Result<(), VmError> {
    let Some(spec) = profile.worktree.as_mut() else {
        return Ok(());
    };
    if spec.repo.trim().is_empty() {
        return Err(VmError::Runtime(
            "worker execution.worktree.repo must not be empty".to_string(),
        ));
    }
    let path = infer_worktree_path(worker_id, spec)?;
    let base_ref = spec.base_ref.clone().unwrap_or_else(|| "HEAD".to_string());
    let branch = spec
        .branch
        .clone()
        .unwrap_or_else(|| format!("harn-{worker_id}"));
    let target = PathBuf::from(&path);
    if target.exists() {
        profile.cwd = Some(path.clone());
        spec.path = Some(path);
        spec.branch = Some(branch);
        spec.base_ref = Some(base_ref);
        return Ok(());
    }
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| VmError::Runtime(format!("worker worktree mkdir error: {e}")))?;
    }
    let output = Command::new("git")
        .current_dir(&spec.repo)
        .args(["worktree", "add", "-B", &branch, &path, &base_ref])
        .output()
        .map_err(|e| VmError::Runtime(format!("worker worktree add failed: {e}")))?;
    if !output.status.success() {
        return Err(VmError::Runtime(format!(
            "worker worktree add failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    profile.cwd = Some(path.clone());
    spec.path = Some(path);
    spec.branch = Some(branch);
    spec.base_ref = Some(base_ref);
    Ok(())
}

pub(super) fn cleanup_worker_execution(profile: &WorkerExecutionProfile) {
    let Some(spec) = &profile.worktree else {
        return;
    };
    if spec.cleanup.as_deref() != Some("remove") {
        return;
    }
    let Some(path) = spec.path.as_deref() else {
        return;
    };
    let _ = Command::new("git")
        .current_dir(&spec.repo)
        .args(["worktree", "remove", "--force", path])
        .output();
}

pub(super) struct WorkerMutationSessionResetGuard;

impl Drop for WorkerMutationSessionResetGuard {
    fn drop(&mut self) {
        crate::orchestration::install_current_mutation_session(None);
    }
}
