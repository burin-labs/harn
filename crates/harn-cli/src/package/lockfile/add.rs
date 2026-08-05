//! `harn add`: normalizing the many spec shapes a user can type into one
//! request, then writing it to the manifest and lock file.

use crate::package::*;

#[derive(Clone, Copy, Debug)]
pub(crate) struct AddPackageRequest<'a> {
    name_or_spec: &'a str,
    alias: Option<&'a str>,
    git_url: Option<&'a str>,
    tag: Option<&'a str>,
    rev: Option<&'a str>,
    branch: Option<&'a str>,
    local_path: Option<&'a str>,
    registry: Option<&'a str>,
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(crate) fn normalize_add_request(
    name_or_spec: &str,
    alias: Option<&str>,
    git_url: Option<&str>,
    tag: Option<&str>,
    rev: Option<&str>,
    branch: Option<&str>,
    local_path: Option<&str>,
    registry: Option<&str>,
) -> Result<(String, Dependency), PackageError> {
    normalize_add_request_in(
        &PackageWorkspace::from_current_dir()?,
        AddPackageRequest {
            name_or_spec,
            alias,
            git_url,
            tag,
            rev,
            branch,
            local_path,
            registry,
        },
    )
}

pub(crate) fn normalize_add_request_in(
    workspace: &PackageWorkspace,
    request: AddPackageRequest<'_>,
) -> Result<(String, Dependency), PackageError> {
    let AddPackageRequest {
        name_or_spec,
        alias,
        git_url,
        tag,
        rev,
        branch,
        local_path,
        registry,
    } = request;

    if local_path.is_some() && (rev.is_some() || tag.is_some() || branch.is_some()) {
        return Err("path dependencies do not accept --rev, --tag, or --branch"
            .to_string()
            .into());
    }
    if git_url.is_none()
        && local_path.is_none()
        && rev.is_none()
        && tag.is_none()
        && branch.is_none()
    {
        if let Some(path) = existing_local_path_spec(name_or_spec) {
            let alias = alias
                .map(str::to_string)
                .map(Ok)
                .unwrap_or_else(|| derive_package_alias_from_path(&path))?;
            validate_package_alias(&alias)?;
            return Ok((
                alias,
                Dependency::Table(Box::new(DepTable {
                    path: Some(name_or_spec.to_string()),
                    ..DepTable::default()
                })),
            ));
        }
        if parse_registry_package_spec(name_or_spec).is_some() {
            return registry_dependency_from_spec_in(workspace, name_or_spec, alias, registry);
        }
    }
    if git_url.is_some() || local_path.is_some() {
        if let Some(path) = local_path {
            let alias = alias
                .map(str::to_string)
                .unwrap_or_else(|| name_or_spec.to_string());
            validate_package_alias(&alias)?;
            return Ok((
                alias,
                Dependency::Table(Box::new(DepTable {
                    path: Some(path.to_string()),
                    ..DepTable::default()
                })),
            ));
        }
        let alias = alias.unwrap_or(name_or_spec).to_string();
        validate_package_alias(&alias)?;
        if rev.is_some() && tag.is_some() {
            return Err("use only one of --rev or --tag".to_string().into());
        }
        if rev.is_none() && tag.is_none() && branch.is_none() {
            return Err(format!(
                "git dependency {alias} must specify `tag`, `rev`, or `branch`; use `harn add <url>@<tag-or-sha>` or pass `--tag`/`--rev`/`--branch`"
            ).into());
        }
        let git = normalize_git_url(git_url.ok_or_else(|| "missing --git URL".to_string())?)?;
        let package_name = derive_repo_name_from_source(&git)?;
        return Ok((
            alias.clone(),
            Dependency::Table(Box::new(DepTable {
                git: Some(git),
                tag: tag.map(str::to_string),
                rev: rev.map(str::to_string),
                branch: branch.map(str::to_string),
                package: (alias != package_name).then_some(package_name),
                ..DepTable::default()
            })),
        ));
    }

    if rev.is_some() && tag.is_some() {
        return Err("use only one of --rev or --tag".to_string().into());
    }
    let (raw_source, inline_ref) = parse_positional_git_spec(name_or_spec);
    if inline_ref.is_some() && (rev.is_some() || tag.is_some() || branch.is_some()) {
        return Err(
            "specify the git ref either inline as @ref or via --tag/--rev/--branch"
                .to_string()
                .into(),
        );
    }
    let git = normalize_git_url(raw_source)?;
    let package_name = derive_repo_name_from_source(&git)?;
    let alias = alias.unwrap_or(package_name.as_str()).to_string();
    validate_package_alias(&alias)?;
    if inline_ref.is_none() && rev.is_none() && tag.is_none() && branch.is_none() {
        return Err(format!(
            "git dependency {alias} must specify `tag`, `rev`, or `branch`; use `harn add {raw_source}@<tag-or-sha>` or pass `--tag`/`--rev`/`--branch`"
        ).into());
    }
    Ok((
        alias.clone(),
        Dependency::Table(Box::new(DepTable {
            git: Some(git),
            tag: tag.map(str::to_string),
            rev: inline_ref.or(rev).map(str::to_string),
            branch: branch.map(str::to_string),
            package: (alias != package_name).then_some(package_name),
            ..DepTable::default()
        })),
    ))
}

#[cfg(test)]
pub fn add_package(
    name_or_spec: &str,
    alias: Option<&str>,
    git_url: Option<&str>,
    tag: Option<&str>,
    rev: Option<&str>,
    branch: Option<&str>,
    local_path: Option<&str>,
) {
    add_package_with_registry(
        name_or_spec,
        alias,
        git_url,
        tag,
        rev,
        branch,
        local_path,
        None,
    );
}

pub fn add_package_with_registry(
    name_or_spec: &str,
    alias: Option<&str>,
    git_url: Option<&str>,
    tag: Option<&str>,
    rev: Option<&str>,
    branch: Option<&str>,
    local_path: Option<&str>,
    registry: Option<&str>,
) {
    let result = PackageWorkspace::from_current_dir().and_then(|workspace| {
        add_package_to(
            &workspace,
            name_or_spec,
            alias,
            git_url,
            tag,
            rev,
            branch,
            local_path,
            registry,
        )
    });

    match result {
        Ok((alias, installed)) => {
            println!("Added {alias} to {MANIFEST}.");
            println!("Installed {installed} package(s).");
        }
        Err(error) => {
            eprintln!("error: {error}");
            process::exit(1);
        }
    }
}
#[allow(clippy::too_many_arguments)]
pub(crate) fn add_package_to(
    workspace: &PackageWorkspace,
    name_or_spec: &str,
    alias: Option<&str>,
    git_url: Option<&str>,
    tag: Option<&str>,
    rev: Option<&str>,
    branch: Option<&str>,
    local_path: Option<&str>,
    registry: Option<&str>,
) -> Result<(String, usize), PackageError> {
    let _mutation_lock = acquire_package_mutation_lock(workspace)?;
    let manifest_path = workspace.manifest_dir().join(MANIFEST);
    let (alias, dependency) = normalize_add_request_in(
        workspace,
        AddPackageRequest {
            name_or_spec,
            alias,
            git_url,
            tag,
            rev,
            branch,
            local_path,
            registry,
        },
    )?;
    upsert_dependency_in_manifest(&manifest_path, &alias, &dependency)?;
    let installed = install_packages_in_locked(workspace, false, None, false)?;
    Ok((alias, installed))
}
