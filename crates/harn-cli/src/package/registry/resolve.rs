//! Turning a user-facing spec into a registry answer: package and version
//! lookup, search, info, and synthesis of the `Dependency` a resolved
//! registry version implies.

use crate::package::*;

/// Look up a registry package by either its scoped registry name
/// (`@burin/notion-sdk`) or any `[[package.version]].package` alias
/// (`notion-sdk-harn`). Bare-name lookup falls back to the alias so
/// `harn add notion-sdk-harn@0.1.0` works the same as the scoped form.
fn lookup_registry_package<'a>(
    index: &'a PackageRegistryIndex,
    name: &str,
) -> Result<&'a RegistryPackage, PackageError> {
    if let Some(package) = index.packages.iter().find(|package| package.name == name) {
        return Ok(package);
    }
    let matches: Vec<&RegistryPackage> = index
        .packages
        .iter()
        .filter(|package| {
            package
                .versions
                .iter()
                .any(|entry| entry.package.as_deref() == Some(name))
        })
        .collect();
    match matches.as_slice() {
        [package] => Ok(package),
        [] => Err(format!("package registry does not contain {name}").into()),
        many => Err(format!(
            "package alias {name} is ambiguous in the registry — found {} packages; use the scoped name (e.g. {})",
            many.len(),
            many[0].name,
        )
        .into()),
    }
}

pub(crate) fn find_registry_package_version(
    index: &PackageRegistryIndex,
    name: &str,
    version: Option<&str>,
) -> Result<RegistryPackageInfo, PackageError> {
    let package = lookup_registry_package(index, name)?;
    let selected_version = match version {
        Some(version) => Some(
            package
                .versions
                .iter()
                .find(|entry| entry.version == version)
                .ok_or_else(|| format!("package registry does not contain {name}@{version}"))?
                .clone(),
        ),
        None => latest_registry_version(package).cloned(),
    };
    Ok(RegistryPackageInfo {
        package: package.clone(),
        selected_version,
    })
}

pub(crate) fn find_registry_package_version_matching(
    index: &PackageRegistryIndex,
    name: &str,
    requirement: &str,
) -> Result<RegistryPackageInfo, PackageError> {
    let package = lookup_registry_package(index, name)?;
    let req = parse_registry_version_req(requirement)?;
    let selected_version = package
        .versions
        .iter()
        .filter(|entry| !entry.yanked)
        .filter_map(|entry| {
            parse_registry_semver(&entry.version)
                .ok()
                .filter(|version| req.matches(version))
                .map(|version| (version, entry.clone()))
        })
        .max_by(|(left, _), (right, _)| left.cmp(right))
        .map(|(_, entry)| entry)
        .ok_or_else(|| {
            format!("package registry does not contain {name} matching {requirement}")
        })?;
    Ok(RegistryPackageInfo {
        package: package.clone(),
        selected_version: Some(selected_version),
    })
}

pub(crate) fn search_package_registry_impl(
    query: Option<&str>,
    registry: Option<&str>,
) -> Result<Vec<RegistryPackage>, PackageError> {
    search_package_registry_in(&PackageWorkspace::from_current_dir()?, query, registry)
}

pub(crate) fn search_package_registry_in(
    workspace: &PackageWorkspace,
    query: Option<&str>,
    registry: Option<&str>,
) -> Result<Vec<RegistryPackage>, PackageError> {
    let (_, index) = load_package_registry_in(workspace, registry)?;
    Ok(index
        .packages
        .into_iter()
        .filter(|package| registry_package_matches(package, query.unwrap_or("")))
        .collect())
}

pub(crate) fn search_rule_package_registry_impl(
    query: Option<&str>,
    registry: Option<&str>,
) -> Result<Vec<RegistryPackage>, PackageError> {
    search_rule_package_registry_in(&PackageWorkspace::from_current_dir()?, query, registry)
}

pub(crate) fn search_rule_package_registry_in(
    workspace: &PackageWorkspace,
    query: Option<&str>,
    registry: Option<&str>,
) -> Result<Vec<RegistryPackage>, PackageError> {
    Ok(search_package_registry_in(workspace, query, registry)?
        .into_iter()
        .filter(registry_package_is_rule_pack)
        .collect())
}

fn registry_package_is_rule_pack(package: &RegistryPackage) -> bool {
    package.rule_pack.is_some()
}

pub(crate) fn package_registry_info_impl(
    spec: &str,
    registry: Option<&str>,
) -> Result<RegistryPackageInfo, PackageError> {
    package_registry_info_in(&PackageWorkspace::from_current_dir()?, spec, registry)
}

pub(crate) fn package_registry_info_in(
    workspace: &PackageWorkspace,
    spec: &str,
    registry: Option<&str>,
) -> Result<RegistryPackageInfo, PackageError> {
    let Some((name, version)) = parse_registry_package_spec(spec) else {
        return Err(format!(
            "invalid registry package name '{spec}'; use names like @burin/notion-sdk or acme-lib"
        )
        .into());
    };
    let (_, index) = load_package_registry_in(workspace, registry)?;
    find_registry_package_version(&index, name, version)
}

