use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Component, Path, PathBuf};

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Counts the filesystem work package resolution performs, so tests can assert
/// that work which is not needed does not happen.
///
/// Counters rather than behavioural assertions because there is nothing else to
/// observe: acquiring a snapshot and discarding it changes no result, only wall
/// time. That is exactly how a 5x regression shipped and stayed hidden for three
/// releases (harn#4815) — the property under test is "this work does not
/// happen", and only a probe can see that.
///
/// The two halves are counted separately because they cost very differently: a
/// root walk is a handful of stats, while an acquire canonicalizes, takes two
/// shared flocks, parses two TOML files, and re-reads plus SHA256s the lockfile.
/// A caller resolving N files under one root should walk N times and acquire
/// ONCE; a single total could not tell that apart from N acquires.
#[cfg(test)]
pub(crate) mod probe_counter {
    use std::cell::Cell;

    thread_local! {
        static ACQUIRE_CALLS: Cell<usize> = const { Cell::new(0) };
        static ROOT_WALK_CALLS: Cell<usize> = const { Cell::new(0) };
    }

    pub(crate) fn record_acquire() {
        ACQUIRE_CALLS.with(|calls| calls.set(calls.get() + 1));
    }

    pub(crate) fn record_root_walk() {
        ROOT_WALK_CALLS.with(|calls| calls.set(calls.get() + 1));
    }

    /// Run `body`, returning its value and how many times it probed the
    /// filesystem for a package at all. Thread-local, so concurrent tests
    /// cannot bleed into each other.
    pub(crate) fn count_probes<T>(body: impl FnOnce() -> T) -> (T, usize) {
        let (value, walks, _) = count_walks_and_acquires(body);
        (value, walks)
    }

    pub(crate) fn count_walks_and_acquires<T>(body: impl FnOnce() -> T) -> (T, usize, usize) {
        ROOT_WALK_CALLS.with(|calls| calls.set(0));
        ACQUIRE_CALLS.with(|calls| calls.set(0));
        let value = body();
        (
            value,
            ROOT_WALK_CALLS.with(|calls| calls.get()),
            ACQUIRE_CALLS.with(|calls| calls.get()),
        )
    }
}

pub const PACKAGE_STATE_DIR: &str = ".harn";
pub const PACKAGE_CURRENT_FILE: &str = "package-current.toml";
pub const PACKAGE_GENERATIONS_DIR: &str = "package-generations";
pub const PACKAGE_PUBLICATION_LOCK_FILE: &str = "package-generation.lock";
pub const PACKAGE_INSTALL_LOCK_FILE: &str = "package-install.lock";
pub const GENERATION_MANIFEST_FILE: &str = "generation.toml";
pub const GENERATION_LOCK_FILE: &str = "harn.lock";
pub const GENERATION_LEASE_FILE: &str = "lease.lock";
pub const GENERATION_PACKAGES_DIR: &str = "packages";
pub const PACKAGE_GENERATION_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageGenerationPointer {
    pub schema_version: u32,
    pub generation: String,
}

impl PackageGenerationPointer {
    pub fn new(generation: impl Into<String>) -> Result<Self, PackageSnapshotError> {
        let generation = generation.into();
        validate_generation_id(&generation)?;
        Ok(Self {
            schema_version: PACKAGE_GENERATION_SCHEMA_VERSION,
            generation,
        })
    }

