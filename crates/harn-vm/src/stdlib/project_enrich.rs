use crate::canonical_json;
use crate::value::VmDictExt;
use harn_kernel::pure::sha256_hex;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};
use serde_yaml_ng::Value as YamlValue;

use crate::llm::{execute_llm_call, extract_llm_options, vm_value_to_json};
use crate::runtime_limits::RuntimeLimits;
use crate::stdlib::json_to_vm_value;
use crate::value::{error_to_category, ErrorCategory, VmError, VmValue};
use crate::vm::Vm;

use super::fs::ignore_policy::BUILTIN_IGNORED_DIRS;
use super::process::resolve_source_relative_path;
use super::project::project_scan_config_value;
use super::template::render_template_result;

mod evidence;
use evidence::*;

const MAX_CONTEXT_FILES: usize = 12;
const MAX_SOURCE_FILES: usize = 8;
const MAX_FILE_CHARS: usize = 4_000;
const MAX_TOTAL_CONTEXT_CHARS: usize = 24_000;
const DEFAULT_BUDGET_TOKENS: i64 = 4_000;
const PROJECT_ENRICH_YAML_MAX_DEPTH: usize = RuntimeLimits::DEFAULT.max_project_enrich_yaml_depth;

#[derive(Debug, Clone)]
struct ProjectEnrichOptions {
    base_evidence: Option<VmValue>,
    prompt: String,
    schema: VmValue,
    budget_tokens: i64,
    model: String,
    provider: String,
    temperature: Option<f64>,
    cache_key: String,
    cache_dir: Option<String>,
    schema_retries: usize,
    include_operator_meta: bool,
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
    vm.register_async_capability_method(
        harn_builtin_meta::CapabilityId::Project,
        "enrich",
        |ctx, args| async move { project_enrich_impl(Some(&ctx), args).await },
    );
}

async fn project_enrich_impl(
    ctx: Option<&crate::vm::AsyncBuiltinCtx>,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
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
    let enriched_evidence =
        augment_project_evidence(&root, &base_evidence, options.include_operator_meta);
    let base_dict = enriched_evidence.as_dict().ok_or_else(|| {
        VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
            "project.enrich: base_evidence must be a dict",
        )))
    })?;

    let relevant_files = collect_relevant_files(&root, &enriched_evidence);
    let bindings = enrichment_bindings(&root, &enriched_evidence, &relevant_files);
    let rendered_prompt = render_template_result(&options.prompt, Some(&bindings), None, None)
        .map_err(VmError::from)?;
    // `canonical_schema` is reused by the token estimate below.
    let canonical_schema = canonical_json::to_string(&vm_value_to_json(&options.schema));
    let canonical_evidence = canonical_json::to_string(&vm_value_to_json(&enriched_evidence));
    let schema_hash = sha256_hex(canonical_schema.as_bytes());
    let prompt_hash = sha256_hex(rendered_prompt.as_bytes());
    let content_hash = hash_relevant_files(&relevant_files);
    let evidence_hash = sha256_hex(canonical_evidence.as_bytes());
    let cache_path = cache_file_path(
        &root,
        options.cache_dir.as_deref(),
        &options.cache_key,
        &root,
        &schema_hash,
        &prompt_hash,
        &content_hash,
        &evidence_hash,
    );

    if let Some(cached) = read_cached_result(&cache_path)? {
        return Ok(result_with_cached_flag(
            json_to_vm_value(&cached.result),
            true,
        ));
    }

    let estimated_input_tokens =
        estimate_tokens(&rendered_prompt) + estimate_tokens(&canonical_schema);
    if estimated_input_tokens > options.budget_tokens {
        let mut budget_result = (*base_dict).clone();
        budget_result.insert(
            crate::value::intern_key("budget_exceeded"),
            VmValue::Bool(true),
        );
        budget_result.insert(
            crate::value::intern_key("_provenance"),
            provenance_value(None, estimated_input_tokens, 0, false),
        );
        return Ok(VmValue::dict(budget_result));
    }

    let llm_options_value = llm_options_value(&options, &rendered_prompt);
    let extracted = extract_llm_options(&[
        VmValue::String(arcstr::ArcStr::from(rendered_prompt.as_str())),
        VmValue::Nil,
        llm_options_value.clone(),
    ])?;
    match execute_llm_call(
        ctx,
        extracted,
        llm_options_value.as_dict().cloned(),
        None,
        None,
    )
    .await
    {
        Ok(response) => {
            let response_dict = response.as_dict().ok_or_else(|| {
                VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
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
                    &enriched_evidence,
                    "LLM response did not contain structured data".to_string(),
                    model,
                    input_tokens,
                    output_tokens,
                ));
            };
            let final_result = attach_ci_metadata(
                attach_provenance(data, model, input_tokens, output_tokens, false),
                &enriched_evidence,
            );
            write_cached_result(&cache_path, &final_result)?;
            Ok(final_result)
        }
        Err(error) if error_to_category(&error) == ErrorCategory::SchemaValidation => {
            Ok(validation_envelope(
                &enriched_evidence,
                crate::llm::llm_error_message(&error),
                None,
                estimated_input_tokens,
                0,
            ))
        }
        Err(error) => Err(error),
    }
}

