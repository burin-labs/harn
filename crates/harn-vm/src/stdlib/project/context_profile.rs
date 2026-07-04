use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::value::{VmDictExt, VmValue};

use super::*;

const CONTEXT_PROFILE_SCHEMA_VERSION: i64 = 1;

#[derive(Debug, Clone, Copy)]
struct ContextProfileDef {
    id: &'static str,
    cap: &'static str,
    skills: &'static [&'static str],
    tool_groups: &'static [&'static str],
    mcp_presets: &'static [&'static str],
    body: &'static str,
}

const CONTEXT_PROFILE_DEFS: &[ContextProfileDef] = &[
    ContextProfileDef {
        id: "git",
        cap: "vcs.git",
        skills: &["git"],
        tool_groups: &["git"],
        mcp_presets: &[],
        body: "Project profile: Git repository detected. Treat branch state, staged changes, and remote history as part of the working context before changing repository state.",
    },
    ContextProfileDef {
        id: "github",
        cap: "remote.github",
        skills: &["github"],
        tool_groups: &["github"],
        mcp_presets: &["github"],
        body: "Project profile: GitHub remote detected. Prefer GitHub-aware issue, pull request, and CI workflows when GitHub tools or MCP presets are available.",
    },
    ContextProfileDef {
        id: "rust",
        cap: "language.rust",
        skills: &["rust"],
        tool_groups: &["cargo"],
        mcp_presets: &[],
        body: "Project profile: Rust project detected. Prefer Cargo-native build, test, lint, and workspace workflows.",
    },
    ContextProfileDef {
        id: "node",
        cap: "ecosystem.node",
        skills: &["node", "typescript"],
        tool_groups: &["node"],
        mcp_presets: &[],
        body: "Project profile: Node or TypeScript project detected. Prefer package-manager scripts and lockfile-aware dependency workflows.",
    },
    ContextProfileDef {
        id: "python",
        cap: "language.python",
        skills: &["python"],
        tool_groups: &["python"],
        mcp_presets: &[],
        body: "Project profile: Python project detected. Prefer the detected environment manager and test runner before falling back to raw Python commands.",
    },
    ContextProfileDef {
        id: "swift",
        cap: "language.swift",
        skills: &["swift"],
        tool_groups: &["swift"],
        mcp_presets: &[],
        body: "Project profile: Swift package detected. Prefer SwiftPM build and test workflows.",
    },
];

#[derive(Debug, Clone, Default)]
pub(super) struct ContextProfileOptions {
    pub(super) fingerprint: Option<ProjectFingerprint>,
    pub(super) remote: Option<GitRemoteSignal>,
    pub(super) signal_source: Option<String>,
    pub(super) credentials: BTreeSet<String>,
    pub(super) include_env_credentials: bool,
}

#[derive(Debug, Clone, Default)]
pub(super) struct ContextSignals {
    pub(super) fingerprint: ProjectFingerprint,
    pub(super) remote: Option<GitRemoteSignal>,
    pub(super) source: String,
    pub(super) credentials: BTreeSet<String>,
}

#[derive(Debug, Clone)]
pub(super) struct ContextProfileFragment {
    pub(super) id: String,
    pub(super) source: String,
    pub(super) body: String,
    pub(super) requires_caps: Vec<String>,
}

#[derive(Debug, Clone)]
pub(super) struct ContextProfileActivation {
    pub(super) id: String,
    pub(super) reason: String,
    pub(super) caps: Vec<String>,
    pub(super) skills: Vec<String>,
    pub(super) tool_groups: Vec<String>,
    pub(super) mcp_presets: Vec<String>,
    pub(super) mcp_preset_candidates: Vec<McpPresetCandidate>,
    pub(super) prompt_fragment: ContextProfileFragment,
}

#[derive(Debug, Clone)]
pub(super) struct ContextProfileResolution {
    pub(super) path: PathBuf,
    pub(super) signals: ContextSignals,
    pub(super) profiles: Vec<ContextProfileActivation>,
    pub(super) always_on_prompt_tokens: i64,
    pub(super) activated_prompt_tokens: i64,
    pub(super) always_on_prompt_bytes: usize,
    pub(super) activated_prompt_bytes: usize,
}

