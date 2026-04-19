use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::rc::Rc;

use serde::{Deserialize, Serialize};
use serde_yaml::Value as YamlValue;
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
    let enriched_evidence =
        augment_project_evidence(&root, &base_evidence, options.include_operator_meta);
    let base_dict = enriched_evidence.as_dict().ok_or_else(|| {
        VmError::Thrown(VmValue::String(Rc::from(
            "project.enrich: base_evidence must be a dict",
        )))
    })?;

    let relevant_files = collect_relevant_files(&root, &enriched_evidence);
    let bindings = enrichment_bindings(&root, &enriched_evidence, &relevant_files);
    let rendered_prompt = render_template_result(&options.prompt, Some(&bindings), None, None)
        .map_err(VmError::from)?;
    let schema_hash = sha256_hex(canonical_json(&vm_value_to_json(&options.schema)));
    let prompt_hash = sha256_hex(rendered_prompt.as_bytes());
    let content_hash = hash_relevant_files(&relevant_files);
    let evidence_hash = sha256_hex(canonical_json(&vm_value_to_json(&enriched_evidence)));
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
        Err(VmError::CategorizedError {
            message,
            category: ErrorCategory::SchemaValidation,
        }) => Ok(validation_envelope(
            &enriched_evidence,
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

#[derive(Debug, Clone, Serialize, Default)]
struct CiEvidence {
    workflows: Vec<WorkflowEvidence>,
    hooks: HookEvidence,
    package_manifests: Vec<PackageManifestEvidence>,
    merge_policy: MergePolicyEvidence,
}

#[derive(Debug, Clone, Serialize)]
struct WorkflowEvidence {
    path: String,
    name: String,
    jobs: Vec<WorkflowJobEvidence>,
}

#[derive(Debug, Clone, Serialize)]
struct WorkflowJobEvidence {
    id: String,
    name: String,
    classifications: Vec<String>,
    commands: Vec<String>,
    actions: Vec<String>,
    required_check: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Default)]
struct HookEvidence {
    providers: Vec<String>,
    files: Vec<String>,
    stages: BTreeMap<String, Vec<String>>,
}

#[derive(Debug, Clone, Serialize)]
struct PackageManifestEvidence {
    ecosystem: String,
    manifests: Vec<String>,
    lockfiles: Vec<String>,
    has_lockfile: bool,
    ci_hints: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Default)]