fn parse_project_enrich_options(value: Option<&VmValue>) -> Result<ProjectEnrichOptions, VmError> {
    let dict = value.and_then(VmValue::as_dict).ok_or_else(|| {
        VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
            "project.enrich: options dict is required",
        )))
    })?;
    let prompt = dict
        .get("prompt")
        .and_then(value_as_string)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
                "project.enrich: options.prompt must be a non-empty string",
            )))
        })?;
    let schema = dict.get("schema").cloned().ok_or_else(|| {
        VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
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
    let temperature = dict.get("temperature").and_then(value_as_float);
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
    let include_operator_meta = dict
        .get("include_operator_meta")
        .and_then(value_as_bool)
        .unwrap_or(true);
    let base_evidence = dict.get("base_evidence").cloned();
    Ok(ProjectEnrichOptions {
        base_evidence,
        prompt,
        schema,
        budget_tokens,
        model,
        provider,
        temperature,
        cache_key,
        cache_dir,
        schema_retries,
        include_operator_meta,
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
            VmError::Thrown(VmValue::String(arcstr::ArcStr::from(format!(
                "project.enrich: failed to resolve path: {error}"
            ))))
        })
    } else {
        Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
            format!("project.enrich: path does not exist: {}", target.display()),
        ))))
    }
}

