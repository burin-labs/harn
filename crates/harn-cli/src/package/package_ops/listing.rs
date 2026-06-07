use super::*;

pub(crate) fn list_packages_impl() -> Result<PackageListReport, PackageError> {
    let workspace = PackageWorkspace::from_current_dir()?;
    list_packages_in(&workspace)
}

pub(super) fn list_packages_in(
    workspace: &PackageWorkspace,
) -> Result<PackageListReport, PackageError> {
    let ctx = workspace.load_manifest_context()?;
    let lock_path = ctx.lock_path();
    let lock = LockFile::load(&lock_path)?;
    let packages = lock
        .as_ref()
        .map(|lock| package_list_entries(&ctx, lock))
        .unwrap_or_default();
    Ok(PackageListReport {
        manifest_path: ctx.manifest_path().display().to_string(),
        lock_path: lock_path.display().to_string(),
        lock_present: lock.is_some(),
        dependency_count: ctx.manifest.dependencies.len(),
        packages,
    })
}

pub(crate) fn doctor_packages_impl() -> Result<PackageDoctorReport, PackageError> {
    let workspace = PackageWorkspace::from_current_dir()?;
    doctor_packages_in(&workspace)
}

pub(super) fn doctor_packages_in(
    workspace: &PackageWorkspace,
) -> Result<PackageDoctorReport, PackageError> {
    let ctx = workspace.load_manifest_context()?;
    let lock_path = ctx.lock_path();
    let mut diagnostics = Vec::new();

    let mut root_errors = Vec::new();
    let mut root_warnings = Vec::new();
    if let Some(package) = ctx.manifest.package.as_ref() {
        if let Some(name) = package.name.as_ref() {
            if let Err(message) = validate_package_alias(name) {
                push_error(&mut root_errors, "[package].name", message);
            }
        }
    }
    validate_package_interface_exports(&ctx, &mut root_errors, &mut root_warnings);
    for diagnostic in root_errors {
        diagnostics.push(package_doctor_diagnostic(
            "error",
            "root-package-contract",
            format!("{}: {}", diagnostic.field, diagnostic.message),
            Some("fix install-facing package metadata in harn.toml"),
        ));
    }
    for diagnostic in root_warnings {
        diagnostics.push(package_doctor_diagnostic(
            "warning",
            "root-package-contract",
            format!("{}: {}", diagnostic.field, diagnostic.message),
            None::<String>,
        ));
    }

    let lock = LockFile::load(&lock_path)?;
    if ctx.manifest.dependencies.is_empty() {
        diagnostics.push(package_doctor_diagnostic(
            "info",
            "no-dependencies",
            "manifest has no package dependencies",
            None::<String>,
        ));
    } else if lock.is_none() {
        diagnostics.push(package_doctor_diagnostic(
            "error",
            "missing-lockfile",
            format!("{} is missing", lock_path.display()),
            Some("run `harn install` to resolve dependencies and write harn.lock"),
        ));
    }

    if let Some(lock) = lock.as_ref() {
        if let Err(error) = validate_lock_matches_manifest(workspace, &ctx, lock) {
            diagnostics.push(package_doctor_diagnostic(
                "error",
                "stale-lockfile",
                error.to_string(),
                Some("run `harn install` to refresh harn.lock"),
            ));
        }
        for entry in &lock.packages {
            validate_installed_package_entry(&ctx, entry, &mut diagnostics);
        }
    }

    let packages = lock
        .as_ref()
        .map(|lock| package_list_entries(&ctx, lock))
        .unwrap_or_default();
    let ok = diagnostics
        .iter()
        .all(|diagnostic| diagnostic.severity != "error");
    Ok(PackageDoctorReport {
        ok,
        manifest_path: ctx.manifest_path().display().to_string(),
        lock_path: lock_path.display().to_string(),
        diagnostics,
        packages,
    })
}

pub(super) fn package_list_entries(
    ctx: &ManifestContext,
    lock: &LockFile,
) -> Vec<PackageListEntry> {
    lock.packages
        .iter()
        .map(|entry| {
            let materialized = materialized_package_exists(ctx, entry);
            PackageListEntry {
                name: entry.name.clone(),
                source: entry.source.clone(),
                package_version: entry.package_version.clone(),
                harn_compat: entry.harn_compat.clone(),
                provenance: entry.provenance.clone(),
                materialized,
                integrity: package_integrity_status(ctx, entry),
                exports: entry.exports.clone(),
                permissions: entry.permissions.clone(),
                host_requirements: entry.host_requirements.clone(),
            }
        })
        .collect()
}