struct MergePolicyEvidence {
    branch: Option<String>,
    required_checks: Option<Vec<String>>,
    require_up_to_date_branch: Option<bool>,
    enforce_admins: Option<bool>,
    required_approvals: Option<i64>,
    require_code_owner_reviews: Option<bool>,
    required_conversation_resolution: Option<bool>,
    allow_force_pushes: Option<bool>,
    allow_deletions: Option<bool>,
    allowed_merge_methods: Option<Vec<String>>,
    squash_only: Option<bool>,
    codeowners_files: Vec<String>,
    codeowner_rules: Vec<CodeownerRule>,
    contributing_files: Vec<String>,
    merge_method_hints: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
struct CodeownerRule {
    path: String,
    owners: Vec<String>,
}

#[derive(Debug, Clone, Default)]
struct WorkflowScan {
    workflows: Vec<WorkflowEvidence>,
    combined_text: String,
}

#[derive(Debug, Clone, Default)]
struct GithubPolicyProbe {
    branch: Option<String>,
    required_checks: Option<Vec<String>>,
    require_up_to_date_branch: Option<bool>,
    enforce_admins: Option<bool>,
    required_approvals: Option<i64>,
    require_code_owner_reviews: Option<bool>,
    required_conversation_resolution: Option<bool>,
    allow_force_pushes: Option<bool>,
    allow_deletions: Option<bool>,
    allowed_merge_methods: Option<Vec<String>>,
    squash_only: Option<bool>,
}

fn augment_project_evidence(
    root: &Path,
    base_evidence: &VmValue,
    include_operator_meta: bool,
) -> VmValue {
    let Some(dict) = base_evidence.as_dict() else {
        return base_evidence.clone();
    };
    let mut merged = (*dict).clone();
    if include_operator_meta {
        merged.insert(
            "ci".to_string(),
            ci_evidence_value(collect_ci_evidence(root)),
        );
    }
    VmValue::Dict(Rc::new(merged))
}

fn collect_ci_evidence(root: &Path) -> CiEvidence {
    let gh_policy = probe_github_policy(root);
    let required_checks = gh_policy
        .as_ref()
        .and_then(|policy| policy.required_checks.as_ref())
        .map(|checks| checks.iter().cloned().collect::<BTreeSet<_>>());
    let workflow_scan = collect_workflow_evidence(root, required_checks.as_ref());
    CiEvidence {
        workflows: workflow_scan.workflows,
        hooks: collect_hook_evidence(root),
        package_manifests: collect_package_manifest_evidence(root, &workflow_scan.combined_text),
        merge_policy: collect_merge_policy_evidence(root, gh_policy),
    }
}

fn ci_evidence_value(ci: CiEvidence) -> VmValue {
    let value = serde_json::to_value(ci).unwrap_or_else(|_| serde_json::json!({}));
    json_to_vm_value(&value)
}

fn collect_workflow_evidence(
    root: &Path,
    required_checks: Option<&BTreeSet<String>>,
) -> WorkflowScan {
    let workflow_dir = root.join(".github/workflows");
    let Ok(entries) = std::fs::read_dir(&workflow_dir) else {
        return WorkflowScan::default();
    };

    let mut files = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            let ext = path.extension().and_then(|value| value.to_str())?;
            if matches!(ext, "yml" | "yaml") {
                Some(path)
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    files.sort();

    let mut scan = WorkflowScan::default();
    for path in files {
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        if !scan.combined_text.is_empty() {
            scan.combined_text.push('\n');
        }
        scan.combined_text.push_str(&content);

        let rel_path = relative_posix(root, &path);
        let parsed = match serde_yaml::from_str::<YamlValue>(&content) {
            Ok(value) => value,
            Err(_) => continue,
        };
        let workflow_name = parsed
            .as_mapping()
            .and_then(|mapping| mapping.get(YamlValue::String("name".to_string())))
            .and_then(YamlValue::as_str)
            .map(ToString::to_string)
            .unwrap_or_else(|| {
                path.file_stem()
                    .and_then(|value| value.to_str())
                    .unwrap_or("workflow")
                    .to_string()
            });
        let jobs = parsed
            .as_mapping()
            .and_then(|mapping| mapping.get(YamlValue::String("jobs".to_string())))
            .and_then(YamlValue::as_mapping)
            .map(|jobs| collect_workflow_jobs(jobs, &workflow_name, required_checks))
            .unwrap_or_default();
        scan.workflows.push(WorkflowEvidence {
            path: rel_path,
            name: workflow_name,
            jobs,
        });
    }
    scan
}

fn collect_workflow_jobs(
    jobs: &serde_yaml::Mapping,
    workflow_name: &str,
    required_checks: Option<&BTreeSet<String>>,
) -> Vec<WorkflowJobEvidence> {
    let mut entries = jobs.iter().collect::<Vec<_>>();
    entries.sort_by_key(|(key, _)| key.as_str().unwrap_or_default().to_string());
    entries
        .into_iter()
        .filter_map(|(job_id, job_value)| {
            let job_id = job_id.as_str()?.to_string();
            let job_map = job_value.as_mapping()?;
            let name = job_map
                .get(YamlValue::String("name".to_string()))
                .and_then(YamlValue::as_str)
                .map(ToString::to_string)
                .unwrap_or_else(|| job_id.clone());
            let mut commands = Vec::new();
            let mut actions = Vec::new();
            if let Some(steps) = job_map
                .get(YamlValue::String("steps".to_string()))
                .and_then(YamlValue::as_sequence)
            {
                for step in steps {
                    let Some(step_map) = step.as_mapping() else {
                        continue;
                    };
                    if let Some(run) = step_map
                        .get(YamlValue::String("run".to_string()))
                        .and_then(YamlValue::as_str)
                    {
                        commands.extend(shell_commands(run));
                    }
                    if let Some(action) = step_map
                        .get(YamlValue::String("uses".to_string()))
                        .and_then(YamlValue::as_str)
                    {
                        push_unique_string(&mut actions, action.to_string());
                    }
                }
            }
            let classifications = classify_workflow_job(&job_id, &name, &commands, &actions);
            let required_check = required_checks.map(|checks| {
                [
                    name.clone(),
                    job_id.clone(),
                    format!("{workflow_name} / {name}"),
                    format!("{workflow_name} / {job_id}"),
                ]
                .into_iter()
                .any(|candidate| checks.contains(&candidate))
            });
            Some(WorkflowJobEvidence {
                id: job_id,
                name,
                classifications,
                commands,
                actions,
                required_check,
            })
        })
        .collect()
}

fn classify_workflow_job(
    job_id: &str,
    name: &str,
    commands: &[String],
    actions: &[String],
) -> Vec<String> {
    let haystack = format!(
        "{}\n{}\n{}\n{}",
        job_id,
        name,
        commands.join("\n"),
        actions.join("\n")
    )
    .to_lowercase();
    let mut classes = Vec::new();
    if contains_any(
        &haystack,
        &[
            "lint",
            "clippy",
            "fmt",
            "markdownlint",
            "eslint",
            "ruff",
            "check-docs-snippets",
        ],
    ) {
        push_unique_string(&mut classes, "lint".to_string());
    }
    if contains_any(
        &haystack,
        &[
            "test",
            "nextest",
            "pytest",
            "vitest",
            "jest",
            "go test",
            "cargo test",
            "make test",
            "conformance",
        ],
    ) {
        push_unique_string(&mut classes, "test".to_string());
    }
    if contains_any(
        &haystack,
        &[
            "build",
            "cargo build",
            "npm run build",
            "vite build",
            "docker build",
            "wasm-pack build",
        ],
    ) {
        push_unique_string(&mut classes, "build".to_string());
    }
    if contains_any(
        &haystack,
        &[
            "release",
            "publish",
            "deploy",
            "create release",
            "action-gh-release",
            "build-push-action",
            "create-pull-request",
        ],
    ) {
        push_unique_string(&mut classes, "release".to_string());
    }
    if classes.is_empty() {
        classes.push("other".to_string());
    }
    classes
}

fn collect_hook_evidence(root: &Path) -> HookEvidence {
    let mut providers = Vec::new();
    let mut files = Vec::new();
    let mut stages = BTreeMap::new();

    let githooks_dir = root.join(".githooks");
    if let Ok(entries) = std::fs::read_dir(&githooks_dir) {
        let mut hook_files = entries
            .flatten()
            .filter_map(|entry| {
                entry
                    .file_type()
                    .ok()
                    .filter(|kind| kind.is_file())
                    .map(|_| entry.path())
            })
            .collect::<Vec<_>>();
        hook_files.sort();
        if !hook_files.is_empty() {
            push_unique_string(&mut providers, ".githooks".to_string());
        }
        for path in hook_files {
            let rel_path = relative_posix(root, &path);
            push_unique_string(&mut files, rel_path);
            let stage = path
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or("hook")
                .to_string();
            let Ok(content) = std::fs::read_to_string(&path) else {
                continue;
            };
            for command in shell_commands(&content) {
                push_stage_command(&mut stages, &stage, command);
            }
        }
    }

    let pre_commit_path = root.join(".pre-commit-config.yaml");
    if pre_commit_path.is_file() {
        push_unique_string(&mut providers, "pre-commit".to_string());
        push_unique_string(&mut files, relative_posix(root, &pre_commit_path));
        if let Ok(content) = std::fs::read_to_string(&pre_commit_path) {
            collect_pre_commit_hooks(&content, &mut stages);
        }
    }

    let lefthook_path = root.join("lefthook.yml");
    if lefthook_path.is_file() {
        push_unique_string(&mut providers, "lefthook".to_string());
        push_unique_string(&mut files, relative_posix(root, &lefthook_path));
        if let Ok(content) = std::fs::read_to_string(&lefthook_path) {
            collect_lefthook_hooks(&content, &mut stages);
        }
    }

    let husky_dir = root.join(".husky");
    if let Ok(entries) = std::fs::read_dir(&husky_dir) {
        let mut hook_files = entries
            .flatten()
            .filter_map(|entry| {
                entry
                    .file_type()
                    .ok()
                    .filter(|kind| kind.is_file())
                    .map(|_| entry.path())
            })
            .filter(|path| {
                path.file_name()
                    .and_then(|value| value.to_str())
                    .is_some_and(|name| !name.starts_with('_'))
            })
            .collect::<Vec<_>>();
        hook_files.sort();
        if !hook_files.is_empty() {
            push_unique_string(&mut providers, "husky".to_string());
        }
        for path in hook_files {
            let rel_path = relative_posix(root, &path);
            push_unique_string(&mut files, rel_path);
            let stage = path
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or("hook")
                .to_string();
            let Ok(content) = std::fs::read_to_string(&path) else {
                continue;
            };
            for command in shell_commands(&content) {
                push_stage_command(&mut stages, &stage, command);
            }
        }
    }

    HookEvidence {
        providers,
        files,
        stages,
    }
}

fn collect_pre_commit_hooks(content: &str, stages: &mut BTreeMap<String, Vec<String>>) {
    let Ok(parsed) = serde_yaml::from_str::<YamlValue>(content) else {
        return;
    };
    let default_stages = parsed
        .as_mapping()
        .and_then(|mapping| mapping.get(YamlValue::String("default_stages".to_string())))
        .and_then(yaml_string_list);
    let repos = parsed
        .as_mapping()
        .and_then(|mapping| mapping.get(YamlValue::String("repos".to_string())))
        .and_then(YamlValue::as_sequence);
    let Some(repos) = repos else {
        return;
    };
    for repo in repos {
        let Some(repo_map) = repo.as_mapping() else {
            continue;
        };
        let hooks = repo_map
            .get(YamlValue::String("hooks".to_string()))
            .and_then(YamlValue::as_sequence);
        let Some(hooks) = hooks else {
            continue;
        };
        for hook in hooks {
            let Some(hook_map) = hook.as_mapping() else {
                continue;
            };
            let mut command = hook_map
                .get(YamlValue::String("entry".to_string()))
                .and_then(YamlValue::as_str)
                .map(ToString::to_string)
                .or_else(|| {
                    hook_map
                        .get(YamlValue::String("name".to_string()))
                        .and_then(YamlValue::as_str)
                        .map(ToString::to_string)
                })
                .or_else(|| {
                    hook_map
                        .get(YamlValue::String("id".to_string()))
                        .and_then(YamlValue::as_str)
                        .map(ToString::to_string)
                })
                .unwrap_or_else(|| "hook".to_string());
            if let Some(args) = hook_map
                .get(YamlValue::String("args".to_string()))
                .and_then(yaml_string_list)
                .filter(|args| !args.is_empty())
            {
                command = format!("{command} {}", args.join(" "));
            }
            let hook_stages = hook_map
                .get(YamlValue::String("stages".to_string()))
                .and_then(yaml_string_list)
                .or_else(|| default_stages.clone())
                .unwrap_or_else(|| vec!["pre-commit".to_string()]);
            for stage in hook_stages {
                push_stage_command(stages, &stage, command.clone());
            }
        }
    }
}

fn collect_lefthook_hooks(content: &str, stages: &mut BTreeMap<String, Vec<String>>) {
    let Ok(parsed) = serde_yaml::from_str::<YamlValue>(content) else {
        return;
    };
    let Some(root) = parsed.as_mapping() else {
        return;
    };
    for (stage, value) in root {
        let Some(stage_name) = stage.as_str() else {
            continue;
        };
        if !stage_name.contains('-') {
            continue;
        }
        collect_nested_run_commands(value, &mut |command| {
            push_stage_command(stages, stage_name, command);
        });
    }
}

fn collect_nested_run_commands(value: &YamlValue, sink: &mut dyn FnMut(String)) {
    match value {
        YamlValue::Mapping(mapping) => {
            if let Some(run) = mapping
                .get(YamlValue::String("run".to_string()))
                .and_then(YamlValue::as_str)
            {
                for command in shell_commands(run) {
                    sink(command);
                }
            }
            let mut entries = mapping.iter().collect::<Vec<_>>();
            entries.sort_by_key(|(key, _)| key.as_str().unwrap_or_default().to_string());
            for (key, child) in entries {
                let Some(key) = key.as_str() else {
                    continue;
                };
                if key == "run" {
                    continue;
                }
                collect_nested_run_commands(child, sink);
            }
        }
        YamlValue::Sequence(items) => {
            for item in items {
                collect_nested_run_commands(item, sink);
            }
        }
        _ => {}
    }
}

fn collect_package_manifest_evidence(
    root: &Path,
    workflow_text: &str,
) -> Vec<PackageManifestEvidence> {
    let manifests = [
        ("cargo", &["Cargo.toml"][..], &["Cargo.lock"][..]),
        (
            "npm",
            &["package.json"][..],
            &[
                "package-lock.json",
                "yarn.lock",
                "pnpm-lock.yaml",
                "bun.lockb",
            ][..],
        ),
        (
            "python",
            &["pyproject.toml", "requirements.txt", "setup.py"][..],
            &["poetry.lock", "Pipfile.lock", "uv.lock"][..],
        ),
        ("ruby", &["Gemfile"][..], &["Gemfile.lock"][..]),
        ("go", &["go.mod"][..], &["go.sum"][..]),
        ("swift", &["Package.swift"][..], &["Package.resolved"][..]),
    ];
    manifests
        .into_iter()
        .filter_map(|(ecosystem, manifest_files, lockfiles)| {
            let present_manifests = manifest_files
                .iter()
                .filter(|name| root.join(name).is_file())
                .map(|name| (*name).to_string())
                .collect::<Vec<_>>();
            let present_lockfiles = lockfiles
                .iter()
                .filter(|name| root.join(name).is_file())
                .map(|name| (*name).to_string())
                .collect::<Vec<_>>();
            if present_manifests.is_empty() && present_lockfiles.is_empty() {
                return None;
            }
            Some(PackageManifestEvidence {
                ecosystem: ecosystem.to_string(),
                manifests: present_manifests,
                lockfiles: present_lockfiles.clone(),
                has_lockfile: !present_lockfiles.is_empty(),
                ci_hints: ci_hints_for_ecosystem(ecosystem, workflow_text),
            })
        })
        .collect()
}

fn ci_hints_for_ecosystem(ecosystem: &str, workflow_text: &str) -> Vec<String> {
    let text = workflow_text.to_lowercase();
    let mut hints = Vec::new();
    let push_if = |hints: &mut Vec<String>, condition: bool, value: &str| {
        if condition {
            push_unique_string(hints, value.to_string());
        }
    };
    match ecosystem {
        "cargo" => {
            push_if(
                &mut hints,
                text.contains("cargo-nextest") || text.contains("cargo nextest"),
                "cargo-nextest installed",
            );
            push_if(
                &mut hints,
                text.contains("swatinem/rust-cache"),
                "rust-cache action",
            );
            push_if(&mut hints, text.contains("sccache"), "sccache enabled");
        }
        "npm" => {
            push_if(
                &mut hints,
                text.contains("actions/setup-node") && text.contains("cache: npm"),
                "setup-node npm cache",
            );
            push_if(
                &mut hints,
                text.contains("actions/setup-node") && text.contains("cache: pnpm"),
                "setup-node pnpm cache",
            );
            push_if(
                &mut hints,
                text.contains("actions/setup-node") && text.contains("cache: yarn"),
                "setup-node yarn cache",
            );
        }
        "python" => {
            push_if(
                &mut hints,
                text.contains("actions/setup-python") && text.contains("cache: pip"),
                "setup-python pip cache",
            );
            push_if(&mut hints, text.contains("poetry"), "poetry in CI");
        }
        "ruby" => {
            push_if(
                &mut hints,
                text.contains("bundler-cache: true"),
                "bundler cache",
            );
        }
        _ => {}
    }
    push_if(
        &mut hints,
        text.contains("cache-from: type=gha") || text.contains("cache-to: type=gha"),
        "github actions cache",
    );
    hints
}

fn collect_merge_policy_evidence(
    root: &Path,
    gh_policy: Option<GithubPolicyProbe>,
) -> MergePolicyEvidence {
    let codeowners_files = existing_paths(
        root,
        &[".github/CODEOWNERS", "CODEOWNERS", "docs/CODEOWNERS"],
    );
    let mut codeowner_rules = Vec::new();
    for rel_path in &codeowners_files {
        let path = root.join(rel_path);
        let Ok(content) = std::fs::read_to_string(path) else {
            continue;
        };
        codeowner_rules.extend(parse_codeowners(&content));
    }

    let contributing_files = existing_paths(root, &["CONTRIBUTING.md"]);
    let mut merge_method_hints = Vec::new();
    for rel_path in &contributing_files {
        let path = root.join(rel_path);
        let Ok(content) = std::fs::read_to_string(path) else {
            continue;
        };
        for hint in merge_method_hints_from_text(&content) {
            push_unique_string(&mut merge_method_hints, hint);
        }
    }

    let mut evidence = MergePolicyEvidence {
        codeowners_files,
        codeowner_rules,
        contributing_files,
        merge_method_hints,
        ..MergePolicyEvidence::default()
    };
    if let Some(gh_policy) = gh_policy {
        evidence.branch = gh_policy.branch;
        evidence.required_checks = gh_policy.required_checks;
        evidence.require_up_to_date_branch = gh_policy.require_up_to_date_branch;
        evidence.enforce_admins = gh_policy.enforce_admins;
        evidence.required_approvals = gh_policy.required_approvals;
        evidence.require_code_owner_reviews = gh_policy.require_code_owner_reviews;
        evidence.required_conversation_resolution = gh_policy.required_conversation_resolution;
        evidence.allow_force_pushes = gh_policy.allow_force_pushes;
        evidence.allow_deletions = gh_policy.allow_deletions;
        evidence.allowed_merge_methods = gh_policy.allowed_merge_methods;
        evidence.squash_only = gh_policy.squash_only;
    }
    evidence
}

fn probe_github_policy(root: &Path) -> Option<GithubPolicyProbe> {
    let gh = gh_command_path();
    probe_github_policy_with_gh(root, &gh)
}

fn probe_github_policy_with_gh(root: &Path, gh: &str) -> Option<GithubPolicyProbe> {
    let status = Command::new(gh)
        .args(["auth", "status"])
        .current_dir(root)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .ok()?;
    if !status.success() {
        return None;
    }

    let remote_url = run_command(root, "git", &["config", "--get", "remote.origin.url"])?;
    let (owner, repo) = parse_github_remote(remote_url.trim())?;
    let branch = git_default_branch(root).or_else(|| Some("main".to_string()))?;
    let protection_raw = run_command(
        root,
        gh,
        &[
            "api",
            &format!("repos/{owner}/{repo}/branches/{branch}/protection"),
        ],
    )?;
    let protection = serde_json::from_str::<serde_json::Value>(&protection_raw).ok()?;
    let repo_raw = run_command(root, gh, &["api", &format!("repos/{owner}/{repo}")]);
    let repo_json = repo_raw
        .as_deref()
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok());

    let required_checks = protection
        .get("required_status_checks")
        .and_then(|value| value.get("contexts"))
        .and_then(serde_json::Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(ToString::to_string))
                .collect::<Vec<_>>()
        });
    let allowed_merge_methods = repo_json.as_ref().map(|repo| {
        let mut methods = Vec::new();
        if repo
            .get("allow_squash_merge")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
        {
            methods.push("squash".to_string());
        }
        if repo
            .get("allow_rebase_merge")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
        {
            methods.push("rebase".to_string());
        }
        if repo
            .get("allow_merge_commit")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
        {
            methods.push("merge".to_string());
        }
        methods
    });
    let squash_only = allowed_merge_methods
        .as_ref()
        .map(|methods| methods == &["squash".to_string()]);

    Some(GithubPolicyProbe {
        branch: Some(branch),
        required_checks,
        require_up_to_date_branch: protection
            .get("required_status_checks")
            .and_then(|value| value.get("strict"))
            .and_then(serde_json::Value::as_bool),
        enforce_admins: protection
            .get("enforce_admins")
            .and_then(|value| value.get("enabled"))
            .and_then(serde_json::Value::as_bool),
        required_approvals: protection
            .get("required_pull_request_reviews")
            .and_then(|value| value.get("required_approving_review_count"))
            .and_then(serde_json::Value::as_i64),
        require_code_owner_reviews: protection
            .get("required_pull_request_reviews")
            .and_then(|value| value.get("require_code_owner_reviews"))
            .and_then(serde_json::Value::as_bool),
        required_conversation_resolution: protection
            .get("required_conversation_resolution")
            .and_then(|value| value.get("enabled"))
            .and_then(serde_json::Value::as_bool),
        allow_force_pushes: protection
            .get("allow_force_pushes")
            .and_then(|value| value.get("enabled"))
            .and_then(serde_json::Value::as_bool),
        allow_deletions: protection
            .get("allow_deletions")
            .and_then(|value| value.get("enabled"))
            .and_then(serde_json::Value::as_bool),
        allowed_merge_methods,
        squash_only,
    })
}

