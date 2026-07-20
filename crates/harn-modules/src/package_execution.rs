use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fmt;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::package_snapshot::{package_lock_digest, PackageSnapshot};

pub const CONTENT_HASH_FILE: &str = ".harn-content-hash";
pub const CACHE_METADATA_FILE: &str = ".harn-package-cache.toml";

pub struct PackageExecutionGuard {
    snapshot: Arc<PackageSnapshot>,
    package_alias: String,
    expected_lock_digest: String,
    package_pins: BTreeMap<String, PackageExecutionPin>,
}

struct PackageExecutionPin {
    root: PathBuf,
    content_hash: String,
}

#[derive(Deserialize)]
struct ExecutionLock {
    #[serde(default, rename = "package")]
    packages: Vec<ExecutionLockPackage>,
}

#[derive(Deserialize)]
struct ExecutionLockPackage {
    name: String,
    content_hash: Option<String>,
}

impl PackageExecutionGuard {
    pub fn new(
        snapshot: Arc<PackageSnapshot>,
        package_alias: impl Into<String>,
        expected_content_hash: impl Into<String>,
    ) -> Result<Self, PackageExecutionError> {
        let expected_lock_digest = snapshot.lock_digest().to_string();
        Self::new_with_lock_digest(
            snapshot,
            package_alias,
            expected_content_hash,
            expected_lock_digest,
        )
    }

    pub fn new_with_lock_digest(
        snapshot: Arc<PackageSnapshot>,
        package_alias: impl Into<String>,
        expected_content_hash: impl Into<String>,
        expected_lock_digest: impl Into<String>,
    ) -> Result<Self, PackageExecutionError> {
        let package_alias = package_alias.into();
        if !is_safe_package_alias(&package_alias)
            || !snapshot
                .package_names()
                .iter()
                .any(|name| name == &package_alias)
        {
            return Err(PackageExecutionError::Invalid(format!(
                "package alias '{package_alias}' is not present in generation {}",
                snapshot.generation()
            )));
        }
        let expected_content_hash = expected_content_hash.into();
        validate_content_hash(&expected_content_hash)?;
        let expected_lock_digest = expected_lock_digest.into();
        validate_content_hash(&expected_lock_digest)?;
        if snapshot.lock_digest() != expected_lock_digest {
            return Err(PackageExecutionError::Invalid(format!(
                "package generation {} lock digest changed since activation: expected {}, got {}",
                snapshot.generation(),
                expected_lock_digest,
                snapshot.lock_digest()
            )));
        }
        let lock_bytes = fs::read(snapshot.lock_path()).map_err(|error| {
            PackageExecutionError::io("read", snapshot.lock_path().to_path_buf(), error)
        })?;
        let actual_lock_digest = package_lock_digest(&lock_bytes);
        if actual_lock_digest != expected_lock_digest {
            return Err(PackageExecutionError::Invalid(format!(
                "package generation {} lock digest changed before guard construction: expected {}, got {}",
                snapshot.generation(),
                expected_lock_digest,
                actual_lock_digest
            )));
        }
        let lock: ExecutionLock =
            toml::from_str(std::str::from_utf8(&lock_bytes).map_err(|error| {
                PackageExecutionError::Invalid(format!(
                    "package generation lock is not valid UTF-8: {error}"
                ))
            })?)
            .map_err(|error| {
                PackageExecutionError::Invalid(format!(
                    "failed to parse package generation lock: {error}"
                ))
            })?;
        let mut package_pins = BTreeMap::new();
        for package in lock.packages {
            if !is_safe_package_alias(&package.name) {
                return Err(PackageExecutionError::Invalid(format!(
                    "package generation contains unsafe alias '{}'",
                    package.name
                )));
            }
            let content_hash = package
                .content_hash
                .or_else(|| (package.name == package_alias).then(|| expected_content_hash.clone()));
            let Some(content_hash) = content_hash else {
                continue;
            };
            validate_content_hash(&content_hash)?;
            let root = snapshot.packages_root().join(&package.name);
            if !root.is_dir() {
                return Err(PackageExecutionError::Invalid(format!(
                    "locked package '{}' is missing from generation {}",
                    package.name,
                    snapshot.generation()
                )));
            }
            // Path dependencies may be generation-owned symlinks. Pin each
            // canonical target so retargeting is rejected before execution.
            let root = root
                .canonicalize()
                .map_err(|error| PackageExecutionError::io("canonicalize", root.clone(), error))?;
            package_pins.insert(package.name, PackageExecutionPin { root, content_hash });
        }
        let primary = package_pins.get(&package_alias).ok_or_else(|| {
            PackageExecutionError::Invalid(format!(
                "package '{package_alias}' has no content hash in generation {}",
                snapshot.generation()
            ))
        })?;
        if primary.content_hash != expected_content_hash {
            return Err(PackageExecutionError::Invalid(format!(
                "package '{package_alias}' activation hash {} does not match generation hash {}",
                expected_content_hash, primary.content_hash
            )));
        }
        Ok(Self {
            snapshot,
            package_alias,
            expected_lock_digest,
            package_pins,
        })
    }

