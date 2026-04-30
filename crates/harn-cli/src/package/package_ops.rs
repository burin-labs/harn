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

pub(crate) fn check_package_impl(anchor: Option<&Path>) -> Result<PackageCheckReport, String> {
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
                "unsupported Harn version range '{range}'; include the current 0.7 line, for example >=0.7,<0.8"
            ),
        ),
        None => push_error(
            &mut errors,
            "[package].harn",
            "missing Harn compatibility metadata; add harn = \">=0.7,<0.8\"",
        ),
    }

    validate_dependencies_for_publish(&ctx, &mut errors, &mut warnings);
    let exports = validate_exports_for_publish(&ctx, &mut errors, &mut warnings);

    Ok(PackageCheckReport {
        package_dir: ctx.dir.display().to_string(),
        manifest_path: manifest_path.display().to_string(),
        name,
        version,
        errors,
        warnings,
        exports,
    })
}

pub(crate) fn pack_package_impl(
    anchor: Option<&Path>,
    output: Option<&Path>,
    dry_run: bool,
) -> Result<PackagePackReport, String> {
    let report = check_package_impl(anchor)?;
    fail_if_package_errors(&report)?;
    let ctx = load_manifest_context_for_anchor(anchor)?;
    let files = collect_package_files(&ctx.dir)?;
    let artifact_dir = output
        .map(Path::to_path_buf)
        .unwrap_or_else(|| default_artifact_dir(&ctx, &report));

    if !dry_run {
        if artifact_dir.exists() {
            return Err(format!(
                "artifact output {} already exists",
                artifact_dir.display()
            ));
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
) -> Result<PathBuf, String> {
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
            ));
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
) -> Result<PackagePublishReport, String> {
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
) -> Result<ManifestContext, String> {
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

pub(crate) fn parse_harn_source(source: &str) -> Result<(), String> {
    let mut lexer = harn_lexer::Lexer::new(source);
    let tokens = lexer.tokenize().map_err(|error| error.to_string())?;
    let mut parser = harn_parser::Parser::new(tokens);
    parser
        .parse()
        .map(|_| ())
        .map_err(|error| error.to_string())
}

pub(crate) fn safe_package_relative_path(root: &Path, rel_path: &str) -> Result<PathBuf, String> {
    let rel = PathBuf::from(rel_path);
    if rel.is_absolute()
        || rel
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(format!("path {rel_path:?} escapes package root"));
    }
    Ok(root.join(rel))
}

pub(crate) fn extract_api_symbols(source: &str) -> Vec<PackageApiSymbol> {
    static DECL_RE: OnceLock<Regex> = OnceLock::new();
    let decl_re = DECL_RE.get_or_init(|| {
        Regex::new(r"^\s*pub\s+(fn|pipeline|tool|skill|struct|enum|type|interface)\s+([A-Za-z_][A-Za-z0-9_]*)\b(.*)$")
            .expect("valid declaration regex")
    });
    let mut docs: Vec<String> = Vec::new();
    let mut symbols = Vec::new();
    for line in source.lines() {
        let trimmed = line.trim();
        if let Some(doc) = trimmed.strip_prefix("///") {
            docs.push(doc.trim().to_string());
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

pub(crate) fn parse_major_minor(raw: &str) -> Option<(u64, u64)> {
    let raw = raw.trim().trim_start_matches('v');
    let mut parts = raw.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.trim_end_matches('x').parse().ok()?;
    Some((major, minor))
}

pub(crate) fn collect_package_files(root: &Path) -> Result<Vec<String>, String> {
    let mut files = Vec::new();
    collect_package_files_inner(root, root, &mut files)?;
    files.sort();
    Ok(files)
}

pub(crate) fn collect_package_files_inner(
    root: &Path,
    dir: &Path,
    out: &mut Vec<String>,
) -> Result<(), String> {
    for entry in
        fs::read_dir(dir).map_err(|error| format!("failed to read {}: {error}", dir.display()))?
    {
        let entry =
            entry.map_err(|error| format!("failed to read {} entry: {error}", dir.display()))?;
        let path = entry.path();
        let name = entry.file_name();
        if path.is_dir() {
            if should_skip_package_dir(&name) {
                continue;
            }
            collect_package_files_inner(root, &path, out)?;
        } else if path.is_file() {
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

pub(crate) fn should_skip_package_dir(name: &OsStr) -> bool {
    matches!(
        name.to_str(),
        Some(".git" | ".harn" | "target" | "node_modules" | "docs/dist")
    )
}

pub(crate) fn default_artifact_dir(ctx: &ManifestContext, report: &PackageCheckReport) -> PathBuf {
    let name = report.name.as_deref().unwrap_or("package");
    let version = report.version.as_deref().unwrap_or("0.0.0");
    ctx.dir
        .join(".harn")
        .join("dist")
        .join(format!("{name}-{version}"))
}

pub(crate) fn fail_if_package_errors(report: &PackageCheckReport) -> Result<(), String> {
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
    ))
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
    harn = ">=0.8,<0.9"
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
}