    pub fn validate(&self, path: &Path) -> Result<(), PackageSnapshotError> {
        validate_schema_version(self.schema_version, path)?;
        validate_generation_id(&self.generation)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageGenerationManifest {
    pub schema_version: u32,
    pub generation: String,
    pub lock_digest: String,
}

impl PackageGenerationManifest {
    pub fn new(
        generation: impl Into<String>,
        lock_digest: impl Into<String>,
    ) -> Result<Self, PackageSnapshotError> {
        let generation = generation.into();
        validate_generation_id(&generation)?;
        let lock_digest = lock_digest.into();
        validate_lock_digest(&lock_digest)?;
        Ok(Self {
            schema_version: PACKAGE_GENERATION_SCHEMA_VERSION,
            generation,
            lock_digest,
        })
    }

    pub fn validate(&self, path: &Path) -> Result<(), PackageSnapshotError> {
        validate_schema_version(self.schema_version, path)?;
        validate_generation_id(&self.generation)?;
        validate_lock_digest(&self.lock_digest)
    }
}

#[derive(Debug)]
pub struct PackageSnapshot {
    project_root: PathBuf,
    generation: String,
    generation_root: PathBuf,
    packages_root: PathBuf,
    lock_path: PathBuf,
    lock_digest: String,
    package_names: Vec<String>,
    _lease: File,
}

impl PackageSnapshot {
    /// Duplicate this snapshot while retaining the same generation lease.
    pub fn retained_clone(&self) -> Result<Self, PackageSnapshotError> {
        Ok(Self {
            project_root: self.project_root.clone(),
            generation: self.generation.clone(),
            generation_root: self.generation_root.clone(),
            packages_root: self.packages_root.clone(),
            lock_path: self.lock_path.clone(),
            lock_digest: self.lock_digest.clone(),
            package_names: self.package_names.clone(),
            _lease: self._lease.try_clone().map_err(|error| {
                PackageSnapshotError::io("clone lease", &self.generation_root, error)
            })?,
        })
    }

    /// Acquire the currently published package generation for `project_root`.
    ///
    /// The publication lock closes the pointer-to-lease race: GC cannot remove
    /// the selected generation until this reader holds its shared lease.
    pub fn acquire(project_root: &Path) -> Result<Option<Self>, PackageSnapshotError> {
        #[cfg(test)]
        probe_counter::record_acquire();
        let project_root = project_root
            .canonicalize()
            .map_err(|error| PackageSnapshotError::io("canonicalize", project_root, error))?;
        let state_path = project_root.join(PACKAGE_STATE_DIR);
        if !state_path.is_dir() {
            return Ok(None);
        }
        let state_dir = canonical_directory_within(&project_root, &state_path)?;
        let pointer_path = state_dir.join(PACKAGE_CURRENT_FILE);
        let publication_lock_path = state_dir.join(PACKAGE_PUBLICATION_LOCK_FILE);
        if !publication_lock_path.exists() && !pointer_path.exists() {
            return Ok(None);
        }
        require_regular_file(&publication_lock_path)?;
        let publication_lock = open_existing_lock_file(&publication_lock_path)?;
        FileExt::lock_shared(&publication_lock)
            .map_err(|error| PackageSnapshotError::io("lock", &publication_lock_path, error))?;

        if !pointer_path.is_file() {
            return Ok(None);
        }
        require_regular_file(&pointer_path)?;

        let pointer = read_toml::<PackageGenerationPointer>(&pointer_path)?;
        pointer.validate(&pointer_path)?;
        let generations_dir =
            canonical_directory_within(&state_dir, &state_dir.join(PACKAGE_GENERATIONS_DIR))?;
        let generation_root = canonical_directory_within(
            &generations_dir,
            &generations_dir.join(&pointer.generation),
        )?;
        let lease_path = generation_root.join(GENERATION_LEASE_FILE);
        require_regular_file(&lease_path)?;
        let lease = open_existing_lock_file(&lease_path)?;
        FileExt::lock_shared(&lease)
            .map_err(|error| PackageSnapshotError::io("lock", &lease_path, error))?;

        // The generation lease now protects every immutable artifact below the
        // selected root, so GC no longer needs to be excluded.
        FileExt::unlock(&publication_lock)
            .map_err(|error| PackageSnapshotError::io("unlock", &publication_lock_path, error))?;

        let manifest_path = generation_root.join(GENERATION_MANIFEST_FILE);
        require_regular_file(&manifest_path)?;
        let manifest = read_toml::<PackageGenerationManifest>(&manifest_path)?;
        manifest.validate(&manifest_path)?;
        if manifest.generation != pointer.generation {
            return Err(PackageSnapshotError::Invalid(format!(
                "{} names generation {:?}, expected {:?}",
                manifest_path.display(),
                manifest.generation,
                pointer.generation
            )));
        }
        let packages_root = canonical_directory_within(
            &generation_root,
            &generation_root.join(GENERATION_PACKAGES_DIR),
        )?;
        let lock_path = generation_root.join(GENERATION_LOCK_FILE);
        require_regular_file(&lock_path)?;
        let lock_bytes = fs::read(&lock_path)
            .map_err(|error| PackageSnapshotError::io("read", &lock_path, error))?;
        let actual_lock_digest = package_lock_digest(&lock_bytes);
        if actual_lock_digest != manifest.lock_digest {
            return Err(PackageSnapshotError::Invalid(format!(
                "{} digest mismatch: generation manifest records {}, actual {}",
                lock_path.display(),
                manifest.lock_digest,
                actual_lock_digest
            )));
        }
        let package_names = parse_package_names(&lock_path, &lock_bytes)?;

        Ok(Some(Self {
            project_root,
            generation: pointer.generation,
            generation_root,
            packages_root,
            lock_path,
            lock_digest: manifest.lock_digest,
            package_names,
            _lease: lease,
        }))
    }

    /// The nearest ancestor of `anchor` that publishes a package generation.
    ///
    /// Split out from `acquire_nearest` so a caller resolving many files can
    /// find each one's root — a cheap stat-walk — and then acquire only once
    /// per DISTINCT root. Acquiring is the expensive half (canonicalize, two
    /// shared flocks, two TOML parses, and a re-read plus SHA256 of the
    /// lockfile), so a caller that acquires per file and dedupes afterwards
    /// pays it once per file and throws all but one result away.
    pub fn nearest_project_root(anchor: &Path) -> Option<PathBuf> {
        #[cfg(test)]
        probe_counter::record_root_walk();
        let mut cursor = if anchor.is_dir() {
            Some(anchor)
        } else {
            anchor.parent()
        };
        while let Some(dir) = cursor {
            if dir
                .join(PACKAGE_STATE_DIR)
                .join(PACKAGE_CURRENT_FILE)
                .is_file()
            {
                return Some(dir.to_path_buf());
            }
            if dir.join(".git").exists() {
                break;
            }
            cursor = dir.parent();
        }
        None
    }

    pub fn acquire_nearest(anchor: &Path) -> Result<Option<Self>, PackageSnapshotError> {
        match Self::nearest_project_root(anchor) {
            Some(root) => Self::acquire(&root),
            None => Ok(None),
        }
    }

    pub fn project_root(&self) -> &Path {
        &self.project_root
    }

    pub fn generation(&self) -> &str {
        &self.generation
    }

    pub fn generation_root(&self) -> &Path {
        &self.generation_root
    }

    pub fn packages_root(&self) -> &Path {
        &self.packages_root
    }

    pub fn lock_path(&self) -> &Path {
        &self.lock_path
    }

    pub fn lock_digest(&self) -> &str {
        &self.lock_digest
    }

    pub fn package_names(&self) -> &[String] {
        &self.package_names
    }
}

#[derive(Debug)]
pub enum PackageSnapshotError {
    Io {
        operation: &'static str,
        path: PathBuf,
        source: io::Error,
    },
    Invalid(String),
}

impl PackageSnapshotError {
    fn io(operation: &'static str, path: &Path, source: io::Error) -> Self {
        Self::Io {
            operation,
            path: path.to_path_buf(),
            source,
        }
    }
}

impl fmt::Display for PackageSnapshotError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io {
                operation,
                path,
                source,
            } => write!(
                formatter,
                "failed to {operation} {}: {source}",
                path.display()
            ),
            Self::Invalid(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for PackageSnapshotError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Invalid(_) => None,
        }
    }
}

