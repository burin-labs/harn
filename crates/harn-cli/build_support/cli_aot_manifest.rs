use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const CLI_AOT_MANIFEST_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CliAotManifest {
    pub schema_version: u32,
    pub harn_version: String,
    pub compiler_fingerprint: String,
    pub scripts: Vec<CliAotScriptRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CliAotScriptRecord {
    pub name: String,
    pub source_path: String,
    pub source_sha256: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact: Option<CliAotArtifactRecord>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skip_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CliAotArtifactRecord {
    pub path: String,
    pub sha256: String,
}

impl CliAotManifest {
    pub fn validate_shape(&self) -> Result<(), String> {
        if self.schema_version != CLI_AOT_MANIFEST_SCHEMA_VERSION {
            return Err(format!(
                "unsupported schema_version {}; expected {CLI_AOT_MANIFEST_SCHEMA_VERSION}",
                self.schema_version
            ));
        }
        if self.harn_version.trim().is_empty() {
            return Err("harn_version must not be empty".to_string());
        }
        if self.compiler_fingerprint.trim().is_empty() {
            return Err("compiler_fingerprint must not be empty".to_string());
        }

        let mut names = BTreeSet::new();
        let mut source_paths = BTreeSet::new();
        let mut artifact_paths = BTreeSet::new();
        for script in &self.scripts {
            if script.name.trim().is_empty() {
                return Err("script name must not be empty".to_string());
            }
            if !names.insert(script.name.as_str()) {
                return Err(format!("duplicate script name `{}`", script.name));
            }
            if !source_paths.insert(script.source_path.as_str()) {
                return Err(format!("duplicate source path `{}`", script.source_path));
            }
            if script.source_sha256.len() != 64 {
                return Err(format!(
                    "script `{}` has an invalid source_sha256",
                    script.name
                ));
            }
            match (&script.artifact, &script.skip_reason) {
                (Some(artifact), None) => {
                    if artifact.sha256.len() != 64 {
                        return Err(format!(
                            "script `{}` has an invalid artifact sha256",
                            script.name
                        ));
                    }
                    if !artifact_paths.insert(artifact.path.as_str()) {
                        return Err(format!("duplicate artifact path `{}`", artifact.path));
                    }
                }
                (None, Some(reason)) if !reason.trim().is_empty() => {}
                _ => {
                    return Err(format!(
                        "script `{}` must declare exactly one of artifact or non-empty skip_reason",
                        script.name
                    ));
                }
            }
        }
        Ok(())
    }
}

pub fn read_manifest(path: &Path) -> Result<CliAotManifest, String> {
    let bytes = fs::read(path).map_err(|error| format!("read {}: {error}", path.display()))?;
    let manifest: CliAotManifest = serde_json::from_slice(&bytes)
        .map_err(|error| format!("parse {}: {error}", path.display()))?;
    manifest.validate_shape()?;
    Ok(manifest)
}

pub fn canonical_manifest_bytes(manifest: &CliAotManifest) -> Result<Vec<u8>, String> {
    manifest.validate_shape()?;
    let mut bytes = serde_json::to_vec_pretty(manifest)
        .map_err(|error| format!("serialize CLI AOT manifest: {error}"))?;
    bytes.push(b'\n');
    Ok(bytes)
}

pub fn sha256_bytes(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub fn canonical_source_text(source: &str) -> String {
    String::from_utf8(crate::codegen_fingerprint::canonical_source_bytes(
        source.as_bytes(),
    ))
    .expect("canonical source preserves UTF-8")
}

pub fn sha256_source_bytes(bytes: &[u8]) -> String {
    sha256_bytes(&crate::codegen_fingerprint::canonical_source_bytes(bytes))
}

pub fn sha256_file(path: &Path) -> Result<String, String> {
    let bytes = fs::read(path).map_err(|error| format!("read {}: {error}", path.display()))?;
    Ok(sha256_bytes(&bytes))
}

pub fn sha256_source_file(path: &Path) -> Result<String, String> {
    let bytes = fs::read(path).map_err(|error| format!("read {}: {error}", path.display()))?;
    Ok(sha256_source_bytes(&bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn compiled_script(name: &str) -> CliAotScriptRecord {
        CliAotScriptRecord {
            name: name.to_string(),
            source_path: format!("../harn-stdlib/src/stdlib/cli/{name}.harn"),
            source_sha256: "a".repeat(64),
            artifact: Some(CliAotArtifactRecord {
                path: format!("generated/cli-bytecode/{name}.harnbc"),
                sha256: "b".repeat(64),
            }),
            skip_reason: None,
        }
    }

    fn manifest(scripts: Vec<CliAotScriptRecord>) -> CliAotManifest {
        CliAotManifest {
            schema_version: CLI_AOT_MANIFEST_SCHEMA_VERSION,
            harn_version: "1.2.3".to_string(),
            compiler_fingerprint: "c".repeat(64),
            scripts,
        }
    }

    #[test]
    fn manifest_requires_unique_names_and_paths() {
        let first = compiled_script("doctor");
        let mut duplicate_name = compiled_script("doctor");
        duplicate_name.source_path = "different.harn".to_string();
        duplicate_name.artifact.as_mut().expect("artifact").path = "different.harnbc".to_string();
        assert!(manifest(vec![first.clone(), duplicate_name])
            .validate_shape()
            .unwrap_err()
            .contains("duplicate script name"));

        let mut duplicate_artifact = compiled_script("echo");
        duplicate_artifact.artifact.as_mut().expect("artifact").path =
            first.artifact.as_ref().expect("artifact").path.clone();
        assert!(manifest(vec![first, duplicate_artifact])
            .validate_shape()
            .unwrap_err()
            .contains("duplicate artifact path"));
    }

    #[test]
    fn manifest_requires_exactly_one_artifact_or_skip_reason() {
        let mut neither = compiled_script("doctor");
        neither.artifact = None;
        assert!(manifest(vec![neither]).validate_shape().is_err());

        let mut both = compiled_script("doctor");
        both.skip_reason = Some("runtime only".to_string());
        assert!(manifest(vec![both]).validate_shape().is_err());

        let skipped = CliAotScriptRecord {
            name: "codemod".to_string(),
            source_path: "../harn-stdlib/src/stdlib/cli/codemod.harn".to_string(),
            source_sha256: "a".repeat(64),
            artifact: None,
            skip_reason: Some("runtime only".to_string()),
        };
        assert!(manifest(vec![skipped]).validate_shape().is_ok());
    }

    #[test]
    fn canonical_manifest_serialization_is_stable_and_newline_terminated() {
        let value = manifest(vec![compiled_script("doctor")]);
        let first = canonical_manifest_bytes(&value).expect("serialize manifest");
        let second = canonical_manifest_bytes(&value).expect("serialize manifest again");
        assert_eq!(first, second);
        assert_eq!(first.last(), Some(&b'\n'));
    }

    #[test]
    fn source_hash_is_line_ending_stable() {
        assert_eq!(
            sha256_source_bytes(b"fn main() {\n  return\n}\n"),
            sha256_source_bytes(b"fn main() {\r\n  return\r\n}\r\n")
        );
    }

    #[test]
    fn source_file_hash_is_line_ending_stable() {
        let dir =
            std::env::temp_dir().join(format!("harn-cli-aot-manifest-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create temp fixture dir");
        let lf = dir.join("lf.harn");
        let crlf = dir.join("crlf.harn");
        fs::write(&lf, b"const value = 1\n").expect("write lf");
        fs::write(&crlf, b"const value = 1\r\n").expect("write crlf");

        assert_eq!(
            sha256_source_file(&lf).expect("hash lf"),
            sha256_source_file(&crlf).expect("hash crlf")
        );
        let _ = fs::remove_dir_all(&dir);
    }
}