    pub fn verify_entry(&self, entry: &Path) -> Result<(), PackageExecutionError> {
        self.verify_entry_source(entry).map(|_| ())
    }

    /// Reject a relative import that would escape the importing package alias.
    ///
    /// This takes the raw import string rather than a `Path`: Windows normalizes
    /// parent components while constructing a `PathBuf`, so a later entry guard
    /// cannot distinguish `agents/../shared` from `shared`.
    pub(crate) fn validate_import_path(
        &self,
        current_file: &Path,
        import_path: &str,
    ) -> Result<(), PackageExecutionError> {
        // Internal module loading can pass an already-resolved absolute path;
        // `verify_entry_source` remains the authority for that representation.
        if Path::new(import_path).is_absolute() {
            return Ok(());
        }
        if import_path.contains('\\') {
            return Err(PackageExecutionError::Invalid(format!(
                "package import '{import_path}' from {} must be a slash-separated relative path",
                current_file.display()
            )));
        }
        let relative = lexical_package_relative_path(
            current_file,
            self.snapshot.packages_root(),
            self.snapshot.generation(),
        )?;
        let mut components = relative.components();
        let package_alias = match components.next() {
            Some(Component::Normal(alias)) => alias.to_str().ok_or_else(|| {
                PackageExecutionError::Invalid(format!(
                    "importing file {} has a non-UTF-8 package alias",
                    current_file.display()
                ))
            })?,
            _ => {
                return Err(PackageExecutionError::Invalid(format!(
                    "importing file {} has no package alias in generation {}",
                    current_file.display(),
                    self.snapshot.generation()
                )));
            }
        };
        let components = components.collect::<Vec<_>>();
        let Some((file_name, parent_components)) = components.split_last() else {
            return Err(PackageExecutionError::Invalid(format!(
                "importing path {} does not name a file inside package '{package_alias}'",
                current_file.display()
            )));
        };
        if !matches!(file_name, Component::Normal(_)) {
            return Err(PackageExecutionError::Invalid(format!(
                "importing path {} does not name a file inside package '{package_alias}'",
                current_file.display()
            )));
        }
        let mut depth = 0usize;
        for component in parent_components {
            match component {
                Component::Normal(_) => depth += 1,
                Component::CurDir => {}
                Component::ParentDir if depth == 0 => {
                    return Err(PackageExecutionError::Invalid(format!(
                        "importing path {} escapes package alias '{package_alias}'",
                        current_file.display()
                    )));
                }
                Component::ParentDir => depth -= 1,
                Component::RootDir | Component::Prefix(_) => {
                    return Err(PackageExecutionError::Invalid(format!(
                        "importing path {} has an unsafe package-relative path",
                        current_file.display()
                    )));
                }
            }
        }
        for component in import_path.split('/') {
            match component {
                "" | "." => {}
                ".." if depth == 0 => {
                    return Err(PackageExecutionError::Invalid(format!(
                        "package import '{import_path}' from {} escapes package alias '{package_alias}'",
                        current_file.display()
                    )));
                }
                ".." => depth -= 1,
                _ => depth += 1,
            }
        }
        Ok(())
    }

