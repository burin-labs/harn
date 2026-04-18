use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::llm::{execute_llm_call, extract_llm_options, vm_value_to_json};
use crate::stdlib::json_to_vm_value;
use crate::value::{ErrorCategory, VmError, VmValue};
use crate::vm::Vm;

use super::process::resolve_source_relative_path;
use super::project::project_scan_config_value;
use super::template::render_template_result;

const STANDARD_VENDOR_DIRS: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    ".venv",
    "__pycache__",
    "build",
    "dist",
    "node_modules",
    "target",
    "venv",
];
const MAX_CONTEXT_FILES: usize = 12;
const MAX_SOURCE_FILES: usize = 8;
const MAX_FILE_CHARS: usize = 4_000;
const MAX_TOTAL_CONTEXT_CHARS: usize = 24_000;
const DEFAULT_BUDGET_TOKENS: i64 = 4_000;

#[derive(Debug, Clone)]
struct ProjectEnrichOptions {
    base_evidence: Option<VmValue>,
    prompt: String,
    schema: VmValue,
    budget_tokens: i64,
    model: String,
    provider: String,
    cache_key: String,
    cache_dir: Option<String>,
    schema_retries: usize,
}

#[derive(Debug, Clone)]
struct RelevantFile {
    rel_path: String,
    content: String,
    truncated: bool,
    digest: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CacheRecord {
    result: serde_json::Value,
}

pub(crate) fn register_project_enrich_builtin(vm: &mut Vm) {
    vm.register_async_builtin("project_enrich_native", |args| async move {
        project_enrich_impl(args).await
    });
}

async fn project_enrich_impl(args: Vec<VmValue>) -> Result<VmValue, VmError> {
    let path = args
        .first()
        .map(VmValue::display)
        .unwrap_or_else(|| ".".to_string());
    let root = resolve_existing_directory(&path)?;
    let options = parse_project_enrich_options(args.get(1))?;
    let base_evidence = options
        .base_evidence
        .clone()
        .unwrap_or_else(|| project_scan_config_value(&root));
    let base_dict = base_evidence.as_dict().ok_or_else(|| {
        VmError::Thrown(VmValue::String(Rc::from(
            "project.enrich: base_evidence must be a dict",
        )))
    })?;

    let relevant_files = collect_relevant_files(&root, &base_evidence);
    let bindings = enrichment_bindings(&root, &base_evidence, &relevant_files);
    let rendered_prompt = render_template_result(&options.prompt, Some(&bindings), None, None)
        .map_err(VmError::from)?;
    let schema_hash = sha256_hex(canonical_json(&vm_value_to_json(&options.schema)));
    let prompt_hash = sha256_hex(rendered_prompt.as_bytes());
    let content_hash = hash_relevant_files(&relevant_files);
    let cache_path = cache_file_path(
        &root,
        options.cache_dir.as_deref(),
        &options.cache_key,
        &root,
        &schema_hash,
        &prompt_hash,
        &content_hash,
    );

    if let Some(cached) = read_cached_result(&cache_path)? {
        return Ok(result_with_cached_flag(
            json_to_vm_value(&cached.result),
            true,
        ));
    }

    let estimated_input_tokens = estimate_tokens(&rendered_prompt)
        + estimate_tokens(&canonical_json(&vm_value_to_json(&options.schema)));
    if estimated_input_tokens > options.budget_tokens {
        let mut budget_result = (*base_dict).clone();
        budget_result.insert("budget_exceeded".to_string(), VmValue::Bool(true));
        budget_result.insert(
            "_provenance".to_string(),
            provenance_value(None, estimated_input_tokens, 0, false),
        );
        return Ok(VmValue::Dict(Rc::new(budget_result)));
    }

    let llm_options_value = llm_options_value(&options, &rendered_prompt);
    let extracted = extract_llm_options(&[
        VmValue::String(Rc::from(rendered_prompt.as_str())),
        VmValue::Nil,
        llm_options_value.clone(),
    ])?;
    match execute_llm_call(extracted, llm_options_value.as_dict().cloned()).await {
        Ok(response) => {
            let response_dict = response.as_dict().ok_or_else(|| {
                VmError::Thrown(VmValue::String(Rc::from(
                    "project.enrich: expected llm response dict",
                )))
            })?;
            let model = response_dict.get("model").map(VmValue::display);
            let input_tokens = response_dict
                .get("input_tokens")
                .and_then(VmValue::as_int)
                .unwrap_or(estimated_input_tokens);
            let output_tokens = response_dict
                .get("output_tokens")
                .and_then(VmValue::as_int)
                .unwrap_or(0);
            let Some(data) = response_dict.get("data").cloned() else {
                return Ok(validation_envelope(
                    &base_evidence,
                    "LLM response did not contain structured data".to_string(),
                    model,
                    input_tokens,
                    output_tokens,
                ));
            };
            let final_result = attach_provenance(data, model, input_tokens, output_tokens, false);
            write_cached_result(&cache_path, &final_result)?;
            Ok(final_result)
        }
        Err(VmError::CategorizedError {
            message,
            category: ErrorCategory::SchemaValidation,
        }) => Ok(validation_envelope(
            &base_evidence,
            message,
            None,
            estimated_input_tokens,
            0,
        )),
        Err(error) => Err(error),
    }
}

fn parse_project_enrich_options(value: Option<&VmValue>) -> Result<ProjectEnrichOptions, VmError> {
    let dict = value.and_then(VmValue::as_dict).ok_or_else(|| {
        VmError::Thrown(VmValue::String(Rc::from(
            "project.enrich: options dict is required",
        )))
    })?;
    let prompt = dict
        .get("prompt")
        .and_then(value_as_string)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            VmError::Thrown(VmValue::String(Rc::from(
                "project.enrich: options.prompt must be a non-empty string",
            )))
        })?;
    let schema = dict.get("schema").cloned().ok_or_else(|| {
        VmError::Thrown(VmValue::String(Rc::from(
            "project.enrich: options.schema is required",
        )))
    })?;
    let budget_tokens = dict
        .get("budget_tokens")
        .and_then(VmValue::as_int)
        .unwrap_or(DEFAULT_BUDGET_TOKENS)
        .max(0);
    let model = dict
        .get("model")
        .and_then(value_as_string)
        .unwrap_or_else(|| "auto".to_string());
    let provider = dict
        .get("provider")
        .and_then(value_as_string)
        .unwrap_or_else(|| "auto".to_string());
    let cache_key = dict
        .get("cache_key")
        .and_then(value_as_string)
        .unwrap_or_else(|| "default".to_string());
    let cache_dir = dict.get("cache_dir").and_then(value_as_string);
    let schema_retries = dict
        .get("schema_retries")
        .and_then(VmValue::as_int)
        .unwrap_or(1)
        .max(0) as usize;
    let base_evidence = dict.get("base_evidence").cloned();
    Ok(ProjectEnrichOptions {
        base_evidence,
        prompt,
        schema,
        budget_tokens,
        model,
        provider,
        cache_key,
        cache_dir,
        schema_retries,
    })
}

