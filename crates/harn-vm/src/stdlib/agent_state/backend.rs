use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::value::VmError;

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConflictPolicy {
    #[default]
    Ignore,
    Warn,
    Error,
}

impl ConflictPolicy {
    pub fn parse(raw: &str) -> Result<Self, VmError> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "" | "ignore" | "off" => Ok(Self::Ignore),
            "warn" | "warning" => Ok(Self::Warn),
            "error" | "strict" => Ok(Self::Error),
            other => Err(VmError::Runtime(format!(
                "agent_state: unknown conflict policy '{other}'"
            ))),
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Ignore => "ignore",
            Self::Warn => "warn",
            Self::Error => "error",
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct WriterIdentity {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub writer_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stage_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker_id: Option<String>,
}

impl WriterIdentity {
    pub fn is_empty(&self) -> bool {
        self.writer_id.is_none()
            && self.stage_id.is_none()
            && self.session_id.is_none()
            && self.worker_id.is_none()
    }

    pub fn display_name(&self) -> String {
        self.writer_id
            .clone()
            .or_else(|| self.worker_id.clone())
            .or_else(|| self.stage_id.clone())
            .or_else(|| self.session_id.clone())
            .unwrap_or_else(|| "unknown".to_string())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BackendScope {
    pub root: PathBuf,
    pub namespace: String,
}

impl BackendScope {
    pub fn namespace_dir(&self) -> PathBuf {
        self.root.join(&self.namespace)
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct BackendWriteOptions {
    pub writer: WriterIdentity,
    pub conflict_policy: ConflictPolicy,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ConflictRecord {
    pub key: String,
    pub previous: WriterIdentity,
    pub current: WriterIdentity,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct BackendWriteOutcome {
    pub conflict: Option<ConflictRecord>,
}

pub trait DurableStateBackend {
    fn backend_name(&self) -> &'static str;
    fn ensure_scope(&self, scope: &BackendScope) -> Result<(), VmError>;
    fn resume_scope(&self, scope: &BackendScope) -> Result<(), VmError>;
    fn read(&self, scope: &BackendScope, key: &str) -> Result<Option<String>, VmError>;
    fn write(
        &self,
        scope: &BackendScope,
        key: &str,
        content: &str,
        options: &BackendWriteOptions,
    ) -> Result<BackendWriteOutcome, VmError>;
    fn delete(&self, scope: &BackendScope, key: &str) -> Result<(), VmError>;
    fn list(&self, scope: &BackendScope) -> Result<Vec<String>, VmError>;
}

const INTERNAL_DIR: &str = ".agent_state_meta";
const TMP_SUFFIX: &str = ".agent_state_tmp";

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct StoredWriterMeta {
    #[serde(default)]
    writer: WriterIdentity,
    #[serde(default)]
    updated_at: Option<u64>,
}

#[derive(Clone, Debug, Default)]
pub struct FilesystemBackend;

impl FilesystemBackend {
    pub fn new() -> Self {
        Self
    }
}

impl DurableStateBackend for FilesystemBackend {
    fn backend_name(&self) -> &'static str {
        "filesystem"
    }

    fn ensure_scope(&self, scope: &BackendScope) -> Result<(), VmError> {
        fs::create_dir_all(scope.namespace_dir())
            .map_err(|error| VmError::Runtime(format!("agent_state mkdir error: {error}")))?;
        Ok(())
    }

    fn resume_scope(&self, scope: &BackendScope) -> Result<(), VmError> {
        let path = scope.namespace_dir();
        if !path.is_dir() {
            return Err(VmError::Runtime(format!(
                "agent_state.resume: session '{}' not found under {}",
                scope.namespace,
                scope.root.display()
            )));
        }
        Ok(())
    }

    fn read(&self, scope: &BackendScope, key: &str) -> Result<Option<String>, VmError> {
        let path = key_path(scope, key)?;
        match fs::read_to_string(&path) {
            Ok(content) => Ok(Some(content)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(VmError::Runtime(format!(
                "agent_state.read: failed to read {}: {error}",
                path.display()
            ))),
        }
    }

    fn write(
        &self,
        scope: &BackendScope,
        key: &str,
        content: &str,
        options: &BackendWriteOptions,
    ) -> Result<BackendWriteOutcome, VmError> {
        let path = key_path(scope, key)?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                VmError::Runtime(format!(
                    "agent_state.write: failed to create {}: {error}",
                    parent.display()
                ))
            })?;
        }

        let previous = read_writer_meta(scope, key)?;
        let conflict = detect_conflict(key, previous.as_ref(), &options.writer);
        if let Some(conflict) = &conflict {
            if matches!(options.conflict_policy, ConflictPolicy::Error) {
                return Err(VmError::Runtime(format!(
                    "agent_state.write: key '{}' was previously written by '{}' and is now being written by '{}'",
                    conflict.key,
                    conflict.previous.display_name(),
                    conflict.current.display_name()
                )));
            }
        }
        atomic_write(&path, content.as_bytes())?;
        write_writer_meta(scope, key, &options.writer)?;
        Ok(BackendWriteOutcome { conflict })
    }

    fn delete(&self, scope: &BackendScope, key: &str) -> Result<(), VmError> {
        let path = key_path(scope, key)?;
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(VmError::Runtime(format!(
                    "agent_state.delete: failed to delete {}: {error}",
                    path.display()
                )))
            }
        }
        remove_writer_meta(scope, key)?;
        prune_empty_ancestors(path.parent(), &scope.namespace_dir());
        Ok(())
    }

    fn list(&self, scope: &BackendScope) -> Result<Vec<String>, VmError> {
        let root = scope.namespace_dir();
        let mut keys = Vec::new();
        if !root.exists() {
            return Ok(keys);
        }
        collect_keys(&root, &root, &mut keys)?;
        keys.sort();
        Ok(keys)
    }
}

fn detect_conflict(
    key: &str,
    previous: Option<&StoredWriterMeta>,
    current: &WriterIdentity,
) -> Option<ConflictRecord> {
    let previous = previous?;
    if previous.writer.is_empty() || current.is_empty() {
        return None;
    }
    let prev_id = previous.writer.writer_id.as_deref();
    let current_id = current.writer_id.as_deref();
    if prev_id.is_some() && current_id.is_some() && prev_id != current_id {
        return Some(ConflictRecord {
            key: key.to_string(),
            previous: previous.writer.clone(),
            current: current.clone(),
        });
    }
    None
}

fn key_path(scope: &BackendScope, key: &str) -> Result<PathBuf, VmError> {
    let normalized = normalize_key(key)?;
    Ok(scope.namespace_dir().join(normalized))
}

fn meta_path(scope: &BackendScope, key: &str) -> Result<PathBuf, VmError> {
    let normalized = normalize_key(key)?;
    let mut path = scope.namespace_dir().join(INTERNAL_DIR);
    for component in normalized.components() {
        path.push(component.as_os_str());
    }
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| VmError::Runtime("agent_state: invalid metadata key".to_string()))?;
    path.set_file_name(format!("{file_name}.json"));
    Ok(path)
}

fn normalize_key(key: &str) -> Result<PathBuf, VmError> {
    let raw = key.trim();
    if raw.is_empty() {
        return Err(VmError::Runtime(
            "agent_state: key must be a non-empty relative path".to_string(),
        ));
    }
    let candidate = Path::new(raw);
    if candidate.is_absolute() {
        return Err(VmError::Runtime(format!(
            "agent_state: key '{raw}' must be relative"
        )));
    }
    let mut normalized = PathBuf::new();
    for component in candidate.components() {
        match component {
            Component::Normal(part) => {
                let name = part.to_string_lossy();
                if name == INTERNAL_DIR || name.contains(TMP_SUFFIX) {
                    return Err(VmError::Runtime(format!(
                        "agent_state: key '{raw}' uses a reserved internal path"
                    )));
                }
                normalized.push(part);
            }
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(VmError::Runtime(format!(
                    "agent_state: key '{raw}' must not escape the session root"
                )))
            }
        }
    }
    if normalized.as_os_str().is_empty() {
        return Err(VmError::Runtime(
            "agent_state: key must contain at least one path component".to_string(),
        ));
    }
    Ok(normalized)
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), VmError> {
    let parent = path.parent().ok_or_else(|| {
        VmError::Runtime(format!(
            "agent_state.write: path '{}' has no parent directory",
            path.display()
        ))
    })?;
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("state");
    let tmp_path = parent.join(format!(
        ".{file_name}.{TMP_SUFFIX}.{}",
        uuid::Uuid::now_v7()
    ));
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&tmp_path)
        .map_err(|error| {
            VmError::Runtime(format!(
                "agent_state.write: failed to open temp file {}: {error}",
                tmp_path.display()
            ))
        })?;
    file.write_all(bytes).map_err(|error| {
        VmError::Runtime(format!(
            "agent_state.write: failed to write temp file {}: {error}",
            tmp_path.display()
        ))
    })?;
    file.sync_all().map_err(|error| {
        VmError::Runtime(format!(
            "agent_state.write: failed to sync temp file {}: {error}",
            tmp_path.display()
        ))
    })?;

    if std::env::var("HARN_AGENT_STATE_ABORT_AFTER_TMP_WRITE")
        .ok()
        .as_deref()
        == Some("1")
    {
        std::process::abort();
    }

    fs::rename(&tmp_path, path).map_err(|error| {
        VmError::Runtime(format!(
            "agent_state.write: failed to rename {} to {}: {error}",
            tmp_path.display(),
            path.display()
        ))
    })?;

    if let Ok(dir) = OpenOptions::new().read(true).open(parent) {
        let _ = dir.sync_all();
    }
    Ok(())
}