fn collect_relevant_files(root: &Path, base_evidence: &VmValue) -> Vec<RelevantFile> {
    let mut seen = BTreeSet::new();
    let mut selected = Vec::new();
    let mut files = Vec::new();
    let Some(dict) = base_evidence.as_dict() else {
        return files;
    };

    for rel_path in collect_operator_context_files(root) {
        push_unique_path(&mut selected, &mut seen, rel_path);
    }

    for key in ["anchors", "lockfiles"] {
        if let Some(values) = dict.get(key).and_then(value_as_list) {
            for entry in values {
                let name = entry.display().trim_end_matches('/').to_string();
                if name.is_empty() {
                    continue;
                }
                let path = root.join(&name);
                if path.is_file() {
                    push_unique_path(&mut selected, &mut seen, name);
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
            push_unique_path(&mut selected, &mut seen, name.to_string());
        }
    }

    let languages = dict
        .get("languages")
        .and_then(value_as_list)
        .map(|items| items.iter().map(VmValue::display).collect::<Vec<_>>())
        .unwrap_or_default();
    let source_files = collect_source_files(root, &languages);
    for source_file in source_files {
        push_unique_path(&mut selected, &mut seen, source_file);
    }

    let mut total_chars = 0usize;
    for rel_path in selected.into_iter().take(MAX_CONTEXT_FILES) {
        let full_path = root.join(&rel_path);
        let Ok(content) = std::fs::read_to_string(&full_path) else {
            continue;
        };
        let truncated = content.chars().count() > MAX_FILE_CHARS;
        let trimmed = crate::text::clip_end(&content, MAX_FILE_CHARS);
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
            if name.starts_with('.') || BUILTIN_IGNORED_DIRS.contains(&name.as_str()) {
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
) -> crate::value::DictMap {
    let mut bindings = crate::value::DictMap::new();
    bindings.put_str("path", root.to_string_lossy());
    bindings.insert(
        crate::value::intern_key("base_evidence"),
        base_evidence.clone(),
    );
    bindings.insert(crate::value::intern_key("evidence"), base_evidence.clone());
    let file_values = files
        .iter()
        .map(|file| {
            let mut value = crate::value::DictMap::new();
            value.put_str("path", file.rel_path.clone());
            value.put_str("content", file.content.clone());
            value.insert(
                crate::value::intern_key("truncated"),
                VmValue::Bool(file.truncated),
            );
            VmValue::dict(value)
        })
        .collect::<Vec<_>>();
    bindings.insert(
        crate::value::intern_key("files"),
        VmValue::List(std::sync::Arc::new(file_values)),
    );
    if let Some(dict) = base_evidence.as_dict() {
        for (key, value) in dict.iter() {
            bindings.entry(key.clone()).or_insert_with(|| value.clone());
        }
    }
    bindings
}

fn llm_options_value(options: &ProjectEnrichOptions, rendered_prompt: &str) -> VmValue {
    let mut llm_options = crate::value::DictMap::new();
    llm_options.put_str("provider", options.provider.clone());
    llm_options.put_str("model", options.model.clone());
    if let Some(temperature) = options.temperature {
        llm_options.insert(
            crate::value::intern_key("temperature"),
            VmValue::Float(temperature),
        );
    }
    let mut output = crate::value::DictMap::new();
    output.insert(crate::value::intern_key("schema"), options.schema.clone());
    output.put_str("validation", "error");
    llm_options.insert(crate::value::intern_key("output"), VmValue::dict(output));
    llm_options.insert(
        crate::value::intern_key("schema_retries"),
        VmValue::Int(options.schema_retries as i64),
    );
    llm_options.insert(
        crate::value::intern_key("messages"),
        VmValue::List(std::sync::Arc::new(vec![json_to_vm_value(
            &serde_json::json!({
                "role": "user",
                "content": rendered_prompt,
            }),
        )])),
    );
    VmValue::dict(llm_options)
}

fn validation_envelope(
    base_evidence: &VmValue,
    message: String,
    model: Option<String>,
    input_tokens: i64,
    output_tokens: i64,
) -> VmValue {
    let mut dict = crate::value::DictMap::new();
    dict.insert(
        crate::value::intern_key("base_evidence"),
        base_evidence.clone(),
    );
    dict.put_str("validation_error", message);
    dict.insert(
        crate::value::intern_key("_provenance"),
        provenance_value(model, input_tokens, output_tokens, false),
    );
    VmValue::dict(dict)
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
                crate::value::intern_key("_provenance"),
                provenance_value(model, input_tokens, output_tokens, cached),
            );
            VmValue::dict(merged)
        }
        other => {
            let mut wrapped = crate::value::DictMap::new();
            wrapped.insert(crate::value::intern_key("data"), other);
            wrapped.insert(
                crate::value::intern_key("_provenance"),
                provenance_value(model, input_tokens, output_tokens, cached),
            );
            VmValue::dict(wrapped)
        }
    }
}

fn attach_ci_metadata(value: VmValue, base_evidence: &VmValue) -> VmValue {
    let Some(ci_value) = base_evidence
        .as_dict()
        .and_then(|dict| dict.get("ci"))
        .cloned()
    else {
        return value;
    };
    let Some(dict) = value.as_dict() else {
        let mut wrapped = crate::value::DictMap::new();
        wrapped.insert(crate::value::intern_key("data"), value);
        wrapped.insert(crate::value::intern_key("ci"), ci_value);
        return VmValue::dict(wrapped);
    };
    let mut merged = (*dict).clone();
    merged.insert(crate::value::intern_key("ci"), ci_value);
    VmValue::dict(merged)
}

fn provenance_value(
    model: Option<String>,
    input_tokens: i64,
    output_tokens: i64,
    cached: bool,
) -> VmValue {
    let mut tokens = crate::value::DictMap::new();
    tokens.insert(crate::value::intern_key("in"), VmValue::Int(input_tokens));
    tokens.insert(crate::value::intern_key("out"), VmValue::Int(output_tokens));

    let mut provenance = crate::value::DictMap::new();
    provenance.insert(
        crate::value::intern_key("model"),
        model
            .map(|value| VmValue::String(arcstr::ArcStr::from(value)))
            .unwrap_or(VmValue::Nil),
    );
    provenance.insert(crate::value::intern_key("tokens"), VmValue::dict(tokens));
    provenance.insert(crate::value::intern_key("cached"), VmValue::Bool(cached));
    VmValue::dict(provenance)
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
    provenance.insert(crate::value::intern_key("cached"), VmValue::Bool(cached));
    merged.insert(
        crate::value::intern_key("_provenance"),
        VmValue::dict(provenance),
    );
    VmValue::dict(merged)
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
    evidence_hash: &str,
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
        "evidence_hash": evidence_hash,
    });
    cache_root.join(format!(
        "{}.json",
        sha256_hex(canonical_json::to_string(&identity).as_bytes())
    ))
}

