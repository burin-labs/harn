use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

use sha2::{Digest, Sha256};

use crate::value::{VmError, VmValue};
use crate::vm::Vm;

pub(crate) const ANTHROPIC_FILES_API_BETA: &str = "files-api-2025-04-14";

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct UploadCacheKey {
    provider: String,
    path: PathBuf,
    media_type: String,
    content_hash: String,
    api_key_hash: String,
}

static FILE_UPLOAD_CACHE: LazyLock<Mutex<BTreeMap<UploadCacheKey, String>>> =
    LazyLock::new(|| Mutex::new(BTreeMap::new()));

fn runtime_error(message: impl Into<String>) -> VmError {
    VmError::Runtime(message.into())
}

fn expect_string(args: &[VmValue], index: usize, builtin: &str) -> Result<String, VmError> {
    match args.get(index) {
        Some(VmValue::String(value)) => Ok(value.to_string()),
        Some(other) => Err(runtime_error(format!(
            "{builtin}: expected string at argument {}, got {}",
            index + 1,
            other.type_name()
        ))),
        None => Err(runtime_error(format!(
            "{builtin}: missing argument {}",
            index + 1
        ))),
    }
}

fn resolve_upload_path(path: &str) -> PathBuf {
    crate::stdlib::process::resolve_source_relative_path(path)
}

fn infer_media_type(path: &Path) -> String {
    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| extension.to_ascii_lowercase())
        .as_deref()
    {
        Some("pdf") => "application/pdf",
        Some("mp3") => "audio/mpeg",
        Some("wav") => "audio/wav",
        Some("m4a") => "audio/mp4",
        Some("mp4") => "video/mp4",
        Some("mpeg") | Some("mpg") => "video/mpeg",
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("txt") => "text/plain",
        Some("json") => "application/json",
        _ => "application/octet-stream",
    }
    .to_string()
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn api_key_fingerprint(api_key: &str) -> String {
    if api_key.is_empty() {
        String::new()
    } else {
        sha256_hex(api_key.as_bytes())
    }
}

fn cache_get(key: &UploadCacheKey) -> Option<String> {
    FILE_UPLOAD_CACHE
        .lock()
        .ok()
        .and_then(|cache| cache.get(key).cloned())
}

fn cache_put(key: UploadCacheKey, file_id: String) {
    if let Ok(mut cache) = FILE_UPLOAD_CACHE.lock() {
        cache.insert(key, file_id);
    }
}

fn cache_key(
    provider: &str,
    path: &Path,
    media_type: &str,
    bytes: &[u8],
    api_key: &str,
) -> UploadCacheKey {
    UploadCacheKey {
        provider: provider.to_string(),
        path: path.canonicalize().unwrap_or_else(|_| path.to_path_buf()),
        media_type: media_type.to_string(),
        content_hash: sha256_hex(bytes),
        api_key_hash: api_key_fingerprint(api_key),
    }
}

fn mock_file_id(provider: &str, media_type: &str, bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(provider.as_bytes());
    hasher.update(b"\0");
    hasher.update(media_type.as_bytes());
    hasher.update(b"\0");
    hasher.update(bytes);
    let digest = hex::encode(hasher.finalize());
    format!("mock_file_{}", &digest[..24])
}

async fn upload_anthropic(
    resolved: &crate::llm::helpers::ResolvedProvider,
    api_key: &str,
    path: &Path,
    media_type: &str,
    bytes: Vec<u8>,
) -> Result<String, VmError> {
    let filename = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("upload")
        .to_string();
    let part = reqwest::multipart::Part::bytes(bytes)
        .file_name(filename)
        .mime_str(media_type)
        .map_err(|error| runtime_error(format!("files.upload: invalid media_type: {error}")))?;
    let form = reqwest::multipart::Form::new().part("file", part);
    let url = format!("{}/files", resolved.base_url.trim_end_matches('/'));
    let req = crate::llm::shared_blocking_client()
        .post(url)
        .timeout(Duration::from_mins(2))
        .multipart(form)
        .header("anthropic-beta", ANTHROPIC_FILES_API_BETA);
    let response = resolved
        .apply_headers(req, api_key)
        .send()
        .await
        .map_err(|error| {
            runtime_error(format!("files.upload: anthropic upload failed: {error}"))
        })?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(runtime_error(format!(
            "files.upload: anthropic upload HTTP {status}: {body}"
        )));
    }
    let json: serde_json::Value = response
        .json()
        .await
        .map_err(|error| runtime_error(format!("files.upload: anthropic response: {error}")))?;
    json.get("id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .ok_or_else(|| runtime_error("files.upload: anthropic response missing id"))
}