impl ContextProfileResolution {
    pub(super) fn into_vm_value(self) -> VmValue {
        let profile_ids = self
            .profiles
            .iter()
            .map(|profile| profile.id.clone())
            .collect::<Vec<_>>();
        let skills = unique_flatten(
            self.profiles
                .iter()
                .flat_map(|profile| profile.skills.clone()),
        );
        let tool_groups = unique_flatten(
            self.profiles
                .iter()
                .flat_map(|profile| profile.tool_groups.clone()),
        );
        let mcp_presets = unique_flatten(
            self.profiles
                .iter()
                .flat_map(|profile| profile.mcp_presets.clone()),
        );
        let caps = unique_flatten(
            self.profiles
                .iter()
                .flat_map(|profile| profile.caps.clone()),
        );
        let prompt_fragments = self
            .profiles
            .iter()
            .map(|profile| profile.prompt_fragment.clone().into_vm_value())
            .collect::<Vec<_>>();
        let mcp_preset_candidates = unique_mcp_candidates(
            self.profiles
                .iter()
                .flat_map(|profile| profile.mcp_preset_candidates.clone()),
        );

        let mut token_delta = BTreeMap::new();
        token_delta.insert(
            "activated_tokens".to_string(),
            VmValue::Int(self.activated_prompt_tokens),
        );
        token_delta.insert(
            "always_on_tokens".to_string(),
            VmValue::Int(self.always_on_prompt_tokens),
        );
        token_delta.insert(
            "saved_tokens".to_string(),
            VmValue::Int((self.always_on_prompt_tokens - self.activated_prompt_tokens).max(0)),
        );
        token_delta.insert(
            "activated_bytes".to_string(),
            VmValue::Int(self.activated_prompt_bytes as i64),
        );
        token_delta.insert(
            "always_on_bytes".to_string(),
            VmValue::Int(self.always_on_prompt_bytes as i64),
        );
        token_delta.insert(
            "saved_bytes".to_string(),
            VmValue::Int(
                self.always_on_prompt_bytes
                    .saturating_sub(self.activated_prompt_bytes) as i64,
            ),
        );

        let mut out = BTreeMap::new();
        out.insert(
            "schema_version".to_string(),
            VmValue::Int(CONTEXT_PROFILE_SCHEMA_VERSION),
        );
        out.put_str("path", self.path.to_string_lossy());
        out.insert("signals".to_string(), self.signals.into_vm_value());
        out.insert("profile_ids".to_string(), string_list_value(profile_ids));
        out.insert(
            "profiles".to_string(),
            VmValue::List(std::sync::Arc::new(
                self.profiles
                    .into_iter()
                    .map(ContextProfileActivation::into_vm_value)
                    .collect(),
            )),
        );
        out.insert("skills".to_string(), string_list_value(skills));
        out.insert("tool_groups".to_string(), string_list_value(tool_groups));
        out.insert("mcp_presets".to_string(), string_list_value(mcp_presets));
        out.insert(
            "mcp_preset_candidates".to_string(),
            VmValue::List(std::sync::Arc::new(
                mcp_preset_candidates
                    .into_iter()
                    .map(McpPresetCandidate::into_vm_value)
                    .collect(),
            )),
        );
        out.insert("caps".to_string(), string_list_value(caps));
        out.insert(
            "prompt_fragments".to_string(),
            VmValue::List(std::sync::Arc::new(prompt_fragments)),
        );
        out.insert("token_delta".to_string(), VmValue::dict(token_delta));
        VmValue::dict(out)
    }
}

impl ContextSignals {
    fn into_vm_value(self) -> VmValue {
        let mut out = BTreeMap::new();
        out.insert("source".to_string(), VmValue::string(self.source));
        out.insert("fingerprint".to_string(), self.fingerprint.into_vm_value());
        out.insert(
            "remote".to_string(),
            self.remote
                .map(GitRemoteSignal::into_vm_value)
                .unwrap_or(VmValue::Nil),
        );
        out.insert(
            "credentials".to_string(),
            string_list_value(self.credentials.into_iter().collect()),
        );
        VmValue::dict(out)
    }
}

impl ContextProfileFragment {
    fn into_vm_value(self) -> VmValue {
        let mut out = BTreeMap::new();
        out.insert("id".to_string(), VmValue::string(self.id));
        out.insert("source".to_string(), VmValue::string(self.source));
        out.insert("body".to_string(), VmValue::string(self.body));
        out.insert(
            "requires_caps".to_string(),
            string_list_value(self.requires_caps),
        );
        VmValue::dict(out)
    }
}

