//! Deciding whether an existing lock entry still answers a dependency, and
//! reading the facts (git request, content hash, manifest, provenance) a fresh
//! entry has to record.

use crate::package::*;

pub(crate) fn compatible_locked_entry(
    workspace: &PackageWorkspace,
    alias: &str,
    dependency: &Dependency,
    lock: &LockEntry,
    manifest_dir: &Path,
) -> Result<bool, PackageError> {
    if lock.name != alias {
        return Ok(false);
    }
    if let Some(path) = dependency.local_path() {
        let source = path_source_uri(&resolve_path_dependency_source(manifest_dir, path)?)?;
        return Ok(lock.source == source);
    }
    if let Some(requirement) = dependency.version() {
        let Dependency::Table(table) = dependency else {
            return Ok(false);
        };
        let Some(registry) = lock.registry.as_ref() else {
            return Ok(false);
        };
        let registry_name = table.registry_name.as_deref().unwrap_or(alias);
        if registry.name != registry_name {
            return Ok(false);
        }
        let expected_source = workspace.resolve_registry_source(table.registry.as_deref())?;
        if registry.source != expected_source {
            return Ok(false);
        }
        let version = parse_registry_semver(&registry.version)?;
        let req = parse_registry_version_req(requirement)?;
        let resolved_source_is_locked = if lock.source.starts_with("git+") {
            lock.commit.is_some() && lock.content_hash.is_some()
        } else if lock.source.starts_with("archive+") {
            lock.content_hash.is_some()
        } else {
            false
        };
        return Ok(req.matches(&version) && resolved_source_is_locked);
    }
    if let Some(url) = dependency.git_url() {
        let source = format!("git+{}", normalize_git_url(url)?);
        let requested = dependency_git_request(dependency).map(str::to_string);
        return Ok(lock.source == source
            && lock.rev_request == requested
            && lock.tag == dependency.tag().map(str::to_string)
            && lock.commit.is_some()
            && lock.content_hash.is_some());
    }
    if let Some(url) = dependency.archive_url() {
        let source = archive_source_uri(url)?;
        return Ok(lock.source == source && lock.content_hash.is_some());
    }
    Ok(false)
}

#[derive(Debug, Clone)]
pub(crate) struct PendingDependency {
    pub(super) alias: String,
    pub(super) dependency: Dependency,
    pub(super) manifest_dir: PathBuf,
    pub(super) parent: Option<String>,
    pub(super) parent_is_remote: bool,
}

pub(crate) fn git_rev_request(
    alias: &str,
    dependency: &Dependency,
) -> Result<String, PackageError> {
    dependency_git_request(dependency)
        .map(str::to_string)
        .ok_or_else(|| {
            PackageError::Lockfile(format!(
                "git dependency {alias} must specify `tag`, `rev`, or `branch`; use `harn add <url>@<tag-or-sha>` or add `tag = \"...\"` to {MANIFEST}"
            ))
        })
}

pub(crate) fn dependency_git_request(dependency: &Dependency) -> Option<&str> {
    dependency
        .branch()
        .or_else(|| dependency.rev())
        .or_else(|| dependency.tag())
}

pub(crate) fn dependency_content_hash(
    alias: &str,
    dependency: &Dependency,
) -> Result<String, PackageError> {
    let Dependency::Table(table) = dependency else {
        return Err(format!("dependency {alias} is missing checksum").into());
    };
    table
        .checksum
        .clone()
        .ok_or_else(|| format!("archive dependency {alias} must specify checksum").into())
}

pub(crate) fn dependency_manifest_dir(source: &Path) -> Option<PathBuf> {
    if source.is_dir() {
        return Some(source.to_path_buf());
    }
    source.parent().map(Path::to_path_buf)
}

pub(crate) fn read_package_manifest_from_dir(dir: &Path) -> Result<Option<Manifest>, PackageError> {
    let manifest_path = dir.join(MANIFEST);
    if !manifest_path.exists() {
        return Ok(None);
    }
    read_manifest_from_path(&manifest_path).map(Some)
}

/// Provenance pulled from a resolved package's manifest. Used to enrich a
/// `LockEntry` so audit/outdated reports stay self-contained.
#[derive(Debug, Clone, Default)]
pub(crate) struct LockEntryProvenance {
    pub(crate) package_version: Option<String>,
    pub(crate) harn_compat: Option<String>,
    pub(crate) provenance: Option<String>,
    pub(crate) manifest_digest: Option<String>,
    pub(crate) exports: PackageLockExports,
    pub(crate) permissions: Vec<String>,
    pub(crate) host_requirements: Vec<String>,
}

pub(crate) fn read_lock_entry_provenance(
    package_dir: &Path,
) -> Result<LockEntryProvenance, PackageError> {
    let manifest_path = package_dir.join(MANIFEST);
    if !manifest_path.exists() {
        return Ok(LockEntryProvenance::default());
    }
    let bytes = fs::read(&manifest_path)
        .map_err(|error| format!("failed to read {}: {error}", manifest_path.display()))?;
    let digest = format!("sha256:{}", sha256_hex(&bytes));
    let manifest = read_manifest_from_path(&manifest_path)?;
    let (package_version, harn_compat, provenance, permissions, host_requirements) = manifest
        .package
        .as_ref()
        .map(|info| {
            (
                info.version.clone(),
                info.harn.clone(),
                info.provenance.clone(),
                info.permissions.clone(),
                info.host_requirements.clone(),
            )
        })
        .unwrap_or((None, None, None, Vec::new(), Vec::new()));
    Ok(LockEntryProvenance {
        package_version,
        harn_compat,
        provenance,
        manifest_digest: Some(digest),
        exports: package_lock_exports_from_manifest(&manifest),
        permissions: normalized_requirements(&permissions),
        host_requirements: normalized_requirements(&host_requirements),
    })
}

pub(super) fn fill_provenance(entry: &mut LockEntry, provenance: LockEntryProvenance) {
    entry.package_version = provenance.package_version;
    entry.harn_compat = provenance.harn_compat;
    entry.provenance = provenance.provenance;
    entry.manifest_digest = provenance.manifest_digest;
    entry.exports = provenance.exports;
    entry.permissions = provenance.permissions;
    entry.host_requirements = provenance.host_requirements;
}
