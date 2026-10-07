//! Repository evidence for project enrichment: CI workflows, hooks, package manifests, and merge policy.

use super::*;

#[derive(Debug, Clone, Serialize, Default)]
pub(super) struct CiEvidence {
    pub(super) workflows: Vec<WorkflowEvidence>,
    pub(super) hooks: HookEvidence,
    pub(super) package_manifests: Vec<PackageManifestEvidence>,
    pub(super) merge_policy: MergePolicyEvidence,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct WorkflowEvidence {
    pub(super) path: String,
    pub(super) name: String,
    pub(super) jobs: Vec<WorkflowJobEvidence>,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct WorkflowJobEvidence {
    pub(super) id: String,
    pub(super) name: String,
    pub(super) classifications: Vec<String>,
    pub(super) commands: Vec<String>,
    pub(super) actions: Vec<String>,
    pub(super) required_check: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub(super) struct HookEvidence {
    pub(super) providers: Vec<String>,
    pub(super) files: Vec<String>,
    pub(super) stages: BTreeMap<String, Vec<String>>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(super) warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct PackageManifestEvidence {
    pub(super) ecosystem: String,
    pub(super) manifests: Vec<String>,
    pub(super) lockfiles: Vec<String>,
    pub(super) has_lockfile: bool,
    pub(super) ci_hints: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub(super) struct MergePolicyEvidence {
    pub(super) branch: Option<String>,
    pub(super) required_checks: Option<Vec<String>>,
    pub(super) require_up_to_date_branch: Option<bool>,
    pub(super) enforce_admins: Option<bool>,
    pub(super) required_approvals: Option<i64>,
    pub(super) require_code_owner_reviews: Option<bool>,
    pub(super) required_conversation_resolution: Option<bool>,
    pub(super) allow_force_pushes: Option<bool>,
    pub(super) allow_deletions: Option<bool>,
    pub(super) allowed_merge_methods: Option<Vec<String>>,
    pub(super) squash_only: Option<bool>,
    pub(super) codeowners_files: Vec<String>,
    pub(super) codeowner_rules: Vec<CodeownerRule>,
    pub(super) contributing_files: Vec<String>,
    pub(super) merge_method_hints: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct CodeownerRule {
    pub(super) path: String,
    pub(super) owners: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub(super) struct WorkflowScan {
    pub(super) workflows: Vec<WorkflowEvidence>,
    pub(super) combined_text: String,
}

#[derive(Debug, Clone, Default)]
pub(super) struct GithubPolicyProbe {
    pub(super) branch: Option<String>,
    pub(super) required_checks: Option<Vec<String>>,
    pub(super) require_up_to_date_branch: Option<bool>,
    pub(super) enforce_admins: Option<bool>,
    pub(super) required_approvals: Option<i64>,
    pub(super) require_code_owner_reviews: Option<bool>,
    pub(super) required_conversation_resolution: Option<bool>,
    pub(super) allow_force_pushes: Option<bool>,
    pub(super) allow_deletions: Option<bool>,
    pub(super) allowed_merge_methods: Option<Vec<String>>,
    pub(super) squash_only: Option<bool>,
}

pub(super) fn augment_project_evidence(
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
            crate::value::intern_key("ci"),
            ci_evidence_value(collect_ci_evidence(root)),
        );
    }
    VmValue::dict(merged)
}

pub(super) fn collect_ci_evidence(root: &Path) -> CiEvidence {
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

pub(super) fn ci_evidence_value(ci: CiEvidence) -> VmValue {
    let value = serde_json::to_value(ci).unwrap_or_else(|_| serde_json::json!({}));
    json_to_vm_value(&value)
}

pub(super) fn collect_workflow_evidence(
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
        let parsed = match serde_yaml_ng::from_str::<YamlValue>(&content) {
            Ok(value) => value,
            Err(_) => continue,
        };
        let workflow_name = parsed
            .as_mapping()
            .and_then(|mapping| mapping.get("name"))
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
            .and_then(|mapping| mapping.get("jobs"))
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

pub(super) fn collect_workflow_jobs(
    jobs: &serde_yaml_ng::Mapping,
    workflow_name: &str,
    required_checks: Option<&BTreeSet<String>>,
) -> Vec<WorkflowJobEvidence> {
    let mut entries = jobs.iter().collect::<Vec<_>>();
    entries.sort_by(|(a, _), (b, _)| a.as_str().cmp(&b.as_str()));
    entries
        .into_iter()
        .filter_map(|(job_id, job_value)| {
            // Workflow job ids are always string keys.
            let job_id = job_id.as_str().unwrap_or_default().to_string();
            let job_map = job_value.as_mapping()?;
            let name = job_map
                .get("name")
                .and_then(YamlValue::as_str)
                .map(ToString::to_string)
                .unwrap_or_else(|| job_id.clone());
            let mut commands = Vec::new();
            let mut actions = Vec::new();
            if let Some(steps) = job_map.get("steps").and_then(YamlValue::as_sequence) {
                for step in steps {
                    let Some(step_map) = step.as_mapping() else {
                        continue;
                    };
                    if let Some(run) = step_map.get("run").and_then(YamlValue::as_str) {
                        commands.extend(shell_commands(run));
                    }
                    if let Some(action) = step_map.get("uses").and_then(YamlValue::as_str) {
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

pub(super) fn classify_workflow_job(
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

pub(super) fn collect_hook_evidence(root: &Path) -> HookEvidence {
    let mut providers = Vec::new();
    let mut files = Vec::new();
    let mut stages = std::collections::BTreeMap::new();
    let mut warnings = Vec::new();

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
            if let Err(error) = collect_lefthook_hooks(&content, &mut stages) {
                warnings.push(format!("lefthook.yml: {error}"));
            }
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
        warnings,
    }
}

pub(super) fn collect_pre_commit_hooks(content: &str, stages: &mut BTreeMap<String, Vec<String>>) {
    let Ok(parsed) = serde_yaml_ng::from_str::<YamlValue>(content) else {
        return;
    };
    let default_stages = parsed
        .as_mapping()
        .and_then(|mapping| mapping.get("default_stages"))
        .and_then(yaml_string_list);
    let repos = parsed
        .as_mapping()
        .and_then(|mapping| mapping.get("repos"))
        .and_then(YamlValue::as_sequence);
    let Some(repos) = repos else {
        return;
    };
    for repo in repos {
        let Some(repo_map) = repo.as_mapping() else {
            continue;
        };
        let hooks = repo_map.get("hooks").and_then(YamlValue::as_sequence);
        let Some(hooks) = hooks else {
            continue;
        };
        for hook in hooks {
            let Some(hook_map) = hook.as_mapping() else {
                continue;
            };
            let mut command = hook_map
                .get("entry")
                .and_then(YamlValue::as_str)
                .map(ToString::to_string)
                .or_else(|| {
                    hook_map
                        .get("name")
                        .and_then(YamlValue::as_str)
                        .map(ToString::to_string)
                })
                .or_else(|| {
                    hook_map
                        .get("id")
                        .and_then(YamlValue::as_str)
                        .map(ToString::to_string)
                })
                .unwrap_or_else(|| "hook".to_string());
            if let Some(args) = hook_map
                .get("args")
                .and_then(yaml_string_list)
                .filter(|args| !args.is_empty())
            {
                command = format!("{command} {}", args.join(" "));
            }
            let hook_stages = hook_map
                .get("stages")
                .and_then(yaml_string_list)
                .or_else(|| default_stages.clone())
                .unwrap_or_else(|| vec!["pre-commit".to_string()]);
            for stage in hook_stages {
                push_stage_command(stages, &stage, command.clone());
            }
        }
    }
}

pub(super) fn collect_lefthook_hooks(
    content: &str,
    stages: &mut BTreeMap<String, Vec<String>>,
) -> Result<(), String> {
    let Ok(parsed) = serde_yaml_ng::from_str::<YamlValue>(content) else {
        return Ok(());
    };
    let Some(root) = parsed.as_mapping() else {
        return Ok(());
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
        })?;
    }
    Ok(())
}

pub(super) fn collect_nested_run_commands(
    value: &YamlValue,
    sink: &mut dyn FnMut(String),
) -> Result<(), String> {
    let mut stack = vec![(value, 0usize)];
    while let Some((value, depth)) = stack.pop() {
        if depth > PROJECT_ENRICH_YAML_MAX_DEPTH {
            return Err(format!(
                "YAML traversal depth exceeded ({PROJECT_ENRICH_YAML_MAX_DEPTH} levels)"
            ));
        }

        match value {
            YamlValue::Mapping(mapping) => {
                if let Some(run) = mapping.get("run").and_then(YamlValue::as_str) {
                    for command in shell_commands(run) {
                        sink(command);
                    }
                }
                let mut entries = mapping.iter().collect::<Vec<_>>();
                entries.sort_by(|(a, _), (b, _)| a.as_str().cmp(&b.as_str()));
                for (key, child) in entries.into_iter().rev() {
                    if key == "run" {
                        continue;
                    }
                    stack.push((child, depth + 1));
                }
            }
            YamlValue::Sequence(items) => {
                for item in items.iter().rev() {
                    stack.push((item, depth + 1));
                }
            }
            _ => {}
        }
    }
    Ok(())
}

pub(super) fn collect_package_manifest_evidence(
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

pub(super) fn ci_hints_for_ecosystem(ecosystem: &str, workflow_text: &str) -> Vec<String> {
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

pub(super) fn collect_merge_policy_evidence(
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

pub(super) fn probe_github_policy(root: &Path) -> Option<GithubPolicyProbe> {
    let gh = gh_command_path();
    probe_github_policy_with_gh(root, &gh)
}

pub(super) fn probe_github_policy_with_gh(root: &Path, gh: &str) -> Option<GithubPolicyProbe> {
    let status = crate::process_sandbox::session_std_command(gh)
        .ok()?
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

pub(super) fn gh_command_path() -> String {
    std::env::var("HARN_PROJECT_ENRICH_GH").unwrap_or_else(|_| "gh".to_string())
}

pub(super) fn run_command(root: &Path, cmd: &str, args: &[&str]) -> Option<String> {
    let mut command = crate::process_sandbox::session_std_command(cmd).ok()?;
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

pub(super) fn clear_git_env(command: &mut Command) {
    command
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_PREFIX");
}

pub(super) fn parse_github_remote(remote: &str) -> Option<(String, String)> {
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

pub(super) fn git_default_branch(root: &Path) -> Option<String> {
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

pub(super) fn parse_codeowners(content: &str) -> Vec<CodeownerRule> {
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

pub(super) fn merge_method_hints_from_text(content: &str) -> Vec<String> {
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

pub(super) fn collect_operator_context_files(root: &Path) -> Vec<String> {
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

pub(super) fn existing_paths(root: &Path, rel_paths: &[&str]) -> Vec<String> {
    rel_paths
        .iter()
        .filter(|rel_path| root.join(rel_path).is_file())
        .map(|rel_path| (*rel_path).to_string())
        .collect()
}

pub(super) fn glob_like_files(root: &Path, rel_dir: &str, extensions: &[&str]) -> Vec<String> {
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

pub(super) fn shell_commands(script: &str) -> Vec<String> {
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

pub(super) fn yaml_string_list(value: &YamlValue) -> Option<Vec<String>> {
    value.as_sequence().map(|items| {
        items
            .iter()
            .filter_map(|item| item.as_str().map(ToString::to_string))
            .collect::<Vec<_>>()
    })
}

pub(super) fn push_stage_command(
    stages: &mut BTreeMap<String, Vec<String>>,
    stage: &str,
    command: String,
) {
    push_unique_string(stages.entry(stage.to_string()).or_default(), command);
}

pub(super) fn push_unique_path(
    items: &mut Vec<String>,
    seen: &mut BTreeSet<String>,
    value: String,
) {
    if seen.insert(value.clone()) {
        items.push(value);
    }
}

pub(super) fn push_unique_string(items: &mut Vec<String>, value: String) {
    if !items.contains(&value) {
        items.push(value);
    }
}

pub(super) fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| haystack.contains(needle))
}