    /// Return the entry bytes from the same package walk whose digest matched
    /// the pinned content hash. Callers must compile these bytes rather than
    /// reopening the path after verification.
    pub fn verify_entry_source(&self, entry: &Path) -> Result<Vec<u8>, PackageExecutionError> {
        let canonical_entry = entry.canonicalize().map_err(|error| {
            PackageExecutionError::io("canonicalize", entry.to_path_buf(), error)
        })?;
        if !canonical_entry.is_file() {
            return Err(PackageExecutionError::Invalid(format!(
                "entry {} is not a regular file in generation {}",
                entry.display(),
                self.snapshot.generation()
            )));
        }
        let relative_to_generation = lexical_package_relative_path(
            entry,
            self.snapshot.packages_root(),
            self.snapshot.generation(),
        )?;
        let mut components = relative_to_generation.components();
        let package_alias = match components.next() {
            Some(Component::Normal(alias)) => alias.to_str().ok_or_else(|| {
                PackageExecutionError::Invalid(format!(
                    "entry {} has a non-UTF-8 package alias",
                    entry.display()
                ))
            })?,
            _ => {
                return Err(PackageExecutionError::Invalid(format!(
                    "entry {} has no package alias in generation {}",
                    entry.display(),
                    self.snapshot.generation()
                )));
            }
        };
        let mut requested_relative = PathBuf::new();
        for component in components {
            match component {
                Component::Normal(part) => requested_relative.push(part),
                Component::CurDir => {}
                Component::ParentDir if requested_relative.pop() => {}
                Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                    return Err(PackageExecutionError::Invalid(format!(
                        "entry {} has an unsafe package-relative path",
                        entry.display()
                    )));
                }
            }
        }
        if requested_relative.as_os_str().is_empty() {
            return Err(PackageExecutionError::Invalid(format!(
                "entry {} does not name a file inside package '{package_alias}'",
                entry.display()
            )));
        }
        let pin = self.package_pins.get(package_alias).ok_or_else(|| {
            PackageExecutionError::Invalid(format!(
                "package alias '{package_alias}' is not content-pinned for activated package '{}'",
                self.package_alias
            ))
        })?;
        if !canonical_entry.starts_with(&pin.root) {
            return Err(PackageExecutionError::Invalid(format!(
                "package alias '{package_alias}' was retargeted outside its pinned root {}",
                pin.root.display()
            )));
        }
        let relative = canonical_entry.strip_prefix(&pin.root).map_err(|error| {
            PackageExecutionError::Invalid(format!(
                "failed to relativize package entry {}: {error}",
                canonical_entry.display()
            ))
        })?;
        if relative != requested_relative {
            return Err(PackageExecutionError::Invalid(format!(
                "entry {} was retargeted within package '{package_alias}' from {} to {}",
                entry.display(),
                requested_relative.display(),
                relative.display()
            )));
        }
        if relative
            .components()
            .any(|component| excluded_package_name(component.as_os_str()))
        {
            return Err(PackageExecutionError::Invalid(format!(
                "entry {} is excluded from package '{}' content identity",
                entry.display(),
                package_alias
            )));
        }
        let lock_bytes = fs::read(self.snapshot.lock_path()).map_err(|error| {
            PackageExecutionError::io("read", self.snapshot.lock_path().to_path_buf(), error)
        })?;
        let actual_lock_digest = package_lock_digest(&lock_bytes);
        if actual_lock_digest != self.expected_lock_digest {
            return Err(PackageExecutionError::Invalid(format!(
                "package generation {} lock digest changed: expected {}, got {}",
                self.snapshot.generation(),
                self.expected_lock_digest,
                actual_lock_digest
            )));
        }
        let (actual_content_hash, source) =
            compute_package_content_hash_capturing(&pin.root, Some(relative))?;
        if actual_content_hash != pin.content_hash {
            return Err(PackageExecutionError::Invalid(format!(
                "package '{}' content changed in generation {}: expected {}, got {}",
                package_alias,
                self.snapshot.generation(),
                pin.content_hash,
                actual_content_hash
            )));
        }
        source.ok_or_else(|| {
            PackageExecutionError::Invalid(format!(
                "entry {} disappeared while verifying package '{}'",
                entry.display(),
                self.package_alias
            ))
        })
    }

    pub fn snapshot(&self) -> &PackageSnapshot {
        &self.snapshot
    }

    pub fn package_alias(&self) -> &str {
        &self.package_alias
    }
}

