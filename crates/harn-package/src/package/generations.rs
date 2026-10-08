use harn_modules::package_snapshot::{
    generation_root, open_lock_file, package_current_path, package_generations_dir,
    package_lock_digest, package_publication_lock_path, PackageGenerationManifest,
    PackageGenerationPointer, PackageSnapshot, GENERATION_LEASE_FILE, GENERATION_LOCK_FILE,
    GENERATION_MANIFEST_FILE, GENERATION_PACKAGES_DIR,
};
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::{
    materialized_hash_matches, validate_package_alias, verify_content_hash_or_compute, LockFile,
    ManifestContext, PackageError,
};

const LEGACY_PACKAGES_DIR: &str = ".harn/packages";
const STAGING_PREFIX: &str = ".staging-";
const GENERATION_PREFIX: &str = "generation-";
const PACKAGE_INSTALL_LOCK_TIMEOUT: Duration = Duration::from_mins(30);
const PACKAGE_PUBLICATION_LOCK_TIMEOUT: Duration = Duration::from_mins(5);

pub(crate) fn publish_package_generation<F>(
    ctx: &ManifestContext,
    lock: &LockFile,
    force_rebuild: bool,
    materialize: F,
) -> Result<usize, PackageError>
where
    F: FnOnce(&Path) -> Result<usize, PackageError>,
{
    let _install_lock = acquire_package_install_lock(ctx)?;
    publish_package_generation_locked(ctx, lock, force_rebuild, materialize)
}

/// Publish while the caller holds this project's package-install lock.
/// Demand-driven publication uses this seam to read, merge, and replace the
/// current immutable generation as one cross-process critical section.
pub(crate) fn publish_package_generation_locked<F>(
    ctx: &ManifestContext,
    lock: &LockFile,
    force_rebuild: bool,
    materialize: F,
) -> Result<usize, PackageError>
where
    F: FnOnce(&Path) -> Result<usize, PackageError>,
{
    let generations_dir = package_generations_dir(&ctx.dir);
    fs::create_dir_all(&generations_dir)
        .map_err(|error| format!("failed to create {}: {error}", generations_dir.display()))?;
    remove_abandoned_staging_directories(&generations_dir)?;
    let lock_bytes = lock.encode()?;
    let lock_digest = package_lock_digest(&lock_bytes);
    if !force_rebuild && current_generation_matches_lock(ctx, lock, &lock_digest)? {
        return Ok(lock.packages.len());
    }

    let unique = uuid::Uuid::now_v7().simple().to_string();
    let generation = format!("{GENERATION_PREFIX}{unique}");
    let staging_root = generations_dir.join(format!("{STAGING_PREFIX}{unique}"));
    let final_root = generation_root(&ctx.dir, &generation);
    fs::create_dir(&staging_root)
        .map_err(|error| format!("failed to create {}: {error}", staging_root.display()))?;
    let prepared = PreparedGeneration::new(staging_root);
    let packages_root = prepared.root.join(GENERATION_PACKAGES_DIR);
    fs::create_dir(&packages_root)
        .map_err(|error| format!("failed to create {}: {error}", packages_root.display()))?;

    let installed = materialize(&packages_root)?;
    validate_staged_packages(&packages_root, lock)?;
    write_generation_file(
        &prepared.root.join(GENERATION_LOCK_FILE),
        lock_bytes.as_slice(),
    )?;
    write_generation_file(&prepared.root.join(GENERATION_LEASE_FILE), &[])?;
    let manifest = PackageGenerationManifest::new(&generation, lock_digest)
        .map_err(|error| PackageError::Lockfile(error.to_string()))?;
    let manifest_bytes = toml::to_string_pretty(&manifest)
        .map_err(|error| format!("failed to encode package generation manifest: {error}"))?;
    write_generation_file(
        &prepared.root.join(GENERATION_MANIFEST_FILE),
        manifest_bytes.as_bytes(),
    )?;
    sync_directory(&prepared.root)?;

    fs::rename(&prepared.root, &final_root).map_err(|error| {
        format!(
            "failed to publish prepared package generation {} as {}: {error}",
            prepared.root.display(),
            final_root.display()
        )
    })?;
    prepared.disarm();
    sync_directory(&generations_dir)?;

    publish_pointer_and_collect(ctx, &generation)?;
    Ok(installed)
}

