use harn_modules::package_snapshot::{
    generation_root, open_lock_file, package_current_path, package_generations_dir,
    package_lock_digest, package_publication_lock_path, PackageGenerationManifest,
    PackageGenerationPointer, PackageSnapshot, GENERATION_LEASE_FILE, GENERATION_LOCK_FILE,
    GENERATION_MANIFEST_FILE, GENERATION_PACKAGES_DIR,
};
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use super::{
    materialized_hash_matches, validate_package_alias, LockFile, ManifestContext, PackageError,
};

const LEGACY_PACKAGES_DIR: &str = ".harn/packages";
const STAGING_PREFIX: &str = ".staging-";
const GENERATION_PREFIX: &str = "generation-";

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
    if snapshot.lock_digest() != expected_lock_digest {
        return Ok(false);
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

fn acquire_package_install_lock(ctx: &ManifestContext) -> Result<File, PackageError> {
    let path = ctx.dir.join(".harn").join("package-install.lock");
    let file = open_lock_file(&path).map_err(|error| PackageError::Lockfile(error.to_string()))?;
    file.lock()
        .map_err(|error| format!("failed to lock {}: {error}", path.display()))?;
    Ok(file)
}

fn publish_pointer_and_collect(
    ctx: &ManifestContext,
    generation: &str,
) -> Result<(), PackageError> {
    let publication_path = package_publication_lock_path(&ctx.dir);
    let publication = open_lock_file(&publication_path)
        .map_err(|error| PackageError::Lockfile(error.to_string()))?;
    publication
        .lock()
        .map_err(|error| format!("failed to lock {}: {error}", publication_path.display()))?;

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
    use crate::package::{ensure_dependencies_materialized, MANIFEST};
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
            "version = 4\n\n[[package]]\nname = \"stale\"\nsource = \"path+/tmp/stale\"\n",
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
}
