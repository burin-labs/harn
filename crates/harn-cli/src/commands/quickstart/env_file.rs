use std::fs;
use std::path::{Path, PathBuf};

use super::{merge_env_file, FileAction, ProviderSelection};

pub(super) struct EnvironmentPlan {
    path: PathBuf,
    content: String,
    status: &'static str,
    ignore_path: PathBuf,
    ignore_content: Option<Vec<u8>>,
    ignore_existed: bool,
}

pub(super) fn prepare(
    root: &Path,
    path: &Path,
    selection: &ProviderSelection,
) -> Result<EnvironmentPlan, String> {
    if harn_hostlib::git_repository::path_is_tracked(root, Path::new(".env"))? {
        return Err(
            "quickstart refuses to write .env because Git already tracks it; remove it from the index or choose an untracked project directory, then retry".to_string(),
        );
    }
    let existing = read_regular_file(path)?;
    let existing_text = std::str::from_utf8(existing.as_deref().unwrap_or_default())
        .map_err(|_| format!("{} is not UTF-8", path.display()))?;
    let content = merge_env_file(existing_text, selection);
    let status = if existing.is_none() {
        "create"
    } else if content == existing_text && permissions_are_private(path)? {
        "skip"
    } else {
        "update"
    };
    let ignore_path = root.join(".gitignore");
    let existing_ignore = read_regular_file(&ignore_path)?;
    let ignore_existed = existing_ignore.is_some();
    let mut ignore = existing_ignore.unwrap_or_default();
    // The final rule wins over an earlier authored negation such as !.env.
    let protected = ignore
        .split(|byte| *byte == b'\n')
        .rev()
        .find(|line| !line.is_empty())
        == Some(b"/.env".as_slice());
    let ignore_content = if protected {
        None
    } else {
        if !ignore.is_empty() && !ignore.ends_with(b"\n") {
            ignore.push(b'\n');
        }
        ignore.extend_from_slice(b"/.env\n");
        Some(ignore)
    };
    Ok(EnvironmentPlan {
        path: path.to_path_buf(),
        content,
        status,
        ignore_path,
        ignore_content,
        ignore_existed,
    })
}

impl EnvironmentPlan {
    pub(super) fn write(self) -> Result<Vec<FileAction>, String> {
        if let Some(content) = &self.ignore_content {
            harn_vm::atomic_io::atomic_write(&self.ignore_path, content).map_err(|error| {
                format!("failed to protect {}: {error}", self.ignore_path.display())
            })?;
        }
        if self.status != "skip" {
            harn_vm::atomic_io::atomic_write_with_mode(&self.path, self.content.as_bytes(), 0o600)
                .map_err(|error| format!("failed to write {}: {error}", self.path.display()))?;
        }
        Ok(vec![
            FileAction {
                path: self.ignore_path,
                status: if self.ignore_content.is_none() {
                    "skip"
                } else if self.ignore_existed {
                    "update"
                } else {
                    "create"
                },
                detail: "exclude the local environment file from Git".to_string(),
            },
            FileAction {
                path: self.path,
                status: self.status,
                detail: "local environment file".to_string(),
            },
        ])
    }
}

fn read_regular_file(path: &Path) -> Result<Option<Vec<u8>>, String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => fs::read(path)
            .map(Some)
            .map_err(|error| format!("failed to read {}: {error}", path.display())),
        Ok(_) => Err(format!("{} must be a regular file", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("failed to inspect {}: {error}", path.display())),
    }
}

#[cfg(unix)]
fn permissions_are_private(path: &Path) -> Result<bool, String> {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path)
        .map(|metadata| metadata.permissions().mode() & 0o077 == 0)
        .map_err(|error| format!("failed to inspect {}: {error}", path.display()))
}

#[cfg(not(unix))]
fn permissions_are_private(_path: &Path) -> Result<bool, String> {
    Ok(true)
}