fn read_writer_meta(scope: &BackendScope, key: &str) -> Result<Option<StoredWriterMeta>, VmError> {
    let path = meta_path(scope, key)?;
    match fs::read_to_string(&path) {
        Ok(content) => serde_json::from_str(&content).map(Some).map_err(|error| {
            VmError::Runtime(format!(
                "agent_state: failed to parse metadata {}: {error}",
                path.display()
            ))
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(VmError::Runtime(format!(
            "agent_state: failed to read metadata {}: {error}",
            path.display()
        ))),
    }
}

fn write_writer_meta(
    scope: &BackendScope,
    key: &str,
    writer: &WriterIdentity,
) -> Result<(), VmError> {
    if writer.is_empty() {
        return Ok(());
    }
    let path = meta_path(scope, key)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            VmError::Runtime(format!(
                "agent_state: failed to create metadata dir {}: {error}",
                parent.display()
            ))
        })?;
    }
    let updated_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_secs());
    let payload = serde_json::to_vec_pretty(&StoredWriterMeta {
        writer: writer.clone(),
        updated_at,
    })
    .map_err(|error| VmError::Runtime(format!("agent_state: metadata encode error: {error}")))?;
    atomic_write(&path, &payload)
}

fn remove_writer_meta(scope: &BackendScope, key: &str) -> Result<(), VmError> {
    let path = meta_path(scope, key)?;
    match fs::remove_file(&path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(VmError::Runtime(format!(
                "agent_state: failed to delete metadata {}: {error}",
                path.display()
            )))
        }
    }
    prune_empty_ancestors(path.parent(), &scope.namespace_dir().join(INTERNAL_DIR));
    Ok(())
}

fn prune_empty_ancestors(mut current: Option<&Path>, stop_at: &Path) {
    while let Some(dir) = current {
        if dir == stop_at || dir == stop_at.parent().unwrap_or(stop_at) {
            break;
        }
        match fs::remove_dir(dir) {
            Ok(()) => current = dir.parent(),
            Err(_) => break,
        }
    }
}

fn collect_keys(root: &Path, current: &Path, out: &mut Vec<String>) -> Result<(), VmError> {
    let entries = fs::read_dir(current).map_err(|error| {
        VmError::Runtime(format!(
            "agent_state.list: failed to read {}: {error}",
            current.display()
        ))
    })?;
    let mut children: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .collect();
    children.sort();
    for child in children {
        let name = child
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("");
        if name == INTERNAL_DIR || name.contains(TMP_SUFFIX) {
            continue;
        }
        if child.is_dir() {
            collect_keys(root, &child, out)?;
            continue;
        }
        if let Ok(relative) = child.strip_prefix(root) {
            let key = relative
                .components()
                .filter_map(|component| match component {
                    Component::Normal(part) => Some(part.to_string_lossy().into_owned()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("/");
            if !key.is_empty() {
                out.push(key);
            }
        }
    }
    Ok(())
}
