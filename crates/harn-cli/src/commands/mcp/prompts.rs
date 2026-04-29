use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::{Component, Path, PathBuf};

use base64::Engine;
use serde::Deserialize;
use serde_json::{json, Value as JsonValue};

use crate::package::Manifest;

#[derive(Clone, Debug, Default)]
pub(crate) struct FilePromptCatalog {
    prompts: Vec<FilePrompt>,
}

#[derive(Clone, Debug)]
struct FilePrompt {
    name: String,
    title: Option<String>,
    description: Option<String>,
    arguments: Vec<FilePromptArgument>,
    path: PathBuf,
    body: String,
    images: Vec<PromptImage>,
}

#[derive(Clone, Debug)]
struct FilePromptArgument {
    name: String,
    description: Option<String>,
    required: bool,
}

#[derive(Clone, Debug)]
struct PromptImage {
    path: Option<PathBuf>,
    data: Option<String>,
    mime_type: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct PromptFrontMatter {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    arguments: Vec<PromptArgumentFrontMatter>,
    #[serde(default)]
    images: Vec<PromptImageFrontMatter>,
}

#[derive(Debug, Deserialize)]
struct PromptArgumentFrontMatter {
    name: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default = "default_required")]
    required: bool,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum PromptImageFrontMatter {
    Path(String),
    Config {
        #[serde(default)]
        path: Option<String>,
        #[serde(default)]
        data: Option<String>,
        #[serde(default, alias = "mimeType")]
        mime_type: Option<String>,
    },
}

#[derive(Clone, Debug)]
struct PromptCandidate {
    prompt: FilePrompt,
    fallback_name: String,
}

fn default_required() -> bool {
    true
}

impl FilePromptCatalog {
    pub(crate) fn discover(project_root: &Path, manifest_source: &str) -> Self {
        let mut candidates = Vec::new();
        collect_prompt_files(project_root, project_root, None, &mut candidates);

        for alias in dependency_aliases(manifest_source) {
            let package_root = project_root.join(".harn/packages").join(&alias);
            if package_root.is_dir() {
                collect_prompt_files(&package_root, &package_root, Some(&alias), &mut candidates);
            }
        }

        Self {
            prompts: resolve_prompt_name_collisions(candidates),
        }
    }

    pub(crate) fn list(&self) -> Vec<JsonValue> {
        self.prompts
            .iter()
            .map(|prompt| {
                let mut entry = json!({
                    "name": prompt.name,
                    "arguments": prompt
                        .arguments
                        .iter()
                        .map(|argument| {
                            let mut value = json!({
                                "name": argument.name,
                                "required": argument.required,
                            });
                            if let Some(description) = &argument.description {
                                value["description"] = json!(description);
                            }
                            value
                        })
                        .collect::<Vec<_>>(),
                });
                if let Some(title) = &prompt.title {
                    entry["title"] = json!(title);
                }
                if let Some(description) = &prompt.description {
                    entry["description"] = json!(description);
                }
                entry
            })
            .collect()
    }

    pub(crate) fn get(&self, name: &str, arguments: &JsonValue) -> Result<JsonValue, String> {
        let prompt = self
            .prompts
            .iter()
            .find(|prompt| prompt.name == name)
            .ok_or_else(|| format!("Unknown prompt: {name}"))?;
        prompt.render(arguments)
    }
}

impl FilePrompt {
    fn render(&self, arguments: &JsonValue) -> Result<JsonValue, String> {
        let object = arguments
            .as_object()
            .ok_or_else(|| "prompt arguments must be an object".to_string())?;
        for argument in &self.arguments {
            if argument.required && object.get(&argument.name).is_none_or(JsonValue::is_null) {
                return Err(format!("Missing required argument: {}", argument.name));
            }
        }

        let mut bindings = BTreeMap::new();
        for (key, value) in object {
            bindings.insert(key.clone(), harn_vm::json_to_vm_value(value));
        }

        let rendered = harn_vm::stdlib::template::render_template_to_string(
            &self.body,
            Some(&bindings),
            self.path.parent(),
            Some(&self.path),
        )?;

        let mut messages = vec![json!({
            "role": "user",
            "content": {
                "type": "text",
                "text": rendered,
            },
        })];

        for image in &self.images {
            messages.push(json!({
                "role": "user",
                "content": {
                    "type": "image",
                    "data": image_data(image, &self.path)?,
                    "mimeType": image_mime_type(image, &self.path),
                },
            }));
        }

        let mut result = json!({ "messages": messages });
        if let Some(description) = &self.description {
            result["description"] = json!(description);
        }
        Ok(result)
    }
}