/// Reject a staged package tree that does not match the lock it claims to
/// realize, before anything can observe it as a published generation.
///
/// Materialization copies each package out of the shared cache with no lock
/// held on the source, so a partial or contaminated copy is reachable without
/// any single step reporting an error. Publishing then fsyncs and renames
/// whatever it was handed, and the reader side only ever re-checks the
/// lockfile digest — never the tree — so a package missing a file stays
/// invisible until some later import trips over it, in an unrelated command,
/// naming a path instead of a cause.
///
/// The same predicate already gates *reuse* of a published generation
/// (`current_generation_matches_lock`). Running it once more on the staged
/// tree is what makes "published" mean "complete": a generation either
/// realizes its lock or it never becomes visible.
fn validate_staged_packages(packages_root: &Path, lock: &LockFile) -> Result<(), PackageError> {
    for entry in &lock.packages {
        validate_package_alias(&entry.name)?;
        let directory = packages_root.join(&entry.name);
        let file = packages_root.join(format!("{}.harn", entry.name));
        if entry.source.starts_with("path+") {
            if !directory.exists() && !file.exists() {
                return Err(incomplete_generation(
                    &entry.name,
                    packages_root,
                    "nothing was materialized for it",
                ));
            }
            continue;
        }
        let Some(expected_hash) = entry.content_hash.as_deref() else {
            return Err(PackageError::Lockfile(format!(
                "cannot publish a package generation: {} has no content hash in the lock",
                entry.name
            )));
        };
        if !directory.is_dir() {
            return Err(incomplete_generation(
                &entry.name,
                packages_root,
                "its package directory is missing",
            ));
        }
        verify_content_hash_or_compute(&directory, expected_hash).map_err(|error| {
            incomplete_generation(
                &entry.name,
                packages_root,
                &format!("its materialized contents do not match the lock ({error})"),
            )
        })?;
    }
    Ok(())
}

fn incomplete_generation(alias: &str, packages_root: &Path, detail: &str) -> PackageError {
    PackageError::Lockfile(format!(
        "refusing to publish an incomplete package generation staged at {}: package {alias} is unusable because {detail}. \
         This usually means the package cache was written concurrently while it was being copied; re-run the command, \
         or run `harn install --refetch {alias}` to repopulate the cache from its source.",
        packages_root.display()
    ))
}

