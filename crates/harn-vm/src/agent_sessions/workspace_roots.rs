//! Validate a complete workspace-root replacement before admitting any grant.

use std::path::{Path, PathBuf};

use crate::workspace_anchor::{MountMode, MountedRoot, WorkspaceAnchor};

/// Refusal to prepare the complete directory grant.
#[derive(Debug)]
pub enum WorkspaceRootsError {
    Invalid(String),
    OutsideProcess(PathBuf),
}

impl From<String> for WorkspaceRootsError {
    fn from(message: String) -> Self {
        Self::Invalid(message)
    }
}

impl From<&str> for WorkspaceRootsError {
    fn from(message: &str) -> Self {
        Self::Invalid(message.into())
    }
}

impl std::fmt::Display for WorkspaceRootsError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(message) => formatter.write_str(message),
            Self::OutsideProcess(path) => write!(
                formatter,
                "workspace root '{}' is outside process confinement",
                path.display()
            ),
        }
    }
}

impl std::error::Error for WorkspaceRootsError {}

/// An ordered root set of readable directories within process confinement,
/// representable on the host protocol. Preparing it has no session effects.
pub struct PreparedWorkspaceRoots {
    anchor: WorkspaceAnchor,
}

impl PreparedWorkspaceRoots {
    pub fn prepare(primary: &Path, directories: &[String]) -> Result<Self, WorkspaceRootsError> {
        let primary = validated_absolute_directory(primary)?;
        let mounted_at = harn_clock::system_now_unix_seconds_text();
        let additional_roots = directories
            .iter()
            .map(|directory| {
                Ok(MountedRoot {
                    path: validated_absolute_directory(Path::new(directory))?,
                    mount_mode: MountMode::Extend,
                    mounted_at: mounted_at.clone(),
                })
            })
            .collect::<Result<Vec<_>, WorkspaceRootsError>>()?;
        Ok(Self {
            anchor: WorkspaceAnchor {
                primary,
                additional_roots,
                anchored_at: mounted_at,
            },
        })
    }

    pub fn anchor(&self) -> &WorkspaceAnchor {
        &self.anchor
    }

    pub(crate) fn into_anchor(self) -> WorkspaceAnchor {
        self.anchor
    }
}

fn validated_absolute_directory(path: &Path) -> Result<PathBuf, WorkspaceRootsError> {
    if !path.is_absolute() {
        return Err(format!("workspace root '{}' must be absolute", path.display()).into());
    }
    let path = path
        .to_str()
        .ok_or_else(|| "workspace root must be representable as UTF-8".to_string())?;
    let canonical = super::metadata::validate_workspace_root_path_with(path, |canonical| {
        if crate::process_sandbox::current_process_confinement()
            .is_some_and(|confinement| !confinement.covers(canonical))
        {
            return Err(WorkspaceRootsError::OutsideProcess(canonical.to_path_buf()));
        }
        Ok(())
    })?;
    if canonical.to_str().is_none() {
        return Err("canonical workspace root must be representable as UTF-8".into());
    }
    Ok(canonical)
}