fn collect_prompt_files(
    root: &Path,
    cursor: &Path,
    package_alias: Option<&str>,
    out: &mut Vec<PromptCandidate>,
) {
    let Ok(entries) = fs::read_dir(cursor) else {
        return;
    };
    let mut entries = entries.flatten().collect::<Vec<_>>();
    entries.sort_by_key(|entry| entry.path());
    for entry in entries {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            if should_skip_dir(root, &path, package_alias.is_some()) {
                continue;
            }
            collect_prompt_files(root, &path, package_alias, out);
        } else if file_type.is_file()
            && path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with(".harn.prompt"))
        {
            if let Some(candidate) = load_prompt_file(root, &path, package_alias) {
                out.push(candidate);
            }
        }
    }
}

fn should_skip_dir(root: &Path, path: &Path, in_package: bool) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    if matches!(
        name,
        ".git" | "target" | "node_modules" | "dist" | "portal-dist"
    ) {
        return true;
    }
    if !in_package && name == ".harn" {
        return true;
    }
    path != root && name.starts_with('.') && name != ".harn"
}

fn load_prompt_file(
    root: &Path,
    path: &Path,
    package_alias: Option<&str>,
) -> Option<PromptCandidate> {
    let text = fs::read_to_string(path).ok()?;
    let (front_matter, body) = split_front_matter(&text);
    let meta = front_matter
        .as_deref()
        .and_then(|source| toml::from_str::<PromptFrontMatter>(source).ok())
        .unwrap_or_default();
    let relative_stem = relative_prompt_stem(root, path);
    let fallback_name = match package_alias {
        Some(alias) => format!("{alias}/{relative_stem}"),
        None => relative_stem.clone(),
    };
    let preferred = meta
        .name
        .as_ref()
        .or(meta.id.as_ref())
        .filter(|value| !value.trim().is_empty())
        .map(|value| match package_alias {
            Some(alias) => format!("{alias}/{value}"),
            None => value.to_string(),
        })
        .unwrap_or_else(|| fallback_name.clone());

    let arguments = if meta.arguments.is_empty() {
        infer_arguments(body)
    } else {
        meta.arguments
            .into_iter()
            .map(|argument| FilePromptArgument {
                name: argument.name,
                description: argument.description,
                required: argument.required,
            })
            .collect()
    };

    let images = meta
        .images
        .into_iter()
        .map(|image| match image {
            PromptImageFrontMatter::Path(path) => PromptImage {
                path: Some(PathBuf::from(path)),
                data: None,
                mime_type: None,
            },
            PromptImageFrontMatter::Config {
                path,
                data,
                mime_type,
            } => PromptImage {
                path: path.map(PathBuf::from),
                data,
                mime_type,
            },
        })
        .collect();

    Some(PromptCandidate {
        prompt: FilePrompt {
            name: preferred,
            title: meta.title,
            description: meta.description,
            arguments,
            path: path.to_path_buf(),
            body: body.to_string(),
            images,
        },
        fallback_name,
    })
}

fn resolve_prompt_name_collisions(candidates: Vec<PromptCandidate>) -> Vec<FilePrompt> {
    let mut counts = HashMap::<String, usize>::new();
    for candidate in &candidates {
        *counts.entry(candidate.prompt.name.clone()).or_default() += 1;
    }

    let mut used = BTreeSet::new();
    candidates
        .into_iter()
        .map(|mut candidate| {
            if counts
                .get(&candidate.prompt.name)
                .copied()
                .unwrap_or_default()
                > 1
            {
                candidate.prompt.name = candidate.fallback_name;
            }
            if !used.insert(candidate.prompt.name.clone()) {
                let mut index = 2;
                let base = candidate.prompt.name.clone();
                while !used.insert(format!("{base}-{index}")) {
                    index += 1;
                }
                candidate.prompt.name = format!("{base}-{index}");
            }
            candidate.prompt
        })
        .collect()
}