pub(super) fn materialized_package_path(ctx: &ManifestContext, entry: &LockEntry) -> PathBuf {
    let packages_dir = ctx.packages_dir();
    let dir = packages_dir.join(&entry.name);
    if dir.exists() {
        return dir;
    }
    packages_dir.join(format!("{}.harn", entry.name))
}

pub(super) fn materialized_package_exists(ctx: &ManifestContext, entry: &LockEntry) -> bool {
    materialized_package_path(ctx, entry).exists()
}

pub(super) fn package_integrity_status(ctx: &ManifestContext, entry: &LockEntry) -> String {
    if !materialized_package_exists(ctx, entry) {
        return "missing".to_string();
    }
    let Some(expected) = entry.content_hash.as_deref() else {
        return "not_checked".to_string();
    };
    let path = materialized_package_path(ctx, entry);
    if path.is_dir() && materialized_hash_matches(&path, expected) {
        "ok".to_string()
    } else {
        "mismatch".to_string()
    }
}

pub(super) fn validate_installed_package_entry(
    ctx: &ManifestContext,
    entry: &LockEntry,
    diagnostics: &mut Vec<PackageDoctorDiagnostic>,
) {
    let materialized_path = materialized_package_path(ctx, entry);
    if !materialized_path.exists() {
        diagnostics.push(package_doctor_diagnostic(
            "error",
            "package-not-materialized",
            format!(
                "package {} is locked but missing from {}",
                entry.name,
                ctx.packages_dir().display()
            ),
            Some("run `harn install` to materialize locked packages"),
        ));
        return;
    }

    if package_integrity_status(ctx, entry) == "mismatch" {
        diagnostics.push(package_doctor_diagnostic(
            "error",
            "content-hash-mismatch",
            format!(
                "package {} does not match its locked content hash",
                entry.name
            ),
            Some(
                "run `harn install --refetch {alias}` or inspect local tampering"
                    .replace("{alias}", &entry.name),
            ),
        ));
    }

    for requirement in &entry.host_requirements {
        if !host_requirement_satisfied(&ctx.manifest.check, requirement) {
            diagnostics.push(package_doctor_diagnostic(
                "error",
                "missing-host-capability",
                format!(
                    "package {} requires host capability {requirement}, but harn.toml does not declare it",
                    entry.name
                ),
                Some("add the capability under [check.host_capabilities] or preflight_allow after the host implements it"),
            ));
        }
    }

    if materialized_path.is_dir() {
        match read_package_manifest_from_dir(&materialized_path) {
            Ok(Some(manifest)) => {
                let installed_ctx = ManifestContext {
                    manifest,
                    dir: materialized_path,
                };
                let mut errors = Vec::new();
                let mut warnings = Vec::new();
                validate_package_interface_exports(&installed_ctx, &mut errors, &mut warnings);
                for diagnostic in errors {
                    diagnostics.push(package_doctor_diagnostic(
                        "error",
                        "installed-package-export",
                        format!("{}: {}", diagnostic.field, diagnostic.message),
                        Some(format!("fix package {} and reinstall it", entry.name)),
                    ));
                }
                for diagnostic in warnings {
                    diagnostics.push(package_doctor_diagnostic(
                        "warning",
                        "installed-package-export-warning",
                        format!("{}: {}", diagnostic.field, diagnostic.message),
                        None::<String>,
                    ));
                }
            }
            Ok(None) => {}
            Err(error) => diagnostics.push(package_doctor_diagnostic(
                "error",
                "installed-manifest-unreadable",
                format!("failed to read package {} manifest: {error}", entry.name),
                Some("repair the package source and run `harn install`"),
            )),
        }
    }
}

pub(super) fn host_requirement_satisfied(check: &CheckConfig, requirement: &str) -> bool {
    if check.preflight_allow.iter().any(|allow| {
        allow == "*"
            || allow == requirement
            || requirement
                .strip_prefix(allow.trim_end_matches(".*"))
                .is_some_and(|rest| allow.ends_with(".*") && rest.starts_with('.'))
            || requirement
                .split_once('.')
                .is_some_and(|(capability, _)| allow == capability)
    }) {
        return true;
    }
    let Some((capability, operation)) = requirement.split_once('.') else {
        return false;
    };
    check
        .host_capabilities
        .get(capability)
        .is_some_and(|ops| ops.iter().any(|op| op == "*" || op == operation))
}

pub(super) fn package_doctor_diagnostic(
    severity: impl Into<String>,
    code: impl Into<String>,
    message: impl Into<String>,
    help: Option<impl Into<String>>,
) -> PackageDoctorDiagnostic {
    PackageDoctorDiagnostic {
        severity: severity.into(),
        code: code.into(),
        message: message.into(),
        help: help.map(Into::into),
    }
}
