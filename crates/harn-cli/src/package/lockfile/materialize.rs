//! Realizing a lock file on disk: populating `packages/` from the cache,
//! checking a lock still matches its manifest, and the entry point every
//! command uses to guarantee dependencies are present.

use crate::package::*;

pub(crate) fn materialize_dependencies_from_lock(
    workspace: &PackageWorkspace,
    ctx: &ManifestContext,
    lock: &LockFile,
    refetch: Option<&str>,
    offline: bool,
) -> Result<usize, PackageError> {
    publish_package_generation(ctx, lock, refetch.is_some(), |packages_dir| {
        materialize_lock_entries(workspace, lock, packages_dir, refetch, offline, None)
    })
}

fn materialize_lock_entries(
    workspace: &PackageWorkspace,
    lock: &LockFile,
    packages_dir: &Path,
    refetch: Option<&str>,
    offline: bool,
    installed_sources: Option<&InstalledPackageSources>,
) -> Result<usize, PackageError> {
    let mut installed = 0usize;
    for entry in &lock.packages {
        let alias = &entry.name;
        validate_package_alias(alias)?;
        if entry.source.starts_with("path+") {
            let source = path_from_source_uri(&entry.source)?;
            materialize_path_dependency(&source, packages_dir, alias)?;
            installed += 1;
            continue;
        }

        let expected_hash = entry
            .content_hash
            .as_deref()
            .ok_or_else(|| format!("missing content hash for {alias}"))?;
        let dest_dir = packages_dir.join(alias);
        if let Some(source) = installed_sources.and_then(|sources| sources.source_for(entry)) {
            copy_dir_recursive(&source, &dest_dir)?;
            write_cached_content_hash(&dest_dir, expected_hash)?;
            installed += 1;
            continue;
        }
        let source = entry.source.clone();
        let refetch_this = refetch == Some("all") || refetch == Some(alias.as_str());
        let cache_dir = if source.starts_with("git+") {
            let commit = entry
                .commit
                .as_deref()
                .ok_or_else(|| format!("missing locked commit for {alias}"))?;
            let url = source.trim_start_matches("git+");
            ensure_git_cache_populated_in(
                workspace,
                url,
                &source,
                commit,
                Some(expected_hash),
                refetch_this,
                offline,
            )?;
            git_cache_dir_in(workspace, &source, commit)?
        } else if source.starts_with("archive+") {
            let url = archive_url_from_source_uri(&source)?;
            ensure_archive_cache_populated_in(
                workspace,
                url,
                &source,
                expected_hash,
                refetch_this,
                offline,
            )?;
            archive_cache_dir_in(workspace, &source, expected_hash)?
        } else {
            return Err(format!("unsupported locked package source for {alias}: {source}").into());
        };
        copy_dir_recursive(&cache_dir, &dest_dir)?;
        write_cached_content_hash(&dest_dir, expected_hash)?;
        installed += 1;
    }
    Ok(installed)
}

pub(crate) fn validate_lock_matches_manifest(
    workspace: &PackageWorkspace,
    ctx: &ManifestContext,
    lock: &LockFile,
) -> Result<(), PackageError> {
    for (alias, dependency) in &ctx.manifest.dependencies {
        validate_package_alias(alias)?;
        let entry = lock.find(alias).ok_or_else(|| {
            format!(
                "{} is missing an entry for {alias}",
                ctx.lock_path().display()
            )
        })?;
        if !compatible_locked_entry(workspace, alias, dependency, entry, &ctx.dir)? {
            return Err(format!(
                "{} is out of date for {alias}; run `harn install`",
                ctx.lock_path().display()
            )
            .into());
        }
    }
    Ok(())
}

pub fn ensure_dependencies_materialized(anchor: &Path) -> Result<(), PackageError> {
    let workspace = PackageWorkspace::from_current_dir()?;
    ensure_dependencies_materialized_in(&workspace, anchor)
}