pub(crate) fn registry_dependency_from_spec_in(
    workspace: &PackageWorkspace,
    spec: &str,
    alias: Option<&str>,
    registry: Option<&str>,
) -> Result<(String, Dependency), PackageError> {
    let Some((name, Some(version))) = parse_registry_package_spec(spec) else {
        return Err(format!(
            "registry dependency '{spec}' must include a version, for example {spec}@1.2.3"
        )
        .into());
    };
    let registry_source = workspace.resolve_registry_source(registry)?;
    let (_, index) = load_package_registry_in(workspace, registry)?;
    // Accept both exact versions (`@1.2.3`) and semver constraints
    // (`@^0.1`, `@~1.4`, `@>=1,<2`). The latter resolve to the highest
    // matching unyanked entry.
    let info = if is_exact_semver(version) {
        find_registry_package_version(&index, name, Some(version))?
    } else {
        find_registry_package_version_matching(&index, name, version)?
    };
    let selected = info
        .selected_version
        .ok_or_else(|| format!("package registry does not contain {name}@{version}"))?;
    if selected.yanked {
        return Err(format!("{name}@{version} is yanked in the package registry").into());
    }
    let package_name = registry_package_version_alias(&info.package.name, &selected)?;
    let alias = alias.unwrap_or(package_name.as_str()).to_string();
    let resolved_version = selected.version.clone();
    Ok((
        alias.clone(),
        registry_dependency_table(
            info.package.name,
            selected,
            package_name,
            alias,
            registry_source,
            resolved_version,
        )?,
    ))
}

fn is_exact_semver(spec: &str) -> bool {
    parse_registry_semver(spec).is_ok()
}

pub(crate) fn registry_dependency_from_manifest_constraint_in(
    workspace: &PackageWorkspace,
    alias: &str,
    table: &DepTable,
) -> Result<Dependency, PackageError> {
    let requirement = table
        .version
        .as_deref()
        .ok_or_else(|| format!("dependency {alias} is missing `version`"))?;
    let registry_source = workspace.resolve_registry_source(table.registry.as_deref())?;
    let registry_name = table.registry_name.as_deref().unwrap_or(alias);
    let (_, index) = load_package_registry_in(workspace, Some(&registry_source))?;
    let info = find_registry_package_version_matching(&index, registry_name, requirement)?;
    let selected = info.selected_version.ok_or_else(|| {
        format!("package registry does not contain {registry_name} matching {requirement}")
    })?;
    let package_name = selected
        .package
        .clone()
        .or_else(|| table.package.clone())
        .unwrap_or_else(|| alias.to_string());
    let resolved_version = selected.version.clone();
    registry_dependency_table(
        registry_name.to_string(),
        selected,
        package_name,
        alias.to_string(),
        registry_source,
        resolved_version,
    )
}

fn registry_package_version_alias(
    registry_name: &str,
    selected: &RegistryPackageVersion,
) -> Result<String, PackageError> {
    if let Some(package) = selected.package.clone() {
        return Ok(package);
    }
    if let Some(git) = selected.git.as_deref() {
        return derive_repo_name_from_source(&normalize_git_url(git)?);
    }
    derive_registry_alias_from_name(registry_name)
}

fn derive_registry_alias_from_name(registry_name: &str) -> Result<String, PackageError> {
    let alias = registry_name
        .strip_prefix('@')
        .and_then(|scoped| scoped.split_once('/').map(|(_, package)| package))
        .unwrap_or(registry_name)
        .to_string();
    validate_package_alias(&alias)?;
    Ok(alias)
}

fn registry_dependency_table(
    registry_name: String,
    selected: RegistryPackageVersion,
    package_name: String,
    alias: String,
    registry_source: String,
    resolved_version: String,
) -> Result<Dependency, PackageError> {
    let package = (alias != package_name).then_some(package_name);
    let RegistryPackageVersion {
        git,
        archive,
        tag,
        rev,
        branch,
        checksum,
        ..
    } = selected;
    let table = if let Some(git) = git {
        let git = normalize_git_url(&git)?;
        let rev = if tag.is_some() { None } else { rev };
        DepTable {
            git: Some(git),
            tag,
            rev,
            branch,
            package,
            registry: Some(registry_source),
            // Store the canonical scoped registry name (e.g. `@burin/notion-sdk`)
            // even when the user typed the bare alias (`notion-sdk-harn`) so
            // re-resolves stay anchored to the same registry row.
            registry_name: Some(registry_name),
            registry_version: Some(resolved_version),
            ..DepTable::default()
        }
    } else if let Some(archive) = archive {
        DepTable {
            archive: Some(normalize_archive_url(&archive)?),
            package,
            checksum,
            registry: Some(registry_source),
            registry_name: Some(registry_name),
            registry_version: Some(resolved_version),
            ..DepTable::default()
        }
    } else {
        return Err("registry package version is missing git or archive source"
            .to_string()
            .into());
    };
    Ok(Dependency::Table(Box::new(table)))
}