fn lexical_package_relative_path(
    entry: &Path,
    canonical_packages_root: &Path,
    generation: &str,
) -> Result<PathBuf, PackageExecutionError> {
    let outside_generation = || {
        PackageExecutionError::Invalid(format!(
            "entry {} is outside package generation {} rooted at '{}'",
            entry.display(),
            generation,
            canonical_packages_root.display()
        ))
    };
    // `Path::strip_prefix` compares normalized components on Windows. Walk the
    // components ourselves so the caller can still reject `alias/../escape`.
    if let Some(relative) = lexical_relative_suffix(entry, canonical_packages_root) {
        return Ok(relative);
    }
    let mut input_packages_root = None;
    for ancestor in entry.ancestors() {
        if ancestor
            .canonicalize()
            .is_ok_and(|canonical| canonical == canonical_packages_root)
        {
            input_packages_root = Some(ancestor);
        }
    }
    let input_packages_root = input_packages_root.ok_or_else(&outside_generation)?;
    lexical_relative_suffix(entry, input_packages_root).ok_or_else(outside_generation)
}

fn lexical_relative_suffix(entry: &Path, root: &Path) -> Option<PathBuf> {
    let mut entry_components = entry.components();
    for root_component in root.components() {
        if entry_components.next() != Some(root_component) {
            return None;
        }
    }
    let mut relative = PathBuf::new();
    relative.extend(entry_components);
    Some(relative)
}

impl fmt::Debug for PackageExecutionGuard {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PackageExecutionGuard")
            .field("project_root", &self.snapshot.project_root())
            .field("generation", &self.snapshot.generation())
            .field("package_alias", &self.package_alias)
            .field("expected_lock_digest", &self.expected_lock_digest)
            .field("pinned_package_count", &self.package_pins.len())
            .finish()
    }
}

impl PartialEq for PackageExecutionGuard {
    fn eq(&self, other: &Self) -> bool {
        self.snapshot.project_root() == other.snapshot.project_root()
            && self.snapshot.generation() == other.snapshot.generation()
            && self.snapshot.lock_digest() == other.snapshot.lock_digest()
            && self.package_alias == other.package_alias
            && self.expected_lock_digest == other.expected_lock_digest
            && self.package_pins.len() == other.package_pins.len()
            && self.package_pins.iter().all(|(name, pin)| {
                other.package_pins.get(name).is_some_and(|other| {
                    pin.root == other.root && pin.content_hash == other.content_hash
                })
            })
    }
}

impl Eq for PackageExecutionGuard {}

#[derive(Debug)]
pub enum PackageExecutionError {
    Io {
        operation: &'static str,
        path: PathBuf,
        source: std::io::Error,
    },
    Invalid(String),
}

impl PackageExecutionError {
    fn io(operation: &'static str, path: PathBuf, source: std::io::Error) -> Self {
        Self::Io {
            operation,
            path,
            source,
        }
    }
}

