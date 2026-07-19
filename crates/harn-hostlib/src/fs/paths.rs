use std::path::{Component, Path, PathBuf};

use crate::error::HostlibError;

use super::STATE_REL;

pub(super) fn active_session_id(explicit: Option<&str>) -> Option<String> {
    explicit
        .map(str::to_string)
        .or_else(harn_vm::agent_sessions::current_session_id)
        .filter(|id| !id.trim().is_empty())
}

pub(super) fn validate_session_id(
    builtin: &'static str,
    session_id: &str,
) -> Result<(), HostlibError> {
    if session_id.trim().is_empty() {
        return Err(HostlibError::InvalidParameter {
            builtin,
            param: "session_id",
            message: "must not be empty".to_string(),
        });
    }
    Ok(())
}

pub(super) fn default_root() -> PathBuf {
    harn_vm::stdlib::process::execution_root_path()
}

pub(super) fn session_dir(root: &Path, session_id: &str) -> PathBuf {
    let mut dir = root.to_path_buf();
    for component in STATE_REL {
        dir.push(component);
    }
    dir.push(sanitize_component(session_id));
    dir
}

pub(super) fn manifest_path(root: &Path, session_id: &str) -> PathBuf {
    session_dir(root, session_id).join("manifest.json")
}

pub(super) fn sanitize_component(input: &str) -> String {
    use sha2::{Digest, Sha256};

    let sanitized: String = input
        .chars()
        .map(|ch| match ch {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' | '.' => ch,
            _ => '_',
        })
        .collect();
    // An empty or all-dot component is a traversal token, not a safe name.
    let is_dotted = sanitized.is_empty() || sanitized.bytes().all(|byte| byte == b'.');
    if sanitized == input && !is_dotted {
        sanitized
    } else {
        let hash = hex::encode(Sha256::digest(input.as_bytes()));
        format!("{sanitized}-{}", &hash[..12])
    }
}

pub(super) fn normalize_logical(path: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        default_root().join(path)
    };
    let mut out = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

pub(super) fn not_found(path: &Path) -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::NotFound,
        format!("staged fs: {} is deleted or absent", path.display()),
    )
}