fn gh_command_path() -> String {
    std::env::var("HARN_PROJECT_ENRICH_GH").unwrap_or_else(|_| "gh".to_string())
}

fn run_command(root: &Path, cmd: &str, args: &[&str]) -> Option<String> {
    let mut command = Command::new(cmd);
    command.args(args).current_dir(root);
    if cmd == "git" {
        clear_git_env(&mut command);
    }
    let output = command.output().ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout).ok()
}

fn clear_git_env(command: &mut Command) {
    command
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_PREFIX");
}

fn parse_github_remote(remote: &str) -> Option<(String, String)> {
    let trimmed = remote.trim_end_matches(".git");
    if let Some(rest) = trimmed.strip_prefix("https://github.com/") {
        let (owner, repo) = rest.split_once('/')?;
        return Some((owner.to_string(), repo.to_string()));
    }
    if let Some(rest) = trimmed.strip_prefix("git@github.com:") {
        let (owner, repo) = rest.split_once('/')?;
        return Some((owner.to_string(), repo.to_string()));
    }
    None
}

fn git_default_branch(root: &Path) -> Option<String> {
    let remote_head = run_command(root, "git", &["symbolic-ref", "refs/remotes/origin/HEAD"])
        .map(|value| value.trim().to_string());
    if let Some(remote_head) = remote_head {
        return remote_head
            .rsplit('/')
            .next()
            .map(ToString::to_string)
            .filter(|value| !value.is_empty());
    }
    run_command(root, "git", &["branch", "--show-current"])
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn parse_codeowners(content: &str) -> Vec<CodeownerRule> {
    content
        .lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                return None;
            }
            let mut parts = trimmed.split_whitespace();
            let path = parts.next()?.to_string();
            let owners = parts.map(ToString::to_string).collect::<Vec<_>>();
            Some(CodeownerRule { path, owners })
        })
        .collect()
}