fn current_generation_matches_lock(
    ctx: &ManifestContext,
    lock: &LockFile,
    expected_lock_digest: &str,
) -> Result<bool, PackageError> {
    let snapshot = match harn_modules::package_snapshot::PackageSnapshot::acquire(&ctx.dir) {
        Ok(Some(snapshot)) => snapshot,
        Ok(None) => return Ok(false),
        Err(harn_modules::package_snapshot::PackageSnapshotError::Io { source, .. })
            if source.kind() == io::ErrorKind::NotFound =>
        {
            return Ok(false);
        }
        Err(error) => return Err(PackageError::Lockfile(error.to_string())),
    };
    // A byte-identical lock is trivially the same resolution, so accept it
    // without inspecting the tree. A *differing* one is not automatically a
    // different resolution: the digest covers the whole `harn.lock`, including
    // `generator_version` / `protocol_artifact_version`, which record which CLI
    // resolved the lock and have no bearing on what was materialized. Bumping
    // Harn rewrites those two lines and nothing else, so gating on the digest
    // alone discarded a byte-for-byte correct generation on every bump and
    // re-fetched every dependency from its source.
    //
    // So compare the stored and requested resolutions when the bytes differ.
    // Package names alone are insufficient: changing an alias from Git to a
    // local path (or between two paths) must publish a new generation, or the
    // old package tree remains silently executable under the new lock.
    if snapshot.lock_digest() != expected_lock_digest {
        let materialized_lock = LockFile::load(snapshot.lock_path())?.ok_or_else(|| {
            PackageError::Lockfile(format!(
                "{} is missing from the active package generation",
                snapshot.lock_path().display()
            ))
        })?;
        if !materialized_lock.same_resolution(lock) {
            return Ok(false);
        }
    }
    for entry in &lock.packages {
        validate_package_alias(&entry.name)?;
        let directory = snapshot.packages_root().join(&entry.name);
        let file = snapshot
            .packages_root()
            .join(format!("{}.harn", entry.name));
        if entry.source.starts_with("path+") {
            if !directory.exists() && !file.exists() {
                return Ok(false);
            }
            continue;
        }
        let Some(expected_hash) = entry.content_hash.as_deref() else {
            return Ok(false);
        };
        if !directory.is_dir() || !materialized_hash_matches(&directory, expected_hash) {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Whether the active immutable generation already supplies every entry in a
/// demand-selected lock. Extra packages are harmless for this read-only reuse
/// check: the reachable graph still controls what can be imported, while a
/// full install continues to require exact whole-lock authority above.
pub(crate) fn current_generation_satisfies_lock_subset(
    ctx: &ManifestContext,
    requested: &LockFile,
) -> Result<bool, PackageError> {
    let snapshot = match PackageSnapshot::acquire(&ctx.dir) {
        Ok(Some(snapshot)) => snapshot,
        Ok(None) => return Ok(false),
        Err(harn_modules::package_snapshot::PackageSnapshotError::Io { source, .. })
            if source.kind() == io::ErrorKind::NotFound =>
        {
            return Ok(false);
        }
        Err(error) => return Err(PackageError::Lockfile(error.to_string())),
    };
    let materialized = LockFile::load(snapshot.lock_path())?.ok_or_else(|| {
        PackageError::Lockfile(format!(
            "{} is missing from the active package generation",
            snapshot.lock_path().display()
        ))
    })?;
    for entry in &requested.packages {
        let Some(installed) = materialized.find(&entry.name) else {
            return Ok(false);
        };
        if !installed.same_resolution(entry) {
            return Ok(false);
        }
        let directory = snapshot.packages_root().join(&entry.name);
        let file = snapshot
            .packages_root()
            .join(format!("{}.harn", entry.name));
        if entry.source.starts_with("path+") {
            if !directory.exists() && !file.exists() {
                return Ok(false);
            }
        } else if entry
            .content_hash
            .as_deref()
            .is_none_or(|hash| !directory.is_dir() || !materialized_hash_matches(&directory, hash))
        {
            return Ok(false);
        }
    }
    Ok(true)
}

pub(crate) fn current_package_snapshot(
    ctx: &ManifestContext,
) -> Result<harn_modules::package_snapshot::PackageSnapshot, PackageError> {
    harn_modules::package_snapshot::PackageSnapshot::acquire(&ctx.dir)
        .map_err(|error| PackageError::Lockfile(error.to_string()))?
        .ok_or_else(|| {
            PackageError::Lockfile(format!(
                "{} is missing; run `harn install`",
                package_current_path(&ctx.dir).display()
            ))
        })
}

pub(crate) fn dependency_package_snapshot(
    manifest: &super::Manifest,
    project_root: &Path,
) -> Result<Option<PackageSnapshot>, PackageError> {
    let snapshot = PackageSnapshot::acquire(project_root)
        .map_err(|error| PackageError::Lockfile(error.to_string()))?;
    if manifest.dependencies.is_empty() {
        if snapshot
            .as_ref()
            .is_some_and(|current| !current.package_names().is_empty())
        {
            return Err(format!(
                "{} is out of date; run `harn install`",
                project_root.join(super::LOCK_FILE).display()
            )
            .into());
        }
        return Ok(snapshot);
    }
    if snapshot.is_none() {
        return Err(PackageError::Lockfile(format!(
            "{} is missing; run `harn install`",
            package_current_path(project_root).display()
        )));
    }
    Ok(snapshot)
}

pub(crate) fn acquire_package_install_lock(ctx: &ManifestContext) -> Result<File, PackageError> {
    let path = ctx.dir.join(".harn").join("package-install.lock");
    let file = open_lock_file(&path).map_err(|error| PackageError::Lockfile(error.to_string()))?;
    harn_flock::lock_with_deadline(
        &file,
        &path,
        harn_flock::LockMode::Exclusive,
        PACKAGE_INSTALL_LOCK_TIMEOUT,
    )
    .map_err(|error| PackageError::Lockfile(error.to_string()))?;
    Ok(file)
}

fn publish_pointer_and_collect(
    ctx: &ManifestContext,
    generation: &str,
) -> Result<(), PackageError> {
    let publication_path = package_publication_lock_path(&ctx.dir);
    let publication = open_lock_file(&publication_path)
        .map_err(|error| PackageError::Lockfile(error.to_string()))?;
    harn_flock::lock_with_deadline(
        &publication,
        &publication_path,
        harn_flock::LockMode::Exclusive,
        PACKAGE_PUBLICATION_LOCK_TIMEOUT,
    )
    .map_err(|error| PackageError::Lockfile(error.to_string()))?;

    let pointer = PackageGenerationPointer::new(generation)
        .map_err(|error| PackageError::Lockfile(error.to_string()))?;
    let pointer_bytes = toml::to_string_pretty(&pointer)
        .map_err(|error| format!("failed to encode package generation pointer: {error}"))?;
    let pointer_path = package_current_path(&ctx.dir);
    harn_vm::atomic_io::atomic_write(&pointer_path, pointer_bytes.as_bytes())
        .map_err(|error| format!("failed to write {}: {error}", pointer_path.display()))?;

    if let Err(error) = remove_legacy_packages_dir(ctx) {
        eprintln!(
            "warning: published package generation but could not remove legacy state: {error}"
        );
    }
    if let Err(error) = collect_old_generations(ctx, generation) {
        eprintln!(
            "warning: published package generation but retained old generation state: {error}"
        );
    }
    Ok(())
}

fn remove_legacy_packages_dir(ctx: &ManifestContext) -> Result<(), PackageError> {
    let path = ctx.dir.join(LEGACY_PACKAGES_DIR);
    match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_symlink() || metadata.is_file() => {
            fs::remove_file(&path)
                .map_err(|error| format!("failed to remove {}: {error}", path.display()).into())
        }
        Ok(metadata) if metadata.is_dir() => fs::remove_dir_all(&path)
            .map_err(|error| format!("failed to remove {}: {error}", path.display()).into()),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("failed to stat {}: {error}", path.display()).into()),
    }
}

fn collect_old_generations(ctx: &ManifestContext, current: &str) -> Result<(), PackageError> {
    let generations_dir = package_generations_dir(&ctx.dir);
    for entry in fs::read_dir(&generations_dir)
        .map_err(|error| format!("failed to read {}: {error}", generations_dir.display()))?
    {
        let entry = entry.map_err(|error| {
            format!(
                "failed to read {} entry: {error}",
                generations_dir.display()
            )
        })?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if name == current {
            continue;
        }
        if name.starts_with(STAGING_PREFIX) {
            remove_generation_path(&entry.path())?;
            continue;
        }
        if !name.starts_with(GENERATION_PREFIX) || !entry.path().is_dir() {
            continue;
        }

        let lease_path = entry.path().join(GENERATION_LEASE_FILE);
        let lease = match OpenOptions::new().read(true).write(true).open(&lease_path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            // On Windows, a shared byte-range lock can reject a write-capable
            // open before we get a handle on which to try the exclusive lock.
            // That is the same observable state as lock contention: a live
            // reader owns this generation, so collection must leave it alone.
            Err(error) if open_is_contended(&error) => continue,
            Err(error) => {
                return Err(format!("failed to open {}: {error}", lease_path.display()).into())
            }
        };
        match lease.try_lock() {
            Ok(()) => {
                lease.unlock().map_err(|error| {
                    format!("failed to unlock {}: {error}", lease_path.display())
                })?;
                drop(lease);
                remove_generation_path(&entry.path())?;
            }
            Err(TryLockError::WouldBlock) => {}
            Err(error) => {
                return Err(format!("failed to lock {}: {error}", lease_path.display()).into())
            }
        }
    }
    Ok(())
}

/// Windows `ERROR_LOCK_VIOLATION`. `std` translates this into
/// [`TryLockError::WouldBlock`] inside `try_lock`, but an `open` rejected by a
/// live byte-range lock surfaces the raw code, and Windows maps only
/// `WSAEWOULDBLOCK` onto [`io::ErrorKind::WouldBlock`].
#[cfg(windows)]
const ERROR_LOCK_VIOLATION: i32 = 33;

/// Whether an `open` failed because another process holds the lease lock.
fn open_is_contended(error: &io::Error) -> bool {
    #[cfg(windows)]
    let lock_violation = error.raw_os_error() == Some(ERROR_LOCK_VIOLATION);
    #[cfg(not(windows))]
    let lock_violation = false;
    error.kind() == io::ErrorKind::WouldBlock || lock_violation
}

fn remove_abandoned_staging_directories(generations_dir: &Path) -> Result<(), PackageError> {
    let entries = match fs::read_dir(generations_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(format!("failed to read {}: {error}", generations_dir.display()).into())
        }
    };
    for entry in entries {
        let entry = entry.map_err(|error| {
            format!(
                "failed to read {} entry: {error}",
                generations_dir.display()
            )
        })?;
        if entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.starts_with(STAGING_PREFIX))
        {
            remove_generation_path(&entry.path())?;
        }
    }
    Ok(())
}

