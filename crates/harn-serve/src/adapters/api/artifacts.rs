//! Artifact resource routes: list/register/get metadata plus conservative
//! local file content reads for `file://` artifacts under registered workspace
//! roots.

use axum::http::header::CONTENT_TYPE;
use axum::http::HeaderValue;

use super::*;

pub(super) fn register_harn_session_artifact(
    state: &ApiState,
    session_id: Option<String>,
    task_id: Option<String>,
    params: &Value,
) {
    let Some(meta) = params.pointer("/update/_meta/harn") else {
        return;
    };
    let Some(artifact_id) = meta.get("artifactId").and_then(Value::as_str) else {
        return;
    };
    let Some(harn_kind) = meta.get("kind").and_then(Value::as_str) else {
        return;
    };
    let spec = meta.get("spec").cloned().unwrap_or_else(|| json!({}));
    let mime_type = meta
        .get("mimeType")
        .and_then(Value::as_str)
        .or_else(|| spec.get("mime_type").and_then(Value::as_str))
        .unwrap_or("application/octet-stream");
    let mime_type = if is_valid_mime_type(mime_type) {
        mime_type
    } else {
        "application/octet-stream"
    };
    let now = now_rfc3339();
    let mut metadata = match meta.get("metadata").and_then(Value::as_object) {
        Some(metadata) => metadata.clone(),
        None => serde_json::Map::new(),
    };
    metadata.insert("harn_kind".to_string(), json!(harn_kind));
    metadata.insert("harn_artifact_id".to_string(), json!(artifact_id));
    if let Some(description) = spec.get("description").cloned() {
        metadata.insert("description".to_string(), description);
    }
    if let Some(path) = spec.get("path").cloned() {
        metadata.insert("path".to_string(), path);
    }
    let workspace_id = {
        let inner = state.inner.lock().expect("api state poisoned");
        session_id
            .as_deref()
            .and_then(|session_id| inner.sessions.get(session_id))
            .and_then(|session| session.get("workspace_id").and_then(Value::as_str))
            .map(str::to_string)
            .unwrap_or_else(|| inner.root_workspace_id.clone())
    };
    let mut artifact = json!({
        "id": artifact_id,
        "object": "artifact",
        "created_at": now,
        "updated_at": now,
        "metadata": metadata,
        "kind": harn_artifact_kind(harn_kind),
        "mime_type": mime_type,
        "uri": spec.get("uri").cloned().unwrap_or(Value::Null),
        "visibility": "public",
        "sha256": spec
            .get("sha256")
            .and_then(Value::as_str)
            .and_then(normalize_sha256),
        "workspace_id": workspace_id,
        "session_id": session_id,
        "task_id": task_id,
        "receipt_id": null
    });
    if let Some(title) = meta.get("title").cloned() {
        artifact["title"] = title;
    }
    if let Some(name) = spec.get("name").cloned() {
        artifact["name"] = name;
    }
    if let Some(size_bytes) = spec
        .get("size_bytes")
        .cloned()
        .or_else(|| meta.get("sizeBytes").cloned())
    {
        artifact["size_bytes"] = size_bytes;
    }

    let (event, artifact) = {
        let mut inner = state.inner.lock().expect("api state poisoned");
        let event = if let Some(existing) = inner.artifacts.get(artifact_id) {
            if let Some(created_at) = existing.get("created_at").cloned() {
                artifact["created_at"] = created_at;
            }
            "artifact.updated"
        } else {
            "artifact.created"
        };
        inner
            .artifacts
            .insert(artifact_id.to_string(), artifact.clone());
        (event, artifact)
    };
    state.append_event_from_resource(
        artifact["session_id"].as_str().map(str::to_string),
        artifact["task_id"].as_str().map(str::to_string),
        event,
        artifact,
    );
}

pub(super) async fn list_artifacts(
    State(state): State<ApiState>,
    Query(query): Query<ListQuery>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = authorize(&state, Method::GET, &uri, &headers, Bytes::new()).await {
        return response;
    }
    let inner = state.inner.lock().expect("api state poisoned");
    let artifacts = inner
        .artifacts
        .values()
        .filter(|artifact| artifact_matches_query(artifact, &query))
        .cloned()
        .collect();
    Json(list_response(limit_values(artifacts, query.limit))).into_response()
}

pub(super) async fn register_artifact(
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
    let artifact = match artifact_from_register_input(&state, &input) {
        Ok(artifact) => artifact,
        Err(error) => return error.into_response(),
    };
    {
        let mut inner = state.inner.lock().expect("api state poisoned");
        inner.artifacts.insert(
            artifact["id"].as_str().unwrap_or_default().to_string(),
            artifact.clone(),
        );
    }
    state.append_event_from_resource(
        artifact["session_id"].as_str().map(str::to_string),
        artifact["task_id"].as_str().map(str::to_string),
        "artifact.created",
        artifact.clone(),
    );
    (StatusCode::CREATED, Json(artifact)).into_response()
}

