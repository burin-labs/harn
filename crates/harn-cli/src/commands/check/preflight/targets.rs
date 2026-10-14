//! Resolving and explaining the file targets that preflight scans name.

use super::*;

pub(in super::super) fn resolve_source_relative(current_file: &Path, target: &str) -> PathBuf {
    let candidate = PathBuf::from(target);
    if candidate.is_absolute() {
        candidate
    } else {
        current_file
            .parent()
            .unwrap_or(Path::new("."))
            .join(candidate)
    }
}

pub(in super::super) fn resolve_preflight_target(
    current_file: &Path,
    target: &str,
    config: &CheckConfig,
) -> Vec<PathBuf> {
    // `@/...` and `@<alias>/...` always anchor at the project root of
    // the calling file, never at the bundle root or the source dir, so
    // the preflight scan must use the same resolver as the runtime
    // (issue #742). When resolution fails (no harn.toml ancestor /
    // unknown alias) we still surface a single candidate path so the
    // caller's diagnostic explains why the target was unreachable.
    if let Some(asset_ref) = harn_modules::asset_paths::parse(target) {
        let anchor = current_file.parent().unwrap_or(Path::new("."));
        let candidates = match harn_modules::asset_paths::resolve(&asset_ref, anchor) {
            Ok(path) => vec![path],
            Err(_) => vec![PathBuf::from(target)],
        };
        super::super::result_cache::record_resolve_target(current_file, target, &candidates);
        return candidates;
    }
    let mut candidates = vec![resolve_source_relative(current_file, target)];
    if let Some(bundle_root) = config.bundle_root.as_deref() {
        let bundle_base = PathBuf::from(bundle_root);
        candidates.push(if PathBuf::from(target).is_absolute() {
            PathBuf::from(target)
        } else {
            bundle_base.join(target)
        });
    }
    candidates.dedup();
    super::super::result_cache::record_resolve_target(current_file, target, &candidates);
    candidates
}

pub(super) fn render_candidate_paths(candidates: &[PathBuf]) -> String {
    candidates
        .iter()
        .map(|path| crate::format::slash_path(path))
        .collect::<Vec<_>>()
        .join(" or ")
}

pub(super) fn scan_stdlib_prompt_target(
    template_path: &str,
    target_label: &str,
    span: harn_lexer::Span,
    file_path: &Path,
    source: &str,
    diagnostics: &mut Vec<PreflightDiagnostic>,
) -> bool {
    if harn_modules::asset_paths::stdlib_prompt_asset_path(template_path).is_none() {
        return false;
    }
    let Some(body) = harn_vm::stdlib_modules::get_stdlib_prompt_asset(template_path) else {
        diagnostics.push(PreflightDiagnostic {
            code: Code::PromptTargetMissing,
            path: file_path.display().to_string(),
            source: source.to_string(),
            span,
            message: format!(
                "preflight: {target_label} target '{template_path}' is not an embedded stdlib prompt asset"
            ),
            help: Some(
                "verify the `std/...harn.prompt` asset path against the stdlib prompt asset catalog"
                    .to_string(),
            ),
            tags: None,
        });
        return true;
    };
    if let Err(err) = harn_vm::stdlib::template::validate_template_syntax(body) {
        diagnostics.push(PreflightDiagnostic {
            code: Code::PromptTemplateParse,
            path: file_path.display().to_string(),
            source: source.to_string(),
            span,
            message: format!("preflight: template '{template_path}' has a syntax error: {err}"),
            help: Some("fix the embedded stdlib prompt asset template syntax".to_string()),
            tags: None,
        });
    }
    true
}

pub(in super::super) fn literal_string(node: &SNode) -> Option<String> {
    match &node.node {
        Node::StringLiteral(value) => Some(value.clone()),
        _ => None,
    }
}

/// `render(...)` and `render_prompt(...)` accept either form of static
/// string literal as their first argument. Both are statically
/// verifiable; only `InterpolatedString` and arbitrary expressions are
/// dynamic and must be skipped.
pub(super) fn literal_template_path(node: &SNode) -> Option<String> {
    match &node.node {
        Node::StringLiteral(value) | Node::RawStringLiteral(value) => Some(value.clone()),
        _ => None,
    }
}

/// Build the help text for a missing `render(...)` / `render_prompt(...)`
/// target. When the basename can be located somewhere else under the
/// caller's project root (and the search produces a unique hit), prepend
/// a "did you mean ...?" suggestion so the most common typo — file
/// misfiled in a sibling directory — is one keystroke from a fix. Falls
/// back to the generic guidance when the search is ambiguous or finds
/// nothing.
pub(super) fn render_target_miss_help(file_path: &Path, template_path: &str) -> String {
    const GENERIC: &str = "keep template paths relative to the pipeline source file, or set [check].bundle_root / --bundle-root for bundled layouts. Use `@/...` for project-root paths";
    let Some(basename) = Path::new(template_path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
    else {
        return GENERIC.to_string();
    };
    let anchor = file_path.parent().unwrap_or(Path::new("."));
    let project_root = harn_modules::asset_paths::find_project_root(anchor)
        .unwrap_or_else(|| anchor.to_path_buf());
    let Some(near) = find_unique_basename(&project_root, &basename) else {
        return GENERIC.to_string();
    };
    let caller_dir = file_path.parent();
    if near.parent() == caller_dir {
        // The runtime would have found the file at the caller's dir
        // anyway; avoid suggesting a redundant "did you mean ...?".
        return GENERIC.to_string();
    }
    let display = near
        .strip_prefix(&project_root)
        .map(crate::format::slash_path)
        .unwrap_or_else(|_| crate::format::slash_path(&near));
    format!(
        "did you mean '{display}'? (found at {}). Otherwise: {GENERIC}",
        crate::format::slash_path(&near)
    )
}

/// Returns the unique location of `basename` under `root`, or `None`
/// when the search finds zero or multiple matches. Skips standard
/// build/dependency directories so a misfiled prompt is not lost in
/// vendor noise.
pub(in super::super) fn find_unique_basename(root: &Path, basename: &str) -> Option<PathBuf> {
    let mut matches: Vec<PathBuf> = Vec::with_capacity(2);
    walk_for_basename(root, basename, 0, 8, &mut matches);
    let result = (matches.len() == 1).then(|| matches.into_iter().next().expect("len == 1"));
    super::super::result_cache::record_walk_unique(root, basename, result.as_deref());
    result
}

pub(super) fn walk_for_basename(
    dir: &Path,
    basename: &str,
    depth: usize,
    max_depth: usize,
    out: &mut Vec<PathBuf>,
) {
    if depth > max_depth || out.len() > 1 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let file_name = entry.file_name();
        let name_str = file_name.to_string_lossy();
        if name_str.starts_with('.')
            || matches!(
                name_str.as_ref(),
                "target" | "node_modules" | "dist" | "build" | "out"
            )
        {
            continue;
        }
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let path = entry.path();
        if file_type.is_file() {
            if name_str == basename {
                out.push(path);
                if out.len() > 1 {
                    return;
                }
            }
        } else if file_type.is_dir() {
            walk_for_basename(&path, basename, depth + 1, max_depth, out);
            if out.len() > 1 {
                return;
            }
        }
    }
}