fn split_front_matter(text: &str) -> (Option<String>, &str) {
    let Some(rest) = text.strip_prefix("---\n") else {
        return (None, text);
    };
    let Some(index) = rest.find("\n---\n") else {
        return (None, text);
    };
    let meta = rest[..index].to_string();
    let body = &rest[index + "\n---\n".len()..];
    (Some(meta), body)
}

fn relative_prompt_stem(root: &Path, path: &Path) -> String {
    let relative = path.strip_prefix(root).unwrap_or(path);
    let mut rendered = relative.to_string_lossy().replace('\\', "/");
    if let Some(stem) = rendered.strip_suffix(".harn.prompt") {
        rendered = stem.to_string();
    }
    rendered
}

fn infer_arguments(body: &str) -> Vec<FilePromptArgument> {
    let mut names = BTreeSet::new();
    for raw in body
        .split("{{")
        .skip(1)
        .filter_map(|part| part.split("}}").next())
    {
        let expr = raw.trim().trim_matches('-').trim();
        if expr.is_empty() || expr.starts_with('#') {
            continue;
        }
        if let Some(condition) = expr
            .strip_prefix("if ")
            .or_else(|| expr.strip_prefix("elif "))
        {
            names.extend(identifiers_in_expr(condition));
            continue;
        }
        if let Some(iterable) = expr
            .strip_prefix("for ")
            .and_then(|for_expr| for_expr.split_once(" in ").map(|(_, iterable)| iterable))
        {
            names.extend(identifiers_in_expr(iterable));
            continue;
        }
        if expr.starts_with("else")
            || expr.starts_with("end")
            || expr.starts_with("include ")
            || expr.starts_with("raw")
        {
            continue;
        }
        if let Some(name) = first_identifier(expr) {
            names.insert(name);
        }
    }
    names
        .into_iter()
        .map(|name| FilePromptArgument {
            name,
            description: None,
            required: true,
        })
        .collect()
}

fn first_identifier(expr: &str) -> Option<String> {
    let name = expr
        .split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_' || ch == '.'))
        .next()
        .unwrap_or_default()
        .split('.')
        .next()
        .unwrap_or_default();
    is_prompt_argument_identifier(name).then(|| name.to_string())
}

fn identifiers_in_expr(expr: &str) -> Vec<String> {
    expr.split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_' || ch == '.'))
        .filter_map(|token| token.split('.').next())
        .filter(|token| is_prompt_argument_identifier(token))
        .map(str::to_string)
        .collect()
}

fn is_prompt_argument_identifier(value: &str) -> bool {
    is_identifier(value)
        && !matches!(
            value,
            "true" | "false" | "nil" | "and" | "or" | "not" | "in" | "loop"
        )
}

fn is_identifier(value: &str) -> bool {
    let mut chars = value.chars();
    chars
        .next()
        .is_some_and(|ch| ch == '_' || ch.is_ascii_alphabetic())
        && chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
}

fn dependency_aliases(manifest_source: &str) -> Vec<String> {
    let Ok(manifest) = toml::from_str::<Manifest>(manifest_source) else {
        return Vec::new();
    };
    let mut aliases = manifest.dependencies.keys().cloned().collect::<Vec<_>>();
    aliases.sort();
    aliases
}

fn image_data(image: &PromptImage, prompt_path: &Path) -> Result<String, String> {
    if let Some(data) = &image.data {
        return Ok(data.clone());
    }
    let path = image
        .path
        .as_ref()
        .ok_or_else(|| "prompt image requires path or data".to_string())?;
    let resolved = resolve_prompt_relative_path(prompt_path, path)?;
    let bytes = fs::read(&resolved).map_err(|error| {
        format!(
            "failed to read prompt image {}: {error}",
            resolved.display()
        )
    })?;
    Ok(base64::engine::general_purpose::STANDARD.encode(bytes))
}

