use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{json, Value as JsonValue};

use crate::mcp_prompts::FilePromptCatalog;

#[derive(Clone, Debug, Default)]
pub(crate) struct McpContextCatalog {
    resources: Vec<McpContextResource>,
    prompt_catalog: FilePromptCatalog,
}

#[derive(Clone, Debug)]
struct McpContextResource {
    uri: String,
    name: String,
    title: Option<String>,
    description: Option<String>,
    mime_type: String,
    text: String,
}

impl McpContextCatalog {
    pub(crate) fn discover(script_path: &Path) -> Self {
        let project_root = project_root_for_script(script_path);
        let manifest_path = project_root.join("harn.toml");
        let manifest_source = fs::read_to_string(&manifest_path).unwrap_or_default();
        let prompt_catalog = FilePromptCatalog::discover(&project_root);

        let mut resources = Vec::new();
        if let Ok(source) = fs::read_to_string(script_path) {
            resources.push(McpContextResource {
                uri: "harn://package/source".to_string(),
                name: "Harn Source".to_string(),
                title: Some("Harn Source".to_string()),
                description: Some(format!(
                    "Source for {}",
                    display_relative_path(&project_root, script_path)
                )),
                mime_type: "text/x-harn".to_string(),
                text: source,
            });
        }

        if !manifest_source.is_empty() {
            resources.push(McpContextResource {
                uri: "harn://package/manifest".to_string(),
                name: "Package Manifest".to_string(),
                title: Some("Package Manifest".to_string()),
                description: Some("Nearest harn.toml manifest for this MCP server".to_string()),
                mime_type: "application/toml".to_string(),
                text: manifest_source,
            });
        }

        let readme_path = project_root.join("README.md");
        if let Ok(readme) = fs::read_to_string(&readme_path) {
            resources.push(McpContextResource {
                uri: "harn://package/readme".to_string(),
                name: "Package README".to_string(),
                title: Some("Package README".to_string()),
                description: Some("README.md from this Harn package".to_string()),
                mime_type: "text/markdown".to_string(),
                text: readme,
            });
        }

        for prompt in prompt_catalog.sources() {
            resources.push(McpContextResource {
                uri: format!("harn://prompt/{}/source", prompt.name),
                name: format!("Prompt {}", prompt.name),
                title: prompt.title,
                description: prompt
                    .description
                    .or_else(|| Some(format!("Source for prompt '{}'", prompt.name))),
                mime_type: "text/x-harn-prompt".to_string(),
                text: prompt.body,
            });
        }

        Self {
            resources,
            prompt_catalog,
        }
    }

    pub(crate) fn has_resources(&self) -> bool {
        !self.resources.is_empty()
    }

    pub(crate) fn has_prompts(&self) -> bool {
        !self.prompt_catalog.is_empty()
    }

    pub(crate) fn resource_entries(&self) -> Vec<JsonValue> {
        self.resources
            .iter()
            .map(McpContextResource::to_resource_entry)
            .collect()
    }

    pub(crate) fn read_resource(&self, uri: &str) -> Option<(String, String)> {
        self.resources
            .iter()
            .find(|resource| resource.uri == uri)
            .map(|resource| (resource.text.clone(), resource.mime_type.clone()))
    }

    pub(crate) fn resource_templates(&self) -> Vec<JsonValue> {
        let mut templates = Vec::new();
        if self
            .resources
            .iter()
            .any(|resource| resource.uri.starts_with("harn://package/"))
        {
            templates.push(json!({
                "uriTemplate": "harn://package/{artifact}",
                "name": "package-artifact",
                "title": "Package Artifact",
                "description": "Read package artifacts exposed by this Harn MCP server.",
                "mimeType": "application/octet-stream",
            }));
        }
        if !self.prompt_catalog.is_empty() {
            templates.push(json!({
                "uriTemplate": "harn://prompt/{name}/source",
                "name": "prompt-source",
                "title": "Prompt Source",
                "description": "Read the source for a file-backed Harn prompt.",
                "mimeType": "text/x-harn-prompt",
            }));
        }
        templates
    }

    pub(crate) fn prompt_entries(&self) -> Vec<JsonValue> {
        self.prompt_catalog.list()
    }

    pub(crate) fn get_prompt(
        &self,
        name: &str,
        arguments: &JsonValue,
    ) -> Result<JsonValue, String> {
        self.prompt_catalog.get(name, arguments)
    }

    pub(crate) fn complete_prompt(
        &self,
        name: &str,
        argument_name: &str,
        value: &str,
    ) -> Result<JsonValue, String> {
        self.prompt_catalog.complete(name, argument_name, value)
    }

    pub(crate) fn complete_resource_template(
        &self,
        uri_template: &str,
        argument_name: &str,
        value: &str,
    ) -> Result<JsonValue, String> {
        let candidates = match (uri_template, argument_name) {
            ("harn://package/{artifact}", "artifact") => self.package_artifacts(),
            ("harn://prompt/{name}/source", "name") => self.prompt_catalog.names(),
            ("harn://package/{artifact}", other) => {
                return Err(format!("Unknown package resource argument: {other}"));
            }
            ("harn://prompt/{name}/source", other) => {
                return Err(format!("Unknown prompt source argument: {other}"));
            }
            (other, _) => return Err(format!("Unknown resource template: {other}")),
        };
        Ok(harn_vm::mcp_protocol::completion_payload(candidates, value))
    }

    fn package_artifacts(&self) -> Vec<String> {
        self.resources
            .iter()
            .filter_map(|resource| resource.uri.strip_prefix("harn://package/"))
            .map(str::to_string)
            .collect()
    }
}

impl McpContextResource {
    fn to_resource_entry(&self) -> JsonValue {
        let mut entry = json!({
            "uri": self.uri,
            "name": self.name,
            "mimeType": self.mime_type,
        });
        if let Some(title) = &self.title {
            entry["title"] = json!(title);
        }
        if let Some(description) = &self.description {
            entry["description"] = json!(description);
        }
        entry
    }
}

fn project_root_for_script(script_path: &Path) -> PathBuf {
    // Resolve the project root via the shared walk, starting from the script's
    // parent (the script itself is a file, not a project directory). Keep this
    // caller's policy of falling back to that parent when the script lives
    // outside any project, so paths still resolve relative to the script.
    let start = script_path.parent().unwrap_or_else(|| Path::new("."));
    harn_modules::manifest_walk::find_project_root(start).unwrap_or_else(|| start.to_path_buf())
}

fn display_relative_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}
