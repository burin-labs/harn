//! The `harn install` / `lock` / `update` / `remove` CLI entry points —
//! argument handling, human and JSON rendering, and process exit codes.

use crate::package::*;

pub(crate) fn install_packages_impl(
    frozen: bool,
    refetch: Option<&str>,
    offline: bool,
) -> Result<usize, PackageError> {
    install_packages_in(
        &PackageWorkspace::from_current_dir()?,
        frozen,
        refetch,
        offline,
    )
}

pub(crate) fn install_packages_in_locked(
    workspace: &PackageWorkspace,
    frozen: bool,
    refetch: Option<&str>,
    offline: bool,
) -> Result<usize, PackageError> {
    let ctx = workspace.load_manifest_context()?;
    let existing = LockFile::load(&ctx.lock_path())?;
    if ctx.manifest.dependencies.is_empty() {
        let empty = LockFile::default();
        if frozen || offline {
            // A lock that still pins packages the manifest no longer
            // declares is a substantive change; surface it instead of
            // silently succeeding against a stale lock.
            if existing
                .as_ref()
                .is_some_and(|lock| !lock.packages.is_empty())
            {
                return Err(format!("{} would need to change", ctx.lock_path().display()).into());
            }
        } else {
            empty.save(&ctx.lock_path())?;
        }
        return materialize_dependencies_from_lock(workspace, &ctx, &empty, refetch, offline);
    }

    if (frozen || offline) && existing.is_none() {
        return Err(format!("{} is missing", ctx.lock_path().display()).into());
    }
    if (frozen || offline)
        && existing
            .as_ref()
            .is_some_and(LockFile::requires_git_hash_migration)
    {
        return Err(format!(
            "{} contains pre-v5 Git content hashes; run `harn install` and commit the migrated lockfile before using --locked or --offline",
            ctx.lock_path().display()
        )
        .into());
    }

    let desired = build_lockfile(
        workspace,
        &ctx,
        existing.as_ref(),
        None,
        false,
        !frozen && !offline,
        offline,
    )?;
    if frozen || offline {
        if !existing
            .as_ref()
            .is_some_and(|lock| lock.same_resolution(&desired))
        {
            return Err(format!("{} would need to change", ctx.lock_path().display()).into());
        }
    } else {
        desired.save(&ctx.lock_path())?;
    }
    materialize_dependencies_from_lock(workspace, &ctx, &desired, refetch, offline)
}

pub fn install_packages(frozen: bool, refetch: Option<&str>, offline: bool, json: bool) {
    match install_packages_impl(frozen, refetch, offline) {
        Ok(installed) if json => {
            print_install_summary_json("install", installed, frozen, offline);
        }
        Ok(0) => println!("No dependencies to install."),
        Ok(installed) => {
            println!("Installed {installed} package(s) in a new immutable generation.");
        }
        Err(error) if json => {
            print_install_error_json("install", &error);
            process::exit(1);
        }
        Err(error) => {
            eprintln!("error: {error}");
            process::exit(1);
        }
    }
}

fn print_install_summary_json(action: &str, installed: usize, frozen: bool, offline: bool) {
    let body = serde_json::json!({
        "action": action,
        "ok": true,
        "installed": installed,
        "frozen": frozen,
        "offline": offline,
        "lock_file": LOCK_FILE,
        "package_pointer": ".harn/package-current.toml",
    });
    println!(
        "{}",
        serde_json::to_string_pretty(&body).unwrap_or_default()
    );
}

fn print_install_error_json(action: &str, error: &PackageError) {
    let body = serde_json::json!({
        "action": action,
        "ok": false,
        "error": error.to_string(),
    });
    println!(
        "{}",
        serde_json::to_string_pretty(&body).unwrap_or_default()
    );
}
pub fn lock_packages() {
    let result = (|| -> Result<usize, PackageError> {
        let workspace = PackageWorkspace::from_current_dir()?;
        let _mutation_lock = acquire_package_mutation_lock(&workspace)?;
        let ctx = workspace.load_manifest_context()?;
        let existing = LockFile::load(&ctx.lock_path())?;
        let lock = build_lockfile(&workspace, &ctx, existing.as_ref(), None, true, true, false)?;
        lock.save(&ctx.lock_path())?;
        Ok(lock.packages.len())
    })();

    match result {
        Ok(count) => println!("Wrote {LOCK_FILE} with {count} package(s)."),
        Err(error) => {
            eprintln!("error: {error}");
            process::exit(1);
        }
    }
}
pub fn update_packages(alias: Option<&str>, all: bool, json: bool) {
    let result = PackageWorkspace::from_current_dir()
        .and_then(|workspace| update_packages_in(&workspace, alias, all));
    print_update_packages_result(result, json);
}

pub(crate) fn update_packages_in(
    workspace: &PackageWorkspace,
    alias: Option<&str>,
    all: bool,
) -> Result<usize, PackageError> {
    let _mutation_lock = acquire_package_mutation_lock(workspace)?;
    if !all && alias.is_none() {
        return Err("specify a dependency alias or pass --all"
            .to_string()
            .into());
    }

    let ctx = workspace.load_manifest_context()?;
    if let Some(alias) = alias {
        validate_package_alias(alias)?;
        if !ctx.manifest.dependencies.contains_key(alias) {
            return Err(format!("{alias} is not present in [dependencies]").into());
        }
    }
    let existing = LockFile::load(&ctx.lock_path())?;
    let lock = build_lockfile(workspace, &ctx, existing.as_ref(), alias, all, true, false)?;
    lock.save(&ctx.lock_path())?;
    materialize_dependencies_from_lock(workspace, &ctx, &lock, None, false)
}

fn print_update_packages_result(result: Result<usize, PackageError>, json: bool) {
    match result {
        Ok(installed) if json => print_install_summary_json("update", installed, false, false),
        Ok(installed) => println!("Updated {installed} package(s)."),
        Err(error) if json => {
            print_install_error_json("update", &error);
            process::exit(1);
        }
        Err(error) => {
            eprintln!("error: {error}");
            process::exit(1);
        }
    }
}
pub fn remove_package(alias: &str) {
    let result = PackageWorkspace::from_current_dir()
        .and_then(|workspace| remove_package_in(&workspace, alias));
    print_remove_package_result(alias, result);
}

pub(crate) fn remove_package_in(
    workspace: &PackageWorkspace,
    alias: &str,
) -> Result<bool, PackageError> {
    let _mutation_lock = acquire_package_mutation_lock(workspace)?;
    validate_package_alias(alias)?;
    let ctx = workspace.load_manifest_context()?;
    let removed = remove_dependency_from_manifest(&ctx.manifest_path(), alias)?;
    if !removed {
        return Ok(false);
    }
    let mut lock = LockFile::load(&ctx.lock_path())?.unwrap_or_default();
    lock.remove(alias);
    lock.save(&ctx.lock_path())?;
    materialize_dependencies_from_lock(workspace, &ctx, &lock, None, false)?;
    Ok(true)
}

fn print_remove_package_result(alias: &str, result: Result<bool, PackageError>) {
    match result {
        Ok(true) => println!("Removed {alias} from {MANIFEST} and {LOCK_FILE}."),
        Ok(false) => {
            eprintln!("error: {alias} is not present in [dependencies]");
            process::exit(1);
        }
        Err(error) => {
            eprintln!("error: {error}");
            process::exit(1);
        }
    }
}
