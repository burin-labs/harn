use std::cell::RefCell;
use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

use crate::orchestration::CapabilityPolicy;
use crate::value::{ErrorCategory, VmError};

const HANDLER_SANDBOX_ENV: &str = "HARN_HANDLER_SANDBOX";

thread_local! {
    static WARNED_KEYS: RefCell<BTreeSet<String>> = const { RefCell::new(BTreeSet::new()) };
}

#[derive(Clone, Copy)]
pub(crate) enum FsAccess {
    Read,
    Write,
    Delete,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SandboxFallback {
    Off,
    Warn,
    Enforce,
}

pub(crate) fn reset_sandbox_state() {
    WARNED_KEYS.with(|keys| keys.borrow_mut().clear());
}

pub(crate) fn enforce_fs_path(builtin: &str, path: &Path, access: FsAccess) -> Result<(), VmError> {
    let Some(policy) = crate::orchestration::current_execution_policy() else {
        return Ok(());
    };
    if policy.workspace_roots.is_empty() {
        return Ok(());
    }
    let candidate = normalize_for_policy(path);
    let roots = normalized_workspace_roots(&policy);
    if roots.iter().any(|root| path_is_within(&candidate, root)) {
        return Ok(());
    }
    Err(sandbox_rejection(format!(
        "sandbox violation: builtin '{builtin}' attempted to {} '{}' outside workspace_roots [{}]",
        access.verb(),
        candidate.display(),
        roots
            .iter()
            .map(|root| root.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    )))
}

pub(crate) fn enforce_process_cwd(path: &Path) -> Result<(), VmError> {
    let Some(policy) = crate::orchestration::current_execution_policy() else {
        return Ok(());
    };
    if policy.workspace_roots.is_empty() {
        return Ok(());
    }
    let candidate = normalize_for_policy(path);
    let roots = normalized_workspace_roots(&policy);
    if roots.iter().any(|root| path_is_within(&candidate, root)) {
        return Ok(());
    }
    Err(sandbox_rejection(format!(
        "sandbox violation: process cwd '{}' is outside workspace_roots [{}]",
        candidate.display(),
        roots
            .iter()
            .map(|root| root.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    )))
}

pub(crate) fn std_command_for(program: &str, args: &[String]) -> Result<Command, VmError> {
    match command_wrapper(program, args)? {
        CommandWrapper::Direct => {
            let mut command = Command::new(program);
            command.args(args);
            Ok(command)
        }
        CommandWrapper::Sandboxed { wrapper, args } => {
            let mut command = Command::new(wrapper);
            command.args(args);
            Ok(command)
        }
    }
}

pub(crate) fn tokio_command_for(
    program: &str,
    args: &[String],
) -> Result<tokio::process::Command, VmError> {
    match command_wrapper(program, args)? {
        CommandWrapper::Direct => {
            let mut command = tokio::process::Command::new(program);
            command.args(args);
            Ok(command)
        }
        CommandWrapper::Sandboxed { wrapper, args } => {
            let mut command = tokio::process::Command::new(wrapper);
            command.args(args);
            Ok(command)
        }
    }
}

pub(crate) fn process_violation_error(output: &std::process::Output) -> Option<VmError> {
    crate::orchestration::current_execution_policy()?;
    if fallback_mode() == SandboxFallback::Off || !platform_sandbox_available() {
        return None;
    }
    let stderr = String::from_utf8_lossy(&output.stderr).to_ascii_lowercase();
    let stdout = String::from_utf8_lossy(&output.stdout).to_ascii_lowercase();
    if !output.status.success()
        && (stderr.contains("operation not permitted")
            || stderr.contains("permission denied")
            || stdout.contains("operation not permitted"))
    {
        return Some(sandbox_rejection(format!(
            "sandbox violation: process was denied by the OS sandbox (status {})",
            output.status.code().unwrap_or(-1)
        )));
    }
    None
}

#[cfg(target_os = "macos")]
fn platform_sandbox_available() -> bool {
    Path::new("/usr/bin/sandbox-exec").exists()
}

#[cfg(not(target_os = "macos"))]
fn platform_sandbox_available() -> bool {
    false
}

enum CommandWrapper {
    Direct,
    Sandboxed { wrapper: String, args: Vec<String> },
}

fn command_wrapper(program: &str, args: &[String]) -> Result<CommandWrapper, VmError> {
    let Some(policy) = crate::orchestration::current_execution_policy() else {
        return Ok(CommandWrapper::Direct);
    };
    if fallback_mode() == SandboxFallback::Off {
        return Ok(CommandWrapper::Direct);
    }
    platform_command_wrapper(program, args, &policy)
}

#[cfg(target_os = "macos")]
fn platform_command_wrapper(
    program: &str,
    args: &[String],
    policy: &CapabilityPolicy,
) -> Result<CommandWrapper, VmError> {
    let sandbox_exec = Path::new("/usr/bin/sandbox-exec");
    if !sandbox_exec.exists() {
        return unavailable("macOS sandbox-exec is not available");
    }
    let mut wrapped_args = vec![
        "-p".to_string(),
        macos_sandbox_profile(policy),
        "--".to_string(),
        program.to_string(),
    ];
    wrapped_args.extend(args.iter().cloned());
    Ok(CommandWrapper::Sandboxed {
        wrapper: sandbox_exec.display().to_string(),
        args: wrapped_args,
    })
}

#[cfg(not(target_os = "macos"))]
fn platform_command_wrapper(
    _program: &str,
    _args: &[String],
    _policy: &CapabilityPolicy,
) -> Result<CommandWrapper, VmError> {
    unavailable(&format!(
        "handler OS sandbox is not implemented for {}",
        std::env::consts::OS
    ))
}

#[cfg(target_os = "macos")]
fn macos_sandbox_profile(policy: &CapabilityPolicy) -> String {
    let roots = process_sandbox_roots(policy);
    let mut profile = String::from(
        "(version 1)\n\
         (deny default)\n\
         (allow process*)\n\
         (allow sysctl-read)\n\
         (allow mach-lookup)\n\
         (allow file-read*)\n\
         (allow file-write* (subpath \"/dev\") (subpath \"/tmp\") (subpath \"/private/tmp\"))\n",
    );
    for root in roots {
        profile.push_str(&format!(
            "(allow file-write* (subpath \"{}\"))\n",
            sandbox_profile_escape(&root.display().to_string())
        ));
    }
    if policy_allows_network(policy) {
        profile.push_str("(allow network*)\n");
    }
    profile
}

#[cfg(target_os = "macos")]
fn sandbox_profile_escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn unavailable(message: &str) -> Result<CommandWrapper, VmError> {
    match fallback_mode() {
        SandboxFallback::Off | SandboxFallback::Warn => {
            warn_once("handler_sandbox_unavailable", message);
            Ok(CommandWrapper::Direct)
        }
        SandboxFallback::Enforce => Err(sandbox_rejection(format!(
            "{message}; set {HANDLER_SANDBOX_ENV}=warn or off to run unsandboxed"
        ))),
    }
}

fn fallback_mode() -> SandboxFallback {
    match std::env::var(HANDLER_SANDBOX_ENV)
        .unwrap_or_else(|_| "warn".to_string())
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "0" | "false" | "off" | "none" => SandboxFallback::Off,
        "1" | "true" | "enforce" | "required" => SandboxFallback::Enforce,
        _ => SandboxFallback::Warn,
    }
}

fn warn_once(key: &str, message: &str) {
    let inserted = WARNED_KEYS.with(|keys| keys.borrow_mut().insert(key.to_string()));
    if inserted {
        crate::events::log_warn("handler_sandbox", message);
    }
}

fn sandbox_rejection(message: String) -> VmError {
    VmError::CategorizedError {
        message,
        category: ErrorCategory::ToolRejected,
    }
}

fn normalized_workspace_roots(policy: &CapabilityPolicy) -> Vec<PathBuf> {
    policy
        .workspace_roots
        .iter()
        .map(|root| normalize_for_policy(&resolve_policy_path(root)))
        .collect()
}

fn process_sandbox_roots(policy: &CapabilityPolicy) -> Vec<PathBuf> {
    let roots = if policy.workspace_roots.is_empty() {
        vec![crate::stdlib::process::execution_root_path()]
    } else {
        normalized_workspace_roots(policy)
    };
    roots
        .into_iter()
        .map(|root| normalize_for_policy(&root))
        .collect()
}

fn resolve_policy_path(path: &str) -> PathBuf {
    let candidate = PathBuf::from(path);
    if candidate.is_absolute() {
        candidate
    } else {
        crate::stdlib::process::execution_root_path().join(candidate)
    }
}

fn normalize_for_policy(path: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        crate::stdlib::process::execution_root_path().join(path)
    };
    let absolute = normalize_lexically(&absolute);
    if let Ok(canonical) = absolute.canonicalize() {
        return canonical;
    }

    let mut existing = absolute.as_path();
    let mut suffix = Vec::new();
    while !existing.exists() {
        let Some(parent) = existing.parent() else {
            return normalize_lexically(&absolute);
        };
        if let Some(name) = existing.file_name() {
            suffix.push(name.to_os_string());
        }
        existing = parent;
    }

    let mut normalized = existing
        .canonicalize()
        .unwrap_or_else(|_| normalize_lexically(existing));
    for component in suffix.iter().rev() {
        normalized.push(component);
    }
    normalize_lexically(&normalized)
}

fn normalize_lexically(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}

fn path_is_within(path: &Path, root: &Path) -> bool {
    path == root || path.starts_with(root)
}

fn policy_allows_network(policy: &CapabilityPolicy) -> bool {
    fn rank(value: &str) -> usize {
        match value {
            "none" => 0,
            "read_only" => 1,
            "workspace_write" => 2,
            "process_exec" => 3,
            "network" => 4,
            _ => 5,
        }
    }
    policy
        .side_effect_level
        .as_ref()
        .map(|level| rank(level) >= rank("network"))
        .unwrap_or(true)
}

impl FsAccess {
    fn verb(self) -> &'static str {
        match self {
            FsAccess::Read => "read",
            FsAccess::Write => "write",
            FsAccess::Delete => "delete",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_create_path_normalizes_against_existing_parent() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("a/../new.txt");
        let normalized = normalize_for_policy(&nested);
        assert_eq!(
            normalized,
            normalize_for_policy(&dir.path().join("new.txt"))
        );
    }

    #[test]
    fn path_within_root_accepts_root_and_children() {
        let root = Path::new("/tmp/harn-root");
        assert!(path_is_within(root, root));
        assert!(path_is_within(Path::new("/tmp/harn-root/file"), root));
        assert!(!path_is_within(
            Path::new("/tmp/harn-root-other/file"),
            root
        ));
    }
}