fn remove_generation_path(path: &Path) -> Result<(), PackageError> {
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("failed to remove {}: {error}", path.display()).into()),
    }
}

fn write_generation_file(path: &Path, bytes: &[u8]) -> Result<(), PackageError> {
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)
        .map_err(|error| format!("failed to write {}: {error}", path.display()))?;
    file.write_all(bytes)
        .map_err(|error| format!("failed to write {}: {error}", path.display()))?;
    file.sync_all()
        .map_err(|error| format!("failed to sync {}: {error}", path.display()))?;
    Ok(())
}

fn sync_directory(path: &Path) -> Result<(), PackageError> {
    match File::open(path).and_then(|directory| directory.sync_all()) {
        Ok(()) => Ok(()),
        // Windows does not permit opening directories through File::open.
        Err(_) if cfg!(windows) => Ok(()),
        Err(error) => Err(format!("failed to sync {}: {error}", path.display()).into()),
    }
}

struct PreparedGeneration {
    root: PathBuf,
    armed: std::cell::Cell<bool>,
}

impl PreparedGeneration {
    fn new(root: PathBuf) -> Self {
        Self {
            root,
            armed: std::cell::Cell::new(true),
        }
    }

    fn disarm(&self) {
        self.armed.set(false);
    }
}

impl Drop for PreparedGeneration {
    fn drop(&mut self) {
        if self.armed.get() {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::package::test_support::{
        create_test_package_generation, write_test_generation_lock,
    };
    use crate::package::{ensure_dependencies_materialized, LockEntry, MANIFEST};
    use harn_modules::package_snapshot::PackageSnapshot;

    fn test_context(root: &Path) -> ManifestContext {
        ManifestContext {
            manifest: toml::from_str(
                "[package]\nname = \"generation-test\"\nversion = \"0.1.0\"\n",
            )
            .unwrap(),
            dir: root.to_path_buf(),
        }
    }

    #[test]
    fn generation_file_write_and_sync_replaces_contents() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("harn.lock");

        write_generation_file(&path, b"long initial contents").unwrap();
        write_generation_file(&path, b"replacement").unwrap();

        assert_eq!(fs::read(path).unwrap(), b"replacement");
    }

    #[test]
    fn dependency_free_materialization_check_does_not_create_package_state() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let anchor = root.join("main.harn");
        fs::write(
            root.join(MANIFEST),
            "[package]\nname = \"dependency-free\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        fs::write(&anchor, "pipeline main() {}\n").unwrap();

        ensure_dependencies_materialized(&anchor).unwrap();

        assert!(!root.join(".harn").exists());
    }

    #[test]
    fn dependency_free_materialization_check_rejects_stale_generation() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let anchor = root.join("main.harn");
        fs::write(
            root.join(MANIFEST),
            "[package]\nname = \"dependency-free\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        fs::write(&anchor, "pipeline main() {}\n").unwrap();
        create_test_package_generation(root);
        write_test_generation_lock(
            root,
            "version = 5\n\n[[package]]\nname = \"stale\"\nsource = \"path+/tmp/stale\"\n",
        );

        let error = ensure_dependencies_materialized(&anchor).unwrap_err();

        assert!(error
            .to_string()
            .contains("is out of date; run `harn install`"));
    }

    #[test]
    fn publishing_new_generation_preserves_leased_old_generation() {
        let temp = tempfile::tempdir().unwrap();
        let ctx = test_context(temp.path());
        let lock = LockFile::default();
        publish_package_generation(&ctx, &lock, false, |packages| {
            fs::write(packages.join("old"), "old").unwrap();
            Ok(0)
        })
        .unwrap();
        let old = PackageSnapshot::acquire(temp.path()).unwrap().unwrap();

        publish_package_generation(&ctx, &lock, true, |packages| {
            fs::write(packages.join("new"), "new").unwrap();
            Ok(0)
        })
        .unwrap();
        let new = PackageSnapshot::acquire(temp.path()).unwrap().unwrap();

        assert_ne!(old.generation(), new.generation());
        assert_eq!(
            fs::read_to_string(old.packages_root().join("old")).unwrap(),
            "old"
        );
        assert_eq!(
            fs::read_to_string(new.packages_root().join("new")).unwrap(),
            "new"
        );
        assert!(old.generation_root().is_dir());

        let old_root = old.generation_root().to_path_buf();
        drop(old);
        publish_package_generation(&ctx, &lock, true, |_| Ok(0)).unwrap();
        assert!(!old_root.exists());
        assert!(new.generation_root().is_dir());
    }

    #[test]
    fn intact_current_generation_skips_materialization() {
        let temp = tempfile::tempdir().unwrap();
        let ctx = test_context(temp.path());
        let lock = LockFile::default();
        publish_package_generation(&ctx, &lock, false, |_| Ok(0)).unwrap();
        let before = PackageSnapshot::acquire(temp.path())
            .unwrap()
            .unwrap()
            .generation()
            .to_string();
        let materialized = std::cell::Cell::new(false);

        publish_package_generation(&ctx, &lock, false, |_| {
            materialized.set(true);
            Ok(0)
        })
        .unwrap();

        assert!(!materialized.get());
        assert_eq!(
            PackageSnapshot::acquire(temp.path())
                .unwrap()
                .unwrap()
                .generation(),
            before
        );
    }

    #[test]
    fn missing_current_packages_tree_is_rebuilt() {
        let temp = tempfile::tempdir().unwrap();
        let ctx = test_context(temp.path());
        let lock = LockFile::default();
        publish_package_generation(&ctx, &lock, false, |packages| {
            fs::write(packages.join("old"), "old").unwrap();
            Ok(0)
        })
        .unwrap();
        let old_generation = PackageSnapshot::acquire(temp.path())
            .unwrap()
            .unwrap()
            .generation()
            .to_string();
        fs::remove_dir_all(
            generation_root(temp.path(), &old_generation).join(GENERATION_PACKAGES_DIR),
        )
        .unwrap();

        publish_package_generation(&ctx, &lock, false, |packages| {
            fs::write(packages.join("new"), "new").unwrap();
            Ok(0)
        })
        .unwrap();

        let rebuilt = PackageSnapshot::acquire(temp.path()).unwrap().unwrap();
        assert_ne!(rebuilt.generation(), old_generation);
        assert_eq!(
            fs::read_to_string(rebuilt.packages_root().join("new")).unwrap(),
            "new"
        );
    }

    #[test]
    fn failed_prepare_keeps_current_generation_and_cleans_staging() {
        let temp = tempfile::tempdir().unwrap();
        let ctx = test_context(temp.path());
        let lock = LockFile::default();
        publish_package_generation(&ctx, &lock, false, |_| Ok(0)).unwrap();
        let before = PackageSnapshot::acquire(temp.path())
            .unwrap()
            .unwrap()
            .generation()
            .to_string();

        let failure = publish_package_generation(&ctx, &lock, true, |_| {
            Err(PackageError::Lockfile(
                "injected prepare failure".to_string(),
            ))
        })
        .unwrap_err();
        assert!(failure.to_string().contains("injected prepare failure"));
        assert_eq!(
            PackageSnapshot::acquire(temp.path())
                .unwrap()
                .unwrap()
                .generation(),
            before
        );
        assert!(fs::read_dir(package_generations_dir(temp.path()))
            .unwrap()
            .all(|entry| !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(STAGING_PREFIX)));
    }

    /// Publish a generation holding one git-sourced package whose recorded
    /// `content_hash` really is the hash of what was materialized, so the
    /// generation is genuinely valid for the lock it ships with.
    fn publish_vendored_generation(ctx: &ManifestContext) -> (LockFile, String) {
        let body = "pipeline main() {}\n";
        let scratch = tempfile::tempdir().unwrap();
        let sample = scratch.path().join("vendored");
        fs::create_dir(&sample).unwrap();
        fs::write(sample.join("main.harn"), body).unwrap();
        let content_hash = crate::package::compute_content_hash(&sample).unwrap();

        let lock = LockFile {
            packages: vec![LockEntry {
                name: "vendored".to_string(),
                source: "git+ssh://git@example.invalid/vendored.git".to_string(),
                content_hash: Some(content_hash),
                ..LockEntry::default()
            }],
            ..LockFile::default()
        };

        publish_package_generation(ctx, &lock, false, |packages| {
            let dir = packages.join("vendored");
            fs::create_dir(&dir).unwrap();
            fs::write(dir.join("main.harn"), body).unwrap();
            Ok(1)
        })
        .unwrap();

        let generation = PackageSnapshot::acquire(&ctx.dir)
            .unwrap()
            .unwrap()
            .generation()
            .to_string();
        (lock, generation)
    }

    /// Re-stamp a published generation's stored lock as if an older CLI had
    /// written it. `encode()` always normalizes the provenance stamps to the
    /// running CLI, so this is the only way to express the real artifact: a
    /// generation committed by Harn N sitting in a checkout now running N+1.
    fn restamp_generation_lock(root: &Path, lock: &LockFile, stamp: &str) {
        let encoded = String::from_utf8(lock.encode().unwrap()).unwrap();
        let aged = encoded.replace(env!("CARGO_PKG_VERSION"), stamp);
        assert_ne!(aged, encoded, "restamping must actually change the bytes");
        write_test_generation_lock(root, &aged);
    }

    /// Bumping Harn rewrites `generator_version` and `protocol_artifact_version`
    /// in `harn.lock` and nothing else. The materialized tree is unaffected, so
    /// re-materializing it is pure waste — and for a private git dependency on a
    /// cold cache it is not merely waste, it is a fetch that needs credentials
    /// the bump does not have.
    #[test]
    fn bumping_only_the_generator_stamp_reuses_the_materialized_generation() {
        let temp = tempfile::tempdir().unwrap();
        let ctx = test_context(temp.path());
        let (lock, generation) = publish_vendored_generation(&ctx);
        restamp_generation_lock(temp.path(), &lock, "0.0.1-aged");

        publish_package_generation(&ctx, &lock, false, |_| {
            panic!("re-materialized a generation that already matches the lock")
        })
        .unwrap();

        assert_eq!(
            PackageSnapshot::acquire(temp.path())
                .unwrap()
                .unwrap()
                .generation(),
            generation,
            "only the provenance stamps moved, so the generation must be reused"
        );
    }

    /// Authentication transport can change between an interactive checkout
    /// and automation without changing the repository, commit, or bytes. That
    /// spelling-only change must not discard a valid committed generation and
    /// force a private dependency fetch.
    #[test]
    fn changing_only_git_transport_reuses_the_materialized_generation() {
        let temp = tempfile::tempdir().unwrap();
        let ctx = test_context(temp.path());
        let (mut lock, generation) = publish_vendored_generation(&ctx);
        lock.packages[0].source = "git+https://example.invalid/vendored".to_string();

        publish_package_generation(&ctx, &lock, false, |_| {
            panic!("re-materialized a generation whose Git repository identity still matches")
        })
        .unwrap();

        assert_eq!(
            PackageSnapshot::acquire(temp.path())
                .unwrap()
                .unwrap()
                .generation(),
            generation,
            "only the Git transport moved, so the generation must be reused"
        );
    }

    #[test]
    fn changing_a_dependency_source_rebuilds_the_generation() {
        let temp = tempfile::tempdir().unwrap();
        let ctx = test_context(temp.path());
        let (mut lock, generation) = publish_vendored_generation(&ctx);

        lock.packages[0].source = "path+file:///tmp/vendored".to_string();
        lock.packages[0].content_hash = None;
        publish_package_generation(&ctx, &lock, false, |packages| {
            let dir = packages.join("vendored");
            fs::create_dir(&dir).unwrap();
            fs::write(dir.join("main.harn"), "pipeline replacement() {}\n").unwrap();
            Ok(1)
        })
        .unwrap();

        let snapshot = PackageSnapshot::acquire(temp.path()).unwrap().unwrap();
        assert_ne!(
            snapshot.generation(),
            generation,
            "a new source must never reuse the old package tree"
        );
        assert_eq!(
            fs::read_to_string(snapshot.packages_root().join("vendored/main.harn")).unwrap(),
            "pipeline replacement() {}\n"
        );
    }

    /// The per-entry integrity check walks the requested lock, so it cannot see
    /// a package that lock no longer names. The stored-resolution comparison
    /// must reject that stale generation before the integrity loop runs.
    #[test]
    fn dropping_a_dependency_rebuilds_even_though_every_remaining_entry_matches() {
        let temp = tempfile::tempdir().unwrap();
        let ctx = test_context(temp.path());
        let (mut lock, generation) = publish_vendored_generation(&ctx);
        restamp_generation_lock(temp.path(), &lock, "0.0.1-aged");

        lock.packages.clear();
        publish_package_generation(&ctx, &lock, false, |_| Ok(0)).unwrap();

        let snapshot = PackageSnapshot::acquire(temp.path()).unwrap().unwrap();
        assert_ne!(
            snapshot.generation(),
            generation,
            "the dependency left the lock, so its materialization must not survive"
        );
        assert!(
            !snapshot.packages_root().join("vendored").exists(),
            "dropped dependency is still importable from the generation"
        );
    }

    /// `RuntimeExtensions` hands out bare paths beneath a generation —
    /// `provider_connectors[].manifest_dir` most importantly — and the files
    /// behind them are opened lazily, when a connector contract is first
    /// loaded. That can be long after the struct was built. Holding the
    /// snapshot is what holds the lease, and the lease is the only thing
    /// stopping a concurrent publisher from collecting the tree those paths
    /// point into.
    #[test]
    fn runtime_extensions_hold_their_generation_against_collection() {
        let temp = tempfile::tempdir().unwrap();
        let ctx = test_context(temp.path());
        let lock = LockFile::default();
        publish_package_generation(&ctx, &lock, false, |packages| {
            let dir = packages.join("connector-pkg/src");
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("lib.harn"), "pipeline main() {}\n").unwrap();
            Ok(0)
        })
        .unwrap();
        let snapshot = PackageSnapshot::acquire(temp.path()).unwrap().unwrap();
        let generation_root = snapshot.generation_root().to_path_buf();
        let connector = snapshot.packages_root().join("connector-pkg/src/lib.harn");

        // Exactly what `try_load_runtime_extensions` produces: bare paths into
        // the generation, plus the snapshot that keeps them resolvable.
        let extensions = crate::package::RuntimeExtensions {
            package_snapshot: Some(std::sync::Arc::new(snapshot)),
            ..crate::package::RuntimeExtensions::default()
        };

        publish_package_generation(&ctx, &lock, true, |_| Ok(0)).unwrap();

        assert!(
            connector.is_file(),
            "a live RuntimeExtensions must keep its connector modules readable \
             across a concurrent publish; a reader that lost the lease sees a \
             bare ENOENT on {}",
            connector.display()
        );
        assert_eq!(
            extensions.package_generation(),
            Some(generation_root.file_name().unwrap().to_str().unwrap()),
            "the extensions must report the generation they resolve against"
        );

        drop(extensions);
        publish_package_generation(&ctx, &lock, true, |_| Ok(0)).unwrap();
        assert!(
            !generation_root.exists(),
            "once no reader holds the lease the generation must be collectable"
        );
    }

