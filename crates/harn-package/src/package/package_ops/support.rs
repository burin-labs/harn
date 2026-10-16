use super::*;

pub fn load_manifest_context_for_anchor(
    anchor: Option<&Path>,
) -> Result<ManifestContext, PackageError> {
    let anchor = anchor
        .map(Path::to_path_buf)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let manifest_path = if anchor.is_dir() {
        anchor.join(MANIFEST)
    } else if anchor.file_name() == Some(OsStr::new(MANIFEST)) {
        anchor
    } else {
        let dir = find_nearest_manifest_dir(&anchor)
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

pub(crate) fn supports_current_harn(range: &str) -> bool {
    let Ok(current) = semver::Version::parse(env!("CARGO_PKG_VERSION")) else {
        return false;
    };
    supports_harn_version(range, &current)
}

fn supports_harn_version(range: &str, current: &semver::Version) -> bool {
    let Ok(mut requirement) = semver::VersionReq::parse(range) else {
        return false;
    };
    if !current.pre.is_empty() {
        // Partial comparators constrain a release line, rather than a specific
        // stable patch. Let their equality case include this development suffix.
        // Fully specified comparators retain their original prerelease ordering.
        for comparator in &mut requirement.comparators {
            if comparator.patch.is_none() {
                comparator.pre = current.pre.clone();
            }
        }
        // Harn development runtimes remain usable within the declared bounds.
        // This tautological lower bound opts the current prerelease into semver
        // matching without erasing its precedence or relaxing any original bound.
        requirement.comparators.push(semver::Comparator {
            op: semver::Op::GreaterEq,
            major: current.major,
            minor: Some(current.minor),
            patch: Some(current.patch),
            pre: current.pre.clone(),
        });
    }
    requirement.matches(current)
}

pub(crate) fn validate_runtime_compatibility(
    manifest: &Manifest,
    manifest_path: &Path,
) -> Result<(), PackageError> {
    if let Some(range) = manifest
        .package
        .as_ref()
        .and_then(|package| package.harn.as_deref())
    {
        if !supports_current_harn(range) {
            return Err(PackageError::Validation(format!(
                "{}: unsupported Harn version range '{range}' in [package].harn; current version is {}",
                manifest_path.display(),
                env!("CARGO_PKG_VERSION"),
            )));
        }
    }
    Ok(())
}

pub fn current_harn_range_example() -> String {
    let current = env!("CARGO_PKG_VERSION");
    let Some((major, minor)) = parse_major_minor(current) else {
        return ">=0.7,<0.8".to_string();
    };
    format!(">={major}.{minor},<{major}.{}", minor + 1)
}

pub(crate) fn parse_major_minor(raw: &str) -> Option<(u64, u64)> {
    let raw = raw.trim().trim_start_matches('v');
    let mut parts = raw.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.trim_end_matches('x').parse().ok()?;
    Some((major, minor))
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
    for persona in &report.personas {
        println!("persona {} -> {}", persona.name, persona.entry_workflow);
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
        if !entry.exports.personas.is_empty() {
            let personas = entry
                .exports
                .personas
                .iter()
                .map(|name| format!("{}/{name}", entry.name))
                .collect::<Vec<_>>();
            println!("    personas: {}", personas.join(", "));
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
mod runtime_version_tests {
    use super::*;

    #[test]
    fn development_runtime_obeys_full_version_bounds() {
        let current = semver::Version::parse("0.10.167-dev").unwrap();
        for range in [">=0.10,<0.11", "=0.10", "<=0.10", "~0.10", ">=0,<1"] {
            assert!(supports_harn_version(range, &current), "{range}");
        }
        for range in [">0.10", "<0.10", ">=0.11", "=0.11", "~0.11", "^0.11"] {
            assert!(!supports_harn_version(range, &current), "{range}");
        }
        assert!(supports_harn_version(">=0.10.166,<0.11", &current));
        assert!(supports_harn_version("^0.10.166", &current));
        assert!(supports_harn_version(">=0.10.167-dev,<0.11", &current));
        assert!(!supports_harn_version(">=0.10.167,<0.11", &current));
        assert!(!supports_harn_version(">=0.10.168-dev,<0.11", &current));
        assert!(!supports_harn_version("<0.10.166", &current));
    }
}
