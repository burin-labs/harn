use super::*;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct LocalDependencyInstallReceipt {
    pub alias: String,
    pub path: String,
    pub installed_packages: usize,
    pub manifest_changed: bool,
    pub generation_changed: bool,
    pub generation: String,
}

pub(crate) fn install_local_package(
    workspace: &PackageWorkspace,
    package_root: &Path,
) -> Result<LocalDependencyInstallReceipt, PackageError> {
    let package_root = package_root.canonicalize().map_err(|error| {
        PackageError::Manifest(format!(
            "failed to canonicalize local package {}: {error}",
            package_root.display()
        ))
    })?;
    if !package_root.join(MANIFEST).is_file() {
        return Err(PackageError::Manifest(format!(
            "local package {} has no {MANIFEST}",
            package_root.display()
        )));
    }

    let ctx = workspace.load_manifest_context()?;
    let dependency_path = local_dependency_path(&ctx.dir, &package_root)?;
    let preferred = derive_package_alias_from_path(&package_root)?;
    let alias = local_dependency_alias(&ctx, &package_root, &preferred, &dependency_path)?;
    let manifest_path = ctx.manifest_path();
    let manifest_before = fs::read(&manifest_path).map_err(|error| {
        PackageError::Manifest(format!(
            "failed to read {}: {error}",
            manifest_path.display()
        ))
    })?;
    let generation_before = current_generation(&ctx.dir)?;

    let (_, installed_packages) = add_package_to(
        workspace,
        &alias,
        Some(&alias),
        None,
        None,
        None,
        None,
        Some(&dependency_path),
        None,
    )?;
    let manifest_after = fs::read(&manifest_path).map_err(|error| {
        PackageError::Manifest(format!(
            "failed to read {}: {error}",
            manifest_path.display()
        ))
    })?;
    let generation = current_generation(&ctx.dir)?.ok_or_else(|| {
        PackageError::Lockfile("local package install published no package generation".to_string())
    })?;

    Ok(LocalDependencyInstallReceipt {
        alias,
        path: dependency_path,
        installed_packages,
        manifest_changed: manifest_before != manifest_after,
        generation_changed: generation_before.as_deref() != Some(generation.as_str()),
        generation,
    })
}

fn local_dependency_path(project_root: &Path, package_root: &Path) -> Result<String, PackageError> {
    let project_root = project_root.canonicalize().map_err(|error| {
        PackageError::Manifest(format!(
            "failed to canonicalize project root {}: {error}",
            project_root.display()
        ))
    })?;
    let selected = package_root
        .strip_prefix(&project_root)
        .unwrap_or(package_root);
    if selected.as_os_str().is_empty() {
        return Err(PackageError::Manifest(
            "a project cannot install itself as a local dependency".to_string(),
        ));
    }
    selected
        .to_str()
        .map(str::to_string)
        .ok_or_else(|| PackageError::Manifest("local package path is not UTF-8".to_string()))
}

fn local_dependency_alias(
    ctx: &ManifestContext,
    package_root: &Path,
    preferred: &str,
    dependency_path: &str,
) -> Result<String, PackageError> {
    for (alias, dependency) in &ctx.manifest.dependencies {
        if dependency_targets(&ctx.dir, dependency, package_root) {
            return Ok(alias.clone());
        }
    }
    if !ctx.manifest.dependencies.contains_key(preferred) {
        validate_package_alias(preferred)?;
        return Ok(preferred.to_string());
    }

    let digest = sha256_hex(dependency_path.as_bytes());
    for length in (8..=digest.len()).step_by(4) {
        let candidate = format!("{preferred}-{}", &digest[..length]);
        validate_package_alias(&candidate)?;
        if !ctx.manifest.dependencies.contains_key(&candidate) {
            return Ok(candidate);
        }
    }
    Err(PackageError::Manifest(format!(
        "could not derive an unused dependency alias for {}",
        package_root.display()
    )))
}

fn dependency_targets(project_root: &Path, dependency: &Dependency, package_root: &Path) -> bool {
    let Some(path) = dependency.local_path() else {
        return false;
    };
    let path = Path::new(path);
    let resolved = if path.is_absolute() {
        path.to_path_buf()
    } else {
        project_root.join(path)
    };
    resolved
        .canonicalize()
        .is_ok_and(|resolved| resolved == package_root)
}

fn current_generation(project_root: &Path) -> Result<Option<String>, PackageError> {
    harn_modules::package_snapshot::PackageSnapshot::acquire(project_root)
        .map(|snapshot| snapshot.map(|snapshot| snapshot.generation().to_string()))
        .map_err(|error| PackageError::Lockfile(error.to_string()))
}