impl ContextProfileActivation {
    fn into_vm_value(self) -> VmValue {
        let mut out = BTreeMap::new();
        out.insert("id".to_string(), VmValue::string(self.id));
        out.insert("reason".to_string(), VmValue::string(self.reason));
        out.insert("caps".to_string(), string_list_value(self.caps));
        out.insert("skills".to_string(), string_list_value(self.skills));
        out.insert(
            "tool_groups".to_string(),
            string_list_value(self.tool_groups),
        );
        out.insert(
            "mcp_presets".to_string(),
            string_list_value(self.mcp_presets),
        );
        out.insert(
            "mcp_preset_candidates".to_string(),
            VmValue::List(std::sync::Arc::new(
                self.mcp_preset_candidates
                    .into_iter()
                    .map(McpPresetCandidate::into_vm_value)
                    .collect(),
            )),
        );
        out.insert(
            "prompt_fragment".to_string(),
            self.prompt_fragment.into_vm_value(),
        );
        VmValue::dict(out)
    }
}

pub(super) fn parse_context_profile_options(value: Option<&VmValue>) -> ContextProfileOptions {
    let mut options = ContextProfileOptions {
        include_env_credentials: true,
        ..ContextProfileOptions::default()
    };
    let Some(dict) = value.and_then(VmValue::as_dict) else {
        options.credentials.extend(env_credentials());
        return options;
    };

    if let Some(include_env) = dict.get("include_env_credentials").and_then(value_as_bool) {
        options.include_env_credentials = include_env;
    }
    if let Some(credentials) = dict.get("credentials") {
        options.credentials.extend(parse_credentials(credentials));
    }
    if let Some(fingerprint) = dict
        .get("fingerprint")
        .and_then(project_fingerprint_from_value)
    {
        options.fingerprint = Some(fingerprint);
        options
            .signal_source
            .get_or_insert_with(|| "provided".to_string());
    }
    if let Some(remote) = dict.get("remote").and_then(remote_signal_from_value) {
        options.remote = Some(remote);
        options
            .signal_source
            .get_or_insert_with(|| "provided".to_string());
    }
    if let Some(source) = dict.get("source").and_then(value_as_string) {
        options.signal_source = Some(source);
    }

    if let Some(signals) = dict.get("signals").and_then(VmValue::as_dict) {
        if options.fingerprint.is_none() {
            let fingerprint_value = signals
                .get("fingerprint")
                .or_else(|| signals.get("project_fingerprint"));
            options.fingerprint = fingerprint_value
                .and_then(project_fingerprint_from_value)
                .or_else(|| project_fingerprint_from_dict(signals));
        }
        if options.remote.is_none() {
            options.remote = signals
                .get("remote")
                .or_else(|| signals.get("git_remote"))
                .and_then(remote_signal_from_value);
        }
        if options.signal_source.is_none() {
            options.signal_source = signals
                .get("source")
                .and_then(value_as_string)
                .or_else(|| {
                    signals
                        .get("_provenance")
                        .and_then(VmValue::as_dict)
                        .and_then(|provenance| provenance.get("source"))
                        .and_then(value_as_string)
                })
                .or_else(|| Some("provided".to_string()));
        }
    }

    if options.include_env_credentials {
        options.credentials.extend(env_credentials());
    }
    options
}

pub(super) fn resolve_context_profile(
    root: &Path,
    options: ContextProfileOptions,
) -> ContextProfileResolution {
    let fingerprint = options
        .fingerprint
        .clone()
        .unwrap_or_else(|| detect_project_fingerprint(root));
    let remote = options.remote.clone().or_else(|| detect_git_remote(root));
    let had_supplied_signals = options.fingerprint.is_some() || options.remote.is_some();
    let source = options.signal_source.unwrap_or_else(|| {
        if had_supplied_signals {
            "provided".to_string()
        } else {
            "scan".to_string()
        }
    });
    let signals = ContextSignals {
        fingerprint,
        remote,
        source,
        credentials: options.credentials,
    };

    let profiles = CONTEXT_PROFILE_DEFS
        .iter()
        .filter(|profile| context_profile_matches(profile, &signals))
        .map(|profile| activate_context_profile(profile, &signals))
        .collect::<Vec<_>>();
    let always_on_prompt = CONTEXT_PROFILE_DEFS
        .iter()
        .map(|profile| profile.body)
        .collect::<Vec<_>>()
        .join("\n\n");
    let activated_prompt = profiles
        .iter()
        .map(|profile| profile.prompt_fragment.body.as_str())
        .collect::<Vec<_>>()
        .join("\n\n");

    ContextProfileResolution {
        path: root.to_path_buf(),
        signals,
        profiles,
        always_on_prompt_tokens: crate::llm::estimate_text_tokens(&always_on_prompt),
        activated_prompt_tokens: crate::llm::estimate_text_tokens(&activated_prompt),
        always_on_prompt_bytes: always_on_prompt.len(),
        activated_prompt_bytes: activated_prompt.len(),
    }
}