pub(super) async fn get_artifact(
    State(state): State<ApiState>,
    AxumPath(artifact_id): AxumPath<String>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = authorize(&state, Method::GET, &uri, &headers, Bytes::new()).await {
        return response;
    }
    let inner = state.inner.lock().expect("api state poisoned");
    match inner.artifacts.get(&artifact_id).cloned() {
        Some(artifact) => Json(artifact).into_response(),
        None => api_error(StatusCode::NOT_FOUND, "not_found", "artifact not found"),
    }
}

pub(super) async fn download_artifact_content(
    State(state): State<ApiState>,
    AxumPath(artifact_id): AxumPath<String>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = authorize(&state, Method::GET, &uri, &headers, Bytes::new()).await {
        return response;
    }
    let artifact = {
        let inner = state.inner.lock().expect("api state poisoned");
        match inner.artifacts.get(&artifact_id).cloned() {
            Some(artifact) => artifact,
            None => return api_error(StatusCode::NOT_FOUND, "not_found", "artifact not found"),
        }
    };
    let Some(path) = artifact_file_path(&state, &artifact) else {
        return api_error(
            StatusCode::BAD_REQUEST,
            "content_unavailable",
            "artifact content is not available through this API",
        );
    };
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) => {
            return api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "artifact_read_failed",
                &error.to_string(),
            )
        }
    };
    let mime_type = artifact
        .get("mime_type")
        .and_then(Value::as_str)
        .unwrap_or("application/octet-stream");
    let header = HeaderValue::from_str(mime_type)
        .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream"));
    let mut response = axum::body::Body::from(bytes).into_response();
    response.headers_mut().insert(CONTENT_TYPE, header);
    response
}

fn artifact_matches_query(artifact: &Value, query: &ListQuery) -> bool {
    query.workspace_id.as_deref().is_none_or(|workspace_id| {
        artifact.get("workspace_id").and_then(Value::as_str) == Some(workspace_id)
    }) && query.session_id.as_deref().is_none_or(|session_id| {
        artifact.get("session_id").and_then(Value::as_str) == Some(session_id)
    }) && query
        .task_id
        .as_deref()
        .is_none_or(|task_id| artifact.get("task_id").and_then(Value::as_str) == Some(task_id))
}

fn harn_artifact_kind(kind: &str) -> &'static str {
    match kind {
        "file" => "file",
        "table" => "dataset",
        _ => "other",
    }
}

#[derive(Clone, Copy, Debug)]
struct ArtifactInputError {
    status: StatusCode,
    code: &'static str,
    message: &'static str,
}

impl ArtifactInputError {
    fn new(status: StatusCode, code: &'static str, message: &'static str) -> Self {
        Self {
            status,
            code,
            message,
        }
    }

    fn into_response(self) -> Response {
        api_error(self.status, self.code, self.message)
    }
}

