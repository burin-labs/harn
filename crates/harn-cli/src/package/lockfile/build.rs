//! Walking a manifest's dependency graph — including transitive dependencies
//! discovered as packages are fetched — into a complete, conflict-free lock
//! file.

use super::resolution::fill_provenance;
use crate::package::*;

pub(crate) fn dependency_conflict_message(
    existing: &LockEntry,
    candidate: &LockEntry,
) -> PackageError {
    PackageError::Lockfile(format!(
        "dependency alias '{}' resolves to multiple packages ({} and {}); use distinct aliases in {MANIFEST}",
        candidate.name, existing.source, candidate.source
    ))
}

pub(crate) fn replace_lock_entry(
    lock: &mut LockFile,
    candidate: LockEntry,
) -> Result<bool, PackageError> {
    validate_package_alias(&candidate.name)?;
    if let Some(existing) = lock.find(&candidate.name) {
        if existing == &candidate {
            return Ok(false);
        }
        return Err(dependency_conflict_message(existing, &candidate));
    }
    lock.replace(candidate);
    Ok(true)
}

pub(crate) fn enqueue_manifest_dependencies(
    pending: &mut Vec<PendingDependency>,
    manifest: Manifest,
    manifest_dir: PathBuf,
    parent: String,
    parent_is_remote: bool,
) {
    let mut aliases: Vec<String> = manifest.dependencies.keys().cloned().collect();
    aliases.sort();
    for alias in aliases.into_iter().rev() {
        if let Some(dependency) = manifest.dependencies.get(&alias).cloned() {
            pending.push(PendingDependency {
                alias,
                dependency,
                manifest_dir: manifest_dir.clone(),
                parent: Some(parent.clone()),
                parent_is_remote,
            });
        }
    }
}

fn resolve_registry_version_dependency(
    workspace: &PackageWorkspace,
    alias: &str,
    dependency: Dependency,
) -> Result<Dependency, PackageError> {
    let Dependency::Table(table) = &dependency else {
        return Ok(dependency);
    };
    if table.version.is_none() {
        return Ok(dependency);
    }
    registry_dependency_from_manifest_constraint_in(workspace, alias, table)
}

fn validate_dependency_source_shape(
    alias: &str,
    dependency: &Dependency,
) -> Result<(), PackageError> {
    let Dependency::Table(table) = dependency else {
        return Ok(());
    };
    let source_count = usize::from(table.git.is_some())
        + usize::from(table.archive.is_some())
        + usize::from(table.path.is_some());
    if table.version.is_some()
        && (source_count > 0
            || table.rev.is_some()
            || table.tag.is_some()
            || table.branch.is_some())
    {
        return Err(format!(
            "dependency {alias} uses `version`; do not combine registry version constraints with git, archive, path, tag, rev, or branch"
        )
        .into());
    }
    if source_count > 1 {
        return Err(
            format!("dependency {alias} must specify only one of git, archive, or path").into(),
        );
    }
    if table.archive.is_some()
        && (table.tag.is_some() || table.rev.is_some() || table.branch.is_some())
    {
        return Err(
            format!("archive dependency {alias} cannot specify tag, rev, or branch").into(),
        );
    }
    Ok(())
}