fn merge_method_hints_from_text(content: &str) -> Vec<String> {
    let text = content.to_lowercase();
    let mut hints = Vec::new();
    if text.contains("squash") {
        hints.push("squash".to_string());
    }
    if text.contains("rebase") {
        hints.push("rebase".to_string());
    }
    if text.contains("merge commit") || text.contains("merge commits") {
        hints.push("merge".to_string());
    }
    hints
}

fn collect_operator_context_files(root: &Path) -> Vec<String> {
    let mut files = existing_paths(
        root,
        &[
            ".github/CODEOWNERS",
            "CODEOWNERS",
            "docs/CODEOWNERS",
            "CONTRIBUTING.md",
            ".pre-commit-config.yaml",
            "lefthook.yml",
        ],
    );
    files.extend(glob_like_files(root, ".github/workflows", &["yml", "yaml"]));
    files.extend(glob_like_files(root, ".githooks", &[]));
    files.extend(
        glob_like_files(root, ".husky", &[])
            .into_iter()
            .filter(|path| {
                Path::new(path)
                    .file_name()
                    .and_then(|value| value.to_str())
                    .is_some_and(|name| !name.starts_with('_'))
            }),
    );
    files.sort();
    files.dedup();
    files
}

fn existing_paths(root: &Path, rel_paths: &[&str]) -> Vec<String> {
    rel_paths
        .iter()
        .filter(|rel_path| root.join(rel_path).is_file())
        .map(|rel_path| (*rel_path).to_string())
        .collect()
}