pub fn package_state_dir(project_root: &Path) -> PathBuf {
    project_root.join(PACKAGE_STATE_DIR)
}

pub fn package_generations_dir(project_root: &Path) -> PathBuf {
    package_state_dir(project_root).join(PACKAGE_GENERATIONS_DIR)
}

pub fn package_publication_lock_path(project_root: &Path) -> PathBuf {
    package_state_dir(project_root).join(PACKAGE_PUBLICATION_LOCK_FILE)
}

pub fn package_current_path(project_root: &Path) -> PathBuf {
    package_state_dir(project_root).join(PACKAGE_CURRENT_FILE)
}

pub fn generation_root(project_root: &Path, generation: &str) -> PathBuf {
    package_generations_dir(project_root).join(generation)
}

pub fn open_lock_file(path: &Path) -> Result<File, PackageSnapshotError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| PackageSnapshotError::io("create", parent, error))?;
    }
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(|error| PackageSnapshotError::io("open", path, error))
}

fn open_existing_lock_file(path: &Path) -> Result<File, PackageSnapshotError> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .map_err(|error| PackageSnapshotError::io("open", path, error))
}

fn read_toml<T>(path: &Path) -> Result<T, PackageSnapshotError>
where
    T: for<'de> Deserialize<'de>,
{
    let source =
        fs::read_to_string(path).map_err(|error| PackageSnapshotError::io("read", path, error))?;
    toml::from_str(&source).map_err(|error| {
        PackageSnapshotError::Invalid(format!("failed to parse {}: {error}", path.display()))
    })
}

