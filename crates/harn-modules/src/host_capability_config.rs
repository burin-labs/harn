//! Project settings for declared and served host operations.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::host_capabilities::{parse_host_capability_document, HostCapabilitySurface};

/// The host-operation fields under `[check]` in `harn.toml`.
#[derive(Debug, Default, Clone, Deserialize)]
pub struct HostCapabilityConfig {
    #[serde(default)]
    pub host_capabilities: HashMap<String, Vec<String>>,
    #[serde(default, alias = "host_capabilities_file")]
    pub host_capabilities_path: Option<String>,
    /// JSON or TOML file that lists the operations the target host serves.
    #[serde(default, alias = "host_served_capabilities_file")]
    pub host_served_capabilities_path: Option<String>,
    /// Exact declared operations whose handlers are added at runtime.
    #[serde(default)]
    pub runtime_installed_host_operations: Vec<String>,
    /// Stop ACP startup when the connected host does not serve a declaration.
    #[serde(default)]
    pub require_declared_operations_served: bool,
}

impl HostCapabilityConfig {
    #[must_use]
    pub fn with_paths_from(mut self, manifest_dir: &Path) -> Self {
        absolutize(&mut self.host_capabilities_path, manifest_dir);
        absolutize(&mut self.host_served_capabilities_path, manifest_dir);
        self
    }
}

fn absolutize(path: &mut Option<String>, manifest_dir: &Path) {
    let Some(value) = path.as_ref() else {
        return;
    };
    let candidate = PathBuf::from(value);
    if !candidate.is_absolute() {
        *path = Some(manifest_dir.join(candidate).display().to_string());
    }
}

#[derive(Default, Deserialize)]
struct ProjectManifest {
    #[serde(default)]
    check: HostCapabilityConfig,
}

/// Load host-operation settings from `<project_root>/harn.toml`.
pub fn load_host_capability_config(project_root: &Path) -> Result<HostCapabilityConfig, String> {
    let path = project_root.join("harn.toml");
    let content = match std::fs::read_to_string(&path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(HostCapabilityConfig::default());
        }
        Err(error) => return Err(format!("failed to read `{}`: {error}", path.display())),
    };
    let manifest = toml::from_str::<ProjectManifest>(&content)
        .map_err(|error| format!("failed to parse `{}`: {error}", path.display()))?;
    Ok(manifest.check.with_paths_from(project_root))
}

/// One resolved declaration snapshot shared by static and runtime checks.
pub struct ResolvedHostCapabilityConfig {
    pub declared: HostCapabilitySurface,
    pub source_content: Option<String>,
    pub source_value: Option<serde_json::Value>,
    pub error: Option<String>,
}

pub fn resolve_host_capability_config(
    config: &HostCapabilityConfig,
) -> ResolvedHostCapabilityConfig {
    let declared = HostCapabilitySurface::from_pairs(config.host_capabilities.iter().flat_map(
        |(capability, operations)| {
            operations
                .iter()
                .map(move |operation| (capability.as_str(), operation.as_str()))
        },
    ));
    let mut resolved = ResolvedHostCapabilityConfig {
        declared,
        source_content: None,
        source_value: None,
        error: None,
    };
    let Some(path) = config.host_capabilities_path.as_deref() else {
        return resolved;
    };
    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(error) => {
            resolved.error = Some(format!(
                "failed to read declared host operations from `{path}`: {error}"
            ));
            return resolved;
        }
    };
    resolved.source_content = Some(content.clone());
    match parse_host_capability_document(&content, path, "declared") {
        Ok(value) => {
            resolved
                .declared
                .extend(HostCapabilitySurface::from_value(&value));
            resolved.source_value = Some(value);
        }
        Err(error) => resolved.error = Some(error),
    }
    resolved
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_loader_keeps_host_settings_in_one_shared_shape() {
        let project = tempfile::tempdir().unwrap();
        std::fs::write(
            project.path().join("harn.toml"),
            r#"
[check]
host_capabilities.workspace = ["read_text"]
host_capabilities_path = "declared.json"
host_served_capabilities_path = "served.json"
runtime_installed_host_operations = ["runtime.prompt_content"]
require_declared_operations_served = true
"#,
        )
        .unwrap();

        let config = load_host_capability_config(project.path()).unwrap();
        assert_eq!(config.host_capabilities["workspace"], ["read_text"]);
        assert!(config
            .host_capabilities_path
            .as_deref()
            .unwrap()
            .ends_with("declared.json"));
        assert!(config
            .host_served_capabilities_path
            .as_deref()
            .unwrap()
            .ends_with("served.json"));
        assert_eq!(
            config.runtime_installed_host_operations,
            ["runtime.prompt_content"]
        );
        assert!(config.require_declared_operations_served);
    }

    #[test]
    fn malformed_declaration_keeps_source_for_cache_keys() {
        let project = tempfile::tempdir().unwrap();
        let path = project.path().join("declared.json");
        std::fs::write(&path, "not JSON or TOML").unwrap();
        let config = HostCapabilityConfig {
            host_capabilities_path: Some(path.display().to_string()),
            ..Default::default()
        };

        let resolved = resolve_host_capability_config(&config);

        assert_eq!(resolved.source_content.as_deref(), Some("not JSON or TOML"));
        assert!(resolved.source_value.is_none());
        assert!(resolved.error.is_some());
    }
}