    /// Materialization copies each package out of the shared cache holding no
    /// lock on the source, so a concurrent writer can make that copy lossy
    /// without any step returning an error. Publishing must not turn a lossy
    /// copy into a generation other processes then import from: the failure
    /// has to land here, naming the package, not later as a bare ENOENT on
    /// whichever file some unrelated command happened to import first.
    #[test]
    fn publishing_rejects_a_package_tree_that_is_missing_a_file() {
        let temp = tempfile::tempdir().unwrap();
        let ctx = test_context(temp.path());
        let scratch = tempfile::tempdir().unwrap();
        let sample = scratch.path().join("vendored");
        fs::create_dir_all(sample.join("src")).unwrap();
        fs::write(sample.join("src/lib.harn"), "pipeline main() {}\n").unwrap();
        fs::write(sample.join("src/extra.harn"), "pipeline extra() {}\n").unwrap();
        let lock = LockFile {
            packages: vec![LockEntry {
                name: "vendored".to_string(),
                source: "git+https://example.invalid/vendored".to_string(),
                content_hash: Some(crate::package::compute_content_hash(&sample).unwrap()),
                ..LockEntry::default()
            }],
            ..LockFile::default()
        };

        // Exactly the observed shape: every directory is created and all but
        // one file is copied, and the copy itself reports success.
        let error = publish_package_generation(&ctx, &lock, false, |packages| {
            let dir = packages.join("vendored/src");
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("lib.harn"), "pipeline main() {}\n").unwrap();
            Ok(1)
        })
        .unwrap_err();

