use super::*;

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
        let rel = path
            .strip_prefix(root)
            .map_err(|error| format!("failed to relativize {}: {error}", path.display()))?;
        if should_skip_package_entry(rel, file_type.is_dir()) {
            continue;
        }
        if file_type.is_dir() {
            collect_package_files_inner(root, &path, out)?;
        } else if file_type.is_file() {
            let rel = rel.to_string_lossy().replace('\\', "/");
            out.push(rel);
        }
    }
    Ok(())
}

pub(crate) fn should_skip_package_entry(rel: &Path, is_dir: bool) -> bool {
    if is_dir && rel == Path::new("docs").join("dist") {
        return true;
    }
    let Some(name) = rel.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    name == ".git"
        || name == ".harn"
        || (is_dir && (name.starts_with(".harn-") || matches!(name, "target" | "node_modules")))
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
            push_api_symbol(&mut out, symbol);
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

/// Render one public symbol as a Markdown section: a `### <kind> `<name>``
/// heading, its HarnDoc description (which already carries any `@effects` /
/// `@errors` / parameter lines the author wrote), and a fenced `harn`
/// signature block. Shared by `harn package docs` and the top-level
/// `harn doc` command so both emit byte-identical symbol entries.
pub(crate) fn push_api_symbol(out: &mut String, symbol: &PackageApiSymbol) {
    out.push_str(&format!("\n### {} `{}`\n\n", symbol.kind, symbol.name));
    if let Some(docs) = symbol.docs.as_deref() {
        out.push_str(docs);
        out.push_str("\n\n");
    }
    out.push_str("```harn\n");
    out.push_str(&symbol.signature);
    out.push_str("\n```\n");
}

pub(crate) fn normalize_newlines(input: &str) -> String {
    input.replace("\r\n", "\n")
}