/// Materialize the nearest manifest's dependencies only when the already-built
/// reachable module graph names one of its declared package aliases.
///
/// This is the shared policy seam for entrypoints and deferred handlers. The
/// caller owns graph construction because it may need to reuse or rebuild that
/// graph after this function publishes a package generation.
pub(crate) fn ensure_reachable_dependencies_materialized(
    anchor: &Path,
    graph: &harn_modules::ModuleGraph,
) -> Result<bool, PackageError> {
    let mut requested = Vec::new();
    for import in graph.package_imports() {
        let Some((manifest, manifest_dir)) =
            load_nearest_manifest(&import.importer).into_result()?
        else {
            return Err(undeclared_package_import(&import.alias, &import.importer));
        };
        let Some(dependency) = manifest.dependencies.get(&import.alias).cloned() else {
            return Err(undeclared_package_import(&import.alias, &import.importer));
        };
        requested.push(ReachableDependency {
            alias: import.alias,
            dependency,
            manifest_dir,
        });
    }
    if requested.is_empty() {
        return Ok(false);
    }
    ensure_dependency_requests_materialized(anchor, requested)?;
    Ok(true)
}

/// Retain only root-manifest aliases as bytecode-cache authority. Transitive
/// imports are reconstructed from those declarations and their package
/// manifests; persisting the flattened graph would make a removed root alias
/// indistinguishable from a still-valid transitive dependency on the next hit.
pub(crate) fn declared_package_import_aliases(
    anchor: &Path,
    package_aliases: &[String],
) -> Result<Vec<String>, PackageError> {
    let declared = governing_manifest_context(anchor)?
        .map(|ctx| ctx.manifest.dependencies)
        .unwrap_or_default();
    Ok(package_aliases
        .iter()
        .filter(|alias| declared.contains_key(alias.as_str()))
        .cloned()
        .collect())
}

/// Revalidate the package authority carried by a bytecode-cache manifest.
///
/// `false` means the cached graph imports an alias the current manifest no
/// longer declares or was compiled against another valid lock generation, so
/// the caller must discard the hit and run normal parse/typecheck diagnostics.
/// Declared aliases retain the ordinary lock checks; a stale revision remains
/// a setup failure rather than executing an older generation.
pub(crate) fn ensure_cached_dependencies_materialized(
    anchor: &Path,
    package_aliases: &[String],
    recorded_lock_digest: Option<&str>,
) -> Result<bool, PackageError> {
    if package_aliases.is_empty() {
        return Ok(true);
    }
    let Some(ctx) = governing_manifest_context(anchor)? else {
        return Ok(false);
    };
    let mut requested = Vec::with_capacity(package_aliases.len());
    for alias in package_aliases {
        let Some(dependency) = ctx.manifest.dependencies.get(alias).cloned() else {
            return Ok(false);
        };
        requested.push(ReachableDependency {
            alias: alias.clone(),
            dependency,
            manifest_dir: ctx.dir.clone(),
        });
    }
    let digest = ensure_dependency_requests_materialized(anchor, requested)?;
    Ok(Some(digest.as_str()) == recorded_lock_digest)
}

/// Digest of the installed lock generation that owns reachable package
/// aliases. This is persisted beside an entry cache manifest after setup.
pub(crate) fn reachable_dependency_lock_digest(
    anchor: &Path,
    package_aliases: &[String],
) -> Result<Option<String>, PackageError> {
    if package_aliases.is_empty() {
        return Ok(None);
    }
    let Some(ctx) = governing_manifest_context(anchor)? else {
        return Ok(None);
    };
    let mut requested = Vec::with_capacity(package_aliases.len());
    for alias in package_aliases {
        let Some(dependency) = ctx.manifest.dependencies.get(alias).cloned() else {
            return Ok(None);
        };
        requested.push(ReachableDependency {
            alias: alias.clone(),
            dependency,
            manifest_dir: ctx.dir.clone(),
        });
    }
    ensure_dependency_requests_materialized(anchor, requested).map(Some)
}

#[derive(Clone)]
struct ReachableDependency {
    alias: String,
    dependency: Dependency,
    manifest_dir: PathBuf,
}