fn resolve_existing_directory(path: &str) -> Result<PathBuf, VmError> {
    let resolved = resolve_source_relative_path(path);
    let target = if resolved.is_dir() {
        resolved
    } else {
        resolved
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."))
    };
    if target.exists() {
        target.canonicalize().map_err(|error| {
            VmError::Thrown(VmValue::String(Rc::from(format!(
                "project.enrich: failed to resolve path: {error}"
            ))))
        })
    } else {
        Err(VmError::Thrown(VmValue::String(Rc::from(format!(
            "project.enrich: path does not exist: {}",
            target.display()
        )))))
    }
}

fn collect_relevant_files(root: &Path, base_evidence: &VmValue) -> Vec<RelevantFile> {
    let mut selected = BTreeSet::new();
    let mut files = Vec::new();
    let Some(dict) = base_evidence.as_dict() else {
        return files;
    };

    for key in ["anchors", "lockfiles"] {
        if let Some(values) = dict.get(key).and_then(value_as_list) {
            for entry in values {
                let name = entry.display().trim_end_matches('/').to_string();
                if name.is_empty() {
                    continue;
                }
                let path = root.join(&name);
                if path.is_file() {
                    selected.insert(name);
                }
            }
        }
    }

    for name in [
        "README.md",
        "README.MD",
        "README",
        "Readme.md",
        "Dockerfile",
        "GNUmakefile",
        "Makefile",
        "makefile",
        "package.json",
        "Cargo.toml",
        "pyproject.toml",
        "go.mod",
        "tsconfig.json",
        "next.config.js",
        "next.config.mjs",
        "next.config.ts",
        "setup.py",
        "requirements.txt",
        "Gemfile",
    ] {
        let path = root.join(name);
        if path.is_file() {
            selected.insert(name.to_string());
        }
    }

    let languages = dict
        .get("languages")
        .and_then(value_as_list)
        .map(|items| items.iter().map(VmValue::display).collect::<Vec<_>>())
        .unwrap_or_default();
    let source_files = collect_source_files(root, &languages);
    selected.extend(source_files);

    let mut total_chars = 0usize;
    for rel_path in selected.into_iter().take(MAX_CONTEXT_FILES) {
        let full_path = root.join(&rel_path);
        let Ok(content) = std::fs::read_to_string(&full_path) else {
            continue;
        };
        let truncated = content.chars().count() > MAX_FILE_CHARS;
        let trimmed = truncate_chars(&content, MAX_FILE_CHARS);
        if total_chars >= MAX_TOTAL_CONTEXT_CHARS {
            break;
        }
        total_chars += trimmed.chars().count();
        files.push(RelevantFile {
            rel_path,
            content: trimmed,
            truncated,
            digest: sha256_hex(content.as_bytes()),
        });
    }
    files
}