impl fmt::Display for PackageExecutionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io {
                operation,
                path,
                source,
            } => write!(
                formatter,
                "failed to {operation} {} while verifying package execution: {source}",
                path.display()
            ),
            Self::Invalid(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for PackageExecutionError {}

pub fn compute_package_content_hash(dir: &Path) -> Result<String, PackageExecutionError> {
    compute_package_content_hash_capturing(dir, None).map(|(hash, _)| hash)
}

fn compute_package_content_hash_capturing(
    dir: &Path,
    capture: Option<&Path>,
) -> Result<(String, Option<Vec<u8>>), PackageExecutionError> {
    let mut files = Vec::new();
    collect_hashable_files(dir, dir, &mut files)?;
    files.sort();
    let mut hasher = Sha256::new();
    let mut captured = None;
    for relative in files {
        let normalized = normalized_package_relative_path(&relative);
        let path = dir.join(&relative);
        let contents = read_regular_file(&path)?;
        hasher.update(normalized.as_bytes());
        hasher.update([0]);
        hasher.update(encode_hex(&Sha256::digest(&contents)).as_bytes());
        if capture == Some(relative.as_path()) {
            captured = Some(contents);
        }
    }
    Ok((
        format!("sha256:{}", encode_hex(&hasher.finalize())),
        captured,
    ))
}

fn collect_hashable_files(
    root: &Path,
    cursor: &Path,
    out: &mut Vec<PathBuf>,
) -> Result<(), PackageExecutionError> {
    let entries = fs::read_dir(cursor).map_err(|error| {
        PackageExecutionError::io("read directory", cursor.to_path_buf(), error)
    })?;
    for entry in entries {
        let entry = entry.map_err(|error| {
            PackageExecutionError::io("read directory entry", cursor.to_path_buf(), error)
        })?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|error| PackageExecutionError::io("stat", path.clone(), error))?;
        let name = entry.file_name();
        if file_type.is_symlink() {
            return Err(PackageExecutionError::Invalid(format!(
                "package content contains unsupported symlink: {}",
                path.display()
            )));
        }
        if excluded_package_name(&name) {
            continue;
        }
        if file_type.is_dir() {
            collect_hashable_files(root, &path, out)?;
        } else if file_type.is_file() {
            let relative = path.strip_prefix(root).map_err(|error| {
                PackageExecutionError::Invalid(format!(
                    "failed to relativize {}: {error}",
                    path.display()
                ))
            })?;
            out.push(relative.to_path_buf());
        }
    }
    Ok(())
}

fn read_regular_file(path: &Path) -> Result<Vec<u8>, PackageExecutionError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| PackageExecutionError::io("stat", path.to_path_buf(), error))?;
    if !metadata.file_type().is_file() {
        return Err(PackageExecutionError::Invalid(format!(
            "package content is not a regular file: {}",
            path.display()
        )));
    }
    fs::read(path).map_err(|error| PackageExecutionError::io("read", path.to_path_buf(), error))
}

fn excluded_package_name(name: &OsStr) -> bool {
    name == OsStr::new(".git")
        || name == OsStr::new(".gitignore")
        || name == OsStr::new(CONTENT_HASH_FILE)
        || name == OsStr::new(CACHE_METADATA_FILE)
}

pub fn normalized_package_relative_path(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

fn validate_content_hash(hash: &str) -> Result<(), PackageExecutionError> {
    let Some(hex) = hash.strip_prefix("sha256:") else {
        return Err(PackageExecutionError::Invalid(format!(
            "package content hash must use sha256:<64 hex>, got {hash}"
        )));
    };
    if hex.len() != 64 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(PackageExecutionError::Invalid(format!(
            "package content hash must use sha256:<64 hex>, got {hash}"
        )));
    }
    Ok(())
}

fn is_safe_package_alias(alias: &str) -> bool {
    let mut components = Path::new(alias).components();
    matches!(components.next(), Some(Component::Normal(_))) && components.next().is_none()
}

