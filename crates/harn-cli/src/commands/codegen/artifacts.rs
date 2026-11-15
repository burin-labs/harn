use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use harn_vm::codegen::{
    CodegenDiagnostic, CodegenDiagnosticCode as Code, CodegenIdentity, CodegenRole,
    GeneratedArtifact, GeneratedRange,
};
use harn_vm::conditional_replace::{
    conditional_replace, ConditionalReplaceOptions, ConditionalReplaceStatus,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[cfg(test)]
mod tests;

const MANIFEST: &str = ".harn-codegen.json";
const MAX_ARTIFACTS: usize = 4096;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct GeneratedArtifactSet {
    artifacts: BTreeMap<String, GeneratedArtifact>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ArtifactManifest {
    schema_version: String,
    identity: CodegenIdentity,
    role: CodegenRole,
    files: Vec<ArtifactFile>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ArtifactFile {
    path: String,
    sha256: String,
    source_mappings: Vec<GeneratedRange>,
}

fn diagnostic(code: Code, path: &str) -> CodegenDiagnostic {
    let mut error = CodegenDiagnostic::new(code);
    error.structural_path = path.into();
    error
}

fn digest(bytes: &[u8]) -> String {
    format!("sha256:{}", hex::encode(Sha256::digest(bytes)))
}

fn relative_path(root: &Path, name: &str) -> Result<PathBuf, CodegenDiagnostic> {
    if name.is_empty()
        || name
            .split('/')
            .next()
            .is_some_and(|part| part.eq_ignore_ascii_case(MANIFEST))
        || name.contains(['\\', ':'])
        || name.chars().any(char::is_control)
        || name
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err(diagnostic(Code::UnsafeArtifactPath, name));
    }
    harn_package::package::safe_package_relative_path(root, name)
        .map_err(|_| diagnostic(Code::UnsafeArtifactPath, name))
}

fn regular_bytes(path: &Path) -> Result<Option<Vec<u8>>, CodegenDiagnostic> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => fs::read(path)
            .map(Some)
            .map_err(|_| diagnostic(Code::ArtifactWriteFailed, &path.display().to_string())),
        Ok(_) => Err(diagnostic(
            Code::UnsafeArtifactPath,
            &path.display().to_string(),
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(diagnostic(
            Code::ArtifactWriteFailed,
            &path.display().to_string(),
        )),
    }
}

fn verify_ancestry(root: &Path, relative: &str) -> Result<(), CodegenDiagnostic> {
    let mut current = root.to_path_buf();
    let components: Vec<_> = relative.split('/').collect();
    for component in &components[..components.len() - 1] {
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
            Ok(_) => return Err(diagnostic(Code::UnsafeArtifactPath, relative)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(_) => return Err(diagnostic(Code::ArtifactWriteFailed, relative)),
        }
    }
    Ok(())
}

impl GeneratedArtifactSet {
    pub(super) fn prepare(artifacts: Vec<GeneratedArtifact>) -> Result<Self, CodegenDiagnostic> {
        if artifacts.is_empty() || artifacts.len() > MAX_ARTIFACTS {
            return Err(CodegenDiagnostic::new(Code::MalformedPluginResponse));
        }
        let mut paths = BTreeSet::new();
        let mut prepared = BTreeMap::new();
        for artifact in artifacts {
            relative_path(Path::new("."), &artifact.path)?;
            let collision_key = artifact.path.to_lowercase();
            if paths.iter().any(|prior: &String| {
                prior == &collision_key
                    || collision_key.starts_with(&format!("{prior}/"))
                    || prior.starts_with(&format!("{collision_key}/"))
            }) {
                return Err(diagnostic(Code::DuplicateArtifactPath, &artifact.path));
            }
            paths.insert(collision_key);
            prepared.insert(artifact.path.clone(), artifact);
        }
        // Materializing into a private directory also detects the current filesystem's
        // physical aliases before any output path can change.
        let stage =
            tempfile::tempdir().map_err(|_| CodegenDiagnostic::new(Code::ArtifactWriteFailed))?;
        for artifact in prepared.values() {
            let path = relative_path(stage.path(), &artifact.path)?;
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)
                    .map_err(|_| diagnostic(Code::ArtifactWriteFailed, &artifact.path))?;
            }
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path)
                .map_err(|_| diagnostic(Code::DuplicateArtifactPath, &artifact.path))?;
            std::io::Write::write_all(&mut file, artifact.content.as_bytes())
                .map_err(|_| diagnostic(Code::ArtifactWriteFailed, &artifact.path))?;
        }
        Ok(Self {
            artifacts: prepared,
        })
    }

    pub(super) fn publish(
        &self,
        root: &Path,
        identity: &CodegenIdentity,
        role: CodegenRole,
        check: bool,
    ) -> Result<(), CodegenDiagnostic> {
        let root = std::path::absolute(root)
            .map_err(|_| CodegenDiagnostic::new(Code::UnsafeArtifactPath))?;
        if let Ok(metadata) = fs::symlink_metadata(&root) {
            if !metadata.is_dir() || metadata.file_type().is_symlink() {
                return Err(CodegenDiagnostic::new(Code::UnsafeArtifactPath));
            }
        }
        let _lock = harn_vm::conditional_replace::acquire_lock(&root)
            .map_err(|_| CodegenDiagnostic::new(Code::ArtifactWriteFailed))?;
        let manifest_path = root.join(MANIFEST);
        let previous_bytes = regular_bytes(&manifest_path)?;
        let previous = previous_bytes
            .as_ref()
            .map(|bytes| serde_json::from_slice::<ArtifactManifest>(bytes))
            .transpose()
            .map_err(|_| diagnostic(Code::ArtifactWriteFailed, MANIFEST))?;
        let mut prior = BTreeMap::new();
        if let Some(previous) = previous {
            if previous.schema_version != "harn-codegen-artifacts/1.0" {
                return Err(diagnostic(Code::ArtifactWriteFailed, MANIFEST));
            }
            for file in previous.files {
                relative_path(&root, &file.path)?;
                if prior.insert(file.path.clone(), file.sha256).is_some() {
                    return Err(diagnostic(Code::DuplicateArtifactPath, &file.path));
                }
            }
        }
        let names: BTreeSet<_> = prior.keys().chain(self.artifacts.keys()).cloned().collect();
        let mut observed = BTreeMap::new();
        for name in &names {
            verify_ancestry(&root, name)?;
            let bytes = regular_bytes(&relative_path(&root, name)?)?;
            if let Some(bytes) = &bytes {
                let already_generated = self
                    .artifacts
                    .get(name)
                    .is_some_and(|artifact| artifact.content.as_bytes() == bytes);
                if !already_generated && prior.get(name) != Some(&digest(bytes)) {
                    return Err(diagnostic(
                        if check {
                            Code::ArtifactDrift
                        } else {
                            Code::ArtifactWriteFailed
                        },
                        name,
                    ));
                }
            }
            observed.insert(name.clone(), bytes);
        }
        let manifest = ArtifactManifest {
            schema_version: "harn-codegen-artifacts/1.0".into(),
            identity: identity.clone(),
            role,
            files: self
                .artifacts
                .values()
                .map(|artifact| ArtifactFile {
                    path: artifact.path.clone(),
                    sha256: digest(artifact.content.as_bytes()),
                    source_mappings: artifact.source_mappings.clone(),
                })
                .collect(),
        };
        let mut manifest_bytes = serde_json::to_vec_pretty(&manifest)
            .map_err(|_| CodegenDiagnostic::new(Code::ArtifactWriteFailed))?;
        manifest_bytes.push(b'\n');
        let matches = previous_bytes.as_ref() == Some(&manifest_bytes)
            && self.artifacts.iter().all(|(name, artifact)| {
                observed[name].as_deref() == Some(artifact.content.as_bytes())
            })
            && prior
                .keys()
                .all(|name| self.artifacts.contains_key(name) || observed[name].is_none());
        if matches {
            return Ok(());
        }
        if check {
            return Err(CodegenDiagnostic::new(Code::ArtifactDrift));
        }
        // All paths, preimages and both plugin responses have been validated before writes.
        for (name, artifact) in &self.artifacts {
            let path = relative_path(&root, name)?;
            let before = observed[name].as_deref();
            let receipt = conditional_replace(
                &path,
                artifact.content.as_bytes(),
                &ConditionalReplaceOptions {
                    expected_sha256: before.map(digest),
                    overwrite: before.is_some(),
                    ..ConditionalReplaceOptions::default()
                },
            )
            .map_err(|_| diagnostic(Code::ArtifactWriteFailed, name))?;
            if receipt.status == ConditionalReplaceStatus::Stale {
                return Err(diagnostic(Code::ArtifactWriteFailed, name));
            }
        }
        for name in prior
            .keys()
            .filter(|name| !self.artifacts.contains_key(*name))
        {
            let path = relative_path(&root, name)?;
            if let Some(before) = &observed[name] {
                if regular_bytes(&path)?.as_ref() != Some(before) {
                    return Err(diagnostic(Code::ArtifactWriteFailed, name));
                }
                fs::remove_file(path).map_err(|_| diagnostic(Code::ArtifactWriteFailed, name))?;
            }
        }
        let receipt = conditional_replace(
            &manifest_path,
            &manifest_bytes,
            &ConditionalReplaceOptions {
                expected_sha256: previous_bytes.as_deref().map(digest),
                overwrite: previous_bytes.is_some(),
                ..ConditionalReplaceOptions::default()
            },
        )
        .map_err(|_| diagnostic(Code::ArtifactWriteFailed, MANIFEST))?;
        if receipt.status == ConditionalReplaceStatus::Stale {
            return Err(diagnostic(Code::ArtifactWriteFailed, MANIFEST));
        }
        Ok(())
    }
}