fn collect_source_files(root: &Path, languages: &[String]) -> Vec<String> {
    let exts = source_extensions(languages);
    if exts.is_empty() {
        return Vec::new();
    }
    let mut files = Vec::new();
    collect_source_files_recursive(root, root, &exts, &mut files);
    files.sort();
    files.truncate(MAX_SOURCE_FILES);
    files
}

fn collect_source_files_recursive(root: &Path, dir: &Path, exts: &[&str], files: &mut Vec<String>) {
    if files.len() >= MAX_SOURCE_FILES {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut children = entries.flatten().collect::<Vec<_>>();
    children.sort_by_key(|entry| entry.file_name());
    for child in children {
        let Ok(file_type) = child.file_type() else {
            continue;
        };
        let name = child.file_name().to_string_lossy().into_owned();
        if file_type.is_dir() {
            if name.starts_with('.') || STANDARD_VENDOR_DIRS.contains(&name.as_str()) {
                continue;
            }
            collect_source_files_recursive(root, &child.path(), exts, files);
            if files.len() >= MAX_SOURCE_FILES {
                return;
            }
            continue;
        }
        let matches_ext = child
            .path()
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| exts.contains(&ext));
        if !matches_ext {
            continue;
        }
        files.push(relative_posix(root, &child.path()));
        if files.len() >= MAX_SOURCE_FILES {
            return;
        }
    }
}

fn source_extensions(languages: &[String]) -> Vec<&'static str> {
    let mut exts = Vec::new();
    for language in languages {
        match language.as_str() {
            "rust" => push_unique_str(&mut exts, "rs"),
            "go" => push_unique_str(&mut exts, "go"),
            "python" => push_unique_str(&mut exts, "py"),
            "typescript" => {
                push_unique_str(&mut exts, "ts");
                push_unique_str(&mut exts, "tsx");
            }
            "javascript" => {
                push_unique_str(&mut exts, "js");
                push_unique_str(&mut exts, "jsx");
                push_unique_str(&mut exts, "mjs");
                push_unique_str(&mut exts, "cjs");
            }
            "ruby" => push_unique_str(&mut exts, "rb"),
            _ => {}
        }
    }
    exts
}

fn enrichment_bindings(
    root: &Path,
    base_evidence: &VmValue,
    files: &[RelevantFile],
) -> BTreeMap<String, VmValue> {
    let mut bindings = BTreeMap::new();
    bindings.insert(
        "path".to_string(),
        VmValue::String(Rc::from(root.to_string_lossy().into_owned())),
    );
    bindings.insert("base_evidence".to_string(), base_evidence.clone());
    bindings.insert("evidence".to_string(), base_evidence.clone());
    let file_values = files
        .iter()
        .map(|file| {
            let mut value = BTreeMap::new();
            value.insert(
                "path".to_string(),
                VmValue::String(Rc::from(file.rel_path.clone())),
            );
            value.insert(
                "content".to_string(),
                VmValue::String(Rc::from(file.content.clone())),
            );
            value.insert("truncated".to_string(), VmValue::Bool(file.truncated));
            VmValue::Dict(Rc::new(value))
        })
        .collect::<Vec<_>>();
    bindings.insert("files".to_string(), VmValue::List(Rc::new(file_values)));
    if let Some(dict) = base_evidence.as_dict() {
        for (key, value) in dict.iter() {
            bindings.entry(key.clone()).or_insert_with(|| value.clone());
        }
    }
    bindings
}