fn artifact_from_register_input(
    state: &ApiState,
    input: &Value,
) -> Result<Value, ArtifactInputError> {
    let Some(kind) = input.get("kind").and_then(Value::as_str) else {
        return Err(ArtifactInputError::new(
            StatusCode::BAD_REQUEST,
            "missing_kind",
            "kind is required",
        ));
    };
    if !is_valid_artifact_kind(kind) {
        return Err(ArtifactInputError::new(
            StatusCode::BAD_REQUEST,
            "invalid_kind",
            "kind must be file, patch, image, log, diff, receipt, snapshot, dataset, or other",
        ));
    }
    let Some(mime_type) = input.get("mime_type").and_then(Value::as_str) else {
        return Err(ArtifactInputError::new(
            StatusCode::BAD_REQUEST,
            "missing_mime_type",
            "mime_type is required",
        ));
    };
    if !is_valid_mime_type(mime_type) {
        return Err(ArtifactInputError::new(
            StatusCode::BAD_REQUEST,
            "invalid_mime_type",
            "mime_type must be a media type such as application/pdf",
        ));
    }
    let Some(visibility) = input.get("visibility").and_then(Value::as_str) else {
        return Err(ArtifactInputError::new(
            StatusCode::BAD_REQUEST,
            "missing_visibility",
            "visibility is required",
        ));
    };
    if !matches!(visibility, "public" | "internal" | "receipt_only") {
        return Err(ArtifactInputError::new(
            StatusCode::BAD_REQUEST,
            "invalid_visibility",
            "visibility must be public, internal, or receipt_only",
        ));
    }
    let sha256 = match input.get("sha256").and_then(Value::as_str) {
        Some(value) => Some(normalize_sha256(value).ok_or_else(|| {
            ArtifactInputError::new(
                StatusCode::BAD_REQUEST,
                "invalid_sha256",
                "sha256 must be 64 hexadecimal characters",
            )
        })?),
        None => None,
    };
    let mut workspace_id = input
        .get("workspace_id")
        .and_then(Value::as_str)
        .map(str::to_string);
    let session_id = input
        .get("session_id")
        .and_then(Value::as_str)
        .map(str::to_string);
    let task_id = input
        .get("task_id")
        .and_then(Value::as_str)
        .map(str::to_string);
    validate_artifact_links(
        state,
        &mut workspace_id,
        session_id.as_deref(),
        task_id.as_deref(),
    )?;

    let now = now_rfc3339();
    let mut artifact = json!({
        "id": format!("artifact_{}", Uuid::now_v7()),
        "object": "artifact",
        "created_at": now,
        "updated_at": now,
        "metadata": input.get("metadata").cloned().unwrap_or_else(|| json!({})),
        "kind": kind,
        "mime_type": mime_type,
        "uri": input.get("uri").cloned().unwrap_or(Value::Null),
        "visibility": visibility,
        "sha256": sha256,
        "workspace_id": workspace_id,
        "session_id": session_id,
        "task_id": task_id,
        "receipt_id": input.get("receipt_id").cloned().unwrap_or(Value::Null)
    });
    copy_optional_field(input, &mut artifact, "title");
    copy_optional_field(input, &mut artifact, "name");
    copy_optional_field(input, &mut artifact, "size_bytes");
    Ok(artifact)
}

fn validate_artifact_links(
    state: &ApiState,
    workspace_id: &mut Option<String>,
    session_id: Option<&str>,
    task_id: Option<&str>,
) -> Result<(), ArtifactInputError> {
    let inner = state.inner.lock().expect("api state poisoned");
    if let Some(session_id) = session_id {
        let Some(session) = inner.sessions.get(session_id) else {
            return Err(ArtifactInputError::new(
                StatusCode::NOT_FOUND,
                "not_found",
                "session not found",
            ));
        };
        if workspace_id.is_none() {
            *workspace_id = session
                .get("workspace_id")
                .and_then(Value::as_str)
                .map(str::to_string);
        }
    }
    if let Some(task_id) = task_id {
        if !inner.tasks.contains_key(task_id) {
            return Err(ArtifactInputError::new(
                StatusCode::NOT_FOUND,
                "not_found",
                "task not found",
            ));
        }
    }
    if let Some(workspace_id) = workspace_id.as_deref() {
        if !inner.workspaces.contains_key(workspace_id) {
            return Err(ArtifactInputError::new(
                StatusCode::NOT_FOUND,
                "not_found",
                "workspace not found",
            ));
        }
    }
    Ok(())
}

fn copy_optional_field(input: &Value, output: &mut Value, field: &str) {
    if let Some(value) = input.get(field) {
        output[field] = value.clone();
    }
}

fn artifact_file_path(state: &ApiState, artifact: &Value) -> Option<PathBuf> {
    let uri = artifact.get("uri")?.as_str()?;
    let url = url::Url::parse(uri).ok()?;
    if url.scheme() != "file" {
        return None;
    }
    let file_path = url.to_file_path().ok()?;
    let root = {
        let inner = state.inner.lock().expect("api state poisoned");
        let workspace_id = artifact
            .get("workspace_id")
            .and_then(Value::as_str)
            .unwrap_or(&inner.root_workspace_id);
        workspace_root_locked(&inner, workspace_id)?
    };
    let root = root.canonicalize().ok()?;
    let resolved = file_path.canonicalize().ok()?;
    resolved.starts_with(root).then_some(resolved)
}

fn is_valid_artifact_kind(kind: &str) -> bool {
    matches!(
        kind,
        "file" | "patch" | "image" | "log" | "diff" | "receipt" | "snapshot" | "dataset" | "other"
    )
}

fn is_valid_mime_type(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.contains('/')
        && !value
            .chars()
            .any(|ch| ch.is_control() || ch.is_whitespace() || ch == '<' || ch == '>')
}

pub(super) fn normalize_sha256(value: &str) -> Option<String> {
    let value = value.strip_prefix("sha256:").unwrap_or(value);
    if value.len() == 64 && value.chars().all(|ch| ch.is_ascii_hexdigit()) {
        Some(value.to_ascii_lowercase())
    } else {
        None
    }
}
