//! Workspace resource routes: list/create/get/update workspace records
//! plus sandboxed UTF-8 file read/write under a registered workspace root.

use super::*;

pub(super) async fn list_workspaces(
    State(state): State<ApiState>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = authorize(&state, Method::GET, &uri, &headers, Bytes::new()).await {
        return response;
    }
    let inner = state.inner.lock().expect("api state poisoned");
    Json(list_response(inner.workspaces.values().cloned().collect())).into_response()
}

pub(super) async fn create_workspace(
    State(state): State<ApiState>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Err(response) = authorize(&state, Method::POST, &uri, &headers, body.clone()).await {
        return response;
    }
    let Ok(input) = parse_json_body(&body) else {
        return invalid_json_response();
    };
    let now = now_rfc3339();
    let id = format!("workspace_{}", Uuid::now_v7());
    let workspace = json!({
        "id": id,
        "object": "workspace",
        "created_at": now,
        "updated_at": now,
        "metadata": input.get("metadata").cloned().unwrap_or_else(|| json!({})),
        "name": input.get("name").and_then(Value::as_str).unwrap_or("Workspace"),
        "root": input.get("root").and_then(Value::as_str).unwrap_or("."),
        "default_branch_id": null,
        "host": "local",
        "repository": null,
        "tenant_id": null,
        "capabilities": ["sessions", "tasks", "events", "permissions", "workspace.files.read"],
        "connectors": [],
        "quota_id": null
    });
    state
        .inner
        .lock()
        .expect("api state poisoned")
        .workspaces
        .insert(
            workspace["id"].as_str().unwrap_or_default().to_string(),
            workspace.clone(),
        );
    (StatusCode::CREATED, Json(workspace)).into_response()
}

pub(super) async fn get_workspace(
    State(state): State<ApiState>,
    AxumPath(workspace_id): AxumPath<String>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = authorize(&state, Method::GET, &uri, &headers, Bytes::new()).await {
        return response;
    }
    let inner = state.inner.lock().expect("api state poisoned");
    match inner.workspaces.get(&workspace_id).cloned() {
        Some(workspace) => Json(workspace).into_response(),
        None => api_error(StatusCode::NOT_FOUND, "not_found", "workspace not found"),
    }
}

pub(super) async fn update_workspace(
    State(state): State<ApiState>,
    AxumPath(workspace_id): AxumPath<String>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Err(response) = authorize(&state, Method::PATCH, &uri, &headers, body.clone()).await {
        return response;
    }
    let Ok(input) = parse_json_body(&body) else {
        return invalid_json_response();
    };
    let mut inner = state.inner.lock().expect("api state poisoned");
    let Some(workspace) = inner.workspaces.get_mut(&workspace_id) else {
        return api_error(StatusCode::NOT_FOUND, "not_found", "workspace not found");
    };
    merge_mutable_fields(workspace, &input, &["name", "metadata", "capabilities"]);
    workspace["updated_at"] = json!(now_rfc3339());
    Json(workspace.clone()).into_response()
}

pub(super) async fn read_workspace_file(
    State(state): State<ApiState>,
    AxumPath(workspace_id): AxumPath<String>,
    Query(query): Query<ListQuery>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = authorize(&state, Method::GET, &uri, &headers, Bytes::new()).await {
        return response;
    }
    let root = match workspace_root(&state, &workspace_id) {
        Some(root) => root,
        None => return api_error(StatusCode::NOT_FOUND, "not_found", "workspace not found"),
    };
    let path = query.path.as_deref().unwrap_or(".");
    let Some(full_path) = safe_read_path(&root, path) else {
        return api_error(
            StatusCode::BAD_REQUEST,
            "invalid_path",
            "path must stay in workspace",
        );
    };
    let root_display = root.canonicalize().unwrap_or(root.clone());
    if full_path.is_dir() {
        let mut entries = Vec::new();
        match std::fs::read_dir(&full_path) {
            Ok(read_dir) => {
                for entry in read_dir.flatten().take(500) {
                    let Ok(metadata) = entry.metadata() else {
                        continue;
                    };
                    entries.push(json!({
                        "name": entry.file_name().to_string_lossy(),
                        "path": entry.path().strip_prefix(&root_display).unwrap_or(entry.path().as_path()).to_string_lossy(),
                        "kind": if metadata.is_dir() { "directory" } else { "file" },
                        "size": metadata.len()
                    }));
                }
                entries.sort_by(|a, b| a["path"].as_str().cmp(&b["path"].as_str()));
                return Json(json!({
                    "object": "file_listing",
                    "workspace_id": workspace_id,
                    "path": path,
                    "entries": entries
                }))
                .into_response();
            }
            Err(error) => {
                return api_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "file_error",
                    &error.to_string(),
                )
            }
        }
    }
    match std::fs::read_to_string(&full_path) {
        Ok(content) => Json(json!({
            "object": "file",
            "workspace_id": workspace_id,
            "path": path,
            "encoding": "utf-8",
            "content": content
        }))
        .into_response(),
        Err(error) => api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "file_error",
            &error.to_string(),
        ),
    }
}