fn llm_options_value(options: &ProjectEnrichOptions, rendered_prompt: &str) -> VmValue {
    let mut llm_options = BTreeMap::new();
    llm_options.insert(
        "provider".to_string(),
        VmValue::String(Rc::from(options.provider.clone())),
    );
    llm_options.insert(
        "model".to_string(),
        VmValue::String(Rc::from(options.model.clone())),
    );
    llm_options.insert("output_schema".to_string(), options.schema.clone());
    llm_options.insert(
        "output_validation".to_string(),
        VmValue::String(Rc::from("error")),
    );
    llm_options.insert(
        "schema_retries".to_string(),
        VmValue::Int(options.schema_retries as i64),
    );
    llm_options.insert(
        "response_format".to_string(),
        VmValue::String(Rc::from("json")),
    );
    llm_options.insert(
        "messages".to_string(),
        VmValue::List(Rc::new(vec![json_to_vm_value(&serde_json::json!({
            "role": "user",
            "content": rendered_prompt,
        }))])),
    );
    VmValue::Dict(Rc::new(llm_options))
}

fn validation_envelope(
    base_evidence: &VmValue,
    message: String,
    model: Option<String>,
    input_tokens: i64,
    output_tokens: i64,
) -> VmValue {
    let mut dict = BTreeMap::new();
    dict.insert("base_evidence".to_string(), base_evidence.clone());
    dict.insert(
        "validation_error".to_string(),
        VmValue::String(Rc::from(message)),
    );
    dict.insert(
        "_provenance".to_string(),
        provenance_value(model, input_tokens, output_tokens, false),
    );
    VmValue::Dict(Rc::new(dict))
}

fn attach_provenance(
    data: VmValue,
    model: Option<String>,
    input_tokens: i64,
    output_tokens: i64,
    cached: bool,
) -> VmValue {
    match data {
        VmValue::Dict(dict) => {
            let mut merged = (*dict).clone();
            merged.insert(
                "_provenance".to_string(),
                provenance_value(model, input_tokens, output_tokens, cached),
            );
            VmValue::Dict(Rc::new(merged))
        }
        other => {
            let mut wrapped = BTreeMap::new();
            wrapped.insert("data".to_string(), other);
            wrapped.insert(
                "_provenance".to_string(),
                provenance_value(model, input_tokens, output_tokens, cached),
            );
            VmValue::Dict(Rc::new(wrapped))
        }
    }
}

fn provenance_value(
    model: Option<String>,
    input_tokens: i64,
    output_tokens: i64,
    cached: bool,
) -> VmValue {
    let mut tokens = BTreeMap::new();
    tokens.insert("in".to_string(), VmValue::Int(input_tokens));
    tokens.insert("out".to_string(), VmValue::Int(output_tokens));

    let mut provenance = BTreeMap::new();
    provenance.insert(
        "model".to_string(),
        model
            .map(|value| VmValue::String(Rc::from(value)))
            .unwrap_or(VmValue::Nil),
    );
    provenance.insert("tokens".to_string(), VmValue::Dict(Rc::new(tokens)));
    provenance.insert("cached".to_string(), VmValue::Bool(cached));
    VmValue::Dict(Rc::new(provenance))
}

fn result_with_cached_flag(value: VmValue, cached: bool) -> VmValue {
    let Some(dict) = value.as_dict() else {
        return value;
    };
    let mut merged = (*dict).clone();
    let mut provenance = merged
        .get("_provenance")
        .and_then(VmValue::as_dict)
        .map(|value| (*value).clone())
        .unwrap_or_default();
    provenance.insert("cached".to_string(), VmValue::Bool(cached));
    merged.insert(
        "_provenance".to_string(),
        VmValue::Dict(Rc::new(provenance)),
    );
    VmValue::Dict(Rc::new(merged))
}

fn hash_relevant_files(files: &[RelevantFile]) -> String {
    let joined = files
        .iter()
        .map(|file| format!("{}:{}", file.rel_path, file.digest))
        .collect::<Vec<_>>()
        .join("|");
    sha256_hex(joined.as_bytes())
}