fn validate_schema_version(version: u32, path: &Path) -> Result<(), PackageSnapshotError> {
    if version == PACKAGE_GENERATION_SCHEMA_VERSION {
        Ok(())
    } else {
        Err(PackageSnapshotError::Invalid(format!(
            "unsupported {} schema version {} (expected {})",
            path.display(),
            version,
            PACKAGE_GENERATION_SCHEMA_VERSION
        )))
    }
}

pub fn validate_generation_id(generation: &str) -> Result<(), PackageSnapshotError> {
    let path = Path::new(generation);
    let mut components = path.components();
    let Some(Component::Normal(component)) = components.next() else {
        return Err(invalid_generation_id(generation));
    };
    if components.next().is_some()
        || component.to_str() != Some(generation)
        || generation.starts_with('.')
        || !generation
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(invalid_generation_id(generation));
    }
    Ok(())
}

fn invalid_generation_id(generation: &str) -> PackageSnapshotError {
    PackageSnapshotError::Invalid(format!("invalid package generation id {generation:?}"))
}

fn validate_lock_digest(digest: &str) -> Result<(), PackageSnapshotError> {
    let Some(hex) = digest.strip_prefix("sha256:") else {
        return Err(PackageSnapshotError::Invalid(format!(
            "invalid package lock digest {digest:?}"
        )));
    };
    if hex.len() != 64 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(PackageSnapshotError::Invalid(format!(
            "invalid package lock digest {digest:?}"
        )));
    }
    Ok(())
}

#[derive(Deserialize)]
struct PublishedLock {
    #[serde(default, rename = "package")]
    packages: Vec<PublishedLockEntry>,
}

#[derive(Deserialize)]
struct PublishedLockEntry {
    name: String,
}

fn parse_package_names(path: &Path, bytes: &[u8]) -> Result<Vec<String>, PackageSnapshotError> {
    let source = std::str::from_utf8(bytes).map_err(|error| {
        PackageSnapshotError::Invalid(format!("{} is not UTF-8: {error}", path.display()))
    })?;
    let lock = toml::from_str::<PublishedLock>(source).map_err(|error| {
        PackageSnapshotError::Invalid(format!("failed to parse {}: {error}", path.display()))
    })?;
    let mut names = std::collections::BTreeSet::new();
    for entry in lock.packages {
        if !is_valid_package_name(&entry.name) || !names.insert(entry.name.clone()) {
            return Err(PackageSnapshotError::Invalid(format!(
                "{} contains an invalid or duplicate package name {:?}",
                path.display(),
                entry.name
            )));
        }
    }
    Ok(names.into_iter().collect())
}

/// Return whether `name` is a safe single-component package import alias.
pub fn is_valid_package_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
}

fn encode_hex(bytes: &[u8]) -> String {
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(encoded, "{byte:02x}");
    }
    encoded
}

pub fn package_lock_digest(bytes: &[u8]) -> String {
    format!("sha256:{}", encode_hex(&Sha256::digest(bytes)))
}

fn canonical_directory_within(root: &Path, path: &Path) -> Result<PathBuf, PackageSnapshotError> {
    let canonical = path
        .canonicalize()
        .map_err(|error| PackageSnapshotError::io("canonicalize", path, error))?;
    if canonical == root || canonical.starts_with(root) {
        Ok(canonical)
    } else {
        Err(PackageSnapshotError::Invalid(format!(
            "package generation directory escapes {}: {}",
            root.display(),
            path.display()
        )))
    }
}

