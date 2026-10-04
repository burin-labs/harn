use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspacePathKind {
    WorkspaceRelative,
    HostAbsolute,
    Invalid,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WorkspacePathInfo {
    pub input: String,
    pub kind: WorkspacePathKind,
    pub normalized: String,
    pub workspace_path: Option<String>,
    pub host_path: Option<String>,
    pub recovered_root_drift: bool,
    pub reason: Option<String>,
}

impl WorkspacePathInfo {
    pub fn normalized_workspace_path(&self) -> Option<&str> {
        self.workspace_path.as_deref()
    }

    pub fn display_path(&self) -> &str {
        self.workspace_path
            .as_deref()
            .or(self.host_path.as_deref())
            .unwrap_or(&self.normalized)
    }

    pub fn policy_candidates(&self) -> Vec<String> {
        let mut seen = BTreeSet::new();
        let mut out = Vec::new();
        for candidate in [
            Some(self.input.as_str()),
            Some(self.normalized.as_str()),
            self.workspace_path.as_deref(),
            self.host_path.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            if !candidate.is_empty() && seen.insert(candidate.to_string()) {
                out.push(candidate.to_string());
            }
        }
        out
    }

    pub fn resolved_host_path(&self) -> Option<PathBuf> {
        self.host_path.as_ref().map(PathBuf::from)
    }
}

pub fn normalize_workspace_path(path: &str, workspace_root: Option<&Path>) -> Option<String> {
    classify_workspace_path(path, workspace_root).workspace_path
}

/// Resolve an existing path through the filesystem and return its canonical
/// workspace-relative spelling only when it remains beneath an existing
/// workspace directory. Unlike [`normalize_workspace_path`], this follows
/// symlinks and never applies leading-slash root-drift recovery.
pub fn canonicalize_existing_workspace_path(path: &Path, workspace_root: &Path) -> Option<String> {
    if path.as_os_str().is_empty() {
        return None;
    }
    let canonical_root = std::fs::canonicalize(workspace_root).ok()?;
    if !canonical_root.is_dir() {
        return None;
    }

    let target = if path.is_absolute() {
        path.to_path_buf()
    } else {
        canonical_root.join(path)
    };
    let canonical_target = std::fs::canonicalize(target).ok()?;
    let relative = canonical_target.strip_prefix(&canonical_root).ok()?;
    let workspace_path = to_posix(&relative.to_string_lossy());
    Some(if workspace_path.is_empty() {
        ".".to_string()
    } else {
        workspace_path
    })
}

pub fn classify_workspace_path(path: &str, workspace_root: Option<&Path>) -> WorkspacePathInfo {
    classify_path(path, workspace_root, true)
}

/// Classify permission resources without interpreting an absolute path as a
/// misspelled workspace-relative path. Display recovery cannot grant access.
pub fn classify_permission_path(path: &str, workspace_root: Option<&Path>) -> WorkspacePathInfo {
    let mut info = classify_path(path, workspace_root, false);
    if info.kind == WorkspacePathKind::Invalid
        || (is_absolute_path_syntax(path.trim()) && !Path::new(path.trim()).is_absolute())
    {
        return info;
    }
    let Some(root) = workspace_root else {
        return info;
    };
    let input = Path::new(path.trim());
    let target = if input.is_absolute() {
        input.to_path_buf()
    } else {
        root.join(input)
    };
    let resolved = std::fs::canonicalize(root).and_then(|canonical_root| {
        canonicalize_existing_ancestor(&target).map(|target| (canonical_root, target))
    });
    match resolved {
        Ok((root, target)) => {
            let host = normalize_host_path(&target);
            info.workspace_path = target
                .strip_prefix(root)
                .ok()
                .map(|relative| to_posix(&relative.to_string_lossy()));
            info.host_path = Some(host.clone());
            if info.workspace_path.is_none() {
                info.kind = WorkspacePathKind::HostAbsolute;
                info.normalized = host;
                info.reason = Some("path resolves outside the workspace root".into());
            } else {
                info.reason = None;
            }
        }
        Err(error) => {
            info.kind = WorkspacePathKind::Invalid;
            info.workspace_path = None;
            info.reason = Some(format!("permission path cannot be resolved: {error}"));
        }
    }
    info
}

fn canonicalize_existing_ancestor(path: &Path) -> std::io::Result<PathBuf> {
    let mut ancestor = path;
    let mut suffix = Vec::new();
    loop {
        match std::fs::canonicalize(ancestor) {
            Ok(mut resolved) => {
                for component in suffix.into_iter().rev() {
                    resolved.push(component);
                }
                return Ok(resolved);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                // An existing dangling symlink is an unresolved resource,
                // not a new file under the symlink's lexical parent.
                if std::fs::symlink_metadata(ancestor).is_ok() {
                    return Err(error);
                }
                let Some(name) = ancestor.file_name() else {
                    return Err(error);
                };
                suffix.push(name.to_os_string());
                let Some(parent) = ancestor.parent() else {
                    return Err(error);
                };
                ancestor = parent;
            }
            Err(error) => return Err(error),
        }
    }
}

fn classify_path(
    path: &str,
    workspace_root: Option<&Path>,
    recover_workspace_drift: bool,
) -> WorkspacePathInfo {
    let input = path.to_string();
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return invalid_info(input, String::new(), "path is empty");
    }
    if trimmed.contains('\0') {
        return invalid_info(input, to_posix(trimmed), "path contains NUL bytes");
    }

    let normalized_input = normalize_lexical(trimmed);
    let root_path = workspace_root.map(normalize_workspace_root);
    let root_norm = root_path
        .as_ref()
        .map(|root| normalize_host_path(root))
        .filter(|root| !root.is_empty());

    if !is_absolute_path_syntax(trimmed) {
        let workspace_path = normalized_input;
        if escapes_workspace(&workspace_path) {
            let host_path = root_path.as_ref().map(|root| {
                normalize_host_path(&root.join(PathBuf::from(workspace_path.as_str())))
            });
            return WorkspacePathInfo {
                input,
                kind: WorkspacePathKind::Invalid,
                normalized: workspace_path,
                workspace_path: None,
                host_path,
                recovered_root_drift: false,
                reason: Some("workspace-relative path escapes the workspace root".to_string()),
            };
        }
        let host_path = root_path
            .as_ref()
            .map(|root| normalize_host_path(&root.join(PathBuf::from(workspace_path.as_str()))));
        return WorkspacePathInfo {
            input,
            kind: WorkspacePathKind::WorkspaceRelative,
            normalized: workspace_path.clone(),
            workspace_path: Some(workspace_path),
            host_path,
            recovered_root_drift: false,
            reason: None,
        };
    }

    let host_path = normalized_input;
    if let Some(root_norm) = root_norm.as_deref() {
        if let Some(workspace_path) = workspace_relative_from_absolute(&host_path, root_norm) {
            return WorkspacePathInfo {
                input,
                kind: WorkspacePathKind::HostAbsolute,
                normalized: host_path.clone(),
                workspace_path: Some(workspace_path),
                host_path: Some(host_path),
                recovered_root_drift: false,
                reason: None,
            };
        }

        if let Some(root_path) = root_path.as_ref().filter(|_| recover_workspace_drift) {
            if let Some(recovered) = recover_root_drift(trimmed, root_path) {
                return WorkspacePathInfo {
                    input,
                    kind: WorkspacePathKind::WorkspaceRelative,
                    normalized: recovered.clone(),
                    workspace_path: Some(recovered.clone()),
                    host_path: Some(normalize_host_path(
                        &root_path.join(PathBuf::from(recovered.as_str())),
                    )),
                    recovered_root_drift: true,
                    reason: None,
                };
            }
        }
    }

    WorkspacePathInfo {
        input,
        kind: WorkspacePathKind::HostAbsolute,
        normalized: host_path.clone(),
        workspace_path: None,
        host_path: Some(host_path),
        recovered_root_drift: false,
        reason: None,
    }
}