fn cache_file_path(
    root: &Path,
    cache_dir: Option<&str>,
    cache_key: &str,
    path: &Path,
    schema_hash: &str,
    prompt_hash: &str,
    content_hash: &str,
) -> PathBuf {
    let cache_root = cache_dir
        .map(resolve_source_relative_path)
        .unwrap_or_else(|| root.join(".harn/cache/enrichment"));
    let identity = serde_json::json!({
        "cache_key": cache_key,
        "path": path.to_string_lossy(),
        "schema_hash": schema_hash,
        "prompt_hash": prompt_hash,
        "content_hash": content_hash,
    });
    cache_root.join(format!(
        "{}.json",
        sha256_hex(canonical_json(&identity).as_bytes())
    ))
}

fn read_cached_result(path: &Path) -> Result<Option<CacheRecord>, VmError> {
    let Ok(content) = std::fs::read_to_string(path) else {
        return Ok(None);
    };
    serde_json::from_str::<CacheRecord>(&content)
        .map(Some)
        .map_err(|error| {
            VmError::Thrown(VmValue::String(Rc::from(format!(
                "project.enrich: failed to parse cache {}: {error}",
                path.display()
            ))))
        })
}

fn write_cached_result(path: &Path, value: &VmValue) -> Result<(), VmError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            VmError::Thrown(VmValue::String(Rc::from(format!(
                "project.enrich: failed to create cache dir {}: {error}",
                parent.display()
            ))))
        })?;
    }
    let record = CacheRecord {
        result: vm_value_to_json(value),
    };
    let serialized = serde_json::to_string_pretty(&record).map_err(|error| {
        VmError::Thrown(VmValue::String(Rc::from(format!(
            "project.enrich: failed to serialize cache record: {error}"
        ))))
    })?;
    std::fs::write(path, serialized).map_err(|error| {
        VmError::Thrown(VmValue::String(Rc::from(format!(
            "project.enrich: failed to write cache {}: {error}",
            path.display()
        ))))
    })
}

fn relative_posix(base: &Path, path: &Path) -> String {
    match path.strip_prefix(base) {
        Ok(rel) => rel.to_string_lossy().replace('\\', "/"),
        Err(_) => path.to_string_lossy().replace('\\', "/"),
    }
}

fn estimate_tokens(text: &str) -> i64 {
    ((text.chars().count() as i64) + 3) / 4
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    text.chars().take(max_chars).collect()
}

fn canonical_json(value: &serde_json::Value) -> String {
    serde_json::to_string(value).unwrap_or_default()
}

fn sha256_hex(data: impl AsRef<[u8]>) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data.as_ref());
    let digest = hasher.finalize();
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write;
        let _ = write!(out, "{byte:02x}");
    }
    out
}

fn push_unique_str<'a>(items: &mut Vec<&'a str>, value: &'a str) {
    if !items.contains(&value) {
        items.push(value);
    }
}

fn value_as_string(value: &VmValue) -> Option<String> {
    match value {
        VmValue::String(text) => Some(text.to_string()),
        _ => None,
    }
}

fn value_as_list(value: &VmValue) -> Option<&[VmValue]> {
    match value {
        VmValue::List(items) => Some(items.as_slice()),
        _ => None,
    }
}

#[cfg(test)]
fn value_as_bool(value: &VmValue) -> Option<bool> {
    match value {
        VmValue::Bool(flag) => Some(*flag),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimate_tokens_uses_simple_char_budget() {
        assert_eq!(estimate_tokens(""), 0);
        assert_eq!(estimate_tokens("abcd"), 1);
        assert_eq!(estimate_tokens("abcde"), 2);
    }

    #[test]
    fn attach_provenance_wraps_non_dict_results() {
        let result = attach_provenance(
            VmValue::String(Rc::from("hi")),
            Some("mock-model".to_string()),
            10,
            4,
            false,
        );
        let dict = result.as_dict().expect("dict");
        assert_eq!(
            dict.get("data").map(VmValue::display).as_deref(),
            Some("hi")
        );
        assert_eq!(
            dict.get("_provenance")
                .and_then(VmValue::as_dict)
                .and_then(|value| value.get("cached"))
                .and_then(value_as_bool),
            Some(false)
        );
    }
}
