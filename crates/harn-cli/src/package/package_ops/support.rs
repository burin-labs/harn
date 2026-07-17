use super::*;

pub(crate) fn load_manifest_context_for_anchor(
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
