//! Package-script command selection shared by project scanner projections.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

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
    NotRequested,
    Absent,
    Resolved(ResolvedScripts),
    Refused(SelectionError),
}

impl Discovery {
    pub(crate) fn state(&self) -> &'static str {
        match self {
            Self::NotRequested => "not_requested",
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
    let evidence = match package_evidence(project_root, manifest_dir) {
        Ok(evidence) => evidence,
        Err(error) => return Discovery::Refused(error),
    };
    let Some(manifest) = evidence.manifest else {
        return Discovery::Absent;
    };
    match resolve_scripts(&evidence.root, &evidence.directory, &manifest) {
        Ok(scripts) => Discovery::Resolved(scripts),
        Err(error) => Discovery::Refused(error),
    }
}

/// Manager metadata does not authorize scripts when no manifest exists.
pub(crate) fn manager_facts(
    project_root: &Path,
    directory: &Path,
) -> Result<Option<String>, SelectionError> {
    let evidence = package_evidence(project_root, directory)?;
    match evidence.manifest {
        Some(manifest) => resolve_scripts(&evidence.root, &evidence.directory, &manifest)
            .map(|scripts| Some(scripts.manager)),
        None => select_manager(&evidence.root, &evidence.directory, &Map::new())
            .map(|manager| manager.map(|manager| manager.name().to_string())),
    }
}

struct PackageEvidence {
    root: PathBuf,
    directory: PathBuf,
    manifest: Option<Map<String, Value>>,
}

fn package_evidence(root: &Path, directory: &Path) -> Result<PackageEvidence, SelectionError> {
    let root = root
        .canonicalize()
        .map_err(|_| SelectionError::UnreadableRoot)?;
    let directory = directory
        .canonicalize()
        .map_err(|_| SelectionError::UnreadableManifest)?;
    if !directory.starts_with(&root) {
        return Err(SelectionError::OutsideProject);
    }
    match read_manifest(&root, &directory)? {
        Some(physical) => Ok(PackageEvidence {
            root,
            directory: physical.directory,
            manifest: Some(
                serde_json::from_slice(&physical.bytes)
                    .map_err(|_| SelectionError::InvalidManifest)?,
            ),
        }),
        None => Ok(PackageEvidence {
            root,
            directory,
            manifest: None,
        }),
    }
}

struct PhysicalManifest {
    directory: PathBuf,
    bytes: Vec<u8>,
}

fn read_manifest(
    project_root: &Path,
    directory: &Path,
) -> Result<Option<PhysicalManifest>, SelectionError> {
    let path = match directory.join("package.json").canonicalize() {
        Ok(path) => path,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(SelectionError::UnreadableManifest),
    };
    if !path.starts_with(project_root) {
        return Err(SelectionError::OutsideProject);
    }
    let directory = path
        .parent()
        .ok_or(SelectionError::UnreadableManifest)?
        .to_path_buf();
    let bytes = std::fs::read(path).map_err(|_| SelectionError::UnreadableManifest)?;
    Ok(Some(PhysicalManifest { directory, bytes }))
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
    let manager = select_manager(project_root, manifest_dir, manifest)?.unwrap_or(DEFAULT_MANAGER);
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
    let (name, version) = declaration
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
    semver::Version::parse(version).map_err(|_| SelectionError::InvalidDeclaration)?;
    Ok(Some(manager))
}

/// Resolve the nearest manifest/lock evidence within the scanned project.
/// Nested declarations override inherited evidence. Contradictory evidence at
/// one directory is refused; absence remains unmeasured manager metadata.
fn select_manager(
    project_root: &Path,
    manifest_dir: &Path,
    manifest: &Map<String, Value>,
) -> Result<Option<PackageManager>, SelectionError> {
    if !manifest_dir.starts_with(project_root) {
        return Err(SelectionError::OutsideProject);
    }
    let mut current = manifest_dir.to_path_buf();
    let mut value = manifest.clone();
    let mut visited = BTreeSet::new();
    loop {
        if !visited.insert(current.clone()) {
            return Err(SelectionError::InvalidAncestor);
        }
        let mut selected = declared_manager(&value)?;
        for (file, manager) in LOCKFILES {
            let path = current.join(file);
            match std::fs::symlink_metadata(&path) {
                Ok(_) => {
                    let physical = path
                        .canonicalize()
                        .map_err(|_| SelectionError::UnreadableAncestor)?;
                    if !physical.starts_with(project_root) {
                        return Err(SelectionError::OutsideProject);
                    }
                    if !physical.is_file() {
                        return Err(SelectionError::InvalidAncestor);
                    }
                    match selected {
                        Some(existing) if existing != *manager => {
                            return Err(SelectionError::ConflictingManagers);
                        }
                        _ => selected = Some(*manager),
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => return Err(SelectionError::UnreadableAncestor),
            }
        }
        if let Some(manager) = selected {
            return Ok(Some(manager));
        }
        if current == project_root {
            return Ok(None);
        }
        current = current
            .parent()
            .ok_or(SelectionError::OutsideProject)?
            .to_path_buf();
        value = match read_manifest(project_root, &current) {
            Ok(Some(physical)) => {
                current = physical.directory;
                serde_json::from_slice(&physical.bytes)
                    .map_err(|_| SelectionError::InvalidAncestor)?
            }
            Ok(None) => Map::new(),
            Err(SelectionError::UnreadableManifest) => {
                return Err(SelectionError::UnreadableAncestor)
            }
            Err(error) => return Err(error),
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[cfg(unix)]
    #[test]
    fn lock_aliases_retain_the_project_boundary() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let manifest = br#"{"scripts":{"test":"echo KNOWN_LOCK_BOUNDARY"}}"#;
        std::fs::write(root.path().join("package.json"), manifest).unwrap();
        let inside = root.path().join("actual-lock");
        let external = outside.path().join("external-lock");
        std::fs::write(&inside, "KNOWN_INSIDE_LOCK").unwrap();
        std::fs::write(&external, "KNOWN_OUTSIDE_LOCK").unwrap();
        let alias = root.path().join("pnpm-lock.yaml");
        std::os::unix::fs::symlink(&inside, &alias).unwrap();
        let Discovery::Resolved(resolved) = discover(root.path(), root.path()) else {
            panic!("a measured internal lock alias must resolve");
        };
        assert_eq!(resolved.manager, "pnpm");
        std::fs::remove_file(&alias).unwrap();
        std::os::unix::fs::symlink(&external, &alias).unwrap();
        assert_eq!(std::fs::read(&alias).unwrap(), b"KNOWN_OUTSIDE_LOCK");
        assert_eq!(
            discover(root.path(), root.path()),
            Discovery::Refused(SelectionError::OutsideProject)
        );
        std::fs::remove_file(&external).unwrap();
        assert_eq!(
            discover(root.path(), root.path()),
            Discovery::Refused(SelectionError::UnreadableAncestor)
        );
    }

    #[cfg(unix)]
    #[test]
    fn inherited_manifest_alias_cycle_is_refused() {
        let root = tempfile::tempdir().unwrap();
        let package = root.path().join("package");
        std::fs::create_dir(&package).unwrap();
        let bytes = br#"{"scripts":{"test":"echo KNOWN_ALIAS_CYCLE"}}"#;
        std::fs::write(package.join("package.json"), bytes).unwrap();
        std::os::unix::fs::symlink(
            package.join("package.json"),
            root.path().join("package.json"),
        )
        .unwrap();
        assert_eq!(
            std::fs::read(root.path().join("package.json")).unwrap(),
            bytes
        );
        assert_eq!(
            discover(root.path(), &package),
            Discovery::Refused(SelectionError::InvalidAncestor)
        );
    }

    #[test]
    fn declarations_select_their_script_runner() {
        let root = tempfile::tempdir().unwrap();
        for (declaration, expected) in [
            ("npm@10.9.2", "npm test"),
            ("pnpm@10.28.0", "pnpm test"),
            ("yarn@4.9.0", "yarn run test"),
            ("bun@1.2.0", "bun run test"),
            ("pnpm@10.28.0-rc.1+sha224.abcdef", "pnpm test"),
        ] {
            let manifest = json!({"packageManager":declaration});
            let manager = select_manager(root.path(), root.path(), manifest.as_object().unwrap())
                .unwrap()
                .unwrap();
            assert_eq!(manager.script_command("test"), expected);
        }
        assert_eq!(
            select_manager(root.path(), root.path(), json!({}).as_object().unwrap()).unwrap(),
            None
        );
    }

    #[test]
    fn malformed_versions_refuse_manager_substitution() {
        for declaration in [
            "npm@10@not-semver",
            "npm@10",
            "npm@latest",
            "pnpm@^10.0.0",
            "yarn@1.2.3.4",
            "bun@1.2.3-01",
        ] {
            assert_eq!(
                declared_manager(json!({"packageManager": declaration}).as_object().unwrap()),
                Err(SelectionError::InvalidDeclaration)
            );
        }
    }

    #[test]
    fn contradictory_and_unsupported_evidence_cannot_default() {
        let root = tempfile::tempdir().unwrap();
        let physical_root = root.path().canonicalize().unwrap();
        std::fs::write(root.path().join("pnpm-lock.yaml"), "lockfileVersion: 9").unwrap();
        assert_eq!(
            select_manager(
                &physical_root,
                &physical_root,
                json!({"packageManager":"npm@10.0.0"}).as_object().unwrap()
            ),
            Err(SelectionError::ConflictingManagers)
        );
        assert_eq!(
            select_manager(
                &physical_root,
                &physical_root,
                json!({"packageManager":"other@1"}).as_object().unwrap()
            ),
            Err(SelectionError::UnsupportedDeclaration("other".into()))
        );
        assert_eq!(
            select_manager(
                &physical_root,
                &physical_root,
                json!({"packageManager":42}).as_object().unwrap()
            ),
            Err(SelectionError::InvalidDeclaration)
        );
    }

    #[test]
    fn nested_packages_inherit_only_within_the_scan_root() {
        let root = tempfile::tempdir().unwrap();
        let physical_root = root.path().canonicalize().unwrap();
        std::fs::write(root.path().join("pnpm-lock.yaml"), "lockfileVersion: 9").unwrap();
        let child = physical_root.join("packages").join("child");
        std::fs::create_dir_all(&child).unwrap();
        assert_eq!(
            select_manager(&physical_root, &child, json!({}).as_object().unwrap()).unwrap(),
            Some(PackageManager::Pnpm)
        );
        assert_eq!(
            select_manager(
                &physical_root,
                &child,
                json!({"packageManager":"npm@10.0.0"}).as_object().unwrap()
            )
            .unwrap(),
            Some(PackageManager::Npm)
        );
        assert_eq!(
            select_manager(&child, &child, json!({}).as_object().unwrap()).unwrap(),
            None
        );
    }
}