fn undeclared_package_import(alias: &str, importer: &Path) -> PackageError {
    PackageError::Validation(format!(
        "package import '{alias}' from {} is not declared in the importing manifest's [dependencies]",
        importer.display()
    ))
}

fn governing_manifest_context(anchor: &Path) -> Result<Option<ManifestContext>, PackageError> {
    let project_root = harn_modules::package_snapshot::PackageSnapshot::acquire_nearest(anchor)
        .map_err(|error| PackageError::Lockfile(error.to_string()))?
        .map(|snapshot| snapshot.project_root().to_path_buf());
    let lookup = project_root.as_deref().unwrap_or(anchor);
    Ok(load_nearest_manifest(lookup)
        .into_result()?
        .map(|(manifest, dir)| ManifestContext { manifest, dir }))
}

fn ensure_dependency_requests_materialized(
    anchor: &Path,
    requested: Vec<ReachableDependency>,
) -> Result<String, PackageError> {
    ensure_dependency_requests_materialized_before_lock(anchor, requested, || {})
}

fn ensure_dependency_requests_materialized_before_lock(
    anchor: &Path,
    requested: Vec<ReachableDependency>,
    before_lock: impl FnOnce(),
) -> Result<String, PackageError> {
    let workspace = PackageWorkspace::from_current_dir()?;
    let Some(ctx) = governing_manifest_context(anchor)? else {
        return Err("package imports require a governing harn.toml".into());
    };
    // Lock authority and the generation pointer are one transaction. A full
    // install saves its replacement lock before publishing its generation, so
    // a demand initializer must not derive from the old lock and then publish
    // after that install completes.
    before_lock();
    let _install_lock = acquire_package_install_lock(&ctx)?;
    let lock = LockFile::load(&ctx.lock_path())?.ok_or_else(|| {
        format!(
            "{} is missing; run `harn install`",
            ctx.lock_path().display()
        )
    })?;
    let runtime_lock = lock_for_materialization(&workspace, &ctx, lock)?;
    let reachable_lock = dependency_closure_lock(&workspace, &ctx, &runtime_lock, requested)?;
    let digest = harn_modules::package_snapshot::package_lock_digest(&reachable_lock.encode()?);
    if current_generation_satisfies_lock_subset(&ctx, &reachable_lock)? {
        return Ok(digest);
    }
    let installed_sources = InstalledPackageSources::acquire(&ctx)?;
    let cumulative_lock =
        cumulative_demand_lock(&runtime_lock, reachable_lock, installed_sources.as_ref());
    publish_package_generation_locked(&ctx, &cumulative_lock, false, |packages_dir| {
        materialize_lock_entries(
            &workspace,
            &cumulative_lock,
            packages_dir,
            None,
            false,
            installed_sources.as_ref(),
        )
    })?;
    Ok(digest)
}

#[cfg(test)]
pub(crate) fn ensure_dependency_alias_materialized_for_test(
    anchor: &Path,
    alias: &str,
) -> Result<String, PackageError> {
    dependency_request_for_test(anchor, alias)
        .and_then(|request| ensure_dependency_requests_materialized(anchor, vec![request]))
}

#[cfg(test)]
pub(crate) fn ensure_dependency_alias_materialized_after_barrier_for_test(
    anchor: &Path,
    alias: &str,
    ready: &std::sync::Barrier,
) -> Result<String, PackageError> {
    let request = dependency_request_for_test(anchor, alias)?;
    ensure_dependency_requests_materialized_before_lock(anchor, vec![request], || {
        ready.wait();
    })
}

#[cfg(test)]
fn dependency_request_for_test(
    anchor: &Path,
    alias: &str,
) -> Result<ReachableDependency, PackageError> {
    let Some(ctx) = governing_manifest_context(anchor)? else {
        return Err("test dependency requires a governing harn.toml".into());
    };
    let dependency = ctx
        .manifest
        .dependencies
        .get(alias)
        .cloned()
        .ok_or_else(|| format!("test dependency {alias} is not declared"))?;
    Ok(ReachableDependency {
        alias: alias.to_string(),
        dependency,
        manifest_dir: ctx.dir,
    })
}