pub(super) async fn write_workspace_file(
    State(state): State<ApiState>,
    AxumPath(workspace_id): AxumPath<String>,
    Query(query): Query<ListQuery>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Err(response) = authorize(&state, Method::PUT, &uri, &headers, body.clone()).await {
        return response;
    }
    let root = match workspace_root(&state, &workspace_id) {
        Some(root) => root,
        None => return api_error(StatusCode::NOT_FOUND, "not_found", "workspace not found"),
    };
    let Ok(input) = parse_json_body(&body) else {
        return invalid_json_response();
    };
    let path = input
        .get("path")
        .and_then(Value::as_str)
        .or(query.path.as_deref())
        .unwrap_or_default();
    let Some(full_path) = safe_write_path(&root, path) else {
        return api_error(
            StatusCode::BAD_REQUEST,
            "invalid_path",
            "path must stay in workspace",
        );
    };
    let Some(content) = input.get("content").and_then(Value::as_str) else {
        return api_error(
            StatusCode::BAD_REQUEST,
            "missing_content",
            "content is required",
        );
    };
    if let Some(parent) = full_path.parent() {
        if let Err(error) = std::fs::create_dir_all(parent) {
            return api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "file_error",
                &error.to_string(),
            );
        }
    }
    match std::fs::write(&full_path, content) {
        Ok(()) => Json(json!({
            "object": "file",
            "workspace_id": workspace_id,
            "path": path,
            "encoding": "utf-8",
            "bytes": content.len()
        }))
        .into_response(),
        Err(error) => api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "file_error",
            &error.to_string(),
        ),
    }
}

fn workspace_root(state: &ApiState, workspace_id: &str) -> Option<PathBuf> {
    let inner = state.inner.lock().expect("api state poisoned");
    workspace_root_locked(&inner, workspace_id)
}

fn clean_relative_components(relative: &str) -> Option<Vec<OsString>> {
    let relative = Path::new(relative);
    if relative.is_absolute()
        || relative
            .components()
            .any(|component| matches!(component, Component::ParentDir | Component::Prefix(_)))
    {
        return None;
    }
    Some(
        relative
            .components()
            .filter_map(|component| match component {
                Component::Normal(part) => Some(part.to_os_string()),
                Component::CurDir => None,
                _ => None,
            })
            .collect(),
    )
}

fn safe_read_path(root: &Path, relative: &str) -> Option<PathBuf> {
    let root = root.canonicalize().ok()?;
    let candidate = clean_relative_components(relative)?.into_iter().fold(
        root.clone(),
        |mut path, component| {
            path.push(component);
            path
        },
    );
    let resolved = candidate.canonicalize().ok()?;
    resolved.starts_with(&root).then_some(resolved)
}

fn safe_write_path(root: &Path, relative: &str) -> Option<PathBuf> {
    let root = root.canonicalize().ok()?;
    let components = clean_relative_components(relative)?;
    let mut path = root.clone();
    for (index, component) in components.iter().enumerate() {
        let next = path.join(component);
        if next.exists() {
            let resolved = next.canonicalize().ok()?;
            if !resolved.starts_with(&root) {
                return None;
            }
            path = resolved;
        } else {
            path = next;
            for remaining in components.iter().skip(index + 1) {
                path.push(remaining);
            }
            return Some(path);
        }
    }
    Some(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn workspace_file_paths_reject_parent_and_symlink_escape() {
        let dir = tempfile::tempdir().expect("tempdir");
        let outside = tempfile::tempdir().expect("outside tempdir");
        std::os::unix::fs::symlink(outside.path(), dir.path().join("outside"))
            .expect("create symlink");

        assert!(safe_read_path(dir.path(), "../secret.txt").is_none());
        assert!(safe_write_path(dir.path(), "../secret.txt").is_none());
        assert!(safe_read_path(dir.path(), "outside").is_none());
        assert!(safe_write_path(dir.path(), "outside/secret.txt").is_none());
    }
}
