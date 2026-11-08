//! Retained source identities and their physical and logical projections.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::workspace::RecordedWorkspace;
use super::{EventTape, ReplaySourceRoot};

/// A source package's current recorded root and earlier physical locations.
/// The oldest physical root remains its logical name in model data.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RecordedSourceRoot {
    pub root: PathBuf,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ancestors: Vec<PathBuf>,
}

#[derive(Debug)]
pub(super) struct ReplaySources {
    roots: Vec<ReplaySourceRoot>,
    physical: Vec<ReplaySourceRoot>,
    logical: Vec<ReplaySourceRoot>,
    recording: Vec<RecordedSourceRoot>,
    boundary_roots: Vec<PathBuf>,
}

impl ReplaySources {
    pub fn from_tape(
        tape: &EventTape,
        roots: Vec<ReplaySourceRoot>,
        location: &super::ReplayWorkspaceLocation,
    ) -> Result<Self, String> {
        let workspace = location.workspace_root();
        let roots = super::scope::validate_source_roots(roots, workspace, tape)?;
        let retained = tape
            .header
            .run
            .as_ref()
            .map_or(&[][..], |run| run.source_workspaces.as_slice());
        let history = |root: &RecordedSourceRoot| {
            let recorded = RecordedWorkspace {
                root: Some(root.root.clone()),
                ancestors: root.ancestors.clone(),
                aliases: Vec::new(),
            };
            recorded.validate()?;
            Ok::<_, String>(recorded)
        };
        let mut trees = Vec::new();
        for (index, prior) in retained.iter().enumerate() {
            history(prior)?;
            if retained[..index]
                .iter()
                .any(|other| other.root == prior.root)
            {
                return Err("recorded source workspace identities must be unique".into());
            }
        }
        for root in &roots {
            let recorded = match retained.iter().find(|prior| prior.root == root.recorded) {
                Some(prior) => history(prior)?,
                None => RecordedWorkspace {
                    root: Some(root.recorded.clone()),
                    ..RecordedWorkspace::default()
                },
            };
            trees.push((recorded, root.restored.clone()));
        }
        for prior in retained {
            if roots.iter().any(|root| root.recorded == prior.root) {
                continue;
            }
            let restored = prior.root.canonicalize().map_err(|error| {
                format!("retained source workspace needs a restored root: {error}")
            })?;
            if restored != prior.root || !restored.is_dir() {
                return Err("retained source workspace needs an explicit restored root".into());
            }
            trees.push((history(prior)?, restored));
        }
        let mut workspace_roots = RecordedWorkspace::from_tape(tape)?.roots();
        workspace_roots.push(workspace.to_path_buf());
        let overlaps = |a: &Path, b: &Path| a.starts_with(b) || b.starts_with(a);
        let mut value = Self {
            roots,
            physical: Vec::new(),
            logical: Vec::new(),
            recording: Vec::new(),
            boundary_roots: Vec::new(),
        };
        for (recorded, restored) in trees {
            if overlaps(&restored, location.storage_pool()) {
                return Err(
                    "replay source workspace must not overlap its replay storage pool".into(),
                );
            }
            let mut names = recorded.physical_roots();
            if !names.contains(&restored) {
                names.push(restored.clone());
            }
            for (index, name) in names.iter().enumerate() {
                if workspace_roots.iter().any(|root| overlaps(name, root))
                    || value.boundary_roots.iter().any(|root| overlaps(name, root))
                    || names[..index].iter().any(|root| overlaps(name, root))
                {
                    return Err("replay source ownership must not overlap other trees".into());
                }
            }
            let identity = recorded.identity(&restored);
            for name in &names {
                if name != &restored {
                    value.physical.push(ReplaySourceRoot {
                        recorded: name.clone(),
                        restored: restored.clone(),
                    });
                }
                if name != identity.logical_root() {
                    value.logical.push(ReplaySourceRoot {
                        recorded: name.clone(),
                        restored: identity.logical_root().to_path_buf(),
                    });
                }
            }
            value.recording.push(RecordedSourceRoot {
                root: restored,
                ancestors: recorded.physical_roots(),
            });
            value.boundary_roots.extend(names);
        }
        Ok(value)
    }

    pub fn roots(&self) -> &[ReplaySourceRoot] {
        &self.roots
    }

    pub fn physical(&self) -> &[ReplaySourceRoot] {
        &self.physical
    }

    pub fn logical(&self) -> &[ReplaySourceRoot] {
        &self.logical
    }

    pub fn recording(&self) -> &[RecordedSourceRoot] {
        &self.recording
    }

    pub fn boundary_roots(&self) -> &[PathBuf] {
        &self.boundary_roots
    }
}
