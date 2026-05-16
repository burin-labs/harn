use super::errors::PackageError;
use super::*;

#[derive(Debug, Clone, Serialize)]
pub struct PackageCheckReport {
    pub package_dir: String,
    pub manifest_path: String,
    pub name: Option<String>,
    pub version: Option<String>,
    pub errors: Vec<PackageCheckDiagnostic>,
    pub warnings: Vec<PackageCheckDiagnostic>,
    pub exports: Vec<PackageExportReport>,
    pub tools: Vec<PackageToolExportReport>,
    pub skills: Vec<PackageSkillExportReport>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PackageCheckDiagnostic {
    pub field: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct PackageExportReport {
    pub name: String,
    pub path: String,
    pub symbols: Vec<PackageApiSymbol>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PackageToolExportReport {
    pub name: String,
    pub module: String,
    pub symbol: String,
    pub permissions: Vec<String>,
    pub host_requirements: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PackageSkillExportReport {
    pub name: String,
    pub path: String,
    pub permissions: Vec<String>,
    pub host_requirements: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PackageApiSymbol {
    pub kind: String,
    pub name: String,
    pub signature: String,
    pub docs: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PackagePackReport {
    pub package_dir: String,
    pub artifact_dir: String,
    pub dry_run: bool,
    pub files: Vec<String>,
    pub check: PackageCheckReport,
}

#[derive(Debug, Clone, Serialize)]
pub struct PackagePublishReport {
    pub dry_run: bool,
    pub registry: String,
    pub artifact_dir: String,
    pub files: Vec<String>,
    pub check: PackageCheckReport,
}

#[derive(Debug, Clone, Serialize)]
pub struct PackageListReport {
    pub manifest_path: String,
    pub lock_path: String,
    pub lock_present: bool,
    pub dependency_count: usize,
    pub packages: Vec<PackageListEntry>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PackageListEntry {
    pub name: String,
    pub source: String,
    pub package_version: Option<String>,
    pub harn_compat: Option<String>,
    pub provenance: Option<String>,
    pub materialized: bool,
    pub integrity: String,
    pub exports: PackageLockExports,
    pub permissions: Vec<String>,
    pub host_requirements: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PackageDoctorReport {
    pub ok: bool,
    pub manifest_path: String,
    pub lock_path: String,
    pub diagnostics: Vec<PackageDoctorDiagnostic>,
    pub packages: Vec<PackageListEntry>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PackageDoctorDiagnostic {
    pub severity: String,
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub help: Option<String>,
}

pub fn check_package(anchor: Option<&Path>, json: bool) {
    match check_package_impl(anchor) {
        Ok(report) => {
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&report)
                        .unwrap_or_else(|error| format!(r#"{{"error":"{error}"}}"#))
                );
            } else {
                print_package_check_report(&report);
            }
            if !report.errors.is_empty() {
                process::exit(1);
            }
        }
        Err(error) => {
            eprintln!("error: {error}");
            process::exit(1);
        }
    }
}

pub fn pack_package(anchor: Option<&Path>, output: Option<&Path>, dry_run: bool, json: bool) {
    match pack_package_impl(anchor, output, dry_run) {
        Ok(report) => {
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&report)
                        .unwrap_or_else(|error| format!(r#"{{"error":"{error}"}}"#))
                );
            } else {
                print_package_pack_report(&report);
            }
        }
        Err(error) => {
            eprintln!("error: {error}");
            process::exit(1);
        }
    }
}

pub fn generate_package_docs(anchor: Option<&Path>, output: Option<&Path>, check: bool) {
    match generate_package_docs_impl(anchor, output, check) {
        Ok(path) if check => println!("{} is up to date.", path.display()),
        Ok(path) => println!("Wrote {}.", path.display()),
        Err(error) => {
            eprintln!("error: {error}");
            process::exit(1);
        }
    }
}

pub fn publish_package(anchor: Option<&Path>, dry_run: bool, registry: Option<&str>, json: bool) {
    if !dry_run {
        eprintln!(
            "error: registry submission is not enabled yet; use `harn publish --dry-run` to validate the package and inspect the artifact"
        );
        process::exit(1);
    }

    match publish_package_impl(anchor, registry) {
        Ok(report) => {
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&report)
                        .unwrap_or_else(|error| format!(r#"{{"error":"{error}"}}"#))
                );
            } else {
                println!("Dry-run publish to {} succeeded.", report.registry);
                println!("artifact: {}", report.artifact_dir);
                println!("files: {}", report.files.len());
            }
        }
        Err(error) => {
            eprintln!("error: {error}");
            process::exit(1);
        }
    }
}

pub fn list_packages(json: bool) {
    match list_packages_impl() {
        Ok(report) if json => {
            println!(
                "{}",
                serde_json::to_string_pretty(&report)
                    .unwrap_or_else(|error| format!(r#"{{"error":"{error}"}}"#))
            );
        }
        Ok(report) => print_package_list_report(&report),
        Err(error) => {
            eprintln!("error: {error}");
            process::exit(1);
        }
    }
}

pub fn doctor_packages(json: bool) {
    match doctor_packages_impl() {
        Ok(report) if json => {
            println!(
                "{}",
                serde_json::to_string_pretty(&report)
                    .unwrap_or_else(|error| format!(r#"{{"error":"{error}"}}"#))
            );
            if !report.ok {
                process::exit(1);
            }
        }
        Ok(report) => {
            print_package_doctor_report(&report);
            if !report.ok {
                process::exit(1);
            }
        }
        Err(error) => {
            eprintln!("error: {error}");
            process::exit(1);
        }
    }
}

pub(crate) fn check_package_impl(
    anchor: Option<&Path>,
) -> Result<PackageCheckReport, PackageError> {
    let ctx = load_manifest_context_for_anchor(anchor)?;
    let manifest_path = ctx.manifest_path();
    let mut errors = Vec::new();
    let mut warnings = Vec::new();

    let package = ctx.manifest.package.as_ref();
    let name = package.and_then(|package| package.name.clone());
    let version = package.and_then(|package| package.version.clone());
    let package_name = required_package_string(
        package.and_then(|package| package.name.as_deref()),
        "[package].name",
        &mut errors,
    );
    if let Some(name) = package_name {
        if let Err(message) = validate_package_alias(name) {
            push_error(&mut errors, "[package].name", message);
        }
    }
    required_package_string(
        package.and_then(|package| package.version.as_deref()),
        "[package].version",
        &mut errors,
    );
    required_package_string(
        package.and_then(|package| package.description.as_deref()),
        "[package].description",
        &mut errors,
    );
    required_package_string(
        package.and_then(|package| package.license.as_deref()),
        "[package].license",
        &mut errors,
    );
    if !ctx.dir.join("README.md").is_file() {
        push_error(&mut errors, "README.md", "package README.md is required");
    }
    if !ctx.dir.join("LICENSE").is_file() && package.and_then(|p| p.license.as_deref()).is_none() {
        push_error(
            &mut errors,
            "[package].license",
            "publishable packages require a license field or LICENSE file",
        );
    }

    validate_optional_url(
        package.and_then(|package| package.repository.as_deref()),
        "[package].repository",
        &mut errors,
    );
    validate_docs_url(
        &ctx.dir,
        package.and_then(|package| package.docs_url.as_deref()),
        &mut errors,
        &mut warnings,
    );
    match package.and_then(|package| package.harn.as_deref()) {
        Some(range) if supports_current_harn(range) => {}
        Some(range) => push_error(
            &mut errors,
            "[package].harn",
            format!(
                "unsupported Harn version range '{range}'; include the current {} line, for example {}",
                current_harn_line_label(),
                current_harn_range_example()
            ),
        ),
        None => push_error(
            &mut errors,
            "[package].harn",
            format!(
                "missing Harn compatibility metadata; add harn = \"{}\"",
                current_harn_range_example()
            ),
        ),
    }

    validate_dependencies_for_publish(&ctx, &mut errors, &mut warnings);
    if let Err(error) = validate_handoff_routes(&ctx.manifest.handoff_routes, &ctx.manifest) {
        push_error(&mut errors, "handoff_routes", error.to_string());
    }
    let exports = validate_exports_for_publish(&ctx, &mut errors, &mut warnings);
    let (tools, skills) = validate_package_interface_exports(&ctx, &mut errors, &mut warnings);

    Ok(PackageCheckReport {
        package_dir: ctx.dir.display().to_string(),
        manifest_path: manifest_path.display().to_string(),
        name,
        version,
        errors,
        warnings,
        exports,
        tools,
        skills,
    })
}

pub(crate) fn list_packages_impl() -> Result<PackageListReport, PackageError> {
    let workspace = PackageWorkspace::from_current_dir()?;
    list_packages_in(&workspace)
}

fn list_packages_in(workspace: &PackageWorkspace) -> Result<PackageListReport, PackageError> {
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

fn doctor_packages_in(workspace: &PackageWorkspace) -> Result<PackageDoctorReport, PackageError> {
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
        if let Err(error) = validate_lock_matches_manifest(&ctx, lock) {
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

fn package_list_entries(ctx: &ManifestContext, lock: &LockFile) -> Vec<PackageListEntry> {
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

fn materialized_package_path(ctx: &ManifestContext, entry: &LockEntry) -> PathBuf {
    let packages_dir = ctx.packages_dir();
    let dir = packages_dir.join(&entry.name);
    if dir.exists() {
        return dir;
    }
    packages_dir.join(format!("{}.harn", entry.name))
}

fn materialized_package_exists(ctx: &ManifestContext, entry: &LockEntry) -> bool {
    materialized_package_path(ctx, entry).exists()
}

fn package_integrity_status(ctx: &ManifestContext, entry: &LockEntry) -> String {
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

fn validate_installed_package_entry(
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

fn host_requirement_satisfied(check: &CheckConfig, requirement: &str) -> bool {
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

fn package_doctor_diagnostic(
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

pub(crate) fn pack_package_impl(
    anchor: Option<&Path>,
    output: Option<&Path>,
    dry_run: bool,
) -> Result<PackagePackReport, PackageError> {
    let report = check_package_impl(anchor)?;
    fail_if_package_errors(&report)?;
    let ctx = load_manifest_context_for_anchor(anchor)?;
    let files = collect_package_files(&ctx.dir)?;
    let artifact_dir = output
        .map(Path::to_path_buf)
        .unwrap_or_else(|| default_artifact_dir(&ctx, &report));

    if !dry_run {
        if artifact_dir.exists() {
            return Err(
                format!("artifact output {} already exists", artifact_dir.display()).into(),
            );
        }
        fs::create_dir_all(&artifact_dir)
            .map_err(|error| format!("failed to create {}: {error}", artifact_dir.display()))?;
        for rel in &files {
            let src = ctx.dir.join(rel);
            let dst = artifact_dir.join(rel);
            if let Some(parent) = dst.parent() {
                fs::create_dir_all(parent)
                    .map_err(|error| format!("failed to create {}: {error}", parent.display()))?;
            }
            fs::copy(&src, &dst)
                .map_err(|error| format!("failed to copy {}: {error}", src.display()))?;
        }
        let manifest_path = artifact_dir.join(".harn-package-manifest.json");
        let manifest_body = serde_json::to_string_pretty(&report)
            .map_err(|error| format!("failed to render package manifest: {error}"))?
            + "\n";
        harn_vm::atomic_io::atomic_write(&manifest_path, manifest_body.as_bytes())
            .map_err(|error| format!("failed to write {}: {error}", manifest_path.display()))?;
    }

    Ok(PackagePackReport {
        package_dir: ctx.dir.display().to_string(),
        artifact_dir: artifact_dir.display().to_string(),
        dry_run,
        files,
        check: report,
    })
}

pub(crate) fn generate_package_docs_impl(
    anchor: Option<&Path>,
    output: Option<&Path>,
    check: bool,
) -> Result<PathBuf, PackageError> {
    let report = check_package_impl(anchor)?;
    let ctx = load_manifest_context_for_anchor(anchor)?;
    let output_path = output
        .map(Path::to_path_buf)
        .unwrap_or_else(|| ctx.dir.join("docs").join("api.md"));
    let rendered = render_package_api_docs(&report);
    if check {
        let existing = fs::read_to_string(&output_path)
            .map_err(|error| format!("failed to read {}: {error}", output_path.display()))?;
        if normalize_newlines(&existing) != normalize_newlines(&rendered) {
            return Err(format!(
                "{} is stale; run `harn package docs`",
                output_path.display()
            )
            .into());
        }
        return Ok(output_path);
    }
    harn_vm::atomic_io::atomic_write(&output_path, rendered.as_bytes())
        .map_err(|error| format!("failed to write {}: {error}", output_path.display()))?;
    Ok(output_path)
}

pub(crate) fn publish_package_impl(
    anchor: Option<&Path>,
    registry: Option<&str>,
) -> Result<PackagePublishReport, PackageError> {
    let pack = pack_package_impl(anchor, None, true)?;
    let registry = resolve_configured_registry_source(registry)?;
    Ok(PackagePublishReport {
        dry_run: true,
        registry,
        artifact_dir: pack.artifact_dir,
        files: pack.files,
        check: pack.check,
    })
}

pub(crate) fn load_manifest_context_for_anchor(
    anchor: Option<&Path>,
) -> Result<ManifestContext, PackageError> {
    let anchor = anchor
        .map(Path::to_path_buf)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let manifest_path = if anchor.is_dir() {
        anchor.join(MANIFEST)
    } else if anchor.file_name() == Some(OsStr::new(MANIFEST)) {
        anchor.clone()
    } else {
        let (_, dir) = find_nearest_manifest(&anchor)
            .ok_or_else(|| format!("no {MANIFEST} found from {}", anchor.display()))?;
        dir.join(MANIFEST)
    };
    let manifest = read_manifest_from_path(&manifest_path)?;
    let dir = manifest_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    Ok(ManifestContext { manifest, dir })
}

pub(crate) fn required_package_string<'a>(
    value: Option<&'a str>,
    field: &str,
    errors: &mut Vec<PackageCheckDiagnostic>,
) -> Option<&'a str> {
    match value.map(str::trim).filter(|value| !value.is_empty()) {
        Some(value) => Some(value),
        None => {
            push_error(errors, field, format!("missing required {field}"));
            None
        }
    }
}

pub(crate) fn push_error(
    diagnostics: &mut Vec<PackageCheckDiagnostic>,
    field: impl Into<String>,
    message: impl Into<String>,
) {
    diagnostics.push(PackageCheckDiagnostic {
        field: field.into(),
        message: message.into(),
    });
}

pub(crate) fn push_warning(
    diagnostics: &mut Vec<PackageCheckDiagnostic>,
    field: impl Into<String>,
    message: impl Into<String>,
) {
    push_error(diagnostics, field, message);
}

pub(crate) fn validate_optional_url(
    value: Option<&str>,
    field: &str,
    errors: &mut Vec<PackageCheckDiagnostic>,
) {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        push_error(errors, field, format!("missing required {field}"));
        return;
    };
    if Url::parse(value).is_err() {
        push_error(errors, field, format!("{field} must be an absolute URL"));
    }
}

pub(crate) fn validate_docs_url(
    root: &Path,
    value: Option<&str>,
    errors: &mut Vec<PackageCheckDiagnostic>,
    warnings: &mut Vec<PackageCheckDiagnostic>,
) {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        push_warning(
            warnings,
            "[package].docs_url",
            "missing docs_url; `harn package docs` defaults to docs/api.md",
        );
        return;
    };
    if Url::parse(value).is_ok() {
        return;
    }
    let path = PathBuf::from(value);
    let path = if path.is_absolute() {
        path
    } else {
        root.join(path)
    };
    if !path.exists() {
        push_error(
            errors,
            "[package].docs_url",
            format!("docs_url path {} does not exist", path.display()),
        );
    }
}

pub(crate) fn validate_dependencies_for_publish(
    ctx: &ManifestContext,
    errors: &mut Vec<PackageCheckDiagnostic>,
    warnings: &mut Vec<PackageCheckDiagnostic>,
) {
    let mut aliases = BTreeSet::new();
    for (alias, dependency) in &ctx.manifest.dependencies {
        let field = format!("[dependencies].{alias}");
        if let Err(message) = validate_package_alias(alias) {
            push_error(errors, &field, message);
        }
        if !aliases.insert(alias) {
            push_error(errors, &field, "duplicate dependency alias");
        }
        match dependency {
            Dependency::Path(path) => push_error(
                errors,
                &field,
                format!("path-only dependency '{path}' is not publishable; pin a git rev or registry version"),
            ),
            Dependency::Table(table) => {
                if table.path.is_some() {
                    push_error(
                        errors,
                        &field,
                        "path dependencies are not publishable; pin a git rev or registry version",
                    );
                }
                if table.git.is_none() && table.path.is_none() {
                    push_error(errors, &field, "dependency must specify git, registry-expanded git, or path");
                }
                if table.rev.is_some() && table.branch.is_some() {
                    push_error(errors, &field, "dependency cannot specify both rev and branch");
                }
                if table.git.is_some() && table.rev.is_none() && table.branch.is_none() {
                    push_error(errors, &field, "git dependency must specify rev or branch");
                }
                if table.branch.is_some() {
                    push_warning(
                        warnings,
                        &field,
                        "branch dependencies are allowed but rev pins are more reproducible for publishing",
                    );
                }
                if let Some(git) = table.git.as_deref() {
                    if normalize_git_url(git).is_err() {
                        push_error(errors, &field, format!("invalid git source '{git}'"));
                    }
                }
            }
        }
    }
}

pub(crate) fn validate_exports_for_publish(
    ctx: &ManifestContext,
    errors: &mut Vec<PackageCheckDiagnostic>,
    warnings: &mut Vec<PackageCheckDiagnostic>,
) -> Vec<PackageExportReport> {
    if ctx.manifest.exports.is_empty() {
        push_error(
            errors,
            "[exports]",
            "publishable packages require at least one stable export",
        );
        return Vec::new();
    }

    let mut exports = Vec::new();
    for (name, rel_path) in &ctx.manifest.exports {
        let field = format!("[exports].{name}");
        if let Err(message) = validate_package_alias(name) {
            push_error(errors, &field, message);
        }
        let Ok(path) = safe_package_relative_path(&ctx.dir, rel_path) else {
            push_error(
                errors,
                &field,
                "export path must stay inside the package directory",
            );
            continue;
        };
        if path.extension() != Some(OsStr::new("harn")) {
            push_error(errors, &field, "export path must point at a .harn file");
            continue;
        }
        let content = match fs::read_to_string(&path) {
            Ok(content) => content,
            Err(error) => {
                push_error(
                    errors,
                    &field,
                    format!("failed to read export {}: {error}", path.display()),
                );
                continue;
            }
        };
        if let Err(error) = parse_harn_source(&content) {
            push_error(errors, &field, format!("failed to parse export: {error}"));
        }
        let symbols = extract_api_symbols(&content);
        if symbols.is_empty() {
            push_warning(
                warnings,
                &field,
                "exported module has no public symbols to document",
            );
        }
        for symbol in &symbols {
            if symbol.docs.is_none() {
                push_warning(
                    warnings,
                    &field,
                    format!(
                        "public {} '{}' has no doc comment",
                        symbol.kind, symbol.name
                    ),
                );
            }
        }
        exports.push(PackageExportReport {
            name: name.clone(),
            path: rel_path.clone(),
            symbols,
        });
    }
    exports.sort_by(|left, right| left.name.cmp(&right.name));
    exports
}

pub(crate) fn validate_package_interface_exports(
    ctx: &ManifestContext,
    errors: &mut Vec<PackageCheckDiagnostic>,
    warnings: &mut Vec<PackageCheckDiagnostic>,
) -> (Vec<PackageToolExportReport>, Vec<PackageSkillExportReport>) {
    let Some(package) = ctx.manifest.package.as_ref() else {
        return (Vec::new(), Vec::new());
    };

    validate_permission_tokens(
        &package.permissions,
        "[package].permissions",
        errors,
        warnings,
    );
    validate_host_requirements(
        &package.host_requirements,
        "[package].host_requirements",
        errors,
    );

    let mut tools = Vec::new();
    for (index, tool) in package.tools.iter().enumerate() {
        let field = format!("[[package.tools]] #{}", index + 1);
        if let Err(message) = validate_package_alias(&tool.name) {
            push_error(errors, format!("{field}.name"), message.to_string());
        }
        validate_required_manifest_string(&tool.module, &format!("{field}.module"), errors);
        validate_required_manifest_string(&tool.symbol, &format!("{field}.symbol"), errors);
        validate_package_module_path(ctx, &tool.module, &format!("{field}.module"), errors);
        validate_permission_tokens(
            &tool.permissions,
            &format!("{field}.permissions"),
            errors,
            warnings,
        );
        validate_host_requirements(
            &tool.host_requirements,
            &format!("{field}.host_requirements"),
            errors,
        );
        validate_schema_value(
            tool.input_schema.as_ref(),
            &format!("{field}.input_schema"),
            errors,
        );
        validate_schema_value(
            tool.output_schema.as_ref(),
            &format!("{field}.output_schema"),
            errors,
        );
        validate_tool_annotations(&tool.annotations, &format!("{field}.annotations"), errors);
        if tool.annotations.is_empty() {
            push_warning(
                warnings,
                format!("{field}.annotations"),
                "tool export has no annotations; policy evaluation will treat it conservatively",
            );
        }
        tools.push(PackageToolExportReport {
            name: tool.name.clone(),
            module: tool.module.clone(),
            symbol: tool.symbol.clone(),
            permissions: merge_package_requirements(&package.permissions, &tool.permissions),
            host_requirements: merge_package_requirements(
                &package.host_requirements,
                &tool.host_requirements,
            ),
        });
    }
    tools.sort_by(|left, right| left.name.cmp(&right.name));

    let mut skills = Vec::new();
    for (index, skill) in package.skills.iter().enumerate() {
        let field = format!("[[package.skills]] #{}", index + 1);
        if let Err(message) = validate_package_alias(&skill.name) {
            push_error(errors, format!("{field}.name"), message.to_string());
        }
        validate_required_manifest_string(&skill.path, &format!("{field}.path"), errors);
        validate_package_skill_path(ctx, &skill.path, &format!("{field}.path"), errors);
        validate_permission_tokens(
            &skill.permissions,
            &format!("{field}.permissions"),
            errors,
            warnings,
        );
        validate_host_requirements(
            &skill.host_requirements,
            &format!("{field}.host_requirements"),
            errors,
        );
        skills.push(PackageSkillExportReport {
            name: skill.name.clone(),
            path: skill.path.clone(),
            permissions: merge_package_requirements(&package.permissions, &skill.permissions),
            host_requirements: merge_package_requirements(
                &package.host_requirements,
                &skill.host_requirements,
            ),
        });
    }
    skills.sort_by(|left, right| left.name.cmp(&right.name));

    (tools, skills)
}

pub(crate) fn merge_package_requirements(base: &[String], item: &[String]) -> Vec<String> {
    let mut merged = BTreeSet::new();
    merged.extend(
        base.iter()
            .filter_map(|value| normalized_requirement(value)),
    );
    merged.extend(
        item.iter()
            .filter_map(|value| normalized_requirement(value)),
    );
    merged.into_iter().collect()
}

fn normalized_requirement(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn validate_required_manifest_string(
    value: &str,
    field: &str,
    errors: &mut Vec<PackageCheckDiagnostic>,
) {
    if value.trim().is_empty() {
        push_error(errors, field, format!("missing required {field}"));
    }
}

fn validate_permission_tokens(
    permissions: &[String],
    field: &str,
    errors: &mut Vec<PackageCheckDiagnostic>,
    warnings: &mut Vec<PackageCheckDiagnostic>,
) {
    let mut seen = BTreeSet::new();
    for permission in permissions {
        let trimmed = permission.trim();
        if trimmed.is_empty() {
            push_error(errors, field, "permission entries cannot be empty");
            continue;
        }
        if trimmed.chars().any(char::is_whitespace) {
            push_error(
                errors,
                field,
                format!("permission {permission:?} cannot contain whitespace"),
            );
        }
        if !trimmed.contains(':') && !trimmed.contains('.') {
            push_warning(
                warnings,
                field,
                format!("permission {permission:?} should use a namespaced token"),
            );
        }
        if !seen.insert(trimmed.to_string()) {
            push_warning(
                warnings,
                field,
                format!("duplicate permission {permission:?}"),
            );
        }
    }
}

pub(crate) fn validate_host_requirements(
    requirements: &[String],
    field: &str,
    errors: &mut Vec<PackageCheckDiagnostic>,
) {
    let mut seen = BTreeSet::new();
    for requirement in requirements {
        let trimmed = requirement.trim();
        if trimmed.is_empty() {
            push_error(errors, field, "host requirement entries cannot be empty");
            continue;
        }
        let Some((capability, operation)) = trimmed.split_once('.') else {
            push_error(
                errors,
                field,
                format!("host requirement {requirement:?} must use capability.operation"),
            );
            continue;
        };
        if !valid_identifier(capability)
            || !(valid_identifier(operation) || operation == "*")
            || trimmed.matches('.').count() != 1
        {
            push_error(
                errors,
                field,
                format!("host requirement {requirement:?} must use valid capability.operation identifiers"),
            );
        }
        if !seen.insert(trimmed.to_string()) {
            push_error(
                errors,
                field,
                format!("duplicate host requirement {requirement:?}"),
            );
        }
    }
}

fn validate_package_module_path(
    ctx: &ManifestContext,
    rel_path: &str,
    field: &str,
    errors: &mut Vec<PackageCheckDiagnostic>,
) {
    let Ok(path) = safe_package_relative_path(&ctx.dir, rel_path) else {
        push_error(errors, field, "module path must stay inside the package");
        return;
    };
    if path.extension() != Some(OsStr::new("harn")) {
        push_error(errors, field, "module path must point at a .harn file");
        return;
    }
    match fs::read_to_string(&path) {
        Ok(content) => {
            if let Err(error) = parse_harn_source(&content) {
                push_error(errors, field, format!("failed to parse module: {error}"));
            }
        }
        Err(error) => push_error(
            errors,
            field,
            format!("failed to read module {}: {error}", path.display()),
        ),
    }
}

fn validate_package_skill_path(
    ctx: &ManifestContext,
    rel_path: &str,
    field: &str,
    errors: &mut Vec<PackageCheckDiagnostic>,
) {
    let Ok(path) = safe_package_relative_path(&ctx.dir, rel_path) else {
        push_error(errors, field, "skill path must stay inside the package");
        return;
    };
    let skill_file = if path.is_dir() {
        path.join("SKILL.md")
    } else {
        path.clone()
    };
    if skill_file.file_name() != Some(OsStr::new("SKILL.md")) {
        push_error(
            errors,
            field,
            "skill path must be a SKILL.md file or skill directory",
        );
        return;
    }
    match fs::read_to_string(&skill_file) {
        Ok(content) => {
            let (frontmatter, _) = harn_vm::skills::split_frontmatter(&content);
            if let Err(error) = harn_vm::skills::parse_frontmatter(frontmatter) {
                push_error(
                    errors,
                    field,
                    format!("invalid SKILL.md frontmatter: {error}"),
                );
            }
        }
        Err(error) => push_error(
            errors,
            field,
            format!("failed to read skill {}: {error}", skill_file.display()),
        ),
    }
}

fn validate_schema_value(
    value: Option<&toml::Value>,
    field: &str,
    errors: &mut Vec<PackageCheckDiagnostic>,
) {
    let Some(value) = value else {
        return;
    };
    let json = match toml_value_to_json(value) {
        Ok(json) => json,
        Err(error) => {
            push_error(errors, field, error);
            return;
        }
    };
    let Some(object) = json.as_object() else {
        push_error(errors, field, "schema must be a table/object");
        return;
    };
    if let Some(schema_type) = object.get("type") {
        if !schema_type.is_string() {
            push_error(errors, field, "schema `type` must be a string when present");
        }
    }
    if let Some(required) = object.get("required") {
        let valid = required
            .as_array()
            .is_some_and(|items| items.iter().all(|item| item.as_str().is_some()));
        if !valid {
            push_error(errors, field, "schema `required` must be a list of strings");
        }
    }
}

fn validate_tool_annotations(
    annotations: &BTreeMap<String, toml::Value>,
    field: &str,
    errors: &mut Vec<PackageCheckDiagnostic>,
) {
    if annotations.is_empty() {
        return;
    }
    let json = match toml_value_to_json(&toml::Value::Table(
        annotations
            .clone()
            .into_iter()
            .collect::<toml::map::Map<String, toml::Value>>(),
    )) {
        Ok(json) => json,
        Err(error) => {
            push_error(errors, field, error);
            return;
        }
    };
    if let Err(error) = serde_json::from_value::<harn_vm::tool_annotations::ToolAnnotations>(json) {
        push_error(
            errors,
            field,
            format!("annotations do not match ToolAnnotations: {error}"),
        );
    }
}

fn toml_value_to_json(value: &toml::Value) -> Result<serde_json::Value, String> {
    serde_json::to_value(value).map_err(|error| format!("failed to normalize TOML value: {error}"))
}

pub(crate) fn parse_harn_source(source: &str) -> Result<(), PackageError> {
    let mut lexer = harn_lexer::Lexer::new(source);
    let tokens = lexer.tokenize().map_err(|error| error.to_string())?;
    let mut parser = harn_parser::Parser::new(tokens);
    parser
        .parse()
        .map(|_| ())
        .map_err(|error| PackageError::Ops(error.to_string()))
}

pub(crate) fn safe_package_relative_path(
    root: &Path,
    rel_path: &str,
) -> Result<PathBuf, PackageError> {
    let rel = PathBuf::from(rel_path);
    if rel.is_absolute()
        || has_windows_rooted_or_drive_relative_prefix(rel_path)
        || rel.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir
                    | std::path::Component::Prefix(_)
                    | std::path::Component::RootDir
            )
        })
    {
        return Err(format!("path {rel_path:?} escapes package root").into());
    }
    Ok(root.join(rel))
}

fn has_windows_rooted_or_drive_relative_prefix(path: &str) -> bool {
    let normalized = path.replace('\\', "/");
    let bytes = normalized.as_bytes();
    normalized.starts_with('/')
        || (bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':')
}

pub(crate) fn extract_api_symbols(source: &str) -> Vec<PackageApiSymbol> {
    static DECL_RE: OnceLock<Regex> = OnceLock::new();
    let decl_re = DECL_RE.get_or_init(|| {
        Regex::new(r"^\s*pub\s+(fn|pipeline|tool|skill|struct|enum|type|interface)\s+([A-Za-z_][A-Za-z0-9_]*)\b(.*)$")
            .expect("valid declaration regex")
    });
    let mut docs: Vec<String> = Vec::new();
    let mut symbols = Vec::new();
    let mut in_block_doc = false;
    for line in source.lines() {
        let trimmed = line.trim();
        if in_block_doc {
            // Collect content between /** and */, stripping the conventional
            // ` * ` continuation marker so docs render the same regardless of
            // which form (`///` or `/** */`) authors picked.
            let (content, closes) = match trimmed.split_once("*/") {
                Some((before, _)) => (before, true),
                None => (trimmed, false),
            };
            let stripped = content
                .strip_prefix("* ")
                .or_else(|| content.strip_prefix('*'))
                .unwrap_or(content)
                .trim();
            if !stripped.is_empty() {
                docs.push(stripped.to_string());
            }
            if closes {
                in_block_doc = false;
            }
            continue;
        }
        if let Some(doc) = trimmed.strip_prefix("///") {
            docs.push(doc.trim().to_string());
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("/**") {
            // `/** … */` on a single line collapses to one doc line; the
            // multi-line opener `/**` (with no `*/` on the same line) flips
            // the block-doc flag so subsequent lines are absorbed above.
            if let Some((inner, _)) = rest.split_once("*/") {
                let stripped = inner.trim();
                if !stripped.is_empty() {
                    docs.push(stripped.to_string());
                }
            } else {
                let stripped = rest.trim();
                if !stripped.is_empty() {
                    docs.push(stripped.to_string());
                }
                in_block_doc = true;
            }
            continue;
        }
        if trimmed.is_empty() {
            continue;
        }
        if let Some(captures) = decl_re.captures(line) {
            let kind = captures.get(1).expect("kind").as_str().to_string();
            let name = captures.get(2).expect("name").as_str().to_string();
            let signature = trim_signature(line);
            let doc_text = (!docs.is_empty()).then(|| docs.join("\n"));
            symbols.push(PackageApiSymbol {
                kind,
                name,
                signature,
                docs: doc_text,
            });
        }
        docs.clear();
    }
    symbols
}

pub(crate) fn trim_signature(line: &str) -> String {
    let mut signature = line.trim().to_string();
    if let Some((before, _)) = signature.split_once('{') {
        signature = before.trim_end().to_string();
    }
    signature
}

pub(crate) fn supports_current_harn(range: &str) -> bool {
    let current = env!("CARGO_PKG_VERSION");
    let Some((major, minor)) = parse_major_minor(current) else {
        return true;
    };
    let range = range.trim();
    if range.is_empty() {
        return false;
    }
    if let Some(rest) = range.strip_prefix('^') {
        return parse_major_minor(rest).is_some_and(|(m, n)| m == major && n == minor);
    }
    if !range.contains([',', '<', '>', '=']) {
        return parse_major_minor(range).is_some_and(|(m, n)| m == major && n == minor);
    }

    let current_value = major * 1000 + minor;
    let mut lower_ok = true;
    let mut upper_ok = true;
    let mut saw_constraint = false;
    for raw in range.split(',') {
        let part = raw.trim();
        if part.is_empty() {
            continue;
        }
        saw_constraint = true;
        if let Some(rest) = part.strip_prefix(">=") {
            if let Some((m, n)) = parse_major_minor(rest.trim()) {
                lower_ok &= current_value >= m * 1000 + n;
            } else {
                return false;
            }
        } else if let Some(rest) = part.strip_prefix('>') {
            if let Some((m, n)) = parse_major_minor(rest.trim()) {
                lower_ok &= current_value > m * 1000 + n;
            } else {
                return false;
            }
        } else if let Some(rest) = part.strip_prefix("<=") {
            if let Some((m, n)) = parse_major_minor(rest.trim()) {
                upper_ok &= current_value <= m * 1000 + n;
            } else {
                return false;
            }
        } else if let Some(rest) = part.strip_prefix('<') {
            if let Some((m, n)) = parse_major_minor(rest.trim()) {
                upper_ok &= current_value < m * 1000 + n;
            } else {
                return false;
            }
        } else if let Some(rest) = part.strip_prefix('=') {
            if let Some((m, n)) = parse_major_minor(rest.trim()) {
                lower_ok &= current_value == m * 1000 + n;
                upper_ok &= current_value == m * 1000 + n;
            } else {
                return false;
            }
        } else {
            return false;
        }
    }
    saw_constraint && lower_ok && upper_ok
}

pub(crate) fn current_harn_range_example() -> String {
    let current = env!("CARGO_PKG_VERSION");
    let Some((major, minor)) = parse_major_minor(current) else {
        return ">=0.7,<0.8".to_string();
    };
    format!(">={major}.{minor},<{major}.{}", minor + 1)
}

pub(crate) fn current_harn_line_label() -> String {
    let current = env!("CARGO_PKG_VERSION");
    let Some((major, minor)) = parse_major_minor(current) else {
        return "0.7".to_string();
    };
    format!("{major}.{minor}")
}

pub(crate) fn parse_major_minor(raw: &str) -> Option<(u64, u64)> {
    let raw = raw.trim().trim_start_matches('v');
    let mut parts = raw.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.trim_end_matches('x').parse().ok()?;
    Some((major, minor))
}

pub(crate) fn collect_package_files(root: &Path) -> Result<Vec<String>, PackageError> {
    let mut files = Vec::new();
    collect_package_files_inner(root, root, &mut files)?;
    files.sort();
    Ok(files)
}

pub(crate) fn collect_package_files_inner(
    root: &Path,
    dir: &Path,
    out: &mut Vec<String>,
) -> Result<(), PackageError> {
    for entry in
        fs::read_dir(dir).map_err(|error| format!("failed to read {}: {error}", dir.display()))?
    {
        let entry =
            entry.map_err(|error| format!("failed to read {} entry: {error}", dir.display()))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|error| format!("failed to inspect {}: {error}", path.display()))?;
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            let rel = path
                .strip_prefix(root)
                .map_err(|error| format!("failed to relativize {}: {error}", path.display()))?;
            if should_skip_package_dir(rel) {
                continue;
            }
            collect_package_files_inner(root, &path, out)?;
        } else if file_type.is_file() {
            let rel = path
                .strip_prefix(root)
                .map_err(|error| format!("failed to relativize {}: {error}", path.display()))?
                .to_string_lossy()
                .replace('\\', "/");
            out.push(rel);
        }
    }
    Ok(())
}

pub(crate) fn should_skip_package_dir(rel: &Path) -> bool {
    if rel == Path::new("docs").join("dist") {
        return true;
    }
    rel.components().any(|component| {
        matches!(
            component.as_os_str().to_str(),
            Some(".git" | ".harn" | "target" | "node_modules")
        )
    })
}

pub(crate) fn default_artifact_dir(ctx: &ManifestContext, report: &PackageCheckReport) -> PathBuf {
    let name = report.name.as_deref().unwrap_or("package");
    let version = report.version.as_deref().unwrap_or("0.0.0");
    ctx.dir
        .join(".harn")
        .join("dist")
        .join(format!("{name}-{version}"))
}

pub(crate) fn fail_if_package_errors(report: &PackageCheckReport) -> Result<(), PackageError> {
    if report.errors.is_empty() {
        return Ok(());
    }
    Err(format!(
        "package check failed:\n{}",
        report
            .errors
            .iter()
            .map(|diagnostic| format!("- {}: {}", diagnostic.field, diagnostic.message))
            .collect::<Vec<_>>()
            .join("\n")
    )
    .into())
}

pub(crate) fn render_package_api_docs(report: &PackageCheckReport) -> String {
    let title = report.name.as_deref().unwrap_or("package");
    let mut out = format!("# API Reference: {title}\n\nGenerated by `harn package docs`.\n");
    if let Some(version) = report.version.as_deref() {
        out.push_str(&format!("\nVersion: `{version}`\n"));
    }
    for export in &report.exports {
        out.push_str(&format!(
            "\n## Export `{}`\n\n`{}`\n",
            export.name, export.path
        ));
        for symbol in &export.symbols {
            out.push_str(&format!("\n### {} `{}`\n\n", symbol.kind, symbol.name));
            if let Some(docs) = symbol.docs.as_deref() {
                out.push_str(docs);
                out.push_str("\n\n");
            }
            out.push_str("```harn\n");
            out.push_str(&symbol.signature);
            out.push_str("\n```\n");
        }
    }
    if !report.tools.is_empty() {
        out.push_str("\n## Tool Exports\n");
        for tool in &report.tools {
            out.push_str(&format!(
                "\n### `{}`\n\n- module: `{}`\n- symbol: `{}`\n",
                tool.name, tool.module, tool.symbol
            ));
            if !tool.permissions.is_empty() {
                out.push_str(&format!(
                    "- permissions: `{}`\n",
                    tool.permissions.join("`, `")
                ));
            }
            if !tool.host_requirements.is_empty() {
                out.push_str(&format!(
                    "- host requirements: `{}`\n",
                    tool.host_requirements.join("`, `")
                ));
            }
        }
    }
    if !report.skills.is_empty() {
        out.push_str("\n## Skill Exports\n");
        for skill in &report.skills {
            out.push_str(&format!("\n### `{}`\n\n`{}`\n", skill.name, skill.path));
        }
    }
    out
}

pub(crate) fn normalize_newlines(input: &str) -> String {
    input.replace("\r\n", "\n")
}

pub(crate) fn print_package_check_report(report: &PackageCheckReport) {
    println!(
        "Package {} {}",
        report.name.as_deref().unwrap_or("<unnamed>"),
        report.version.as_deref().unwrap_or("<unversioned>")
    );
    println!("manifest: {}", report.manifest_path);
    for export in &report.exports {
        println!(
            "export {} -> {} ({} public symbol(s))",
            export.name,
            export.path,
            export.symbols.len()
        );
    }
    for tool in &report.tools {
        println!("tool {} -> {}::{}", tool.name, tool.module, tool.symbol);
    }
    for skill in &report.skills {
        println!("skill {} -> {}", skill.name, skill.path);
    }
    if !report.warnings.is_empty() {
        println!("\nwarnings:");
        for warning in &report.warnings {
            println!("- {}: {}", warning.field, warning.message);
        }
    }
    if !report.errors.is_empty() {
        println!("\nerrors:");
        for error in &report.errors {
            println!("- {}: {}", error.field, error.message);
        }
    } else {
        println!("\npackage check passed");
    }
}

pub(crate) fn print_package_pack_report(report: &PackagePackReport) {
    if report.dry_run {
        println!("Package pack dry run succeeded.");
    } else {
        println!("Packed package artifact.");
    }
    println!("artifact: {}", report.artifact_dir);
    println!("files:");
    for file in &report.files {
        println!("- {file}");
    }
}

pub(crate) fn print_package_list_report(report: &PackageListReport) {
    println!("manifest: {}", report.manifest_path);
    println!("lock: {}", report.lock_path);
    if !report.lock_present {
        println!("lock status: missing");
        if report.dependency_count > 0 {
            println!(
                "run `harn install` to resolve {} dependency(s)",
                report.dependency_count
            );
        }
        return;
    }
    if report.packages.is_empty() {
        println!("No packages installed.");
        return;
    }
    println!("Packages ({}):", report.packages.len());
    for entry in &report.packages {
        let version = entry.package_version.as_deref().unwrap_or("unversioned");
        let status = if entry.materialized {
            "installed"
        } else {
            "missing"
        };
        println!(
            "  {}  {}  {}  integrity={}",
            entry.name, version, status, entry.integrity
        );
        if !entry.exports.modules.is_empty() {
            let modules: Vec<&str> = entry
                .exports
                .modules
                .iter()
                .map(|export| export.name.as_str())
                .collect();
            println!("    modules: {}", modules.join(", "));
        }
        if !entry.exports.tools.is_empty() {
            let tools: Vec<&str> = entry
                .exports
                .tools
                .iter()
                .map(|export| export.name.as_str())
                .collect();
            println!("    tools: {}", tools.join(", "));
        }
        if !entry.exports.skills.is_empty() {
            let skills: Vec<&str> = entry
                .exports
                .skills
                .iter()
                .map(|export| export.name.as_str())
                .collect();
            println!("    skills: {}", skills.join(", "));
        }
        if !entry.permissions.is_empty() {
            println!("    permissions: {}", entry.permissions.join(", "));
        }
        if !entry.host_requirements.is_empty() {
            println!(
                "    host requirements: {}",
                entry.host_requirements.join(", ")
            );
        }
    }
}

pub(crate) fn print_package_doctor_report(report: &PackageDoctorReport) {
    println!("Package doctor");
    println!("manifest: {}", report.manifest_path);
    println!("lock: {}", report.lock_path);
    if report.diagnostics.is_empty() {
        println!("ok: no package issues found");
        return;
    }
    for diagnostic in &report.diagnostics {
        println!(
            "{} [{}] {}",
            diagnostic.severity, diagnostic.code, diagnostic.message
        );
        if let Some(help) = diagnostic.help.as_deref() {
            println!("  help: {help}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::package::test_support::*;

    #[test]
    fn package_check_accepts_publishable_package() {
        let tmp = tempfile::tempdir().unwrap();
        write_publishable_package(tmp.path());

        let report = check_package_impl(Some(tmp.path())).unwrap();

        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert_eq!(report.name.as_deref(), Some("acme-lib"));
        assert_eq!(report.exports[0].symbols[0].name, "greet");
    }

    #[test]
    fn package_check_rejects_path_dependencies_and_bad_harn_range() {
        let tmp = tempfile::tempdir().unwrap();
        write_publishable_package(tmp.path());
        fs::write(
            tmp.path().join(MANIFEST),
            r#"[package]
    name = "acme-lib"
    version = "0.1.0"
    description = "Acme helpers"
    license = "MIT"
    repository = "https://github.com/acme/acme-lib"
    harn = ">=999.0,<999.1"
    docs_url = "docs/api.md"

    [exports]
    lib = "lib/main.harn"

    [dependencies]
    local = { path = "../local" }
    "#,
        )
        .unwrap();

        let report = check_package_impl(Some(tmp.path())).unwrap();
        let messages = report
            .errors
            .iter()
            .map(|diagnostic| diagnostic.message.as_str())
            .collect::<Vec<_>>()
            .join("\n");

        assert!(messages.contains("unsupported Harn version range"));
        assert!(messages.contains("path dependencies are not publishable"));
    }

    #[test]
    fn extract_api_symbols_recognizes_block_doc_comments() {
        // `/** … */` (the canonical HarnDoc form preferred by the linter)
        // and `///` lines must produce the same `docs` body so package
        // check, package docs, and the missing-doc warning agree on what
        // counts as documented.
        let single = extract_api_symbols("/** Block doc. */\npub fn one() {}\n");
        assert_eq!(single.len(), 1);
        assert_eq!(single[0].docs.as_deref(), Some("Block doc."));

        let multi =
            extract_api_symbols("/**\n * First line.\n * Second line.\n */\npub fn two() {}\n");
        assert_eq!(multi.len(), 1);
        assert_eq!(multi[0].docs.as_deref(), Some("First line.\nSecond line."));

        let triple = extract_api_symbols("/// Slash doc.\npub fn three() {}\n");
        assert_eq!(triple.len(), 1);
        assert_eq!(triple[0].docs.as_deref(), Some("Slash doc."));

        // A non-doc, non-empty intermediate line clears the pending
        // doc buffer so an unrelated comment three lines up does not
        // accidentally bind to the declaration.
        let detached = extract_api_symbols("/** Detached. */\nlet x = 1\npub fn four() {}\n");
        assert_eq!(detached.len(), 1);
        assert!(detached[0].docs.is_none());
    }

    #[test]
    fn package_docs_and_pack_use_exports() {
        let tmp = tempfile::tempdir().unwrap();
        write_publishable_package(tmp.path());

        let docs_path = generate_package_docs_impl(Some(tmp.path()), None, false).unwrap();
        let docs = fs::read_to_string(docs_path).unwrap();
        assert!(docs.contains("### fn `greet`"));
        assert!(docs.contains("Return a greeting."));

        let pack = pack_package_impl(Some(tmp.path()), None, true).unwrap();
        assert!(pack.files.contains(&"harn.toml".to_string()));
        assert!(pack.files.contains(&"lib/main.harn".to_string()));
    }

    #[test]
    fn package_pack_skips_generated_docs_dist() {
        let tmp = tempfile::tempdir().unwrap();
        write_publishable_package(tmp.path());
        fs::create_dir_all(tmp.path().join("docs/dist")).unwrap();
        fs::write(tmp.path().join("docs/dist/index.html"), "<html></html>\n").unwrap();

        let pack = pack_package_impl(Some(tmp.path()), None, true).unwrap();

        assert!(
            !pack.files.iter().any(|path| path.starts_with("docs/dist/")),
            "{:?}",
            pack.files
        );
    }

    #[cfg(unix)]
    #[test]
    fn package_pack_does_not_follow_symlinked_files() {
        let tmp = tempfile::tempdir().unwrap();
        write_publishable_package(tmp.path());
        let outside = tempfile::NamedTempFile::new().unwrap();
        fs::write(outside.path(), "secret\n").unwrap();
        std::os::unix::fs::symlink(outside.path(), tmp.path().join("secret.txt")).unwrap();

        let pack = pack_package_impl(Some(tmp.path()), None, true).unwrap();

        assert!(
            !pack.files.contains(&"secret.txt".to_string()),
            "{:?}",
            pack.files
        );
    }

    #[test]
    fn package_relative_paths_reject_windows_rooted_forms() {
        let tmp = tempfile::tempdir().unwrap();
        for rel_path in [
            "/repo/secret.harn",
            r"\repo\secret.harn",
            r"C:\repo\secret.harn",
            "C:secret.harn",
            r"\\server\share\secret.harn",
        ] {
            assert!(
                safe_package_relative_path(tmp.path(), rel_path).is_err(),
                "{rel_path:?} must not be accepted as package-relative"
            );
        }
    }

    #[test]
    fn package_check_validates_tool_and_skill_exports() {
        let tmp = tempfile::tempdir().unwrap();
        write_publishable_package(tmp.path());
        fs::create_dir_all(tmp.path().join("skills/review")).unwrap();
        fs::write(
            tmp.path().join("harn.toml"),
            format!(
                r#"[package]
name = "acme-lib"
version = "0.1.0"
description = "Acme helpers"
license = "MIT"
repository = "https://github.com/acme/acme-lib"
harn = "{}"
docs_url = "docs/api.md"
permissions = ["tool:read_only"]
host_requirements = ["workspace.read_text"]

[exports]
lib = "lib/main.harn"

[[package.tools]]
name = "read-note"
module = "lib/main.harn"
symbol = "tools"
permissions = ["tool:read_only"]

[package.tools.input_schema]
type = "object"
required = ["path"]

[package.tools.annotations]
kind = "read"
side_effect_level = "read_only"

[package.tools.annotations.arg_schema]
required = ["path"]

[[package.skills]]
name = "review"
path = "skills/review"
permissions = ["skill:prompt"]

[dependencies]
"#,
                current_harn_range_example()
            ),
        )
        .unwrap();
        fs::write(
            tmp.path().join("skills/review/SKILL.md"),
            "---\nname: review\nshort: Review changes\n---\n# Review\n",
        )
        .unwrap();

        let report = check_package_impl(Some(tmp.path())).unwrap();

        assert!(report.errors.is_empty(), "{:?}", report.errors);
        assert_eq!(report.tools[0].name, "read-note");
        assert_eq!(
            report.tools[0].host_requirements,
            vec!["workspace.read_text"]
        );
        assert_eq!(report.skills[0].name, "review");
    }

    #[test]
    fn package_check_rejects_invalid_tool_schema_and_host_requirement() {
        let tmp = tempfile::tempdir().unwrap();
        write_publishable_package(tmp.path());
        fs::write(
            tmp.path().join(MANIFEST),
            format!(
                r#"[package]
name = "acme-lib"
version = "0.1.0"
description = "Acme helpers"
license = "MIT"
repository = "https://github.com/acme/acme-lib"
harn = "{}"
docs_url = "docs/api.md"

[exports]
lib = "lib/main.harn"

[[package.tools]]
name = "broken"
module = "lib/main.harn"
symbol = "tools"
host_requirements = ["workspace"]

[package.tools.input_schema]
required = [1]

[dependencies]
"#,
                current_harn_range_example()
            ),
        )
        .unwrap();

        let report = check_package_impl(Some(tmp.path())).unwrap();
        let messages = report
            .errors
            .iter()
            .map(|diagnostic| diagnostic.message.as_str())
            .collect::<Vec<_>>()
            .join("\n");

        assert!(messages.contains("capability.operation"));
        assert!(messages.contains("schema `required` must be a list of strings"));
    }

    #[test]
    fn package_doctor_accepts_application_manifests_with_tool_exports() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join(MANIFEST),
            r#"[package]
name = "acme-app"

[[package.tools]]
name = "echo"
module = "tools.harn"
symbol = "tools"

[package.tools.input_schema]
type = "object"

[package.tools.annotations]
kind = "read"
side_effect_level = "read_only"
"#,
        )
        .unwrap();
        fs::write(tmp.path().join("tools.harn"), "pub fn tools() {}\n").unwrap();
        let workspace = TestWorkspace::new(tmp.path());

        let report = doctor_packages_in(workspace.env()).unwrap();

        assert!(report.ok, "{:?}", report.diagnostics);
        assert!(
            report
                .diagnostics
                .iter()
                .all(|diagnostic| diagnostic.code != "root-package-check"),
            "{:?}",
            report.diagnostics
        );
    }

    #[test]
    fn package_list_reports_locked_tool_and_skill_exports() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join(MANIFEST),
            r#"[package]
name = "consumer"
"#,
        )
        .unwrap();
        let lock = LockFile {
            packages: vec![LockEntry {
                name: "acme-tools".to_string(),
                source: "path+../acme-tools".to_string(),
                package_version: Some("0.1.0".to_string()),
                provenance: Some(
                    "https://github.com/acme/acme-tools/releases/tag/v0.1.0".to_string(),
                ),
                exports: PackageLockExports {
                    modules: vec![PackageLockExport {
                        name: "tools".to_string(),
                        path: Some("lib/tools.harn".to_string()),
                        symbol: None,
                    }],
                    tools: vec![PackageLockExport {
                        name: "echo".to_string(),
                        path: Some("lib/tools.harn".to_string()),
                        symbol: Some("tools".to_string()),
                    }],
                    skills: vec![PackageLockExport {
                        name: "review".to_string(),
                        path: Some("skills/review".to_string()),
                        symbol: None,
                    }],
                    personas: Vec::new(),
                },
                permissions: vec!["tool:read_only".to_string()],
                host_requirements: vec!["workspace.read_text".to_string()],
                ..LockEntry::default()
            }],
            ..LockFile::default()
        };
        let lock_body = toml::to_string_pretty(&lock).unwrap();
        fs::write(tmp.path().join(LOCK_FILE), lock_body).unwrap();
        let workspace = TestWorkspace::new(tmp.path());

        let report = list_packages_in(workspace.env()).unwrap();

        assert_eq!(report.packages.len(), 1);
        let package = &report.packages[0];
        assert_eq!(package.name, "acme-tools");
        assert_eq!(
            package.provenance.as_deref(),
            Some("https://github.com/acme/acme-tools/releases/tag/v0.1.0")
        );
        assert_eq!(package.exports.tools[0].name, "echo");
        assert_eq!(package.exports.skills[0].name, "review");
        assert_eq!(package.permissions, vec!["tool:read_only"]);
        assert_eq!(package.host_requirements, vec!["workspace.read_text"]);
    }
}
