use super::*;

pub(crate) fn with_manifest_write_lock<T>(
    manifest_path: &Path,
    operation: impl FnOnce() -> Result<T, PackageError>,
) -> Result<T, PackageError> {
    let project_root = manifest_path.parent().ok_or_else(|| {
        PackageError::Manifest(format!(
            "manifest {} has no project directory",
            manifest_path.display()
        ))
    })?;
    let lock_path = project_root.join(".harn/package-manifest.lock");
    let lock = harn_modules::package_snapshot::open_lock_file(&lock_path)
        .map_err(|error| PackageError::Lockfile(error.to_string()))?;
    lock.lock().map_err(|error| {
        PackageError::Lockfile(format!("failed to lock {}: {error}", lock_path.display()))
    })?;
    let result = operation();
    let unlock = lock.unlock().map_err(|error| {
        PackageError::Lockfile(format!("failed to unlock {}: {error}", lock_path.display()))
    });
    match (result, unlock) {
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error),
        (Ok(value), Ok(())) => Ok(value),
    }
}

pub(crate) fn write_manifest_content_locked(
    path: &Path,
    content: &str,
) -> Result<(), PackageError> {
    harn_vm::atomic_io::atomic_write(path, content.as_bytes()).map_err(|error| {
        PackageError::Manifest(format!("failed to write {}: {error}", path.display()))
    })
}

pub(crate) fn upsert_dependency_in_manifest(
    manifest_path: &Path,
    alias: &str,
    dependency: &Dependency,
) -> Result<(), PackageError> {
    with_manifest_write_lock(manifest_path, || {
        upsert_dependency_in_manifest_locked(manifest_path, alias, dependency)
    })
}

pub(crate) fn remove_dependency_from_manifest(
    manifest_path: &Path,
    alias: &str,
) -> Result<bool, PackageError> {
    with_manifest_write_lock(manifest_path, || {
        remove_dependency_from_manifest_locked(manifest_path, alias)
    })
}

pub(crate) fn install_packages_in(
    workspace: &PackageWorkspace,
    frozen: bool,
    refetch: Option<&str>,
    offline: bool,
) -> Result<usize, PackageError> {
    let _mutation_lock = acquire_package_mutation_lock(workspace)?;
    install_packages_in_locked(workspace, frozen, refetch, offline)
}

pub(crate) fn acquire_package_mutation_lock(
    workspace: &PackageWorkspace,
) -> Result<ProjectMutationLock, PackageError> {
    acquire_project_mutation_lock(workspace.manifest_dir())
        .map_err(|error| PackageError::Ops(error.to_string()))
}