fn encode_hex(bytes: &[u8]) -> String {
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use fmt::Write as _;
        let _ = write!(encoded, "{byte:02x}");
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::package_snapshot::{
        generation_root, package_current_path, package_publication_lock_path,
        PackageGenerationManifest, PackageGenerationPointer, GENERATION_LEASE_FILE,
        GENERATION_LOCK_FILE, GENERATION_MANIFEST_FILE, GENERATION_PACKAGES_DIR,
    };
    use std::fs::File;

    fn fixture() -> (tempfile::TempDir, Arc<PackageSnapshot>, PathBuf, String) {
        let temp = tempfile::tempdir().unwrap();
        let generation = "generation_a";
        let generation_root = generation_root(temp.path(), generation);
        let package_root = generation_root.join(GENERATION_PACKAGES_DIR).join("agents");
        fs::create_dir_all(&package_root).unwrap();
        let entry = package_root.join("run.harn");
        fs::write(&entry, "pub pipeline run() { return 1 }\n").unwrap();
        fs::write(
            package_root.join("harn.toml"),
            "[package]\nname = \"agents\"\n",
        )
        .unwrap();
        fs::create_dir_all(package_root.join("workflows")).unwrap();
        fs::write(
            package_root.join("workflows/run.harn"),
            "pub pipeline run() { return 1 }\n",
        )
        .unwrap();
        fs::write(
            package_root.join("helper.harn"),
            "pub fn helper() { return 1 }\n",
        )
        .unwrap();
        let content_hash = compute_package_content_hash(&package_root).unwrap();
        let dependency_root = generation_root.join(GENERATION_PACKAGES_DIR).join("shared");
        fs::create_dir_all(&dependency_root).unwrap();
        fs::write(
            dependency_root.join("helper.harn"),
            "pub fn helper() { return 1 }\n",
        )
        .unwrap();
        fs::write(
            dependency_root.join("harn.toml"),
            "[package]\nname = \"shared\"\n\n[exports]\napi = \"safe.harn\"\n",
        )
        .unwrap();
        fs::write(
            dependency_root.join("safe.harn"),
            "pub fn value() { return 1 }\n",
        )
        .unwrap();
        fs::write(
            dependency_root.join("payload.harn"),
            "pub fn value() { return 2 }\n",
        )
        .unwrap();
        let dependency_hash = compute_package_content_hash(&dependency_root).unwrap();
        let lock = format!(
            "version = 4\n\n[[package]]\nname = \"agents\"\ncontent_hash = \"{content_hash}\"\n\n[[package]]\nname = \"shared\"\ncontent_hash = \"{dependency_hash}\"\n"
        );
        fs::write(generation_root.join(GENERATION_LOCK_FILE), &lock).unwrap();
        fs::write(generation_root.join(GENERATION_LEASE_FILE), []).unwrap();
        let manifest =
            PackageGenerationManifest::new(generation, package_lock_digest(lock.as_bytes()))
                .unwrap();
        fs::write(
            generation_root.join(GENERATION_MANIFEST_FILE),
            toml::to_string_pretty(&manifest).unwrap(),
        )
        .unwrap();
        fs::write(
            package_current_path(temp.path()),
            toml::to_string_pretty(&PackageGenerationPointer::new(generation).unwrap()).unwrap(),
        )
        .unwrap();
        File::create(package_publication_lock_path(temp.path())).unwrap();
        let snapshot = Arc::new(PackageSnapshot::acquire(temp.path()).unwrap().unwrap());
        (temp, snapshot, entry, content_hash)
    }

    #[test]
    fn guard_rejects_package_mutation_before_execution() {
        let (_temp, snapshot, entry, content_hash) = fixture();
        let guard = PackageExecutionGuard::new(snapshot, "agents", content_hash).unwrap();
        let source = guard.verify_entry_source(&entry).unwrap();
        assert_eq!(source, b"pub pipeline run() { return 1 }\n");

        fs::write(&entry, "pub pipeline run() { return 2 }\n").unwrap();
        let error = guard.verify_entry(&entry).unwrap_err();
        assert!(error.to_string().contains("content changed"));
    }

    #[test]
    fn guard_retains_generation_lease() {
        let (_temp, snapshot, entry, content_hash) = fixture();
        let lease_path = snapshot.generation_root().join(GENERATION_LEASE_FILE);
        let guard =
            PackageExecutionGuard::new(Arc::clone(&snapshot), "agents", content_hash).unwrap();
        drop(snapshot);
        let lease = File::open(lease_path).unwrap();
        assert!(lease.try_lock().is_err());
        guard.verify_entry(&entry).unwrap();
        drop(guard);
        lease.try_lock().unwrap();
    }

    #[test]
    fn guard_rejects_lock_bytes_not_validated_by_snapshot() {
        let (_temp, snapshot, _entry, content_hash) = fixture();
        let mut lock = fs::read(snapshot.lock_path()).unwrap();
        lock.push(b'\n');
        fs::write(snapshot.lock_path(), lock).unwrap();

        let error = PackageExecutionGuard::new(snapshot, "agents", content_hash).unwrap_err();

        assert!(error.to_string().contains("before guard construction"));
    }

    #[test]
    fn guard_allows_content_pinned_dependency_entry() {
        let (_temp, snapshot, _entry, content_hash) = fixture();
        let dependency = snapshot.packages_root().join("shared/helper.harn");
        let guard = PackageExecutionGuard::new(snapshot, "agents", content_hash).unwrap();

        let source = guard.verify_entry_source(&dependency).unwrap();

        assert_eq!(source, b"pub fn helper() { return 1 }\n");
    }

    #[test]
    fn guarded_export_resolution_rejects_unverified_manifest_mapping() {
        let (_temp, snapshot, entry, content_hash) = fixture();
        let manifest = snapshot.packages_root().join("shared/harn.toml");
        let guard = PackageExecutionGuard::new(snapshot, "agents", content_hash).unwrap();
        fs::write(
            manifest,
            "[package]\nname = \"shared\"\n\n[exports]\napi = \"payload.harn\"\n",
        )
        .unwrap();

        let error =
            crate::package_imports::resolve_import_path_with_guard(&entry, "shared/api", &guard)
                .unwrap_err();

        assert!(error.to_string().contains("content changed"));
    }

    #[cfg(unix)]
    #[test]
    fn guard_rejects_descendant_entry_retargeted_within_package() {
        let (_temp, snapshot, _entry, content_hash) = fixture();
        let safe = snapshot.packages_root().join("shared/safe.harn");
        let guard = PackageExecutionGuard::new(snapshot, "agents", content_hash).unwrap();
        fs::remove_file(&safe).unwrap();
        std::os::unix::fs::symlink("payload.harn", &safe).unwrap();

        let error = guard.verify_entry_source(&safe).unwrap_err();

        assert!(error.to_string().contains("retargeted within package"));
    }

    #[test]
    fn guard_normalizes_parent_import_within_package() {
        let (_temp, snapshot, _entry, content_hash) = fixture();
        let entry = snapshot
            .packages_root()
            .join("agents/workflows/../helper.harn");
        let guard = PackageExecutionGuard::new(snapshot, "agents", content_hash).unwrap();

        let source = guard.verify_entry_source(&entry).unwrap();

        assert_eq!(source, b"pub fn helper() { return 1 }\n");
    }

    #[test]
    fn guard_rejects_parent_import_escaping_alias_root() {
        let (_temp, snapshot, _entry, content_hash) = fixture();
        let entry = snapshot.packages_root().join("agents/run.harn");
        let guard = PackageExecutionGuard::new(snapshot, "agents", content_hash).unwrap();

        let error = crate::package_imports::resolve_import_path_with_guard(
            &entry,
            "../shared/helper",
            &guard,
        )
        .unwrap_err();

        assert!(error.to_string().contains("escapes package alias"));
    }

    #[test]
    fn guard_allows_parent_import_within_package_alias() {
        let (_temp, snapshot, _entry, content_hash) = fixture();
        let entry = snapshot.packages_root().join("agents/workflows/run.harn");
        let guard = PackageExecutionGuard::new(snapshot, "agents", content_hash).unwrap();

        let path =
            crate::package_imports::resolve_import_path_with_guard(&entry, "../helper", &guard)
                .expect("parent traversal remains inside agents")
                .expect("helper resolves inside agents");
        assert_eq!(path, entry.parent().unwrap().join("../helper.harn"));
    }

    #[test]
    fn guard_rejects_parent_import_after_normalizing_importer_path() {
        let (_temp, snapshot, _entry, content_hash) = fixture();
        let entry = snapshot
            .packages_root()
            .join("agents/workflows/../helper.harn");
        let guard = PackageExecutionGuard::new(snapshot, "agents", content_hash).unwrap();

        let error = crate::package_imports::resolve_import_path_with_guard(
            &entry,
            "../shared/helper",
            &guard,
        )
        .unwrap_err();

        assert!(error.to_string().contains("escapes package alias"));
    }

    #[test]
    fn guard_leaves_absolute_internal_module_paths_to_entry_verification() {
        let (_temp, snapshot, entry, content_hash) = fixture();
        let guard = PackageExecutionGuard::new(snapshot, "agents", content_hash).unwrap();

        guard
            .validate_import_path(&entry, entry.to_str().unwrap())
            .expect("absolute internal module path is checked by verify_entry_source");
    }

    #[cfg(unix)]
    #[test]
    fn guard_rejects_primary_alias_retargeted_to_pinned_dependency() {
        let (_temp, snapshot, _entry, content_hash) = fixture();
        let packages = snapshot.packages_root().to_path_buf();
        let primary = packages.join("agents");
        let original = packages.join("agents-original");
        let guard = PackageExecutionGuard::new(snapshot, "agents", content_hash).unwrap();
        fs::rename(&primary, &original).unwrap();
        std::os::unix::fs::symlink(packages.join("shared"), &primary).unwrap();

        let error = guard
            .verify_entry_source(&primary.join("helper.harn"))
            .unwrap_err();

        assert!(error.to_string().contains("alias 'agents' was retargeted"));
    }

    #[cfg(unix)]
    #[test]
    fn guard_rejects_dependency_alias_retargeted_to_primary() {
        let (_temp, snapshot, _entry, content_hash) = fixture();
        let packages = snapshot.packages_root().to_path_buf();
        let dependency = packages.join("shared");
        let original = packages.join("shared-original");
        let guard = PackageExecutionGuard::new(snapshot, "agents", content_hash).unwrap();
        fs::rename(&dependency, &original).unwrap();
        std::os::unix::fs::symlink(packages.join("agents"), &dependency).unwrap();

        let error = guard
            .verify_entry_source(&dependency.join("run.harn"))
            .unwrap_err();

        assert!(error.to_string().contains("alias 'shared' was retargeted"));
    }

    #[cfg(unix)]
    #[test]
    fn content_hash_rejects_descendant_symlink() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("target.harn"), "pub fn value() { 1 }\n").unwrap();
        std::os::unix::fs::symlink("target.harn", temp.path().join("alias.harn")).unwrap();

        let error = compute_package_content_hash(temp.path()).unwrap_err();
        assert!(error.to_string().contains("unsupported symlink"));
    }

    #[cfg(unix)]
    #[test]
    fn guard_accepts_equivalent_root_alias_without_losing_escape_detection() {
        let (temp, snapshot, entry, content_hash) = fixture();
        let alias = temp.path().join("project-alias");
        std::os::unix::fs::symlink(".", &alias).unwrap();
        let aliased_entry = alias.join(entry.strip_prefix(temp.path()).unwrap());
        let guard = PackageExecutionGuard::new(snapshot, "agents", content_hash).unwrap();

        let source = guard.verify_entry_source(&aliased_entry).unwrap();

        assert_eq!(source, b"pub pipeline run() { return 1 }\n");
        let aliased_packages_root = aliased_entry.parent().unwrap().parent().unwrap();
        let escape = aliased_packages_root.join("agents/../shared/helper.harn");
        let error = guard.verify_entry_source(&escape).unwrap_err();
        assert!(error.to_string().contains("unsafe package-relative path"));
    }
}
