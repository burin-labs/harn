//! Validate a complete workspace-root replacement before admitting any grant.

use std::path::{Path, PathBuf};

use crate::workspace_anchor::{MountMode, MountedRoot, WorkspaceAnchor};

/// An ordered root set whose directories all exist and can be represented on
/// the host protocol. Preparing it has no session or persistence effects.
pub struct PreparedWorkspaceRoots {
    anchor: WorkspaceAnchor,
}

impl PreparedWorkspaceRoots {
    pub fn prepare(primary: &Path, directories: &[String]) -> Result<Self, String> {
        let primary = validated_absolute_directory(primary)?;
        let mounted_at = crate::orchestration::now_unix_seconds_text();
        let additional_roots = directories
            .iter()
            .map(|directory| {
                Ok(MountedRoot {
                    path: validated_absolute_directory(Path::new(directory))?,
                    mount_mode: MountMode::Extend,
                    mounted_at: mounted_at.clone(),
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
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
}

fn validated_absolute_directory(path: &Path) -> Result<PathBuf, String> {
    if !path.is_absolute() {
        return Err(format!(
            "workspace root '{}' must be absolute",
            path.display()
        ));
    }
    let path = path
        .to_str()
        .ok_or_else(|| "workspace root must be representable as UTF-8".to_string())?;
    let canonical = super::metadata::validate_workspace_root_path(path)?;
    if canonical.to_str().is_none() {
        return Err("canonical workspace root must be representable as UTF-8".to_string());
    }
    Ok(canonical)
}
