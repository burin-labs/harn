use super::*;

pub(crate) fn harn_roots_list_response(id: serde_json::Value) -> serde_json::Value {
    crate::jsonrpc::response(
        id,
        serde_json::json!({
            "roots": current_mcp_roots()
                .iter()
                .map(McpRoot::protocol_json)
                .collect::<Vec<_>>()
        }),
    )
}

pub(crate) fn current_mcp_roots() -> Vec<McpRoot> {
    compact_root_paths(current_mcp_root_candidates())
        .into_iter()
        .filter_map(|path| {
            let uri = url::Url::from_file_path(&path).ok()?.to_string();
            Some(McpRoot {
                name: root_display_name(&path),
                path: path.to_string_lossy().into_owned(),
                uri,
            })
        })
        .collect()
}

pub(crate) fn current_mcp_root_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    let explicit_project_root = crate::stdlib::process::project_root_path();
    if let Some(context) = crate::stdlib::process::current_execution_context() {
        if let Some(path) = non_empty_path(context.worktree_path.as_deref()) {
            candidates.push(path);
        }
        if let Some(project_root) = explicit_project_root {
            candidates.push(project_root);
        }
        if let Some(cwd) = non_empty_path(context.cwd.as_deref()) {
            push_project_root_or_path(&mut candidates, cwd);
        }
        if let Some(source_dir) = non_empty_path(context.source_dir.as_deref()) {
            push_project_root_or_path(&mut candidates, source_dir);
        }
    } else {
        push_project_root_or_path(
            &mut candidates,
            crate::stdlib::process::execution_root_path(),
        );
        push_project_root_or_path(&mut candidates, crate::stdlib::process::source_root_path());
    }

    if candidates.is_empty() {
        candidates.push(crate::stdlib::process::execution_root_path());
    }
    candidates
}

pub(crate) fn non_empty_path(raw: Option<&str>) -> Option<PathBuf> {
    raw.filter(|path| !path.trim().is_empty())
        .map(PathBuf::from)
}

pub(crate) fn push_project_root_or_path(candidates: &mut Vec<PathBuf>, path: PathBuf) {
    let normalized = crate::stdlib::process::normalize_context_path(&path);
    match crate::stdlib::process::find_project_root(&normalized) {
        Some(root) => candidates.push(root),
        None => candidates.push(normalized),
    }
}

pub(crate) fn compact_root_paths(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut normalized = paths
        .into_iter()
        .map(normalize_root_path)
        .collect::<Vec<_>>();
    normalized.sort_by_key(|path| {
        (
            path.components().count(),
            path.to_string_lossy().to_string(),
        )
    });

    let mut roots: Vec<PathBuf> = Vec::new();
    for path in normalized {
        if roots
            .iter()
            .any(|existing| path == *existing || path.starts_with(existing))
        {
            continue;
        }
        roots.push(path);
    }
    roots
}

pub(crate) fn normalize_root_path(path: PathBuf) -> PathBuf {
    let absolute = crate::stdlib::process::normalize_context_path(&path);
    std::fs::canonicalize(&absolute).unwrap_or(absolute)
}

pub(crate) fn root_display_name(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| path.display().to_string())
}