fn context_profile_matches(profile: &ContextProfileDef, signals: &ContextSignals) -> bool {
    match profile.id {
        "git" => signals.fingerprint.vcs.as_deref() == Some("git") || signals.remote.is_some(),
        "github" => signals.remote.as_ref().is_some_and(|remote| {
            remote.host == "github.com" || (remote.host.is_empty() && remote.slug.is_some())
        }),
        "rust" => signal_has_language(&signals.fingerprint, "rust"),
        "node" => {
            signal_has_language(&signals.fingerprint, "typescript")
                || signal_has_language(&signals.fingerprint, "javascript")
                || ["npm", "pnpm", "yarn"]
                    .iter()
                    .any(|manager| signal_has_package_manager(&signals.fingerprint, manager))
        }
        "python" => signal_has_language(&signals.fingerprint, "python"),
        "swift" => signal_has_language(&signals.fingerprint, "swift"),
        _ => false,
    }
}

fn activate_context_profile(
    profile: &ContextProfileDef,
    signals: &ContextSignals,
) -> ContextProfileActivation {
    let candidates = profile
        .mcp_presets
        .iter()
        .map(|id| mcp_preset_candidate(id, &signals.credentials))
        .collect::<Vec<_>>();
    let ready_presets = candidates
        .iter()
        .filter(|candidate| candidate.status == "ready")
        .map(|candidate| candidate.id.clone())
        .collect::<Vec<_>>();
    ContextProfileActivation {
        id: profile.id.to_string(),
        reason: context_profile_reason(profile, signals),
        caps: vec![profile.cap.to_string()],
        skills: profile
            .skills
            .iter()
            .map(|skill| (*skill).to_string())
            .collect(),
        tool_groups: profile
            .tool_groups
            .iter()
            .map(|group| (*group).to_string())
            .collect(),
        mcp_presets: ready_presets,
        mcp_preset_candidates: candidates,
        prompt_fragment: ContextProfileFragment {
            id: format!("profile:{}", profile.id),
            source: format!("profile:{}", profile.id),
            body: profile.body.to_string(),
            requires_caps: vec![profile.cap.to_string()],
        },
    }
}

fn context_profile_reason(profile: &ContextProfileDef, signals: &ContextSignals) -> String {
    match profile.id {
        "git" => "vcs=git".to_string(),
        "github" => match signals
            .remote
            .as_ref()
            .and_then(|remote| remote.slug.as_ref())
        {
            Some(slug) => format!("github remote `{slug}`"),
            None => "github remote".to_string(),
        },
        "rust" => "language=rust".to_string(),
        "node" => "ecosystem=node".to_string(),
        "python" => "language=python".to_string(),
        "swift" => "language=swift".to_string(),
        _ => "matched".to_string(),
    }
}

fn signal_has_language(fingerprint: &ProjectFingerprint, language: &str) -> bool {
    fingerprint.primary_language == language
        || fingerprint
            .languages
            .iter()
            .any(|candidate| candidate == language)
}

fn signal_has_package_manager(fingerprint: &ProjectFingerprint, package_manager: &str) -> bool {
    fingerprint.package_manager.as_deref() == Some(package_manager)
        || fingerprint
            .package_managers
            .iter()
            .any(|candidate| candidate == package_manager)
}

pub(super) fn unique_flatten(values: impl Iterator<Item = String>) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for value in values {
        if seen.insert(value.clone()) {
            out.push(value);
        }
    }
    out
}

fn unique_mcp_candidates(
    values: impl Iterator<Item = McpPresetCandidate>,
) -> Vec<McpPresetCandidate> {
    let mut out: BTreeMap<String, McpPresetCandidate> = BTreeMap::new();
    for value in values {
        out.entry(value.id.clone()).or_insert(value);
    }
    out.into_values().collect()
}