fn invalid_info(input: String, normalized: String, reason: &str) -> WorkspacePathInfo {
    WorkspacePathInfo {
        input,
        kind: WorkspacePathKind::Invalid,
        normalized,
        workspace_path: None,
        host_path: None,
        recovered_root_drift: false,
        reason: Some(reason.to_string()),
    }
}

fn normalize_workspace_root(root: &Path) -> PathBuf {
    if root.is_absolute() {
        root.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(root)
    }
}

fn to_posix(s: &str) -> String {
    s.replace('\\', "/")
}

/// Return whether a serialized path is absolute in either POSIX or Windows
/// syntax, independent of the host running Harn.
pub(crate) fn is_absolute_path_syntax(path: &str) -> bool {
    let path = to_posix(path);
    if path.starts_with('/') {
        return true;
    }
    let bytes = path.as_bytes();
    bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && bytes[2] == b'/'
}

fn split_segments(path: &str) -> (bool, Option<String>, Vec<String>) {
    let posix = to_posix(path);
    let mut drive: Option<String> = None;
    let mut rest = posix.as_str();
    let bytes = posix.as_bytes();
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        let (drive_prefix, remainder) = posix.split_at(2);
        drive = Some(drive_prefix.to_string());
        rest = remainder;
    }
    let absolute = rest.starts_with('/');
    let segments = rest
        .split('/')
        .filter(|segment| !segment.is_empty())
        .map(|segment| segment.to_string())
        .collect();
    (absolute, drive, segments)
}

