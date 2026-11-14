use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

use serde::Deserialize;
use serde_json::Value;

use super::LlmTranscriptDescriptorError;

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) struct RawProviderArtifactPath(PathBuf);

#[derive(Deserialize)]
struct CapturePointer {
    path: String,
}

pub(super) fn pointer_path(
    event: &Value,
) -> Result<RawProviderArtifactPath, LlmTranscriptDescriptorError> {
    let pointer: CapturePointer = serde_json::from_value(event.clone()).map_err(|_| {
        LlmTranscriptDescriptorError::new(
            "invalid_raw_provider_pointer",
            "raw provider capture must have a string path",
        )
    })?;
    let path = PathBuf::from(&pointer.path);
    let mut components = path.components();
    let safe = !pointer.path.contains(['\\', '\0'])
        && components.next() == Some(Component::Normal("raw-provider".as_ref()))
        && components.clone().next().is_some()
        && components.all(|part| matches!(part, Component::Normal(_)));
    if !safe {
        return Err(LlmTranscriptDescriptorError::new(
            "invalid_raw_provider_path",
            "raw provider capture path must stay inside raw-provider",
        ));
    }
    Ok(RawProviderArtifactPath(path))
}

pub(super) fn validate(
    transcript: &Path,
    paths: &BTreeSet<RawProviderArtifactPath>,
) -> Result<(), LlmTranscriptDescriptorError> {
    for path in paths {
        checked_source(transcript_root(transcript), &path.0)?;
    }
    Ok(())
}

pub(super) fn copy(
    source: &Path,
    destination: &Path,
    paths: &BTreeSet<RawProviderArtifactPath>,
) -> Result<(), LlmTranscriptDescriptorError> {
    let source_root = transcript_root(source);
    let destination_root = transcript_root(destination);
    for path in paths {
        let source_path = checked_source(source_root, &path.0)?;
        prepare_destination(destination_root, &path.0)?;
        crate::atomic_io::atomic_copy_with_mode(
            &source_path,
            &destination_root.join(&path.0),
            0o600,
        )
        .map_err(|_| unavailable("failed to persist a referenced raw provider artifact"))?;
    }
    Ok(())
}

fn transcript_root(transcript: &Path) -> &Path {
    transcript
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

fn checked_source(root: &Path, relative: &Path) -> Result<PathBuf, LlmTranscriptDescriptorError> {
    checked_metadata(root, true)?;
    let mut current = root.to_path_buf();
    let mut parts = relative.components().peekable();
    while let Some(part) = parts.next() {
        current.push(part);
        checked_metadata(&current, parts.peek().is_some())?;
    }
    Ok(current)
}

fn prepare_destination(root: &Path, relative: &Path) -> Result<(), LlmTranscriptDescriptorError> {
    checked_metadata(root, true)?;
    let mut current = root.to_path_buf();
    let mut parts = relative.components().peekable();
    while let Some(part) = parts.next() {
        current.push(part);
        if parts.peek().is_some() {
            match std::fs::create_dir(&current) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(_) => return Err(unavailable("failed to create a raw provider directory")),
            }
            checked_metadata(&current, true)?;
        } else {
            match std::fs::symlink_metadata(&current) {
                Ok(_) => checked_metadata(&current, false)?,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => return Err(unavailable("raw provider destination is unavailable")),
            }
        }
    }
    Ok(())
}

fn checked_metadata(path: &Path, directory: bool) -> Result<(), LlmTranscriptDescriptorError> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|_| unavailable("referenced raw provider artifact is unavailable"))?;
    if metadata.file_type().is_symlink() {
        return Err(LlmTranscriptDescriptorError::new(
            "raw_provider_artifact_symlink",
            "raw provider artifact paths must not traverse symlinks",
        ));
    }
    if (directory && !metadata.is_dir()) || (!directory && !metadata.is_file()) {
        return Err(LlmTranscriptDescriptorError::new(
            "invalid_raw_provider_artifact",
            "raw provider artifact is not a regular file inside directories",
        ));
    }
    Ok(())
}

fn unavailable(message: &'static str) -> LlmTranscriptDescriptorError {
    LlmTranscriptDescriptorError::new("raw_provider_artifact_unavailable", message)
}