fn image_mime_type(image: &PromptImage, prompt_path: &Path) -> String {
    image.mime_type.clone().unwrap_or_else(|| {
        image
            .path
            .as_ref()
            .and_then(|path| {
                resolve_prompt_relative_path(prompt_path, path)
                    .ok()
                    .and_then(|resolved| mime_type_for_path(&resolved).map(str::to_string))
            })
            .unwrap_or_else(|| "application/octet-stream".to_string())
    })
}

fn resolve_prompt_relative_path(prompt_path: &Path, path: &Path) -> Result<PathBuf, String> {
    if path.is_absolute() {
        return Err("prompt image path must be relative to the prompt file".to_string());
    }
    let mut safe = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => safe.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(format!(
                    "prompt image path {} escapes the prompt directory",
                    path.display()
                ));
            }
        }
    }
    let base = prompt_path
        .parent()
        .ok_or_else(|| format!("prompt path {} has no parent", prompt_path.display()))?;
    Ok(base.join(safe))
}

fn mime_type_for_path(path: &Path) -> Option<&'static str> {
    match path.extension().and_then(|extension| extension.to_str()) {
        Some("png") => Some("image/png"),
        Some("jpg") | Some("jpeg") => Some("image/jpeg"),
        Some("gif") => Some("image/gif"),
        Some("webp") => Some("image/webp"),
        Some("svg") => Some("image/svg+xml"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write(path: &Path, text: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, text).unwrap();
    }

    #[test]
    fn discovers_and_renders_front_matter_prompt() {
        let temp = TempDir::new().unwrap();
        write(
            temp.path().join("harn.toml").as_path(),
            "[package]\nname = \"x\"\n",
        );
        write(
            temp.path().join("prompts/review.harn.prompt").as_path(),
            r#"---
id = "review"
description = "Review code"
[[arguments]]
name = "code"
description = "Code to review"
required = true
---
Review this: {{ code }}
"#,
        );

        let catalog = FilePromptCatalog::discover(temp.path(), "[package]\nname = \"x\"\n");
        assert_eq!(catalog.list()[0]["name"], "review");
        assert_eq!(
            catalog.list()[0]["arguments"][0]["description"],
            "Code to review"
        );

        let rendered = catalog
            .get("review", &json!({ "code": "fn main() {}" }))
            .unwrap();
        assert!(rendered["messages"][0]["content"]["text"]
            .as_str()
            .unwrap()
            .contains("fn main"));
    }

    #[test]
    fn prefixes_dependency_package_prompts() {
        let temp = TempDir::new().unwrap();
        let manifest = r#"[package]
name = "root"
[dependencies]
pack = { path = "../pack" }
"#;
        write(temp.path().join("harn.toml").as_path(), manifest);
        write(
            temp.path()
                .join(".harn/packages/pack/prompts/helper.harn.prompt")
                .as_path(),
            "Use {{ topic }}",
        );

        let catalog = FilePromptCatalog::discover(temp.path(), manifest);
        assert_eq!(catalog.list()[0]["name"], "pack/prompts/helper");
    }

    #[test]
    fn infers_arguments_from_template_expressions() {
        let args =
            infer_arguments("{{ if enabled }}{{ user.name }}{{ for item in items }}x{{ end }}");
        let names = args
            .into_iter()
            .map(|argument| argument.name)
            .collect::<Vec<_>>();
        assert_eq!(names, vec!["enabled", "items", "user"]);
    }

    #[test]
    fn prompt_images_cannot_escape_prompt_directory() {
        let temp = TempDir::new().unwrap();
        write(
            temp.path().join("harn.toml").as_path(),
            "[package]\nname = \"x\"\n",
        );
        write(
            temp.path().join("prompts/review.harn.prompt").as_path(),
            r#"---
id = "review"
images = [{ path = "../secret.png", mime_type = "image/png" }]
---
hello
"#,
        );

        let catalog = FilePromptCatalog::discover(temp.path(), "[package]\nname = \"x\"\n");
        let error = catalog.get("review", &json!({})).unwrap_err();
        assert!(error.contains("escapes the prompt directory"));
    }
}