fn cumulative_demand_lock(
    authoritative: &LockFile,
    mut requested: LockFile,
    installed: Option<&InstalledPackageSources>,
) -> LockFile {
    if let Some(installed) = installed {
        for entry in &installed.lock.packages {
            let Some(authoritative_entry) = authoritative.find(&entry.name) else {
                continue;
            };
            if entry.same_resolution(authoritative_entry) && requested.find(&entry.name).is_none() {
                requested.packages.push(authoritative_entry.clone());
            }
        }
    }
    requested.sort_entries();
    requested
}

fn dependency_closure_lock(
    workspace: &PackageWorkspace,
    root: &ManifestContext,
    lock: &LockFile,
    mut pending: Vec<ReachableDependency>,
) -> Result<LockFile, PackageError> {
    let installed_sources = InstalledPackageSources::acquire(root)?;
    let mut reachable = LockFile {
        version: lock.version,
        generator_version: lock.generator_version.clone(),
        protocol_artifact_version: lock.protocol_artifact_version.clone(),
        packages: Vec::new(),
    };
    let mut visited = std::collections::BTreeSet::new();
    while let Some(next) = pending.pop() {
        validate_package_alias(&next.alias)?;
        let entry = lock.find(&next.alias).ok_or_else(|| {
            format!(
                "{} is missing an entry for {}",
                root.lock_path().display(),
                next.alias
            )
        })?;
        if !compatible_locked_entry(
            workspace,
            &next.alias,
            &next.dependency,
            entry,
            &next.manifest_dir,
        )? {
            return Err(format!(
                "{} is out of date for {}; run `harn install`",
                root.lock_path().display(),
                next.alias
            )
            .into());
        }
        if !visited.insert(next.alias.clone()) {
            continue;
        }
        reachable.packages.push(entry.clone());
        let source_dir = locked_package_source_dir(workspace, installed_sources.as_ref(), entry)?;
        let Some(manifest) = read_package_manifest_from_dir(&source_dir)? else {
            continue;
        };
        let mut dependencies = manifest.dependencies.into_iter().collect::<Vec<_>>();
        dependencies.sort_by(|left, right| left.0.cmp(&right.0));
        for (alias, dependency) in dependencies.into_iter().rev() {
            pending.push(ReachableDependency {
                alias,
                dependency,
                manifest_dir: source_dir.clone(),
            });
        }
    }
    reachable.sort_entries();
    Ok(reachable)
}

struct InstalledPackageSources {
    snapshot: harn_modules::package_snapshot::PackageSnapshot,
    lock: LockFile,
}

impl InstalledPackageSources {
    fn acquire(root: &ManifestContext) -> Result<Option<Self>, PackageError> {
        let Some(snapshot) = harn_modules::package_snapshot::PackageSnapshot::acquire(&root.dir)
            .map_err(|error| PackageError::Lockfile(error.to_string()))?
        else {
            return Ok(None);
        };
        let Some(lock) = LockFile::load(snapshot.lock_path())? else {
            return Ok(None);
        };
        Ok(Some(Self { snapshot, lock }))
    }

    fn source_for(&self, entry: &LockEntry) -> Option<PathBuf> {
        let installed = self.lock.find(&entry.name)?;
        if !installed.same_resolution(entry) {
            return None;
        }
        let directory = self.snapshot.packages_root().join(&entry.name);
        let expected_hash = entry.content_hash.as_deref()?;
        (directory.is_dir() && materialized_hash_matches(&directory, expected_hash))
            .then_some(directory)
    }
}

