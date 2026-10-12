//! Package-script command selection shared by project scanner projections.

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::{Map, Value};

/// The package manager that owns one manifest's scripts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PackageManager {
    Npm,
    Pnpm,
    Yarn,
    Bun,
}

pub(crate) const DEFAULT_MANAGER: PackageManager = PackageManager::Npm;

impl PackageManager {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Npm => "npm",
            Self::Pnpm => "pnpm",
            Self::Yarn => "yarn",
            Self::Bun => "bun",
        }
    }

    /// Invoke a named script, rather than a manager's similarly named builtin.
    pub(crate) fn script_command(self, script: &str) -> String {
        match (self, script) {
            (Self::Npm | Self::Pnpm, "test") => format!("{} test", self.name()),
            _ => format!("{} run {}", self.name(), shell_words::quote(script)),
        }
    }
}

/// Evidence cannot safely select a different runner when this is returned.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SelectionError {
    OutsideProject,
    InvalidDeclaration,
    UnsupportedDeclaration(String),
    ConflictingManagers,
    UnreadableAncestor,
    InvalidAncestor,
    InvalidScripts,
    UnreadableManifest,
    InvalidManifest,
    UnreadableRoot,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum Discovery {
    #[default]
    Absent,
    Resolved(ResolvedScripts),
    Refused(SelectionError),
}

impl Discovery {
    pub(crate) fn state(&self) -> &'static str {
        match self {
            Self::Absent => "absent",
            Self::Resolved(_) => "resolved",
            Self::Refused(_) => "refused",
        }
    }
}