async fn upload_gemini(
    resolved: &crate::llm::helpers::ResolvedProvider,
    api_key: &str,
    path: &Path,
    media_type: &str,
    bytes: Vec<u8>,
) -> Result<String, VmError> {
    let filename = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("upload");
    let start_url = format!(
        "{}/upload/v1beta/files",
        resolved.base_url.trim_end_matches('/')
    );
    let metadata = serde_json::json!({
        "file": {
            "display_name": filename,
        }
    });
    let start_req = crate::llm::shared_blocking_client()
        .post(start_url)
        .timeout(Duration::from_mins(2))
        .header("Content-Type", "application/json")
        .header("X-Goog-Upload-Protocol", "resumable")
        .header("X-Goog-Upload-Command", "start")
        .header(
            "X-Goog-Upload-Header-Content-Length",
            bytes.len().to_string(),
        )
        .header("X-Goog-Upload-Header-Content-Type", media_type)
        .json(&metadata);
    let start_response = resolved
        .apply_headers(start_req, api_key)
        .send()
        .await
        .map_err(|error| {
            runtime_error(format!("files.upload: gemini upload start failed: {error}"))
        })?;
    if !start_response.status().is_success() {
        let status = start_response.status();
        let body = start_response.text().await.unwrap_or_default();
        return Err(runtime_error(format!(
            "files.upload: gemini upload start HTTP {status}: {body}"
        )));
    }
    let upload_url = start_response
        .headers()
        .get("x-goog-upload-url")
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| runtime_error("files.upload: gemini response missing upload URL"))?
        .to_string();

    let upload_response = crate::llm::shared_blocking_client()
        .post(upload_url)
        .timeout(Duration::from_mins(2))
        .header("Content-Length", bytes.len().to_string())
        .header("X-Goog-Upload-Offset", "0")
        .header("X-Goog-Upload-Command", "upload, finalize")
        .body(bytes)
        .send()
        .await
        .map_err(|error| runtime_error(format!("files.upload: gemini upload failed: {error}")))?;
    if !upload_response.status().is_success() {
        let status = upload_response.status();
        let body = upload_response.text().await.unwrap_or_default();
        return Err(runtime_error(format!(
            "files.upload: gemini upload HTTP {status}: {body}"
        )));
    }
    let json: serde_json::Value = upload_response
        .json()
        .await
        .map_err(|error| runtime_error(format!("files.upload: gemini response: {error}")))?;
    json.pointer("/file/uri")
        .or_else(|| json.pointer("/file/name"))
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .ok_or_else(|| runtime_error("files.upload: gemini response missing file uri"))
}

async fn upload_file(path: String, provider: String) -> Result<String, VmError> {
    let resolved_path = resolve_upload_path(&path);
    crate::stdlib::sandbox::enforce_fs_path(
        "files.upload",
        &resolved_path,
        crate::stdlib::sandbox::FsAccess::Read,
    )?;
    let bytes = std::fs::read(&resolved_path).map_err(|error| {
        runtime_error(format!(
            "files.upload: failed to read {}: {error}",
            resolved_path.display()
        ))
    })?;
    let media_type = infer_media_type(&resolved_path);
    let api_key = if provider == "mock" {
        String::new()
    } else {
        crate::llm::helpers::resolve_api_key(&provider)?
    };
    let key = cache_key(&provider, &resolved_path, &media_type, &bytes, &api_key);
    if let Some(file_id) = cache_get(&key) {
        return Ok(file_id);
    }

    let file_id = if provider == "mock" {
        mock_file_id(&provider, &media_type, &bytes)
    } else {
        match crate::llm::provider::provider_file_upload_wire_format(&provider).as_deref() {
            Some("anthropic") => {
                let resolved = crate::llm::helpers::ResolvedProvider::resolve(&provider);
                upload_anthropic(&resolved, &api_key, &resolved_path, &media_type, bytes).await?
            }
            Some("gemini") => {
                let resolved = crate::llm::helpers::ResolvedProvider::resolve(&provider);
                upload_gemini(&resolved, &api_key, &resolved_path, &media_type, bytes).await?
            }
            _ => {
                return Err(runtime_error(format!(
                    "files.upload: provider `{provider}` does not support file uploads"
                )));
            }
        }
    };
    cache_put(key, file_id.clone());
    Ok(file_id)
}

pub(crate) fn register_file_builtins(vm: &mut Vm) {
    vm.register_async_builtin("__files_upload", |args| async move {
        let path = expect_string(&args, 0, "__files_upload")?;
        let provider = expect_string(&args, 1, "__files_upload")?;
        let file_id = upload_file(path, provider).await?;
        Ok(VmValue::String(Rc::from(file_id)))
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestration::CapabilityPolicy;
    use tempfile::tempdir;

    struct PolicyGuard;

    impl Drop for PolicyGuard {
        fn drop(&mut self) {
            crate::orchestration::pop_execution_policy();
        }
    }

    fn push_policy(policy: CapabilityPolicy) -> PolicyGuard {
        crate::orchestration::push_execution_policy(policy);
        PolicyGuard
    }

    #[tokio::test]
    async fn mock_upload_returns_stable_file_id() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("sample.pdf");
        std::fs::write(&path, b"%PDF-1.4\n").unwrap();

        let first = upload_file(path.to_string_lossy().into_owned(), "mock".to_string())
            .await
            .unwrap();
        let second = upload_file(path.to_string_lossy().into_owned(), "mock".to_string())
            .await
            .unwrap();

        assert_eq!(first, second);
        assert!(first.starts_with("mock_file_"));
    }

    #[tokio::test]
    async fn upload_respects_workspace_roots() {
        let allowed = tempdir().unwrap();
        let denied = tempdir().unwrap();
        let path = denied.path().join("secret.pdf");
        std::fs::write(&path, b"%PDF-1.4\n").unwrap();
        let _guard = push_policy(CapabilityPolicy {
            workspace_roots: vec![allowed.path().display().to_string()],
            ..CapabilityPolicy::default()
        });

        let err = upload_file(path.to_string_lossy().into_owned(), "mock".to_string())
            .await
            .unwrap_err();

        assert!(
            err.to_string().contains("sandbox violation"),
            "expected sandbox violation, got {err}"
        );
    }
}