fn read_cached_result(path: &Path) -> Result<Option<CacheRecord>, VmError> {
    let Ok(content) = std::fs::read_to_string(path) else {
        return Ok(None);
    };
    serde_json::from_str::<CacheRecord>(&content)
        .map(Some)
        .map_err(|error| {
            VmError::Thrown(VmValue::String(arcstr::ArcStr::from(format!(
                "project.enrich: failed to parse cache {}: {error}",
                path.display()
            ))))
        })
}

fn write_cached_result(path: &Path, value: &VmValue) -> Result<(), VmError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            VmError::Thrown(VmValue::String(arcstr::ArcStr::from(format!(
                "project.enrich: failed to create cache dir {}: {error}",
                parent.display()
            ))))
        })?;
    }
    let record = CacheRecord {
        result: vm_value_to_json(value),
    };
    let serialized = serde_json::to_string_pretty(&record).map_err(|error| {
        VmError::Thrown(VmValue::String(arcstr::ArcStr::from(format!(
            "project.enrich: failed to serialize cache record: {error}"
        ))))
    })?;
    std::fs::write(path, serialized).map_err(|error| {
        VmError::Thrown(VmValue::String(arcstr::ArcStr::from(format!(
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

fn value_as_float(value: &VmValue) -> Option<f64> {
    match value {
        VmValue::Float(number) => Some(*number),
        VmValue::Int(number) => Some(*number as f64),
        _ => None,
    }
}

fn value_as_bool(value: &VmValue) -> Option<bool> {
    match value {
        VmValue::Bool(flag) => Some(*flag),
        _ => None,
    }
}

// Git/gh integration tests use a mocked POSIX shell and run only on Unix.
#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn temp_dir(label: &str) -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix(&format!("harn-project-enrich-{label}-"))
            .tempdir()
            .expect("tempdir")
    }

    fn write_file(path: &Path, content: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create parent dirs");
        }
        std::fs::write(path, content).expect("write file");
    }

    fn run_git(root: &Path, args: &[&str]) {
        let mut command = Command::new("git");
        command.args(args).current_dir(root);
        clear_git_env(&mut command);
        let status = command.status().expect("run git");
        assert!(status.success(), "git {args:?} should succeed");
    }

    fn install_mock_gh(path: &Path) {
        write_file(
            path,
            r#"#!/bin/sh
if [ "$1" = "auth" ] && [ "$2" = "status" ]; then
  exit 0
fi
if [ "$1" = "api" ] && [ "$2" = "repos/acme/operator-demo/branches/main/protection" ]; then
  cat <<'JSON'
{"required_status_checks":{"strict":true,"contexts":["Format check","Rust (lint + test + conformance)"]},"enforce_admins":{"enabled":false},"required_pull_request_reviews":{"required_approving_review_count":2,"require_code_owner_reviews":true},"required_conversation_resolution":{"enabled":true},"allow_force_pushes":{"enabled":false},"allow_deletions":{"enabled":false}}
JSON
  exit 0
fi
if [ "$1" = "api" ] && [ "$2" = "repos/acme/operator-demo" ]; then
  cat <<'JSON'
{"allow_squash_merge":true,"allow_rebase_merge":false,"allow_merge_commit":false}
JSON
  exit 0
fi
exit 1
"#,
        );
        let mut perms = std::fs::metadata(path).expect("metadata").permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(path, perms).expect("chmod");
    }

    #[test]
    fn estimate_tokens_uses_simple_char_budget() {
        assert_eq!(estimate_tokens(""), 0);
        assert_eq!(estimate_tokens("abcd"), 1);
        assert_eq!(estimate_tokens("abcde"), 2);
    }

    #[test]
    fn lefthook_run_collection_preserves_sorted_depth_first_order() {
        let value: YamlValue = serde_yaml_ng::from_str(
            r"
commands:
  z:
    run: echo z
  a:
    run: echo a
  list:
    - run: echo list
",
        )
        .expect("yaml");
        let mut commands = Vec::new();

        collect_nested_run_commands(&value, &mut |command| commands.push(command))
            .expect("collect commands");

        assert_eq!(commands, vec!["echo a", "echo list", "echo z"]);
    }

    #[test]
    fn lefthook_run_collection_reports_yaml_depth_limit() {
        let mut run = serde_yaml_ng::Mapping::new();
        run.insert(
            YamlValue::String("run".to_string()),
            YamlValue::String("echo too-deep".to_string()),
        );
        let mut value = YamlValue::Mapping(run);
        for _ in 0..=PROJECT_ENRICH_YAML_MAX_DEPTH {
            value = YamlValue::Sequence(vec![value]);
        }

        let mut commands = Vec::new();
        let err = collect_nested_run_commands(&value, &mut |command| commands.push(command))
            .expect_err("depth limit");

        assert!(commands.is_empty());
        assert!(err.contains("YAML traversal depth exceeded"));
        assert!(err.contains(&format!("({PROJECT_ENRICH_YAML_MAX_DEPTH} levels)")));
    }

    #[test]
    fn attach_provenance_wraps_non_dict_results() {
        let result = attach_provenance(
            VmValue::String(arcstr::ArcStr::from("hi")),
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

    #[test]
    fn llm_options_value_forwards_temperature() {
        let options = ProjectEnrichOptions {
            base_evidence: None,
            prompt: "Return JSON.".to_string(),
            schema: VmValue::dict(crate::value::DictMap::new()),
            budget_tokens: 4000,
            model: "mock-model".to_string(),
            provider: "mock".to_string(),
            temperature: Some(0.25),
            cache_key: "cache-v1".to_string(),
            cache_dir: None,
            schema_retries: 1,
            include_operator_meta: true,
        };

        let llm_options = llm_options_value(&options, "rendered prompt");
        let dict = llm_options.as_dict().expect("dict");
        assert_eq!(dict.get("temperature").and_then(value_as_float), Some(0.25));
    }

    #[test]
    fn operator_meta_collects_workflows_hooks_manifests_and_merge_policy() {
        let dir = temp_dir("operator-meta");
        write_file(
            &dir.path().join(".github/workflows/ci.yml"),
            r"name: CI
jobs:
  fmt:
    name: Format check
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - run: cargo fmt --all -- --check
  rust:
    name: Rust (lint + test + conformance)
    runs-on: ubuntu-latest
    steps:
      - uses: Swatinem/rust-cache@v2
      - run: cargo nextest run --workspace
      - run: cargo clippy --workspace -- -D warnings
",
        );
        write_file(
            &dir.path().join(".githooks/pre-commit"),
            "#!/bin/sh\nset -e\ncargo fmt --all\ncargo clippy --workspace -- -D warnings\n",
        );
        write_file(
            &dir.path().join(".husky/pre-push"),
            "#!/bin/sh\n. \"$(dirname -- \"$0\")/_/husky.sh\"\nnpm test\n",
        );
        write_file(
            &dir.path().join(".pre-commit-config.yaml"),
            r"repos:
  - repo: local
    hooks:
      - id: lint
        name: local lint
        entry: cargo clippy
        stages: [pre-commit, pre-push]
",
        );
        write_file(
            &dir.path().join("lefthook.yml"),
            r"pre-push:
  commands:
    tests:
      run: cargo test --workspace
",
        );
        write_file(
            &dir.path().join(".github/CODEOWNERS"),
            "* @acme/core\n/docs/ @acme/docs\n",
        );
        write_file(
            &dir.path().join("CONTRIBUTING.md"),
            "Please squash merges before landing.\n",
        );
        write_file(
            &dir.path().join("Cargo.toml"),
            "[package]\nname = \"operator-demo\"\nversion = \"0.1.0\"\n",
        );
        write_file(&dir.path().join("Cargo.lock"), "# lock\n");
        write_file(
            &dir.path().join("package.json"),
            "{\"name\":\"operator-demo\"}\n",
        );
        write_file(&dir.path().join("package-lock.json"), "{}\n");

        run_git(dir.path(), &["init"]);
        run_git(dir.path(), &["branch", "-M", "main"]);
        run_git(
            dir.path(),
            &[
                "remote",
                "add",
                "origin",
                "https://github.com/acme/operator-demo.git",
            ],
        );

        let gh_path = dir.path().join("mock-gh");
        install_mock_gh(&gh_path);

        let gh_policy =
            probe_github_policy_with_gh(dir.path(), gh_path.to_str().expect("utf8 gh path"))
                .expect("gh policy");
        assert_eq!(gh_policy.required_approvals, Some(2));
        assert_eq!(gh_policy.squash_only, Some(true));

        let required_checks = gh_policy
            .required_checks
            .as_ref()
            .map(|checks| checks.iter().cloned().collect::<BTreeSet<_>>())
            .expect("required checks");
        let workflow_scan = collect_workflow_evidence(dir.path(), Some(&required_checks));
        assert_eq!(workflow_scan.workflows.len(), 1);
        assert_eq!(workflow_scan.workflows[0].jobs.len(), 2);
        assert_eq!(
            workflow_scan.workflows[0].jobs[0].classifications,
            vec!["lint".to_string()]
        );
        assert_eq!(
            workflow_scan.workflows[0].jobs[0].required_check,
            Some(true)
        );
        assert_eq!(
            workflow_scan.workflows[0].jobs[1].classifications,
            vec!["lint".to_string(), "test".to_string()]
        );
        assert_eq!(
            workflow_scan.workflows[0].jobs[1].required_check,
            Some(true)
        );

        let hooks = collect_hook_evidence(dir.path());
        assert!(hooks.providers.contains(&".githooks".to_string()));
        assert!(hooks.providers.contains(&"pre-commit".to_string()));
        assert!(hooks.providers.contains(&"lefthook".to_string()));
        assert!(hooks.providers.contains(&"husky".to_string()));
        assert!(hooks
            .stages
            .get("pre-commit")
            .is_some_and(|commands| commands.contains(&"cargo fmt --all".to_string())));
        assert!(hooks
            .stages
            .get("pre-push")
            .is_some_and(|commands| commands.contains(&"cargo test --workspace".to_string())));

        let manifests = collect_package_manifest_evidence(dir.path(), &workflow_scan.combined_text);
        let cargo = manifests
            .iter()
            .find(|manifest| manifest.ecosystem == "cargo")
            .expect("cargo manifest");
        assert!(cargo.has_lockfile);
        assert!(cargo
            .ci_hints
            .contains(&"cargo-nextest installed".to_string()));
        assert!(cargo.ci_hints.contains(&"rust-cache action".to_string()));

        let merge_policy = collect_merge_policy_evidence(dir.path(), Some(gh_policy));
        assert_eq!(merge_policy.branch.as_deref(), Some("main"));
        assert_eq!(merge_policy.required_approvals, Some(2));
        assert_eq!(merge_policy.squash_only, Some(true));
        assert_eq!(
            merge_policy.required_checks.as_deref(),
            Some(
                &[
                    "Format check".to_string(),
                    "Rust (lint + test + conformance)".to_string(),
                ][..]
            )
        );
        assert!(merge_policy
            .codeowner_rules
            .iter()
            .any(|rule| rule.path == "*" && rule.owners == vec!["@acme/core".to_string()]));
        assert!(merge_policy
            .merge_method_hints
            .contains(&"squash".to_string()));
    }

    #[test]
    fn collect_relevant_files_prioritizes_operator_files() {
        let dir = temp_dir("context");
        write_file(
            &dir.path().join(".github/workflows/ci.yml"),
            "name: CI\njobs: {}\n",
        );
        write_file(
            &dir.path().join(".githooks/pre-commit"),
            "#!/bin/sh\ncargo fmt --all\n",
        );
        write_file(&dir.path().join("CONTRIBUTING.md"), "Use squash merges.\n");
        write_file(
            &dir.path().join("Cargo.toml"),
            "[package]\nname = \"context\"\nversion = \"0.1.0\"\n",
        );
        write_file(&dir.path().join("Cargo.lock"), "# lock\n");
        write_file(
            &dir.path().join("src/lib.rs"),
            "pub fn greet() -> &'static str { \"hi\" }\n",
        );

        let base = VmValue::dict(crate::value::DictMap::from_iter([
            (
                crate::value::intern_key("languages"),
                VmValue::List(std::sync::Arc::new(vec![VmValue::String(
                    arcstr::ArcStr::from("rust"),
                )])),
            ),
            (
                crate::value::intern_key("anchors"),
                VmValue::List(std::sync::Arc::new(vec![VmValue::String(
                    arcstr::ArcStr::from("Cargo.toml"),
                )])),
            ),
            (
                crate::value::intern_key("lockfiles"),
                VmValue::List(std::sync::Arc::new(vec![VmValue::String(
                    arcstr::ArcStr::from("Cargo.lock"),
                )])),
            ),
        ]));
        let files = collect_relevant_files(dir.path(), &base);
        let paths = files
            .iter()
            .map(|file| file.rel_path.clone())
            .collect::<Vec<_>>();
        assert!(paths.contains(&".github/workflows/ci.yml".to_string()));
        assert!(paths.contains(&".githooks/pre-commit".to_string()));
        assert!(paths.contains(&"CONTRIBUTING.md".to_string()));
    }

    #[test]
    fn attach_ci_metadata_merges_ci_block_into_result() {
        let base = VmValue::dict(crate::value::DictMap::from_iter([(
            crate::value::intern_key("ci"),
            VmValue::dict(crate::value::DictMap::from_iter([(
                crate::value::intern_key("merge_policy"),
                VmValue::dict(crate::value::DictMap::from_iter([(
                    crate::value::intern_key("squash_only"),
                    VmValue::Bool(true),
                )])),
            )])),
        )]));
        let result = attach_ci_metadata(
            VmValue::dict(crate::value::DictMap::from_iter([(
                crate::value::intern_key("summary"),
                VmValue::String(arcstr::ArcStr::from("ok")),
            )])),
            &base,
        );
        let dict = result.as_dict().expect("dict");
        assert_eq!(
            dict.get("ci")
                .and_then(VmValue::as_dict)
                .and_then(|ci| ci.get("merge_policy"))
                .and_then(VmValue::as_dict)
                .and_then(|policy| policy.get("squash_only"))
                .and_then(value_as_bool),
            Some(true)
        );
    }

    #[test]
    fn collect_workflow_jobs_sorts_by_string_key() {
        // serde_yaml_ng is Value-keyed, so job ids are `YamlValue`s, not
        // `String`s. The collector sorts via `as_str().cmp(..)`; this pins the
        // deterministic ordering that the old String-keyed `sort_by_key` gave.
        let workflow: YamlValue = serde_yaml_ng::from_str(
            "jobs:\n  zeta:\n    steps: []\n  alpha:\n    steps: []\n  mid:\n    steps: []\n",
        )
        .expect("parse workflow yaml");
        let jobs = workflow
            .get("jobs")
            .and_then(YamlValue::as_mapping)
            .expect("jobs mapping");
        let evidence = collect_workflow_jobs(jobs, "CI", None);
        let ids = evidence.iter().map(|j| j.id.as_str()).collect::<Vec<_>>();
        assert_eq!(ids, vec!["alpha", "mid", "zeta"]);
    }

    #[test]
    fn lefthook_non_string_stage_key_does_not_crash() {
        // A Value-keyed mapping can carry a non-string top-level key (here a
        // number). The stage collector must skip it via its `as_str()` guard
        // rather than panic, while still harvesting valid hyphenated stages.
        let content = "123:\n  commands:\n    lint:\n      run: echo numeric\n\
                       pre-commit:\n  commands:\n    fmt:\n      run: cargo fmt\n";
        let mut stages: BTreeMap<String, Vec<String>> = BTreeMap::new();
        collect_lefthook_hooks(content, &mut stages).expect("non-string stage key must not error");
        assert!(
            stages.contains_key("pre-commit"),
            "valid hyphenated stage must still be collected: {stages:?}",
        );
        assert!(
            stages["pre-commit"].iter().any(|c| c.contains("cargo fmt")),
            "expected the pre-commit run command: {:?}",
            stages["pre-commit"],
        );
    }
}