impl SelectionError {
    pub(crate) fn code(&self) -> &'static str {
        match self {
            Self::OutsideProject => "outside_project",
            Self::InvalidDeclaration => "invalid_declaration",
            Self::UnsupportedDeclaration(_) => "unsupported_declaration",
            Self::ConflictingManagers => "conflicting_managers",
            Self::UnreadableAncestor => "unreadable_ancestor",
            Self::InvalidAncestor => "invalid_ancestor",
            Self::InvalidScripts => "invalid_scripts",
            Self::UnreadableManifest => "unreadable_manifest",
            Self::InvalidManifest => "invalid_manifest",
            Self::UnreadableRoot => "unreadable_root",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedScripts {
    pub manager: String,
    pub name: Option<String>,
    pub scripts: BTreeMap<String, Script>,
}

/// Read and normalize one manifest for scanners and command-planning adapters.
pub fn discover(project_root: &Path, manifest_dir: &Path) -> Discovery {
    let project_root = match project_root.canonicalize() {
        Ok(root) => root,
        Err(_) => return Discovery::Refused(SelectionError::UnreadableRoot),
    };
    let manifest_dir = match manifest_dir.canonicalize() {
        Ok(dir) => dir,
        Err(_) => return Discovery::Refused(SelectionError::UnreadableManifest),
    };
    if !manifest_dir.starts_with(&project_root) {
        return Discovery::Refused(SelectionError::OutsideProject);
    }
    let bytes = match std::fs::read(manifest_dir.join("package.json")) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Discovery::Absent,
        Err(_) => return Discovery::Refused(SelectionError::UnreadableManifest),
    };
    let manifest: Map<String, Value> = match serde_json::from_slice(&bytes) {
        Ok(manifest) => manifest,
        Err(_) => return Discovery::Refused(SelectionError::InvalidManifest),
    };
    match resolve_scripts(&project_root, &manifest_dir, &manifest) {
        Ok(scripts) => Discovery::Resolved(scripts),
        Err(error) => Discovery::Refused(error),
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Script {
    pub command: String,
    pub body: String,
}

/// One normalized package-script inventory for all scanner projections.
fn resolve_scripts(
    project_root: &Path,
    manifest_dir: &Path,
    manifest: &Map<String, Value>,
) -> Result<ResolvedScripts, SelectionError> {
    let manager = select_manager(project_root, manifest_dir, manifest)?;
    let empty_scripts = Map::new();
    let scripts = match manifest.get("scripts") {
        Some(value) => value.as_object().ok_or(SelectionError::InvalidScripts)?,
        None => &empty_scripts,
    };
    let scripts = scripts
        .iter()
        .map(|(name, value)| {
            let body = value.as_str().ok_or(SelectionError::InvalidScripts)?;
            Ok((
                name.clone(),
                Script {
                    command: manager.script_command(name),
                    body: body.to_string(),
                },
            ))
        })
        .collect::<Result<BTreeMap<_, _>, _>>()?;
    Ok(ResolvedScripts {
        manager: manager.name().to_string(),
        name: manifest
            .get("name")
            .and_then(Value::as_str)
            .map(str::to_string),
        scripts,
    })
}

pub(crate) const LOCKFILES: &[(&str, PackageManager)] = &[
    ("package-lock.json", PackageManager::Npm),
    ("npm-shrinkwrap.json", PackageManager::Npm),
    ("pnpm-lock.yaml", PackageManager::Pnpm),
    ("yarn.lock", PackageManager::Yarn),
    ("bun.lock", PackageManager::Bun),
    ("bun.lockb", PackageManager::Bun),
];

pub(crate) const LOCKFILE_NAMES: [&str; LOCKFILES.len()] = {
    let mut names = [""; LOCKFILES.len()];
    let mut index = 0;
    while index < LOCKFILES.len() {
        names[index] = LOCKFILES[index].0;
        index += 1;
    }
    names
};

/// Parse the explicit declaration once for command and fingerprint consumers.
pub(crate) fn declared_manager(
    manifest: &Map<String, Value>,
) -> Result<Option<PackageManager>, SelectionError> {
    let Some(value) = manifest.get("packageManager") else {
        return Ok(None);
    };
    let declaration = value.as_str().ok_or(SelectionError::InvalidDeclaration)?;
    let (name, _) = declaration
        .split_once('@')
        .filter(|(_, version)| !version.is_empty())
        .ok_or(SelectionError::InvalidDeclaration)?;
    let manager = match name {
        "npm" => PackageManager::Npm,
        "pnpm" => PackageManager::Pnpm,
        "yarn" => PackageManager::Yarn,
        "bun" => PackageManager::Bun,
        _ => return Err(SelectionError::UnsupportedDeclaration(name.to_string())),
    };
    Ok(Some(manager))
}

/// Resolve the nearest manifest/lock evidence within the scanned project.
/// Nested declarations override inherited evidence. Contradictory evidence at
/// one directory is refused; no evidence uses the same npm default everywhere.
fn select_manager(
    project_root: &Path,
    manifest_dir: &Path,
    manifest: &Map<String, Value>,
) -> Result<PackageManager, SelectionError> {
    if !manifest_dir.starts_with(project_root) {
        return Err(SelectionError::OutsideProject);
    }
    let mut current = manifest_dir;
    let mut value = manifest.clone();
    loop {
        let mut selected = declared_manager(&value)?;
        for (file, manager) in LOCKFILES {
            match current.join(file).try_exists() {
                Ok(true) => match selected {
                    Some(existing) if existing != *manager => {
                        return Err(SelectionError::ConflictingManagers);
                    }
                    _ => selected = Some(*manager),
                },
                Ok(false) => {}
                Err(_) => return Err(SelectionError::UnreadableAncestor),
            }
        }
        if let Some(manager) = selected {
            return Ok(manager);
        }
        if current == project_root {
            return Ok(DEFAULT_MANAGER);
        }
        current = current.parent().ok_or(SelectionError::OutsideProject)?;
        value = match std::fs::read(current.join("package.json")) {
            Ok(bytes) => {
                serde_json::from_slice(&bytes).map_err(|_| SelectionError::InvalidAncestor)?
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Map::new(),
            Err(_) => return Err(SelectionError::UnreadableAncestor),
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn declarations_select_their_script_runner() {
        let root = tempfile::tempdir().unwrap();
        for (declaration, expected) in [
            ("npm@10.9.2", "npm test"),
            ("pnpm@10.28.0", "pnpm test"),
            ("yarn@4.9.0", "yarn run test"),
            ("bun@1.2.0", "bun run test"),
        ] {
            let manifest = json!({"packageManager":declaration});
            let manager =
                select_manager(root.path(), root.path(), manifest.as_object().unwrap()).unwrap();
            assert_eq!(manager.script_command("test"), expected);
        }
        assert_eq!(
            select_manager(root.path(), root.path(), json!({}).as_object().unwrap()).unwrap(),
            PackageManager::Npm
        );
    }

    #[test]
    fn contradictory_and_unsupported_evidence_cannot_default() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("pnpm-lock.yaml"), "lockfileVersion: 9").unwrap();
        assert_eq!(
            select_manager(
                root.path(),
                root.path(),
                json!({"packageManager":"npm@10"}).as_object().unwrap()
            ),
            Err(SelectionError::ConflictingManagers)
        );
        assert_eq!(
            select_manager(
                root.path(),
                root.path(),
                json!({"packageManager":"other@1"}).as_object().unwrap()
            ),
            Err(SelectionError::UnsupportedDeclaration("other".into()))
        );
        assert_eq!(
            select_manager(
                root.path(),
                root.path(),
                json!({"packageManager":42}).as_object().unwrap()
            ),
            Err(SelectionError::InvalidDeclaration)
        );
    }

    #[test]
    fn nested_packages_inherit_only_within_the_scan_root() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("pnpm-lock.yaml"), "lockfileVersion: 9").unwrap();
        let child = root.path().join("packages").join("child");
        std::fs::create_dir_all(&child).unwrap();
        assert_eq!(
            select_manager(root.path(), &child, json!({}).as_object().unwrap()).unwrap(),
            PackageManager::Pnpm
        );
        assert_eq!(
            select_manager(
                root.path(),
                &child,
                json!({"packageManager":"npm@10"}).as_object().unwrap()
            )
            .unwrap(),
            PackageManager::Npm
        );
        assert_eq!(
            select_manager(&child, &child, json!({}).as_object().unwrap()).unwrap(),
            PackageManager::Npm
        );
    }
}