pub(crate) fn build_lockfile(
    workspace: &PackageWorkspace,
    ctx: &ManifestContext,
    existing: Option<&LockFile>,
    refresh_alias: Option<&str>,
    refresh_all: bool,
    allow_resolve: bool,
    offline: bool,
) -> Result<LockFile, PackageError> {
    if manifest_has_git_dependencies(&ctx.manifest) {
        ensure_git_available()?;
    }

    let mut lock = LockFile::default();
    let mut pending: Vec<PendingDependency> = Vec::new();
    let mut aliases: Vec<String> = ctx.manifest.dependencies.keys().cloned().collect();
    aliases.sort();
    for alias in aliases.into_iter().rev() {
        let dependency = ctx
            .manifest
            .dependencies
            .get(&alias)
            .ok_or_else(|| format!("dependency {alias} disappeared while locking"))?
            .clone();
        pending.push(PendingDependency {
            alias,
            dependency,
            manifest_dir: ctx.dir.clone(),
            parent: None,
            parent_is_remote: false,
        });
    }

    while let Some(next) = pending.pop() {
        let alias = next.alias;
        validate_package_alias(&alias)?;
        let dependency = next.dependency;
        if dependency.local_path().is_some() && next.parent_is_remote {
            let parent = next.parent.as_deref().unwrap_or("a remote package");
            return Err(format!(
                "package {parent} declares local path dependency {alias}, but path dependencies are not supported inside remote-installed packages; publish {alias} as a git or registry dependency"
            ).into());
        }
        if dependency.requires_git() {
            ensure_git_available()?;
            if dependency.git_url().is_some() {
                git_rev_request(&alias, &dependency)?;
            }
        }
        validate_dependency_source_shape(&alias, &dependency)?;
        let refresh = refresh_all || refresh_alias == Some(alias.as_str());
        if let Some(existing_lock) = existing.and_then(|lock| lock.find(&alias)) {
            if !refresh
                && compatible_locked_entry(
                    workspace,
                    &alias,
                    &dependency,
                    existing_lock,
                    &next.manifest_dir,
                )?
            {
                let mut entry = existing_lock.clone();
                if entry.source.starts_with("git+") && entry.content_hash.is_none() {
                    let url = entry.source.trim_start_matches("git+");
                    let commit = entry
                        .commit
                        .as_deref()
                        .ok_or_else(|| format!("missing locked commit for {alias}"))?;
                    entry.content_hash = Some(ensure_git_cache_populated_in(
                        workspace,
                        url,
                        &entry.source,
                        commit,
                        None,
                        false,
                        offline,
                    )?);
                }
                if entry.source.starts_with("git+") {
                    let url = entry.source.trim_start_matches("git+");
                    let commit = entry
                        .commit
                        .as_deref()
                        .ok_or_else(|| format!("missing locked commit for {alias}"))?;
                    let expected_hash = entry
                        .content_hash
                        .as_deref()
                        .ok_or_else(|| format!("missing content hash for {alias}"))?;
                    ensure_git_cache_populated_in(
                        workspace,
                        url,
                        &entry.source,
                        commit,
                        Some(expected_hash),
                        false,
                        offline,
                    )?;
                    let cache_dir = git_cache_dir_in(workspace, &entry.source, commit)?;
                    if entry.manifest_digest.is_none()
                        || entry.package_version.is_none()
                        || entry.provenance.is_none()
                    {
                        fill_provenance(&mut entry, read_lock_entry_provenance(&cache_dir)?);
                    }
                    if entry.registry.is_none() {
                        entry.registry = dependency.registry_provenance();
                    }
                    let inserted = replace_lock_entry(&mut lock, entry.clone())?;
                    if inserted {
                        if let Some(manifest) = read_package_manifest_from_dir(&cache_dir)? {
                            enqueue_manifest_dependencies(
                                &mut pending,
                                manifest,
                                cache_dir,
                                alias,
                                true,
                            );
                        }
                    }
                } else if entry.source.starts_with("archive+") {
                    let url = archive_url_from_source_uri(&entry.source)?;
                    let expected_hash = entry
                        .content_hash
                        .as_deref()
                        .ok_or_else(|| format!("missing content hash for {alias}"))?;
                    ensure_archive_cache_populated_in(
                        workspace,
                        url,
                        &entry.source,
                        expected_hash,
                        false,
                        offline,
                    )?;
                    let cache_dir = archive_cache_dir_in(workspace, &entry.source, expected_hash)?;
                    if entry.manifest_digest.is_none()
                        || entry.package_version.is_none()
                        || entry.provenance.is_none()
                    {
                        fill_provenance(&mut entry, read_lock_entry_provenance(&cache_dir)?);
                    }
                    if entry.registry.is_none() {
                        entry.registry = dependency.registry_provenance();
                    }
                    let inserted = replace_lock_entry(&mut lock, entry.clone())?;
                    if inserted {
                        if let Some(manifest) = read_package_manifest_from_dir(&cache_dir)? {
                            enqueue_manifest_dependencies(
                                &mut pending,
                                manifest,
                                cache_dir,
                                alias,
                                true,
                            );
                        }
                    }
                } else if entry.source.starts_with("path+") {
                    let source = path_from_source_uri(&entry.source)?;
                    let manifest_dir = dependency_manifest_dir(&source);
                    if entry.manifest_digest.is_none()
                        || entry.package_version.is_none()
                        || entry.provenance.is_none()
                    {
                        if let Some(dir) = manifest_dir.as_deref() {
                            fill_provenance(&mut entry, read_lock_entry_provenance(dir)?);
                        }
                    }
                    let inserted = replace_lock_entry(&mut lock, entry.clone())?;
                    if inserted {
                        if let Some(manifest_dir) = manifest_dir {
                            if let Some(manifest) = read_package_manifest_from_dir(&manifest_dir)? {
                                enqueue_manifest_dependencies(
                                    &mut pending,
                                    manifest,
                                    manifest_dir,
                                    alias,
                                    false,
                                );
                            }
                        }
                    }
                } else {
                    replace_lock_entry(&mut lock, entry)?;
                }
                continue;
            }
        }

        if !allow_resolve {
            return Err(format!("{} would need to change", ctx.lock_path().display()).into());
        }

        let dependency = resolve_registry_version_dependency(workspace, &alias, dependency)?;
        validate_dependency_source_shape(&alias, &dependency)?;
        if dependency.requires_git() {
            ensure_git_available()?;
            if dependency.git_url().is_some() {
                git_rev_request(&alias, &dependency)?;
            }
        }

        if let Some(path) = dependency.local_path() {
            let source = resolve_path_dependency_source(&next.manifest_dir, path)?;
            let package_alias = alias.clone();
            let manifest_dir = dependency_manifest_dir(&source);
            let provenance = manifest_dir
                .as_deref()
                .map(read_lock_entry_provenance)
                .transpose()?
                .unwrap_or_default();
            let mut entry = LockEntry {
                name: alias.clone(),
                source: path_source_uri(&source)?,
                tag: None,
                rev_request: None,
                commit: None,
                content_hash: None,
                package_version: None,
                harn_compat: None,
                provenance: None,
                manifest_digest: None,
                registry: None,
                exports: PackageLockExports::default(),
                permissions: Vec::new(),
                host_requirements: Vec::new(),
            };
            fill_provenance(&mut entry, provenance);
            let inserted = replace_lock_entry(&mut lock, entry)?;
            if inserted {
                if let Some(manifest_dir) = manifest_dir {
                    if let Some(manifest) = read_package_manifest_from_dir(&manifest_dir)? {
                        enqueue_manifest_dependencies(
                            &mut pending,
                            manifest,
                            manifest_dir,
                            package_alias,
                            false,
                        );
                    }
                }
            }
            continue;
        }

        if let Some(url) = dependency.archive_url() {
            let normalized_url = normalize_archive_url(url)?;
            let source = format!("archive+{normalized_url}");
            let expected_hash = dependency_content_hash(&alias, &dependency)?;
            let content_hash = ensure_archive_cache_populated_in(
                workspace,
                &normalized_url,
                &source,
                &expected_hash,
                false,
                offline,
            )?;
            let cache_dir = archive_cache_dir_in(workspace, &source, &content_hash)?;
            let provenance = read_lock_entry_provenance(&cache_dir)?;
            let mut entry = LockEntry {
                name: alias.clone(),
                source: source.clone(),
                tag: None,
                rev_request: None,
                commit: None,
                content_hash: Some(content_hash.clone()),
                package_version: None,
                harn_compat: None,
                provenance: None,
                manifest_digest: None,
                registry: dependency.registry_provenance(),
                exports: PackageLockExports::default(),
                permissions: Vec::new(),
                host_requirements: Vec::new(),
            };
            fill_provenance(&mut entry, provenance);
            let inserted = replace_lock_entry(&mut lock, entry)?;
            if inserted {
                if let Some(manifest) = read_package_manifest_from_dir(&cache_dir)? {
                    enqueue_manifest_dependencies(&mut pending, manifest, cache_dir, alias, true);
                }
            }
            continue;
        }

        if let Some(url) = dependency.git_url() {
            let rev_request = git_rev_request(&alias, &dependency)?;
            let normalized_url = normalize_git_url(url)?;
            let source = format!("git+{normalized_url}");
            let commit = resolve_git_commit(
                &normalized_url,
                dependency.rev(),
                dependency.tag(),
                dependency.branch(),
            )?;
            let content_hash = ensure_git_cache_populated_in(
                workspace,
                &normalized_url,
                &source,
                &commit,
                None,
                false,
                offline,
            )?;
            let cache_dir = git_cache_dir_in(workspace, &source, &commit)?;
            let provenance = read_lock_entry_provenance(&cache_dir)?;
            let mut entry = LockEntry {
                name: alias.clone(),
                source: source.clone(),
                tag: dependency.tag().map(str::to_string),
                rev_request: Some(rev_request),
                commit: Some(commit.clone()),
                content_hash: Some(content_hash),
                package_version: None,
                harn_compat: None,
                provenance: None,
                manifest_digest: None,
                registry: dependency.registry_provenance(),
                exports: PackageLockExports::default(),
                permissions: Vec::new(),
                host_requirements: Vec::new(),
            };
            fill_provenance(&mut entry, provenance);
            let inserted = replace_lock_entry(&mut lock, entry)?;
            if inserted {
                if let Some(manifest) = read_package_manifest_from_dir(&cache_dir)? {
                    enqueue_manifest_dependencies(&mut pending, manifest, cache_dir, alias, true);
                }
            }
            continue;
        }

        return Err(format!("dependency {alias} is missing a git, archive, or path source").into());
    }
    Ok(lock)
}