fn normalize_lexical(path: &str) -> String {
    let (absolute, drive, segments) = split_segments(path);
    let mut stack = Vec::new();
    for segment in segments {
        match segment.as_str() {
            "." => {}
            ".." => {
                if let Some(top) = stack.last() {
                    if top != ".." {
                        stack.pop();
                        continue;
                    }
                }
                if !absolute {
                    stack.push("..".to_string());
                }
            }
            _ => stack.push(segment),
        }
    }

    let mut normalized = String::new();
    if let Some(drive) = drive {
        normalized.push_str(&drive);
    }
    if absolute {
        normalized.push('/');
    }
    normalized.push_str(&stack.join("/"));
    if normalized.is_empty() {
        ".".to_string()
    } else {
        normalized
    }
}

fn normalize_host_path(path: &Path) -> String {
    normalize_lexical(&path.to_string_lossy())
}

fn escapes_workspace(path: &str) -> bool {
    path == ".." || path.starts_with("../")
}

fn workspace_relative_from_absolute(path: &str, workspace_root: &str) -> Option<String> {
    let (path_abs, path_drive, path_segments) = split_segments(path);
    let (root_abs, root_drive, root_segments) = split_segments(workspace_root);
    if !path_abs || !root_abs || path_drive != root_drive {
        return None;
    }
    if path_segments.len() < root_segments.len()
        || !path_segments.starts_with(root_segments.as_slice())
    {
        return None;
    }
    let remainder = &path_segments[root_segments.len()..];
    if remainder.is_empty() {
        Some(".".to_string())
    } else {
        Some(remainder.join("/"))
    }
}