        let message = error.to_string();
        assert!(
            message.contains("incomplete package generation") && message.contains("vendored"),
            "publish must fail loudly and name the package, got: {message}"
        );
        assert!(
            PackageSnapshot::acquire(temp.path()).unwrap().is_none(),
            "an incomplete tree must never become the published generation"
        );
    }

    /// A package the materializer skipped entirely is the same defect one step
    /// further along, and must not reach a reader either.
    #[test]
    fn publishing_rejects_a_package_that_was_never_materialized() {
        let temp = tempfile::tempdir().unwrap();
        let ctx = test_context(temp.path());
        let (lock, generation) = publish_vendored_generation(&ctx);
        let mut lock = lock;
        lock.packages.push(LockEntry {
            name: "absent".to_string(),
            source: "git+https://example.invalid/absent".to_string(),
            content_hash: Some("sha256-v2:".to_string() + &"ab".repeat(32)),
            ..LockEntry::default()
        });

        let error = publish_package_generation(&ctx, &lock, true, |packages| {
            let dir = packages.join("vendored");
            fs::create_dir(&dir).unwrap();
            fs::write(dir.join("main.harn"), "pipeline main() {}\n").unwrap();
            Ok(2)
        })
        .unwrap_err();

        assert!(
            error.to_string().contains("absent"),
            "publish must name the package it could not find, got: {error}"
        );
        assert_eq!(
            PackageSnapshot::acquire(temp.path())
                .unwrap()
                .unwrap()
                .generation(),
            generation,
            "a rejected publish must leave the previous generation serving"
        );
        assert!(
            fs::read_dir(package_generations_dir(&ctx.dir))
                .unwrap()
                .all(|entry| !entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with(STAGING_PREFIX)),
            "a rejected publish must not leave its staging directory behind"
        );
    }
}
