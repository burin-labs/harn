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
                return Err(
                    format!("unsupported locked package source for {alias}: {source}").into(),
                );
            };
            let dest_dir = packages_dir.join(alias);
            copy_dir_recursive(&cache_dir, &dest_dir)?;
            write_cached_content_hash(&dest_dir, expected_hash)?;
            installed += 1;
        }
        Ok(installed)
    })
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
    let declared_dependencies = load_nearest_manifest(anchor)
        .into_result()?
        .map(|(manifest, _)| manifest.dependencies)
        .unwrap_or_default();
    let needs_dependencies = graph
        .package_import_aliases()
        .iter()
        .any(|alias| declared_dependencies.contains_key(alias));
    if needs_dependencies {
        ensure_dependencies_materialized(anchor)?;
    }
    Ok(needs_dependencies)
}

/// Revalidate the package authority carried by a bytecode-cache manifest.
///
/// `false` means the cached graph imports an alias the current manifest no
/// longer declares, so the caller must discard the hit and run normal
/// parse/typecheck diagnostics. Declared aliases retain the ordinary lock and
/// generation checks; a stale revision remains a setup failure rather than
/// executing the generation captured by an older cache entry.
pub(crate) fn ensure_cached_dependencies_materialized(
    anchor: &Path,
    package_aliases: &[String],
) -> Result<bool, PackageError> {
    if package_aliases.is_empty() {
        return Ok(true);
    }
    let declared_dependencies = load_nearest_manifest(anchor)
        .into_result()?
        .map(|(manifest, _)| manifest.dependencies)
        .unwrap_or_default();
    if package_aliases
        .iter()
        .any(|alias| !declared_dependencies.contains_key(alias))
    {
        return Ok(false);
    }
    ensure_dependencies_materialized(anchor)?;
    Ok(true)
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