fn recover_root_drift(path: &str, workspace_root: &Path) -> Option<String> {
    let posix = to_posix(path);
    if !posix.starts_with('/') {
        return None;
    }
    let trimmed = posix.trim_start_matches('/');
    if trimmed.is_empty() {
        return None;
    }
    let workspace_path = normalize_lexical(trimmed);
    if workspace_path == "." || escapes_workspace(&workspace_path) {
        return None;
    }
    if Path::new(path).exists() {
        return None;
    }
    let candidate = workspace_root.join(PathBuf::from(workspace_path.as_str()));
    if candidate.exists() || candidate.parent().is_some_and(Path::exists) {
        Some(workspace_path)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_path_is_workspace_relative() {
        let dir = tempfile::tempdir().unwrap();
        let info = classify_workspace_path("src/main.rs", Some(dir.path()));
        assert_eq!(info.kind, WorkspacePathKind::WorkspaceRelative);
        assert_eq!(info.workspace_path.as_deref(), Some("src/main.rs"));
        assert_eq!(
            info.host_path.as_deref(),
            Some(normalize_host_path(&dir.path().join("src/main.rs")).as_str())
        );
    }

    #[test]
    fn parent_escape_is_invalid() {
        let dir = tempfile::tempdir().unwrap();
        let info = classify_workspace_path("../secret.txt", Some(dir.path()));
        assert_eq!(info.kind, WorkspacePathKind::Invalid);
        assert_eq!(
            info.reason.as_deref(),
            Some("workspace-relative path escapes the workspace root")
        );
    }

    #[test]
    fn windows_drive_relative_path_is_not_host_absolute() {
        let dir = tempfile::tempdir().unwrap();
        let info = classify_workspace_path("C:src/main.harn", Some(dir.path()));
        assert_eq!(info.kind, WorkspacePathKind::WorkspaceRelative);
        assert_eq!(info.workspace_path.as_deref(), Some("C:src/main.harn"));
    }

    #[test]
    fn absolute_path_inside_workspace_gets_relative_projection() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("packages/app/host.harn");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, "ok").unwrap();
        let info = classify_workspace_path(file.to_string_lossy().as_ref(), Some(dir.path()));
        assert_eq!(info.kind, WorkspacePathKind::HostAbsolute);
        assert_eq!(
            info.workspace_path.as_deref(),
            Some("packages/app/host.harn")
        );
        assert!(!info.recovered_root_drift);
    }

    #[test]
    fn leading_slash_workspace_drift_recovers_when_workspace_candidate_exists() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("packages/app/host.harn");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, "ok").unwrap();
        let info = classify_workspace_path("/packages/app/host.harn", Some(dir.path()));
        assert_eq!(info.kind, WorkspacePathKind::WorkspaceRelative);
        assert_eq!(
            info.workspace_path.as_deref(),
            Some("packages/app/host.harn")
        );
        assert!(info.recovered_root_drift);
    }

    #[test]
    fn permission_path_does_not_recover_an_external_absolute_path() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("packages/app/host.harn");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, "ok").unwrap();
        let external = classify_permission_path("/packages/app/host.harn", Some(dir.path()));
        assert_eq!(external.kind, WorkspacePathKind::HostAbsolute);
        assert!(external.workspace_path.is_none());
        assert_eq!(
            external.host_path.as_deref(),
            Some("/packages/app/host.harn")
        );
        assert!(!external.recovered_root_drift);
        let inside = classify_permission_path(file.to_str().unwrap(), Some(dir.path()));
        assert_eq!(
            inside.workspace_path.as_deref(),
            Some("packages/app/host.harn")
        );
        let relative = classify_permission_path("packages/app/host.harn", Some(dir.path()));
        assert_eq!(relative.host_path, inside.host_path);
        assert!(!relative.recovered_root_drift);
    }

    #[cfg(unix)]
    #[test]
    fn permission_path_follows_existing_ancestors_without_admitting_symlink_escape() {
        let workspace = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), workspace.path().join("linked")).unwrap();
        let escaped = classify_permission_path("linked/new.txt", Some(workspace.path()));
        assert!(escaped.workspace_path.is_none());
        assert_eq!(escaped.kind, WorkspacePathKind::HostAbsolute);
        assert_eq!(
            escaped.host_path.as_deref(),
            Some(normalize_host_path(&outside.path().join("new.txt")).as_str())
        );
        let inside = classify_permission_path("new/nested.txt", Some(workspace.path()));
        assert_eq!(inside.workspace_path.as_deref(), Some("new/nested.txt"));
        std::os::unix::fs::symlink(
            outside.path().join("absent"),
            workspace.path().join("dangling"),
        )
        .unwrap();
        let unresolved = classify_permission_path("dangling", Some(workspace.path()));
        assert_eq!(unresolved.kind, WorkspacePathKind::Invalid);
        assert!(unresolved.workspace_path.is_none());
    }

    #[cfg(unix)]
    #[test]
    fn permission_path_resolves_symlinks_before_parent_traversal() {
        let workspace = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::create_dir(outside.path().join("child")).unwrap();
        std::os::unix::fs::symlink(
            outside.path().join("child"),
            workspace.path().join("linked"),
        )
        .unwrap();
        let escaped = classify_permission_path("linked/../secret.txt", Some(workspace.path()));
        assert_eq!(escaped.kind, WorkspacePathKind::HostAbsolute);
        assert!(escaped.workspace_path.is_none());
        assert_eq!(
            escaped.host_path.as_deref(),
            Some(normalize_host_path(&outside.path().join("secret.txt")).as_str())
        );
    }

    #[cfg(unix)]
    #[test]
    fn permission_path_accepts_physical_and_declared_root_aliases() {
        let workspace = tempfile::tempdir().unwrap();
        let alias_parent = tempfile::tempdir().unwrap();
        let alias = alias_parent.path().join("project");
        std::os::unix::fs::symlink(workspace.path(), &alias).unwrap();
        let physical = classify_permission_path(
            workspace.path().join("new.txt").to_str().unwrap(),
            Some(&alias),
        );
        let declared =
            classify_permission_path(alias.join("new.txt").to_str().unwrap(), Some(&alias));
        let relative = classify_permission_path("new.txt", Some(&alias));
        assert_eq!(physical.workspace_path.as_deref(), Some("new.txt"));
        assert_eq!(physical.host_path, declared.host_path);
        assert_eq!(physical.host_path, relative.host_path);
    }

    #[test]
    fn unknown_absolute_path_stays_host_absolute() {
        let dir = tempfile::tempdir().unwrap();
        let info = classify_workspace_path("/tmp/harn-issue-125-nope", Some(dir.path()));
        assert_eq!(info.kind, WorkspacePathKind::HostAbsolute);
        assert!(info.workspace_path.is_none());
        assert!(!info.recovered_root_drift);
    }

    #[test]
    fn normalize_workspace_path_returns_relative_projection() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("packages/app")).unwrap();
        assert_eq!(
            normalize_workspace_path("/packages/app", Some(dir.path())).as_deref(),
            Some("packages/app")
        );
    }

    #[test]
    fn canonical_existing_workspace_path_returns_contained_relative_child() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("packages/app/test.harn");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, "ok").unwrap();

        assert_eq!(
            canonicalize_existing_workspace_path(Path::new("packages/app/test.harn"), root.path(),)
                .as_deref(),
            Some("packages/app/test.harn")
        );
        assert_eq!(
            canonicalize_existing_workspace_path(&file, root.path()).as_deref(),
            Some("packages/app/test.harn")
        );
    }

    #[test]
    fn canonical_existing_workspace_path_rejects_missing_and_parent_escape() {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("workspace");
        std::fs::create_dir(&root).unwrap();
        let secret = parent.path().join("secret.harn");
        std::fs::write(&secret, "secret").unwrap();

        assert_eq!(
            canonicalize_existing_workspace_path(Path::new("missing.harn"), &root),
            None
        );
        assert_eq!(
            canonicalize_existing_workspace_path(Path::new(""), &root),
            None
        );
        assert_eq!(
            canonicalize_existing_workspace_path(Path::new("../secret.harn"), &root),
            None
        );
        assert_eq!(canonicalize_existing_workspace_path(&secret, &root), None);
    }

    #[cfg(unix)]
    #[test]
    fn canonical_existing_workspace_path_rejects_outside_symlink_and_projects_inside_one() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let secret = outside.path().join("secret.harn");
        std::fs::write(&secret, "secret").unwrap();
        std::os::unix::fs::symlink(&secret, root.path().join("outside-link")).unwrap();

        let actual = root.path().join("actual/test.harn");
        std::fs::create_dir_all(actual.parent().unwrap()).unwrap();
        std::fs::write(&actual, "ok").unwrap();
        std::os::unix::fs::symlink("actual", root.path().join("inside-link")).unwrap();

        assert_eq!(
            canonicalize_existing_workspace_path(&root.path().join("outside-link"), root.path()),
            None
        );
        assert_eq!(
            canonicalize_existing_workspace_path(
                &root.path().join("inside-link/test.harn"),
                root.path(),
            )
            .as_deref(),
            Some("actual/test.harn")
        );
    }

    #[cfg(unix)]
    #[test]
    fn canonical_existing_workspace_path_accepts_a_symlinked_workspace_root() {
        let parent = tempfile::tempdir().unwrap();
        let actual_root = parent.path().join("actual-root");
        let file = actual_root.join("nested/test.harn");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, "ok").unwrap();
        let linked_root = parent.path().join("workspace-link");
        std::os::unix::fs::symlink("actual-root", &linked_root).unwrap();

        assert_eq!(
            canonicalize_existing_workspace_path(Path::new("nested/test.harn"), &linked_root)
                .as_deref(),
            Some("nested/test.harn")
        );
    }
}