fn glob_like_files(root: &Path, rel_dir: &str, extensions: &[&str]) -> Vec<String> {
    let dir = root.join(rel_dir);
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut paths = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            let is_file = entry.file_type().ok()?.is_file();
            if !is_file {
                return None;
            }
            if !extensions.is_empty() {
                let ext = path.extension().and_then(|value| value.to_str())?;
                if !extensions.contains(&ext) {
                    return None;
                }
            }
            Some(relative_posix(root, &path))
        })
        .collect::<Vec<_>>();
    paths.sort();
    paths
}

fn shell_commands(script: &str) -> Vec<String> {
    let mut commands = Vec::new();
    for line in script.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty()
            || trimmed.starts_with('#')
            || trimmed.starts_with("#!")
            || trimmed == "set -e"
            || trimmed == "set -eu"
            || trimmed == "set -eux"
            || trimmed.contains("husky.sh")
        {
            continue;
        }
        commands.push(trimmed.to_string());
    }
    commands
}

fn yaml_string_list(value: &YamlValue) -> Option<Vec<String>> {
    value.as_sequence().map(|items| {
        items
            .iter()
            .filter_map(|item| item.as_str().map(ToString::to_string))
            .collect::<Vec<_>>()
    })
}

fn push_stage_command(stages: &mut BTreeMap<String, Vec<String>>, stage: &str, command: String) {
    push_unique_string(stages.entry(stage.to_string()).or_default(), command);
}