fn locked_package_source_dir(
    workspace: &PackageWorkspace,
    installed: Option<&InstalledPackageSources>,
    entry: &LockEntry,
) -> Result<PathBuf, PackageError> {
    if entry.source.starts_with("path+") {
        return path_from_source_uri(&entry.source);
    }
    let expected_hash = entry
        .content_hash
        .as_deref()
        .ok_or_else(|| format!("missing content hash for {}", entry.name))?;
    if let Some(source) = installed.and_then(|sources| sources.source_for(entry)) {
        return Ok(source);
    }
    if entry.source.starts_with("git+") {
        let commit = entry
            .commit
            .as_deref()
            .ok_or_else(|| format!("missing locked commit for {}", entry.name))?;
        let url = entry.source.trim_start_matches("git+");
        ensure_git_cache_populated_in(
            workspace,
            url,
            &entry.source,
            commit,
            Some(expected_hash),
            false,
            false,
        )?;
        return git_cache_dir_in(workspace, &entry.source, commit);
    }
    if entry.source.starts_with("archive+") {
        let url = archive_url_from_source_uri(&entry.source)?;
        ensure_archive_cache_populated_in(
            workspace,
            url,
            &entry.source,
            expected_hash,
            false,
            false,
        )?;
        return archive_cache_dir_in(workspace, &entry.source, expected_hash);
    }
    Err(format!(
        "unsupported locked package source for {}: {}",
        entry.name, entry.source
    )
    .into())
}

pub(crate) fn ensure_dependencies_materialized_in(
    workspace: &PackageWorkspace,
    anchor: &Path,
) -> Result<(), PackageError> {
    let Some((manifest, dir)) = load_nearest_manifest(anchor).into_result()? else {
        return Ok(());
    };
    let ctx = ManifestContext { manifest, dir };
    if ctx.manifest.dependencies.is_empty() {
        return dependency_package_snapshot(&ctx.manifest, &ctx.dir).map(|_| ());
    }
    let lock = LockFile::load(&ctx.lock_path())?.ok_or_else(|| {
        format!(
            "{} is missing; run `harn install`",
            ctx.lock_path().display()
        )
    })?;
    validate_lock_matches_manifest(workspace, &ctx, &lock)?;
    let runtime_lock = lock_for_materialization(workspace, &ctx, lock)?;
    materialize_dependencies_from_lock(workspace, &ctx, &runtime_lock, None, false)?;
    Ok(())
}

fn lock_for_materialization(
    workspace: &PackageWorkspace,
    ctx: &ManifestContext,
    lock: LockFile,
) -> Result<LockFile, PackageError> {
    if !lock.requires_git_hash_migration() {
        return Ok(lock);
    }

    // Old lock files remain valid inputs to read-only package commands. Build
    // a current in-memory projection for the immutable runtime generation;
    // only `harn install` rewrites the project's harn.lock.
    build_lockfile(workspace, ctx, Some(&lock), None, false, true, false)
}

pub(super) fn dependency_manifest_item(
    alias: &str,
    dependency: &Dependency,
) -> Result<toml_edit::Item, PackageError> {
    validate_package_alias(alias)?;
    let mut fields = toml_edit::InlineTable::new();
    let table = match dependency {
        Dependency::Path(path) => {
            fields.insert("path", path.clone().into());
            return Ok(toml_edit::Item::Value(fields.into()));
        }
        Dependency::Table(table) => table,
    };
    for (name, value) in [
        ("path", table.path.as_deref()),
        ("git", table.git.as_deref()),
        ("archive", table.archive.as_deref()),
    ] {
        if let Some(value) = value {
            fields.insert(name, value.into());
        }
    }
    if let Some(branch) = table.branch.as_deref() {
        fields.insert("branch", branch.into());
    } else if let Some(tag) = table.tag.as_deref() {
        fields.insert("tag", tag.into());
    } else if let Some(rev) = table.rev.as_deref() {
        fields.insert("rev", rev.into());
    }
    for (name, value) in [
        ("version", table.version.as_deref()),
        ("package", table.package.as_deref()),
        ("checksum", table.checksum.as_deref()),
        ("registry", table.registry.as_deref()),
        ("registry_name", table.registry_name.as_deref()),
        ("registry_version", table.registry_version.as_deref()),
        ("registry_commit", table.registry_commit.as_deref()),
        ("registry_provenance", table.registry_provenance.as_deref()),
    ] {
        if let Some(value) = value {
            fields.insert(name, value.into());
        }
    }
    Ok(toml_edit::Item::Value(fields.into()))
}