fn require_regular_file(path: &Path) -> Result<(), PackageSnapshotError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| PackageSnapshotError::io("stat", path, error))?;
    if metadata.file_type().is_file() {
        return Ok(());
    }
    Err(PackageSnapshotError::Invalid(format!(
        "package generation file is not a regular file: {}",
        path.display()
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};

    fn publish_fixture(root: &Path, generation: &str, body: &str) {
        let generation_root = generation_root(root, generation);
        fs::create_dir_all(generation_root.join(GENERATION_PACKAGES_DIR)).unwrap();
        fs::write(generation_root.join(GENERATION_LOCK_FILE), body).unwrap();
        fs::write(generation_root.join(GENERATION_LEASE_FILE), []).unwrap();
        let digest = package_lock_digest(body.as_bytes());
        let manifest = PackageGenerationManifest::new(generation, digest).unwrap();
        fs::write(
            generation_root.join(GENERATION_MANIFEST_FILE),
            toml::to_string_pretty(&manifest).unwrap(),
        )
        .unwrap();
        let pointer = PackageGenerationPointer::new(generation).unwrap();
        fs::create_dir_all(package_state_dir(root)).unwrap();
        fs::write(
            package_current_path(root),
            toml::to_string_pretty(&pointer).unwrap(),
        )
        .unwrap();
        File::create(package_publication_lock_path(root)).unwrap();
    }

    #[test]
    fn snapshot_holds_generation_lease_until_drop() {
        let temp = tempfile::tempdir().unwrap();
        publish_fixture(temp.path(), "generation_a", "version = 4\n# lock a\n");

        let snapshot = PackageSnapshot::acquire(temp.path()).unwrap().unwrap();
        let lease =
            open_existing_lock_file(&snapshot.generation_root().join(GENERATION_LEASE_FILE))
                .unwrap();
        assert!(FileExt::try_lock_exclusive(&lease).is_err());

        drop(snapshot);
        FileExt::try_lock_exclusive(&lease).unwrap();
    }

    #[test]
    fn reader_selects_generation_published_before_publication_unlock() {
        let temp = tempfile::tempdir().unwrap();
        publish_fixture(temp.path(), "generation_a", "version = 4\n# lock a\n");
        let root = temp.path().to_path_buf();
        let publication = open_lock_file(&package_publication_lock_path(&root)).unwrap();
        FileExt::lock_exclusive(&publication).unwrap();

        let started = Arc::new(Barrier::new(2));
        let reader_started = Arc::clone(&started);
        let reader_root = root.clone();
        let reader = std::thread::spawn(move || {
            reader_started.wait();
            PackageSnapshot::acquire(&reader_root).unwrap().unwrap()
        });
        started.wait();

        publish_fixture(&root, "generation_b", "version = 4\n# lock b\n");
        FileExt::unlock(&publication).unwrap();

        let snapshot = reader.join().unwrap();
        assert_eq!(snapshot.generation(), "generation_b");
        assert_eq!(
            fs::read_to_string(snapshot.lock_path()).unwrap(),
            "version = 4\n# lock b\n"
        );
    }

    #[test]
    fn malformed_pointer_cannot_escape_generation_root() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir_all(package_state_dir(temp.path())).unwrap();
        fs::write(
            package_current_path(temp.path()),
            "schema_version = 1\ngeneration = \"../outside\"\n",
        )
        .unwrap();
        File::create(package_publication_lock_path(temp.path())).unwrap();

        let error = PackageSnapshot::acquire(temp.path()).unwrap_err();
        assert!(
            error.to_string().contains("invalid package generation id"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn lock_package_name_cannot_escape_packages_root() {
        let temp = tempfile::tempdir().unwrap();
        publish_fixture(
            temp.path(),
            "generation_a",
            "version = 4\n\n[[package]]\nname = \"../outside\"\n",
        );

        let error = PackageSnapshot::acquire(temp.path()).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("invalid or duplicate package name"),
            "unexpected error: {error}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_generation_root_cannot_escape_generations_directory() {
        let temp = tempfile::tempdir().unwrap();
        publish_fixture(temp.path(), "generation_a", "version = 4\n");
        let generation = generation_root(temp.path(), "generation_a");
        let outside = temp.path().join("outside-generation");
        fs::rename(&generation, &outside).unwrap();
        std::os::unix::fs::symlink(&outside, &generation).unwrap();

        let error = PackageSnapshot::acquire(temp.path()).unwrap_err();
        assert!(
            error.to_string().contains("escapes"),
            "unexpected error: {error}"
        );
    }
}