fn push_unique_path(items: &mut Vec<String>, seen: &mut BTreeSet<String>, value: String) {
    if seen.insert(value.clone()) {
        items.push(value);
    }
}

fn push_unique_string(items: &mut Vec<String>, value: String) {
    if !items.contains(&value) {
        items.push(value);
    }
}

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| haystack.contains(needle))
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
    if let Some(temperature) = options.temperature {
        llm_options.insert("temperature".to_string(), VmValue::Float(temperature));
    }
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

fn attach_ci_metadata(value: VmValue, base_evidence: &VmValue) -> VmValue {
    let Some(ci_value) = base_evidence
        .as_dict()
        .and_then(|dict| dict.get("ci"))
        .cloned()
    else {
        return value;
    };
    let Some(dict) = value.as_dict() else {
        let mut wrapped = BTreeMap::new();
        wrapped.insert("data".to_string(), value);
        wrapped.insert("ci".to_string(), ci_value);
        return VmValue::Dict(Rc::new(wrapped));
    };
    let mut merged = (*dict).clone();
    merged.insert("ci".to_string(), ci_value);
    VmValue::Dict(Rc::new(merged))
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

#[cfg(test)]
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
        assert!(status.success(), "git {:?} should succeed", args);
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

    #[test]
    fn llm_options_value_forwards_temperature() {
        let options = ProjectEnrichOptions {
            base_evidence: None,
            prompt: "Return JSON.".to_string(),
            schema: VmValue::Dict(Rc::new(BTreeMap::new())),
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
            r#"name: CI
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
"#,
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
            r#"repos:
  - repo: local
    hooks:
      - id: lint
        name: local lint
        entry: cargo clippy
        stages: [pre-commit, pre-push]
"#,
        );
        write_file(
            &dir.path().join("lefthook.yml"),
            r#"pre-push:
  commands:
    tests:
      run: cargo test --workspace
"#,
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

        let base = VmValue::Dict(Rc::new(BTreeMap::from([
            (
                "languages".to_string(),
                VmValue::List(Rc::new(vec![VmValue::String(Rc::from("rust"))])),
            ),
            (
                "anchors".to_string(),
                VmValue::List(Rc::new(vec![VmValue::String(Rc::from("Cargo.toml"))])),
            ),
            (
                "lockfiles".to_string(),
                VmValue::List(Rc::new(vec![VmValue::String(Rc::from("Cargo.lock"))])),
            ),
        ])));
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
        let base = VmValue::Dict(Rc::new(BTreeMap::from([(
            "ci".to_string(),
            VmValue::Dict(Rc::new(BTreeMap::from([(
                "merge_policy".to_string(),
                VmValue::Dict(Rc::new(BTreeMap::from([(
                    "squash_only".to_string(),
                    VmValue::Bool(true),
                )]))),
            )]))),
        )])));
        let result = attach_ci_metadata(
            VmValue::Dict(Rc::new(BTreeMap::from([(
                "summary".to_string(),
                VmValue::String(Rc::from("ok")),
            )]))),
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
}
