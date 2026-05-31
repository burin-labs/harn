use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use ignore::gitignore::{Gitignore, GitignoreBuilder};
use ignore::WalkBuilder;
use sha2::{Digest, Sha256};

use crate::runtime_limits::RuntimeLimits;
use crate::value::{VmError, VmValue};
use crate::vm::Vm;

use super::process::resolve_source_relative_path;
use super::project_catalog::{project_catalog, ProjectCatalogEntry};
use super::project_enrich::register_project_enrich_builtin;

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

const FINGERPRINT_SKIP_DIRS: &[&str] = &[
    ".git",
    ".hg",
    ".next",
    ".svn",
    ".venv",
    "__pycache__",
    "build",
    "coverage",
    "dist",
    "node_modules",
    "target",
    "venv",
];
const PROJECT_FINGERPRINT_MAX_DEPTH: usize = RuntimeLimits::DEFAULT.max_project_fingerprint_depth;
const PROJECT_LANGUAGE_ORDER: &[&str] = &["rust", "typescript", "python", "go", "swift", "ruby"];
const PROJECT_FRAMEWORK_ORDER: &[&str] = &["axum", "next", "react", "django", "fastapi", "rails"];
const PROJECT_PACKAGE_MANAGER_ORDER: &[&str] = &[
    "cargo", "spm", "pnpm", "npm", "yarn", "uv", "poetry", "pip", "go-mod", "bundler",
];
const PROJECT_TEST_RUNNER_ORDER: &[&str] = &[
    "nextest",
    "cargo-test",
    "vitest",
    "jest",
    "mocha",
    "pytest",
    "unittest",
    "go-test",
    "xctest",
    "rspec",
    "minitest",
];
const PROJECT_BUILD_TOOL_ORDER: &[&str] = &[
    "cargo", "spm", "next", "vite", "uv", "poetry", "pnpm", "npm", "yarn", "go", "bundler", "pip",
];
const PROJECT_CI_ORDER: &[&str] = &[
    "github-actions",
    "gitlab-ci",
    "circleci",
    "buildkite",
    "azure-pipelines",
    "bitrise",
];
const PROJECT_LOCKFILES: &[(&str, Option<&str>)] = &[
    ("Cargo.lock", Some("cargo")),
    ("package-lock.json", Some("npm")),
    ("pnpm-lock.yaml", Some("pnpm")),
    ("yarn.lock", Some("yarn")),
    ("uv.lock", Some("uv")),
    ("poetry.lock", Some("poetry")),
    ("Pipfile.lock", Some("pip")),
    ("requirements.lock", Some("pip")),
    ("go.sum", Some("go-mod")),
    ("Gemfile.lock", Some("bundler")),
    ("Package.resolved", Some("spm")),
    ("bun.lockb", Some("npm")),
    ("bun.lock", Some("npm")),
];
const TEST_DIR_NAMES: &[&str] = &["tests", "test", "__tests__", "spec", "e2e", "cypress"];
const NEXT_CONFIG_NAMES: &[&str] = &["next.config.js", "next.config.mjs", "next.config.ts"];
const VITEST_CONFIG_NAMES: &[&str] = &["vitest.config.js", "vitest.config.ts", "vitest.config.mjs"];
const JEST_CONFIG_NAMES: &[&str] = &["jest.config.js", "jest.config.ts", "jest.config.cjs"];
const VITE_CONFIG_NAMES: &[&str] = &["vite.config.js", "vite.config.ts", "vite.config.mjs"];
const MOCHA_CONFIG_NAMES: &[&str] = &[
    ".mocharc.js",
    ".mocharc.json",
    ".mocharc.yml",
    ".mocharc.yaml",
];
const PYTEST_CONFIG_NAMES: &[&str] = &["pytest.ini", "tox.ini", "conftest.py"];
const NEXTEST_CONFIG_NAMES: &[&str] = &["nextest.toml"];
const CI_FILE_NAMES: &[&str] = &[
    ".gitlab-ci.yml",
    "azure-pipelines.yml",
    "bitrise.yml",
    "circle.yml",
];
const CONTEXT_PROFILE_SCHEMA_VERSION: i64 = 1;
const GITHUB_CREDENTIAL_ENV_KEYS: &[&str] =
    &["GITHUB_PERSONAL_ACCESS_TOKEN", "GITHUB_TOKEN", "GH_TOKEN"];

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

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct ProjectFingerprint {
    primary_language: String,
    languages: Vec<String>,
    frameworks: Vec<String>,
    package_manager: Option<String>,
    package_managers: Vec<String>,
    test_runner: Option<String>,
    build_tool: Option<String>,
    vcs: Option<String>,
    ci: Vec<String>,
    has_tests: bool,
    has_ci: bool,
    lockfile_paths: Vec<String>,
}

impl ProjectFingerprint {
    fn into_vm_value(self) -> VmValue {
        let mut value = BTreeMap::new();
        value.insert(
            "primary_language".to_string(),
            VmValue::String(std::sync::Arc::from(self.primary_language)),
        );
        value.insert(
            "languages".to_string(),
            VmValue::List(std::sync::Arc::new(
                self.languages
                    .into_iter()
                    .map(|item| VmValue::String(std::sync::Arc::from(item)))
                    .collect(),
            )),
        );
        value.insert(
            "frameworks".to_string(),
            VmValue::List(std::sync::Arc::new(
                self.frameworks
                    .into_iter()
                    .map(|item| VmValue::String(std::sync::Arc::from(item)))
                    .collect(),
            )),
        );
        value.insert(
            "package_manager".to_string(),
            self.package_manager
                .map(|value| VmValue::String(std::sync::Arc::from(value)))
                .unwrap_or(VmValue::Nil),
        );
        value.insert(
            "package_managers".to_string(),
            VmValue::List(std::sync::Arc::new(
                self.package_managers
                    .into_iter()
                    .map(|item| VmValue::String(std::sync::Arc::from(item)))
                    .collect(),
            )),
        );
        value.insert(
            "test_runner".to_string(),
            self.test_runner
                .map(|value| VmValue::String(std::sync::Arc::from(value)))
                .unwrap_or(VmValue::Nil),
        );
        value.insert(
            "build_tool".to_string(),
            self.build_tool
                .map(|value| VmValue::String(std::sync::Arc::from(value)))
                .unwrap_or(VmValue::Nil),
        );
        value.insert(
            "vcs".to_string(),
            self.vcs
                .map(|value| VmValue::String(std::sync::Arc::from(value)))
                .unwrap_or(VmValue::Nil),
        );
        value.insert(
            "ci".to_string(),
            VmValue::List(std::sync::Arc::new(
                self.ci
                    .into_iter()
                    .map(|item| VmValue::String(std::sync::Arc::from(item)))
                    .collect(),
            )),
        );
        value.insert("has_tests".to_string(), VmValue::Bool(self.has_tests));
        value.insert("has_ci".to_string(), VmValue::Bool(self.has_ci));
        value.insert(
            "lockfile_paths".to_string(),
            VmValue::List(std::sync::Arc::new(
                self.lockfile_paths
                    .into_iter()
                    .map(|item| VmValue::String(std::sync::Arc::from(item)))
                    .collect(),
            )),
        );
        VmValue::Dict(std::sync::Arc::new(value))
    }
}

#[derive(Debug, Default)]
struct FingerprintSignals {
    languages: BTreeSet<String>,
    frameworks: BTreeSet<String>,
    package_managers: BTreeSet<String>,
    test_runners: BTreeSet<String>,
    build_tools: BTreeSet<String>,
    ci: BTreeSet<String>,
    lockfile_paths: BTreeSet<String>,
    has_tests: bool,
    has_spec_dir: bool,
    has_test_dir: bool,
    node_project: bool,
    python_project: bool,
    python_needs_pip: bool,
    ruby_project: bool,
    has_next_dep: bool,
    has_next_config: bool,
    has_vite_dep: bool,
    has_vite_config: bool,
    has_pytest_signal: bool,
    has_unittest_signal: bool,
}

#[derive(Debug, Clone, Copy, Default, Eq, Ord, PartialEq, PartialOrd)]
enum ScanTier {
    #[default]
    Ambient,
    Config,
}

#[derive(Debug, Clone)]
struct ProjectScanOptions {
    tiers: BTreeSet<ScanTier>,
    depth: Option<usize>,
    include_hidden: bool,
    include_vendor: bool,
    respect_gitignore: bool,
}

impl Default for ProjectScanOptions {
    fn default() -> Self {
        Self {
            tiers: BTreeSet::from([ScanTier::Ambient]),
            depth: Some(3),
            include_hidden: false,
            include_vendor: false,
            respect_gitignore: true,
        }
    }
}

#[derive(Debug, Clone, Default)]
struct ProjectTreeEntry {
    relative_path: String,
    metadata_path: String,
    structure_hash: String,
    content_hash: String,
}

impl ProjectTreeEntry {
    fn into_vm_value(self) -> VmValue {
        let mut value = BTreeMap::new();
        value.insert(
            "path".to_string(),
            VmValue::String(std::sync::Arc::from(self.relative_path)),
        );
        value.insert(
            "dir".to_string(),
            VmValue::String(std::sync::Arc::from(self.metadata_path)),
        );
        value.insert(
            "structure_hash".to_string(),
            VmValue::String(std::sync::Arc::from(self.structure_hash)),
        );
        value.insert(
            "content_hash".to_string(),
            VmValue::String(std::sync::Arc::from(self.content_hash)),
        );
        VmValue::Dict(std::sync::Arc::new(value))
    }
}

#[derive(Debug, Clone, Default)]
struct ProjectEvidence {
    path: PathBuf,
    language_scores: BTreeMap<String, f64>,
    framework_scores: BTreeMap<String, f64>,
    build_systems: BTreeSet<String>,
    vcs: Option<String>,
    lockfiles: BTreeSet<String>,
    anchors: BTreeSet<String>,
    package_name: Option<String>,
    build_commands: Vec<String>,
    declared_scripts: BTreeMap<String, String>,
    readme_code_fences: Vec<String>,
    dockerfile_commands: Vec<String>,
    makefile_targets: Vec<String>,
}

impl ProjectEvidence {
    fn into_vm_value(self) -> VmValue {
        let confidence = confidence_value(&self);
        let mut result = BTreeMap::new();
        result.insert(
            "path".to_string(),
            VmValue::String(std::sync::Arc::from(
                self.path.to_string_lossy().into_owned(),
            )),
        );
        result.insert(
            "languages".to_string(),
            VmValue::List(std::sync::Arc::new(
                sorted_confident_labels(&self.language_scores)
                    .into_iter()
                    .map(|name| VmValue::String(std::sync::Arc::from(name)))
                    .collect(),
            )),
        );
        result.insert(
            "frameworks".to_string(),
            VmValue::List(std::sync::Arc::new(
                sorted_confident_labels(&self.framework_scores)
                    .into_iter()
                    .map(|name| VmValue::String(std::sync::Arc::from(name)))
                    .collect(),
            )),
        );
        result.insert(
            "build_systems".to_string(),
            VmValue::List(std::sync::Arc::new(
                self.build_systems
                    .into_iter()
                    .map(|name| VmValue::String(std::sync::Arc::from(name)))
                    .collect(),
            )),
        );
        result.insert(
            "vcs".to_string(),
            self.vcs
                .map(|value| VmValue::String(std::sync::Arc::from(value)))
                .unwrap_or(VmValue::Nil),
        );
        result.insert(
            "lockfiles".to_string(),
            VmValue::List(std::sync::Arc::new(
                self.lockfiles
                    .into_iter()
                    .map(|name| VmValue::String(std::sync::Arc::from(name)))
                    .collect(),
            )),
        );
        result.insert(
            "anchors".to_string(),
            VmValue::List(std::sync::Arc::new(
                self.anchors
                    .into_iter()
                    .map(|name| VmValue::String(std::sync::Arc::from(name)))
                    .collect(),
            )),
        );
        result.insert("confidence".to_string(), confidence);
        result.insert(
            "package_name".to_string(),
            self.package_name
                .map(|value| VmValue::String(std::sync::Arc::from(value)))
                .unwrap_or(VmValue::Nil),
        );
        result.insert(
            "build_commands".to_string(),
            VmValue::List(std::sync::Arc::new(
                self.build_commands
                    .into_iter()
                    .map(|cmd| VmValue::String(std::sync::Arc::from(cmd)))
                    .collect(),
            )),
        );
        result.insert(
            "declared_scripts".to_string(),
            VmValue::Dict(std::sync::Arc::new(
                self.declared_scripts
                    .into_iter()
                    .map(|(k, v)| (k, VmValue::String(std::sync::Arc::from(v))))
                    .collect(),
            )),
        );
        result.insert(
            "readme_code_fences".to_string(),
            VmValue::List(std::sync::Arc::new(
                self.readme_code_fences
                    .into_iter()
                    .map(|lang| VmValue::String(std::sync::Arc::from(lang)))
                    .collect(),
            )),
        );
        result.insert(
            "dockerfile_commands".to_string(),
            VmValue::List(std::sync::Arc::new(
                self.dockerfile_commands
                    .into_iter()
                    .map(|cmd| VmValue::String(std::sync::Arc::from(cmd)))
                    .collect(),
            )),
        );
        result.insert(
            "makefile_targets".to_string(),
            VmValue::List(std::sync::Arc::new(
                self.makefile_targets
                    .into_iter()
                    .map(|target| VmValue::String(std::sync::Arc::from(target)))
                    .collect(),
            )),
        );
        VmValue::Dict(std::sync::Arc::new(result))
    }
}

#[derive(Debug, Clone, Default)]
struct ContextProfileOptions {
    fingerprint: Option<ProjectFingerprint>,
    remote: Option<GitRemoteSignal>,
    signal_source: Option<String>,
    credentials: BTreeSet<String>,
    include_env_credentials: bool,
}

#[derive(Debug, Clone, Default)]
struct ContextSignals {
    fingerprint: ProjectFingerprint,
    remote: Option<GitRemoteSignal>,
    source: String,
    credentials: BTreeSet<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct GitRemoteSignal {
    name: String,
    host: String,
    slug: Option<String>,
    redacted_url: String,
}

#[derive(Debug, Clone)]
struct ContextProfileFragment {
    id: String,
    source: String,
    body: String,
    requires_caps: Vec<String>,
}

#[derive(Debug, Clone)]
struct McpPresetCandidate {
    id: String,
    status: String,
    missing_credentials: Vec<String>,
}

#[derive(Debug, Clone)]
struct ContextProfileActivation {
    id: String,
    reason: String,
    caps: Vec<String>,
    skills: Vec<String>,
    tool_groups: Vec<String>,
    mcp_presets: Vec<String>,
    mcp_preset_candidates: Vec<McpPresetCandidate>,
    prompt_fragment: ContextProfileFragment,
}

#[derive(Debug, Clone)]
struct ContextProfileResolution {
    path: PathBuf,
    signals: ContextSignals,
    profiles: Vec<ContextProfileActivation>,
    always_on_prompt_tokens: i64,
    activated_prompt_tokens: i64,
    always_on_prompt_bytes: usize,
    activated_prompt_bytes: usize,
}

impl ContextProfileResolution {
    fn into_vm_value(self) -> VmValue {
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
        out.insert(
            "path".to_string(),
            VmValue::String(std::sync::Arc::from(
                self.path.to_string_lossy().into_owned(),
            )),
        );
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
        out.insert(
            "token_delta".to_string(),
            VmValue::Dict(std::sync::Arc::new(token_delta)),
        );
        VmValue::Dict(std::sync::Arc::new(out))
    }
}

impl ContextSignals {
    fn into_vm_value(self) -> VmValue {
        let mut out = BTreeMap::new();
        out.insert("source".to_string(), string_value(self.source));
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
        VmValue::Dict(std::sync::Arc::new(out))
    }
}

impl GitRemoteSignal {
    fn into_vm_value(self) -> VmValue {
        let mut out = BTreeMap::new();
        out.insert("name".to_string(), string_value(self.name));
        out.insert("host".to_string(), string_value(self.host));
        out.insert(
            "slug".to_string(),
            self.slug.map(string_value).unwrap_or(VmValue::Nil),
        );
        out.insert("url".to_string(), string_value(self.redacted_url));
        VmValue::Dict(std::sync::Arc::new(out))
    }
}

impl ContextProfileFragment {
    fn into_vm_value(self) -> VmValue {
        let mut out = BTreeMap::new();
        out.insert("id".to_string(), string_value(self.id));
        out.insert("source".to_string(), string_value(self.source));
        out.insert("body".to_string(), string_value(self.body));
        out.insert(
            "requires_caps".to_string(),
            string_list_value(self.requires_caps),
        );
        VmValue::Dict(std::sync::Arc::new(out))
    }
}

impl McpPresetCandidate {
    fn into_vm_value(self) -> VmValue {
        let mut out = BTreeMap::new();
        out.insert("id".to_string(), string_value(self.id));
        out.insert("status".to_string(), string_value(self.status));
        out.insert(
            "missing_credentials".to_string(),
            string_list_value(self.missing_credentials),
        );
        VmValue::Dict(std::sync::Arc::new(out))
    }
}

impl ContextProfileActivation {
    fn into_vm_value(self) -> VmValue {
        let mut out = BTreeMap::new();
        out.insert("id".to_string(), string_value(self.id));
        out.insert("reason".to_string(), string_value(self.reason));
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
        VmValue::Dict(std::sync::Arc::new(out))
    }
}

pub(crate) fn register_project_builtins(vm: &mut Vm) {
    for def in MODULE_BUILTINS {
        vm.register_builtin_def(def);
    }
    register_project_enrich_builtin(vm);
}

pub(crate) const MODULE_BUILTINS: &[&crate::stdlib::macros::VmBuiltinDef] = &[
    &PROJECT_CONTEXT_PROFILE_NATIVE_IMPL_DEF,
    &PROJECT_FINGERPRINT_IMPL_DEF,
    &PROJECT_SCAN_NATIVE_IMPL_DEF,
    &PROJECT_SCAN_TREE_NATIVE_IMPL_DEF,
    &PROJECT_WALK_TREE_NATIVE_IMPL_DEF,
    &PROJECT_CATALOG_NATIVE_IMPL_DEF,
];

#[crate::stdlib::macros::harn_builtin(
    sig = "project_context_profile_native(path?: string, options?: dict) -> dict",
    category = "project"
)]
fn project_context_profile_native_impl(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    if args.len() > 2 {
        return Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
            "project_context_profile: expected at most 2 arguments",
        ))));
    }
    let path = args
        .first()
        .map(|value| value.display())
        .unwrap_or_else(|| ".".to_string());
    let options = parse_context_profile_options(args.get(1));
    let root = if options.fingerprint.is_none() {
        resolve_existing_directory(&path)?
    } else {
        resolve_source_relative_path(&path)
            .canonicalize()
            .unwrap_or_else(|_| resolve_source_relative_path(&path))
    };
    Ok(resolve_context_profile(&root, options).into_vm_value())
}

#[crate::stdlib::macros::harn_builtin(
    sig = "project_fingerprint(path?: string) -> dict",
    category = "project"
)]
fn project_fingerprint_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    if args.len() > 1 {
        return Err(VmError::Thrown(VmValue::String(std::sync::Arc::from(
            "project_fingerprint: expected at most 1 argument",
        ))));
    }
    let path = args
        .first()
        .map(|value| value.display())
        .unwrap_or_else(|| ".".to_string());
    let root = resolve_existing_directory(&path)?;
    Ok(detect_project_fingerprint(&root).into_vm_value())
}

#[crate::stdlib::macros::harn_builtin(
    sig = "project_scan_native(path?: string, options?: dict) -> dict",
    category = "project"
)]
fn project_scan_native_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let path = args
        .first()
        .map(|value| value.display())
        .unwrap_or_else(|| ".".to_string());
    let options = parse_project_options(args.get(1));
    let root = resolve_existing_directory(&path)?;
    Ok(scan_exact_directory(&root, &options).into_vm_value())
}

#[crate::stdlib::macros::harn_builtin(
    sig = "project_scan_tree_native(path?: string, options?: dict) -> dict",
    category = "project"
)]
fn project_scan_tree_native_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let path = args
        .first()
        .map(|value| value.display())
        .unwrap_or_else(|| ".".to_string());
    let options = parse_project_options(args.get(1));
    let base = resolve_existing_directory(&path)?;
    let tree = scan_project_tree(&base, &options)?;
    Ok(VmValue::Dict(std::sync::Arc::new(
        tree.into_iter()
            .map(|(rel, evidence)| (rel, evidence.into_vm_value()))
            .collect(),
    )))
}

#[crate::stdlib::macros::harn_builtin(
    sig = "project_walk_tree_native(path?: string, options?: dict) -> list",
    category = "project"
)]
fn project_walk_tree_native_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let path = args
        .first()
        .map(|value| value.display())
        .unwrap_or_else(|| ".".to_string());
    let options = parse_project_options(args.get(1));
    let base = resolve_existing_directory(&path)?;
    let tree = walk_project_tree(&base, &options)?;
    Ok(VmValue::List(std::sync::Arc::new(
        tree.into_iter()
            .map(ProjectTreeEntry::into_vm_value)
            .collect(),
    )))
}

#[crate::stdlib::macros::harn_builtin(
    sig = "project_catalog_native() -> list",
    category = "project"
)]
fn project_catalog_native_impl(_args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let entries = project_catalog()
        .iter()
        .map(catalog_entry_value)
        .collect::<Vec<_>>();
    Ok(VmValue::List(std::sync::Arc::new(entries)))
}

pub(crate) fn project_scan_config_value(dir: &Path) -> VmValue {
    let mut options = ProjectScanOptions::default();
    options.tiers.insert(ScanTier::Config);
    scan_exact_directory(dir, &options).into_vm_value()
}

fn parse_project_options(value: Option<&VmValue>) -> ProjectScanOptions {
    let mut options = ProjectScanOptions::default();
    let Some(dict) = value.and_then(VmValue::as_dict) else {
        return options;
    };

    if let Some(depth_value) = dict.get("depth") {
        options.depth = match depth_value {
            VmValue::Nil => None,
            _ => depth_value
                .as_int()
                .map(|raw_depth| raw_depth.max(0) as usize),
        };
    }
    if let Some(include_hidden) = dict.get("include_hidden").and_then(value_as_bool) {
        options.include_hidden = include_hidden;
    }
    if let Some(include_vendor) = dict.get("include_vendor").and_then(value_as_bool) {
        options.include_vendor = include_vendor;
    }
    if let Some(respect_gitignore) = dict.get("respect_gitignore").and_then(value_as_bool) {
        options.respect_gitignore = respect_gitignore;
    }
    if let Some(tiers) = dict.get("tiers").and_then(value_as_list) {
        options.tiers.clear();
        for tier in tiers.iter().map(VmValue::display) {
            match tier.as_str() {
                "ambient" => {
                    options.tiers.insert(ScanTier::Ambient);
                }
                "config" => {
                    options.tiers.insert(ScanTier::Config);
                }
                _ => {}
            }
        }
        if options.tiers.is_empty() {
            options.tiers.insert(ScanTier::Ambient);
        }
    }

    options
}

fn parse_context_profile_options(value: Option<&VmValue>) -> ContextProfileOptions {
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

fn resolve_context_profile(
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

fn mcp_preset_candidate(id: &str, credentials: &BTreeSet<String>) -> McpPresetCandidate {
    let missing_credentials = missing_preset_credentials(id, credentials);
    McpPresetCandidate {
        id: id.to_string(),
        status: if missing_credentials.is_empty() {
            "ready".to_string()
        } else {
            "needs_credentials".to_string()
        },
        missing_credentials,
    }
}

fn missing_preset_credentials(id: &str, credentials: &BTreeSet<String>) -> Vec<String> {
    let Some(preset) = crate::mcp_presets::preset(id) else {
        return Vec::new();
    };
    let mut missing = Vec::new();
    for placeholder in preset.placeholders {
        if !placeholder.required {
            continue;
        }
        if placeholder.target != crate::mcp_presets::PlaceholderTarget::Env {
            missing.push(placeholder.key.to_string());
            continue;
        }
        let alias = credential_alias(placeholder.key);
        if !credentials.contains(&alias) {
            missing.push(placeholder.key.to_string());
        }
    }
    missing
}

fn parse_credentials(value: &VmValue) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    match value {
        VmValue::List(items) => {
            for item in items.iter() {
                let credential = credential_alias(&item.display());
                if !credential.is_empty() {
                    out.insert(credential);
                }
            }
        }
        VmValue::Dict(dict) => {
            for (key, value) in dict.iter() {
                if matches!(value, VmValue::Bool(false) | VmValue::Nil) {
                    continue;
                }
                let credential = credential_alias(key);
                if !credential.is_empty() {
                    out.insert(credential);
                }
            }
        }
        _ => {}
    }
    out
}

fn env_credentials() -> BTreeSet<String> {
    GITHUB_CREDENTIAL_ENV_KEYS
        .iter()
        .filter(|key| {
            std::env::var(key)
                .ok()
                .is_some_and(|value| !value.is_empty())
        })
        .map(|key| credential_alias(key))
        .collect()
}

fn credential_alias(value: &str) -> String {
    let normalized = value.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        return String::new();
    }
    if normalized.contains("github") || normalized == "gh_token" || normalized == "gh-token" {
        return "github".to_string();
    }
    normalized.replace('-', "_")
}

fn project_fingerprint_from_value(value: &VmValue) -> Option<ProjectFingerprint> {
    project_fingerprint_from_dict(value.as_dict()?)
}

fn project_fingerprint_from_dict(dict: &BTreeMap<String, VmValue>) -> Option<ProjectFingerprint> {
    if !dict_has_project_fingerprint_shape(dict) {
        return None;
    }
    let languages = string_list_field(dict, "languages");
    let primary_language = dict
        .get("primary_language")
        .and_then(value_as_string)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| match languages.as_slice() {
            [] => "unknown".to_string(),
            [only] => only.clone(),
            _ => "mixed".to_string(),
        });
    let ci = string_list_field(dict, "ci");
    Some(ProjectFingerprint {
        primary_language,
        languages,
        frameworks: string_list_field(dict, "frameworks"),
        package_manager: optional_string_field(dict, "package_manager"),
        package_managers: string_list_field(dict, "package_managers"),
        test_runner: optional_string_field(dict, "test_runner"),
        build_tool: optional_string_field(dict, "build_tool"),
        vcs: optional_string_field(dict, "vcs"),
        has_tests: dict
            .get("has_tests")
            .and_then(value_as_bool)
            .unwrap_or(false),
        has_ci: dict
            .get("has_ci")
            .and_then(value_as_bool)
            .unwrap_or(!ci.is_empty()),
        ci,
        lockfile_paths: string_list_field(dict, "lockfile_paths"),
    })
}

fn dict_has_project_fingerprint_shape(dict: &BTreeMap<String, VmValue>) -> bool {
    [
        "primary_language",
        "languages",
        "frameworks",
        "package_manager",
        "package_managers",
        "test_runner",
        "build_tool",
        "vcs",
        "has_tests",
        "has_ci",
        "ci",
        "lockfile_paths",
    ]
    .iter()
    .any(|key| dict.contains_key(*key))
}

fn remote_signal_from_value(value: &VmValue) -> Option<GitRemoteSignal> {
    match value {
        VmValue::String(url) => remote_signal_from_url("origin", url),
        VmValue::Dict(dict) => {
            let name = optional_string_field(dict, "name").unwrap_or_else(|| "origin".to_string());
            let url = optional_string_field(dict, "url").unwrap_or_default();
            let host = optional_string_field(dict, "host")
                .or_else(|| remote_host(&url))
                .map(normalize_remote_host)
                .unwrap_or_default();
            let slug = optional_string_field(dict, "slug")
                .and_then(|slug| normalize_github_slug(&slug))
                .or_else(|| github_slug_from_remote(&url));
            if host.is_empty() && slug.is_none() && url.is_empty() {
                return None;
            }
            Some(GitRemoteSignal {
                name,
                host,
                slug,
                redacted_url: redact_remote_url(&url),
            })
        }
        _ => None,
    }
}

fn detect_git_remote(dir: &Path) -> Option<GitRemoteSignal> {
    let git_path = find_git_path(dir)?;
    let mut remotes = Vec::new();
    for config in git_config_paths(&git_path) {
        let Some(text) = read_text_if_exists(config) else {
            continue;
        };
        remotes.extend(parse_git_config_remotes(&text));
    }
    remotes
        .iter()
        .find(|remote| remote.name == "origin")
        .cloned()
        .or_else(|| remotes.into_iter().next())
}

fn find_git_path(dir: &Path) -> Option<PathBuf> {
    let mut cursor = Some(dir);
    while let Some(path) = cursor {
        let git_path = path.join(".git");
        if git_path.exists() {
            return Some(git_path);
        }
        cursor = path.parent();
    }
    None
}

fn git_config_paths(git_path: &Path) -> Vec<PathBuf> {
    if git_path.is_dir() {
        return vec![git_path.join("config")];
    }
    let Some(git_dir) = read_gitdir_file(git_path) else {
        return Vec::new();
    };
    let mut out = vec![git_dir.join("config")];
    if let Some(common_dir) = read_commondir(&git_dir) {
        out.push(common_dir.join("config"));
    }
    out
}

fn read_gitdir_file(git_path: &Path) -> Option<PathBuf> {
    let text = read_text_if_exists(git_path.to_path_buf())?;
    let raw = text.trim().strip_prefix("gitdir:")?.trim();
    let candidate = PathBuf::from(raw);
    if candidate.is_absolute() {
        Some(candidate)
    } else {
        Some(git_path.parent()?.join(candidate))
    }
}

fn read_commondir(git_dir: &Path) -> Option<PathBuf> {
    let raw = read_text_if_exists(git_dir.join("commondir"))?;
    let candidate = PathBuf::from(raw.trim());
    if candidate.is_absolute() {
        Some(candidate)
    } else {
        Some(git_dir.join(candidate))
    }
}

fn parse_git_config_remotes(config: &str) -> Vec<GitRemoteSignal> {
    let mut current_remote: Option<String> = None;
    let mut remotes = Vec::new();
    for line in config.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            current_remote = parse_remote_section(trimmed);
            continue;
        }
        let Some(name) = current_remote.as_deref() else {
            continue;
        };
        let Some((key, value)) = trimmed.split_once('=') else {
            continue;
        };
        if key.trim() == "url" {
            if let Some(signal) = remote_signal_from_url(name, value.trim()) {
                remotes.push(signal);
            }
        }
    }
    remotes
}

fn parse_remote_section(section: &str) -> Option<String> {
    let inner = section.strip_prefix('[')?.strip_suffix(']')?.trim();
    let rest = inner.strip_prefix("remote")?.trim();
    let quoted = rest.strip_prefix('"')?;
    let (name, _) = quoted.split_once('"')?;
    Some(name.to_string())
}

fn remote_signal_from_url(name: &str, url: &str) -> Option<GitRemoteSignal> {
    let host = remote_host(url).map(normalize_remote_host)?;
    Some(GitRemoteSignal {
        name: name.to_string(),
        slug: github_slug_from_remote(url),
        host,
        redacted_url: redact_remote_url(url),
    })
}

fn remote_host(url: &str) -> Option<String> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some(rest) = trimmed.split_once("://").map(|(_, rest)| rest) {
        let authority = rest.split('/').next().unwrap_or_default();
        let host_port = authority.rsplit('@').next().unwrap_or(authority);
        let host = host_port.split(':').next().unwrap_or_default();
        return (!host.is_empty()).then(|| host.to_string());
    }
    if let Some((left, _path)) = trimmed.split_once(':') {
        let host = left.rsplit('@').next().unwrap_or(left);
        return (!host.is_empty()).then(|| host.to_string());
    }
    None
}

fn normalize_remote_host(host: String) -> String {
    let normalized = host.trim().trim_end_matches('.').to_ascii_lowercase();
    if normalized == "github" {
        "github.com".to_string()
    } else {
        normalized
    }
}

fn github_slug_from_remote(url: &str) -> Option<String> {
    let host = remote_host(url).map(normalize_remote_host)?;
    if host != "github.com" {
        return None;
    }
    let path = if let Some(rest) = url.split_once("://").map(|(_, rest)| rest) {
        rest.split_once('/')
            .map(|(_, path)| path)
            .unwrap_or_default()
            .to_string()
    } else {
        url.split_once(':')
            .map(|(_, path)| path)
            .unwrap_or_default()
            .to_string()
    };
    normalize_github_slug(&path)
}

fn normalize_github_slug(value: &str) -> Option<String> {
    let mut path = strip_url_suffix(value.trim())
        .trim_start_matches('/')
        .trim_end_matches('/')
        .to_string();
    if let Some(stripped) = path.strip_suffix(".git") {
        path = stripped.to_string();
    }
    let mut parts = path.split('/').filter(|part| !part.is_empty());
    let owner = parts.next()?;
    let repo = parts.next()?;
    Some(format!("{owner}/{repo}"))
}

fn redact_remote_url(url: &str) -> String {
    let sanitized = strip_url_suffix(url.trim()).to_string();
    let Some((scheme, rest)) = sanitized.split_once("://") else {
        return sanitized;
    };
    let Some((userinfo, tail)) = rest.split_once('@') else {
        return sanitized;
    };
    if userinfo.is_empty() || tail.is_empty() {
        return sanitized;
    }
    format!("{scheme}://<redacted>@{tail}")
}

fn strip_url_suffix(value: &str) -> &str {
    let mut sanitized = value;
    for marker in ['?', '#'] {
        if let Some((head, _)) = sanitized.split_once(marker) {
            sanitized = head;
        }
    }
    sanitized
}

fn string_list_field(dict: &BTreeMap<String, VmValue>, key: &str) -> Vec<String> {
    match dict.get(key) {
        Some(VmValue::List(items)) => items
            .iter()
            .map(VmValue::display)
            .filter(|value| !value.is_empty())
            .collect(),
        Some(value) => {
            let value = value.display();
            if value.is_empty() {
                Vec::new()
            } else {
                vec![value]
            }
        }
        None => Vec::new(),
    }
}

fn optional_string_field(dict: &BTreeMap<String, VmValue>, key: &str) -> Option<String> {
    dict.get(key)
        .and_then(value_as_string)
        .filter(|value| !value.is_empty())
}

fn value_as_string(value: &VmValue) -> Option<String> {
    match value {
        VmValue::Nil => None,
        VmValue::String(s) => Some(s.to_string()),
        other => Some(other.display()),
    }
}

fn string_value(value: impl Into<String>) -> VmValue {
    VmValue::String(std::sync::Arc::from(value.into()))
}

fn string_list_value(values: Vec<String>) -> VmValue {
    VmValue::List(std::sync::Arc::new(
        values.into_iter().map(string_value).collect(),
    ))
}

fn unique_flatten(values: impl Iterator<Item = String>) -> Vec<String> {
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

fn value_as_bool(value: &VmValue) -> Option<bool> {
    match value {
        VmValue::Bool(flag) => Some(*flag),
        _ => None,
    }
}

fn value_as_list(value: &VmValue) -> Option<&[VmValue]> {
    match value {
        VmValue::List(items) => Some(items.as_slice()),
        _ => None,
    }
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
        target.canonicalize().map_err(path_error)
    } else {
        Err(path_missing_error(&target))
    }
}

fn detect_project_fingerprint(dir: &Path) -> ProjectFingerprint {
    let mut signals = FingerprintSignals::default();
    walk_project_fingerprint(dir, dir, 0, &mut signals);

    if signals.has_next_dep && signals.has_next_config {
        signals.frameworks.insert("next".to_string());
        signals.languages.insert("typescript".to_string());
        signals.build_tools.insert("next".to_string());
    }
    if signals.node_project
        && !signals.package_managers.contains("npm")
        && !signals.package_managers.contains("pnpm")
        && !signals.package_managers.contains("yarn")
    {
        signals.package_managers.insert("npm".to_string());
    }
    if signals.python_project
        && !signals.package_managers.contains("poetry")
        && !signals.package_managers.contains("uv")
        && signals.python_needs_pip
    {
        signals.package_managers.insert("pip".to_string());
    }
    if signals.languages.contains("rust") {
        signals.build_tools.insert("cargo".to_string());
        signals.test_runners.insert("cargo-test".to_string());
    }
    if signals.languages.contains("go") {
        signals.package_managers.insert("go-mod".to_string());
        signals.build_tools.insert("go".to_string());
        signals.test_runners.insert("go-test".to_string());
    }
    if signals.languages.contains("swift") {
        signals.package_managers.insert("spm".to_string());
        signals.build_tools.insert("spm".to_string());
        signals.test_runners.insert("xctest".to_string());
    }
    if signals.python_project && signals.test_runners.is_empty() {
        signals.test_runners.insert("pytest".to_string());
    }
    if signals.ruby_project {
        signals.package_managers.insert("bundler".to_string());
        signals.build_tools.insert("bundler".to_string());
        if signals.test_runners.is_empty() {
            if signals.has_spec_dir {
                signals.test_runners.insert("rspec".to_string());
            } else if signals.has_test_dir {
                signals.test_runners.insert("minitest".to_string());
            }
        }
    }
    if signals.has_vite_dep || signals.has_vite_config {
        signals.build_tools.insert("vite".to_string());
        if signals.has_tests && signals.test_runners.is_empty() {
            signals.test_runners.insert("vitest".to_string());
        }
    }
    if signals.node_project && signals.build_tools.is_empty() {
        if signals.package_managers.contains("pnpm") {
            signals.build_tools.insert("pnpm".to_string());
        } else if signals.package_managers.contains("yarn") {
            signals.build_tools.insert("yarn".to_string());
        } else if signals.package_managers.contains("npm") {
            signals.build_tools.insert("npm".to_string());
        }
    }
    if signals.python_project && signals.build_tools.is_empty() {
        if signals.package_managers.contains("uv") {
            signals.build_tools.insert("uv".to_string());
        } else if signals.package_managers.contains("poetry") {
            signals.build_tools.insert("poetry".to_string());
        } else if signals.package_managers.contains("pip") {
            signals.build_tools.insert("pip".to_string());
        }
    }

    let languages = ordered_values(&signals.languages, PROJECT_LANGUAGE_ORDER);
    let frameworks = ordered_values(&signals.frameworks, PROJECT_FRAMEWORK_ORDER);
    let package_managers = ordered_values(&signals.package_managers, PROJECT_PACKAGE_MANAGER_ORDER);
    let test_runners = ordered_values(&signals.test_runners, PROJECT_TEST_RUNNER_ORDER);
    let build_tools = ordered_values(&signals.build_tools, PROJECT_BUILD_TOOL_ORDER);
    let ci = ordered_values(&signals.ci, PROJECT_CI_ORDER);
    let primary_language = match languages.as_slice() {
        [] => "unknown".to_string(),
        [only] => only.clone(),
        _ => "mixed".to_string(),
    };

    ProjectFingerprint {
        primary_language,
        languages,
        frameworks,
        package_manager: first_ordered_value(&package_managers),
        package_managers,
        test_runner: first_ordered_value(&test_runners),
        build_tool: first_ordered_value(&build_tools),
        vcs: detect_vcs(dir),
        ci: ci.clone(),
        has_tests: signals.has_tests,
        has_ci: !ci.is_empty(),
        lockfile_paths: signals.lockfile_paths.into_iter().collect(),
    }
}

fn walk_project_fingerprint(
    base: &Path,
    dir: &Path,
    depth: usize,
    signals: &mut FingerprintSignals,
) {
    let Ok(read_dir) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries = read_dir.flatten().collect::<Vec<_>>();
    entries.sort_by_key(|entry| entry.file_name());

    for entry in entries {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = entry.path();
        let rel = relative_posix(base, &path);

        if file_type.is_dir() {
            inspect_fingerprint_dir(&rel, &name, signals);
            if depth < PROJECT_FINGERPRINT_MAX_DEPTH
                && !FINGERPRINT_SKIP_DIRS.contains(&name.as_str())
            {
                walk_project_fingerprint(base, &path, depth + 1, signals);
            }
            continue;
        }

        if file_type.is_file() {
            inspect_fingerprint_file(&path, &rel, &name, signals);
        }
    }
}

fn inspect_fingerprint_dir(rel: &str, name: &str, signals: &mut FingerprintSignals) {
    let lower_name = name.to_ascii_lowercase();
    if TEST_DIR_NAMES.contains(&lower_name.as_str()) {
        signals.has_tests = true;
        if lower_name == "spec" {
            signals.has_spec_dir = true;
        }
        if lower_name == "test" || lower_name == "tests" {
            signals.has_test_dir = true;
        }
    }
    if rel == ".github/workflows" || rel.ends_with("/.github/workflows") {
        signals.ci.insert("github-actions".to_string());
    }
    if name == ".circleci" {
        signals.ci.insert("circleci".to_string());
    }
    if name == ".buildkite" {
        signals.ci.insert("buildkite".to_string());
    }
    match name {
        "crates" => {
            signals.languages.insert("rust".to_string());
        }
        "cmd" | "pkg" => {
            signals.languages.insert("go".to_string());
        }
        ".git" => {}
        ".hg" => {}
        _ => {}
    }
}

fn inspect_fingerprint_file(path: &Path, rel: &str, name: &str, signals: &mut FingerprintSignals) {
    if let Some((_lockfile, manager)) = PROJECT_LOCKFILES
        .iter()
        .find(|(lockfile, _manager)| *lockfile == name)
    {
        signals.lockfile_paths.insert(rel.to_string());
        if let Some(manager) = manager {
            signals.package_managers.insert((*manager).to_string());
        }
    }
    if rel.starts_with(".github/workflows/") || rel == ".github/workflows" {
        signals.ci.insert("github-actions".to_string());
    }
    if CI_FILE_NAMES.contains(&name) {
        match name {
            ".gitlab-ci.yml" => {
                signals.ci.insert("gitlab-ci".to_string());
            }
            "azure-pipelines.yml" => {
                signals.ci.insert("azure-pipelines".to_string());
            }
            "bitrise.yml" => {
                signals.ci.insert("bitrise".to_string());
            }
            "circle.yml" => {
                signals.ci.insert("circleci".to_string());
            }
            _ => {}
        }
    }

    match name {
        "Cargo.toml" => inspect_cargo_manifest(path, signals),
        "package.json" => inspect_package_json(path, signals),
        "pyproject.toml" => inspect_pyproject(path, signals),
        "requirements.txt" | "requirements-dev.txt" | "requirements-test.txt" => {
            inspect_python_requirements(path, signals);
        }
        "setup.py" => {
            signals.languages.insert("python".to_string());
            signals.python_project = true;
            signals.python_needs_pip = true;
            inspect_python_text(read_text_if_exists(path.to_path_buf()).as_deref(), signals);
        }
        "go.mod" => {
            signals.languages.insert("go".to_string());
            signals.package_managers.insert("go-mod".to_string());
            signals.build_tools.insert("go".to_string());
            signals.test_runners.insert("go-test".to_string());
        }
        "Package.swift" => {
            signals.languages.insert("swift".to_string());
            signals.package_managers.insert("spm".to_string());
            signals.build_tools.insert("spm".to_string());
            signals.test_runners.insert("xctest".to_string());
        }
        "Gemfile" => inspect_gemfile(path, signals),
        _ => {}
    }

    if NEXT_CONFIG_NAMES.contains(&name) {
        signals.has_next_config = true;
        signals.languages.insert("typescript".to_string());
        signals.build_tools.insert("next".to_string());
    }
    if VITEST_CONFIG_NAMES.contains(&name) {
        signals.test_runners.insert("vitest".to_string());
        signals.has_tests = true;
    }
    if JEST_CONFIG_NAMES.contains(&name) {
        signals.test_runners.insert("jest".to_string());
        signals.has_tests = true;
    }
    if VITE_CONFIG_NAMES.contains(&name) {
        signals.has_vite_config = true;
        signals.build_tools.insert("vite".to_string());
    }
    if MOCHA_CONFIG_NAMES.contains(&name) {
        signals.test_runners.insert("mocha".to_string());
        signals.has_tests = true;
    }
    if PYTEST_CONFIG_NAMES.contains(&name) {
        signals.has_pytest_signal = true;
        signals.test_runners.insert("pytest".to_string());
        signals.has_tests = true;
    }
    if NEXTEST_CONFIG_NAMES.contains(&name) || rel.ends_with("/.config/nextest.toml") {
        signals.test_runners.insert("nextest".to_string());
    }

    match path.extension().and_then(|ext| ext.to_str()) {
        Some("rs") => {
            signals.languages.insert("rust".to_string());
        }
        Some("ts") | Some("tsx") | Some("js") | Some("jsx") | Some("mjs") | Some("cjs") => {
            signals.languages.insert("typescript".to_string());
            signals.node_project = true;
        }
        Some("py") => {
            signals.languages.insert("python".to_string());
            signals.python_project = true;
        }
        Some("go") => {
            signals.languages.insert("go".to_string());
        }
        Some("swift") => {
            signals.languages.insert("swift".to_string());
        }
        Some("rb") => {
            signals.languages.insert("ruby".to_string());
            signals.ruby_project = true;
        }
        _ => {}
    }
}

fn inspect_cargo_manifest(path: &Path, signals: &mut FingerprintSignals) {
    signals.languages.insert("rust".to_string());
    signals.package_managers.insert("cargo".to_string());
    signals.build_tools.insert("cargo".to_string());
    signals.test_runners.insert("cargo-test".to_string());
    let Some(text) = read_text_if_exists(path.to_path_buf()) else {
        return;
    };
    let Ok(parsed) = toml::from_str::<toml::Value>(&text) else {
        return;
    };
    let deps = collect_toml_keys(
        &parsed,
        &[
            &["dependencies"],
            &["dev-dependencies"],
            &["build-dependencies"],
            &["workspace", "dependencies"],
        ],
    );
    if deps.contains("axum") {
        signals.frameworks.insert("axum".to_string());
    }
}

fn inspect_package_json(path: &Path, signals: &mut FingerprintSignals) {
    let Some(parsed) = read_json_object(path.to_path_buf()) else {
        return;
    };
    signals.node_project = true;

    let deps = collect_json_dependency_names(&parsed);
    if deps.contains("next") {
        signals.has_next_dep = true;
        signals.build_tools.insert("next".to_string());
    }
    if deps.contains("react") {
        signals.frameworks.insert("react".to_string());
        signals.languages.insert("typescript".to_string());
    }
    if deps.contains("typescript") {
        signals.languages.insert("typescript".to_string());
    }
    if deps.contains("vite") {
        signals.has_vite_dep = true;
        signals.build_tools.insert("vite".to_string());
    }
    if deps.contains("vitest") {
        signals.test_runners.insert("vitest".to_string());
        signals.has_tests = true;
    }
    if deps.contains("jest") {
        signals.test_runners.insert("jest".to_string());
        signals.has_tests = true;
    }
    if deps.contains("mocha") {
        signals.test_runners.insert("mocha".to_string());
        signals.has_tests = true;
    }

    if let Some(package_manager) = parsed
        .get("packageManager")
        .and_then(|value| value.as_str())
    {
        if package_manager.starts_with("pnpm@") {
            signals.package_managers.insert("pnpm".to_string());
        } else if package_manager.starts_with("yarn@") {
            signals.package_managers.insert("yarn".to_string());
        } else if package_manager.starts_with("npm@") {
            signals.package_managers.insert("npm".to_string());
        }
    }

    if let Some(scripts) = parsed.get("scripts").and_then(|value| value.as_object()) {
        for command in scripts.values().filter_map(serde_json::Value::as_str) {
            record_command_signal(command, signals);
        }
    }
}

fn inspect_pyproject(path: &Path, signals: &mut FingerprintSignals) {
    let Some(text) = read_text_if_exists(path.to_path_buf()) else {
        return;
    };
    signals.languages.insert("python".to_string());
    signals.python_project = true;
    inspect_python_text(Some(&text), signals);

    let Ok(parsed) = toml::from_str::<toml::Value>(&text) else {
        signals.python_needs_pip = true;
        return;
    };
    let has_poetry = table_path_exists(&parsed, &["tool", "poetry"]);
    let has_uv = table_path_exists(&parsed, &["tool", "uv"]);
    if has_poetry {
        signals.package_managers.insert("poetry".to_string());
        signals.build_tools.insert("poetry".to_string());
    }
    if has_uv {
        signals.package_managers.insert("uv".to_string());
        signals.build_tools.insert("uv".to_string());
    }
    if !has_poetry && !has_uv {
        signals.python_needs_pip = true;
    }
}

fn inspect_python_requirements(path: &Path, signals: &mut FingerprintSignals) {
    signals.languages.insert("python".to_string());
    signals.python_project = true;
    signals.python_needs_pip = true;
    signals.build_tools.insert("pip".to_string());
    inspect_python_text(read_text_if_exists(path.to_path_buf()).as_deref(), signals);
}

fn inspect_python_text(text: Option<&str>, signals: &mut FingerprintSignals) {
    let Some(text) = text else {
        return;
    };
    let lower = text.to_ascii_lowercase();
    if lower.contains("fastapi") {
        signals.frameworks.insert("fastapi".to_string());
    }
    if lower.contains("django") {
        signals.frameworks.insert("django".to_string());
    }
    if lower.contains("pytest") || lower.contains("[tool.pytest") {
        signals.has_pytest_signal = true;
        signals.test_runners.insert("pytest".to_string());
        signals.has_tests = true;
    }
    if lower.contains("unittest") {
        signals.has_unittest_signal = true;
        signals.test_runners.insert("unittest".to_string());
        signals.has_tests = true;
    }
}

fn inspect_gemfile(path: &Path, signals: &mut FingerprintSignals) {
    signals.languages.insert("ruby".to_string());
    signals.ruby_project = true;
    signals.package_managers.insert("bundler".to_string());
    signals.build_tools.insert("bundler".to_string());
    let Some(text) = read_text_if_exists(path.to_path_buf()) else {
        return;
    };
    if text.contains("gem \"rails\"") || text.contains("gem 'rails'") {
        signals.frameworks.insert("rails".to_string());
    }
    if text.contains("gem \"rspec\"") || text.contains("gem 'rspec'") {
        signals.test_runners.insert("rspec".to_string());
    }
    if text.contains("gem \"minitest\"") || text.contains("gem 'minitest'") {
        signals.test_runners.insert("minitest".to_string());
    }
}

fn record_command_signal(command: &str, signals: &mut FingerprintSignals) {
    let normalized = command.to_ascii_lowercase();
    if normalized.contains("next build") || normalized.contains("next dev") {
        signals.build_tools.insert("next".to_string());
    }
    if normalized.contains("vite build")
        || normalized.contains("vite dev")
        || normalized.contains("npx vite")
    {
        signals.has_vite_dep = true;
        signals.build_tools.insert("vite".to_string());
    }
    if normalized.contains("vitest") {
        signals.test_runners.insert("vitest".to_string());
        signals.has_tests = true;
    }
    if normalized.contains("jest") {
        signals.test_runners.insert("jest".to_string());
        signals.has_tests = true;
    }
    if normalized.contains("mocha") {
        signals.test_runners.insert("mocha".to_string());
        signals.has_tests = true;
    }
    if normalized.contains("cargo nextest") {
        signals.test_runners.insert("nextest".to_string());
        signals.has_tests = true;
    }
    if normalized.contains("cargo test") {
        signals.test_runners.insert("cargo-test".to_string());
        signals.has_tests = true;
    }
    if normalized.contains("pytest") {
        signals.test_runners.insert("pytest".to_string());
        signals.has_tests = true;
    }
    if normalized.contains("python -m unittest") || normalized.contains("unittest") {
        signals.test_runners.insert("unittest".to_string());
        signals.has_tests = true;
    }
    if normalized.contains("go test") {
        signals.test_runners.insert("go-test".to_string());
        signals.has_tests = true;
    }
    if normalized.contains("swift test") {
        signals.test_runners.insert("xctest".to_string());
        signals.has_tests = true;
    }
    if normalized.contains("swift build") {
        signals.build_tools.insert("spm".to_string());
    }
}

fn collect_json_dependency_names(
    parsed: &serde_json::Map<String, serde_json::Value>,
) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for key in [
        "dependencies",
        "devDependencies",
        "peerDependencies",
        "optionalDependencies",
    ] {
        let Some(entries) = parsed.get(key).and_then(|value| value.as_object()) else {
            continue;
        };
        names.extend(entries.keys().cloned());
    }
    names
}

fn collect_toml_keys(parsed: &toml::Value, paths: &[&[&str]]) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for path in paths {
        let Some(table) = lookup_toml_path(parsed, path).and_then(toml::Value::as_table) else {
            continue;
        };
        names.extend(table.keys().cloned());
    }
    names
}

fn table_path_exists(parsed: &toml::Value, path: &[&str]) -> bool {
    lookup_toml_path(parsed, path).is_some()
}

fn lookup_toml_path<'a>(value: &'a toml::Value, path: &[&str]) -> Option<&'a toml::Value> {
    let mut current = value;
    for segment in path {
        current = current.get(*segment)?;
    }
    Some(current)
}

fn ordered_values(values: &BTreeSet<String>, order: &[&str]) -> Vec<String> {
    let mut ordered = Vec::new();
    for wanted in order {
        if values.contains(*wanted) {
            ordered.push((*wanted).to_string());
        }
    }
    for value in values {
        if !order.iter().any(|candidate| candidate == &value.as_str()) {
            ordered.push(value.clone());
        }
    }
    ordered
}

fn first_ordered_value(values: &[String]) -> Option<String> {
    values.first().cloned()
}

fn path_error(error: std::io::Error) -> VmError {
    VmError::Thrown(VmValue::String(std::sync::Arc::from(format!(
        "project.scan: failed to resolve path: {error}"
    ))))
}

fn path_missing_error(path: &Path) -> VmError {
    VmError::Thrown(VmValue::String(std::sync::Arc::from(format!(
        "project.scan: path does not exist: {}",
        path.display()
    ))))
}

fn scan_project_tree(
    base: &Path,
    options: &ProjectScanOptions,
) -> Result<BTreeMap<String, ProjectEvidence>, VmError> {
    let builder = build_project_walk_builder(base, options);

    let mut results = BTreeMap::new();
    results.insert(".".to_string(), scan_exact_directory(base, options));

    for entry in builder.build() {
        let Ok(entry) = entry else {
            continue;
        };
        if entry.depth() == 0 || !entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false) {
            continue;
        }
        let dir = entry.path();
        if !is_project_root_candidate(dir) {
            continue;
        }
        let rel = relative_posix(base, dir);
        results
            .entry(rel)
            .or_insert_with(|| scan_exact_directory(dir, options));
    }

    Ok(results)
}

fn walk_project_tree(
    base: &Path,
    options: &ProjectScanOptions,
) -> Result<Vec<ProjectTreeEntry>, VmError> {
    let builder = build_project_walk_builder(base, options);
    let metadata_root = resolve_source_relative_path(".")
        .canonicalize()
        .unwrap_or_else(|_| resolve_source_relative_path("."));
    let mut entries = Vec::new();
    entries.push(ProjectTreeEntry {
        relative_path: ".".to_string(),
        metadata_path: relative_posix(&metadata_root, base),
        structure_hash: compute_directory_structure_hash(base, base, options),
        content_hash: compute_directory_content_hash(base, base, options),
    });

    for entry in builder.build() {
        let Ok(entry) = entry else {
            continue;
        };
        if entry.depth() == 0 || !entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false) {
            continue;
        }
        let dir = entry.path();
        entries.push(ProjectTreeEntry {
            relative_path: relative_posix(base, dir),
            metadata_path: relative_posix(&metadata_root, dir),
            structure_hash: compute_directory_structure_hash(base, dir, options),
            content_hash: compute_directory_content_hash(base, dir, options),
        });
    }

    Ok(entries)
}

fn build_project_walk_builder(base: &Path, options: &ProjectScanOptions) -> WalkBuilder {
    let gitignore = build_gitignore(base, options.respect_gitignore);
    let mut builder = WalkBuilder::new(base);
    builder
        .hidden(!options.include_hidden)
        .follow_links(false)
        .git_ignore(false)
        .git_global(false)
        .git_exclude(false)
        .parents(false)
        .ignore(false)
        .max_depth(options.depth)
        .sort_by_file_name(|left, right| left.cmp(right));

    let include_vendor = options.include_vendor;
    builder.filter_entry(move |entry| {
        if entry.depth() == 0 {
            return true;
        }
        let Some(file_type) = entry.file_type() else {
            return true;
        };
        if gitignore
            .matched_path_or_any_parents(entry.path(), file_type.is_dir())
            .is_ignore()
        {
            return false;
        }
        if !file_type.is_dir() {
            return true;
        }
        if include_vendor {
            return true;
        }
        let name = entry.file_name().to_string_lossy();
        !STANDARD_VENDOR_DIRS.contains(&name.as_ref())
    });

    builder
}

fn build_gitignore(base: &Path, enabled: bool) -> Gitignore {
    let mut builder = GitignoreBuilder::new(base);
    if enabled {
        let _ = builder.add(base.join(".gitignore"));
    }
    builder.build().unwrap_or_else(|_| Gitignore::empty())
}

fn compute_directory_structure_hash(
    base: &Path,
    dir: &Path,
    options: &ProjectScanOptions,
) -> String {
    let gitignore = build_gitignore(base, options.respect_gitignore);
    let mut entries = Vec::new();
    for child in list_immediate_entries(dir) {
        if !should_include_tree_entry(base, &child, &gitignore, options) {
            continue;
        }
        let name = child.file_name().to_string_lossy().into_owned();
        let Ok(file_type) = child.file_type() else {
            continue;
        };
        entries.push(format!(
            "{}:{}",
            name,
            if file_type.is_dir() { "dir" } else { "file" }
        ));
    }
    stable_sha256(entries)
}

fn compute_directory_content_hash(base: &Path, dir: &Path, options: &ProjectScanOptions) -> String {
    let gitignore = build_gitignore(base, options.respect_gitignore);
    let mut digest = Sha256::new();
    for child in list_immediate_entries(dir) {
        if !should_include_tree_entry(base, &child, &gitignore, options) {
            continue;
        }
        let name = child.file_name().to_string_lossy().into_owned();
        let Ok(file_type) = child.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            continue;
        }
        digest.update(name.as_bytes());
        digest.update([0]);
        if let Ok(bytes) = std::fs::read(child.path()) {
            digest.update(bytes);
        }
        digest.update([0xff]);
    }
    hex_digest(digest.finalize())
}

fn list_immediate_entries(dir: &Path) -> Vec<std::fs::DirEntry> {
    let Ok(read_dir) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut entries = read_dir.flatten().collect::<Vec<_>>();
    entries.sort_by_key(|entry| entry.file_name());
    entries
}

fn should_include_tree_entry(
    base: &Path,
    child: &std::fs::DirEntry,
    gitignore: &Gitignore,
    options: &ProjectScanOptions,
) -> bool {
    let Ok(file_type) = child.file_type() else {
        return false;
    };
    let name = child.file_name().to_string_lossy().into_owned();
    if !options.include_hidden && name.starts_with('.') {
        return false;
    }
    if gitignore
        .matched_path_or_any_parents(
            child
                .path()
                .strip_prefix(base)
                .unwrap_or(child.path().as_path()),
            file_type.is_dir(),
        )
        .is_ignore()
    {
        return false;
    }
    if file_type.is_dir()
        && !options.include_vendor
        && STANDARD_VENDOR_DIRS.contains(&name.as_str())
    {
        return false;
    }
    true
}

fn stable_sha256(mut entries: Vec<String>) -> String {
    entries.sort();
    let mut digest = Sha256::new();
    for entry in entries {
        digest.update(entry.as_bytes());
        digest.update([0xff]);
    }
    hex_digest(digest.finalize())
}

fn hex_digest(bytes: impl AsRef<[u8]>) -> String {
    bytes
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn scan_exact_directory(dir: &Path, options: &ProjectScanOptions) -> ProjectEvidence {
    let mut evidence = ProjectEvidence {
        path: dir.to_path_buf(),
        vcs: detect_vcs(dir),
        ..ProjectEvidence::default()
    };
    let mut build_commands = Vec::new();

    for entry in project_catalog() {
        let found_anchors = collect_present(dir, entry.anchors);
        let found_lockfiles = collect_present(dir, entry.lockfiles);
        if found_anchors.is_empty() && found_lockfiles.is_empty() {
            continue;
        }

        let has_source = entry_has_source(dir, entry, options);
        let score = entry_confidence(
            entry,
            !found_anchors.is_empty(),
            !found_lockfiles.is_empty(),
            has_source,
        );
        if score <= 0.0 {
            continue;
        }

        evidence.anchors.extend(
            found_anchors
                .into_iter()
                .map(|value| maybe_dir_suffix(dir, &value)),
        );
        evidence.lockfiles.extend(found_lockfiles);

        for language in entry.languages {
            record_score(&mut evidence.language_scores, language, score);
        }
        for framework in entry.frameworks {
            record_score(&mut evidence.framework_scores, framework, score);
        }
        if score >= 0.5 {
            evidence
                .build_systems
                .extend(entry.build_systems.iter().map(|value| value.to_string()));
            push_unique_option(&mut build_commands, entry.default_build_cmd);
            push_unique_option(&mut build_commands, entry.default_test_cmd);
        }
    }

    if options.tiers.contains(&ScanTier::Config) {
        evidence.package_name = detect_package_name(dir);
        apply_config_tier(dir, &mut evidence, &mut build_commands);
    }

    evidence.build_commands = build_commands;
    evidence
}

fn apply_config_tier(dir: &Path, evidence: &mut ProjectEvidence, build_commands: &mut Vec<String>) {
    if let Some(package_json) = read_json_object(dir.join("package.json")) {
        if let Some(scripts) = package_json
            .get("scripts")
            .and_then(|value| value.as_object())
        {
            for (name, command) in scripts {
                let Some(command) = command.as_str() else {
                    continue;
                };
                evidence
                    .declared_scripts
                    .insert(name.clone(), command.to_string());
            }
            for key in ["build", "test", "lint", "dev", "start"] {
                if scripts.contains_key(key) {
                    let command = if key == "test" {
                        "npm test".to_string()
                    } else {
                        format!("npm run {key}")
                    };
                    push_unique(build_commands, command);
                }
            }
        }
    }

    if let Some(dockerfile) = read_text_if_exists(dir.join("Dockerfile")) {
        evidence.dockerfile_commands = parse_dockerfile_commands(&dockerfile);
    }

    if let Some(makefile) = read_first_existing_text(dir, &["GNUmakefile", "Makefile", "makefile"])
    {
        evidence.makefile_targets = parse_makefile_targets(&makefile);
        for key in ["build", "test"] {
            if evidence.makefile_targets.iter().any(|target| target == key) {
                push_unique(build_commands, format!("make {key}"));
            }
        }
    }

    if let Some(readme) =
        read_first_existing_text(dir, &["README.md", "README.MD", "README", "Readme.md"])
    {
        evidence.readme_code_fences = parse_readme_code_fences(&readme);
    }
}

fn detect_package_name(dir: &Path) -> Option<String> {
    package_name_from_pyproject(dir)
        .or_else(|| package_name_from_package_json(dir))
        .or_else(|| package_name_from_go_mod(dir))
        .or_else(|| package_name_from_cargo_toml(dir))
}

fn package_name_from_pyproject(dir: &Path) -> Option<String> {
    let text = read_text_if_exists(dir.join("pyproject.toml"))?;
    let parsed = toml::from_str::<toml::Value>(&text).ok()?;
    parsed
        .get("project")
        .and_then(|value| value.get("name"))
        .and_then(toml::Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            parsed
                .get("tool")
                .and_then(|value| value.get("poetry"))
                .and_then(|value| value.get("name"))
                .and_then(toml::Value::as_str)
                .map(str::to_string)
        })
}

fn package_name_from_package_json(dir: &Path) -> Option<String> {
    let parsed = read_json_object(dir.join("package.json"))?;
    parsed
        .get("name")
        .and_then(|value| value.as_str())
        .map(str::to_string)
}

fn package_name_from_go_mod(dir: &Path) -> Option<String> {
    let text = read_text_if_exists(dir.join("go.mod"))?;
    text.lines().find_map(|line| {
        let trimmed = line.trim();
        let module_path = trimmed.strip_prefix("module ")?;
        module_path.rsplit('/').next().map(str::to_string)
    })
}

fn package_name_from_cargo_toml(dir: &Path) -> Option<String> {
    let text = read_text_if_exists(dir.join("Cargo.toml"))?;
    let parsed = toml::from_str::<toml::Value>(&text).ok()?;
    parsed
        .get("package")
        .and_then(|value| value.get("name"))
        .and_then(toml::Value::as_str)
        .map(str::to_string)
}

fn read_first_existing_text(dir: &Path, names: &[&str]) -> Option<String> {
    names
        .iter()
        .find_map(|name| read_text_if_exists(dir.join(name)))
}

fn read_text_if_exists(path: PathBuf) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

fn read_json_object(path: PathBuf) -> Option<serde_json::Map<String, serde_json::Value>> {
    let text = std::fs::read_to_string(path).ok()?;
    let parsed = serde_json::from_str::<serde_json::Value>(&text).ok()?;
    parsed.as_object().cloned()
}

fn parse_readme_code_fences(readme: &str) -> Vec<String> {
    let mut fences = Vec::new();
    for line in readme.lines() {
        let trimmed = line.trim();
        if let Some(lang) = trimmed.strip_prefix("```") {
            let lang = lang.split_whitespace().next().unwrap_or_default().trim();
            if !lang.is_empty() {
                push_unique(&mut fences, lang.to_string());
            }
        }
    }
    fences
}

fn parse_dockerfile_commands(dockerfile: &str) -> Vec<String> {
    let mut commands = Vec::new();
    let mut pending = String::new();

    for raw_line in dockerfile.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        if pending.is_empty() {
            pending = line.to_string();
        } else {
            pending.push(' ');
            pending.push_str(line);
        }

        if pending.ends_with('\\') {
            pending.pop();
            pending = pending.trim_end().to_string();
            continue;
        }

        let upper = pending.to_ascii_uppercase();
        for keyword in ["RUN ", "CMD ", "ENTRYPOINT "] {
            if upper.starts_with(keyword) {
                push_unique(&mut commands, pending[keyword.len()..].trim().to_string());
                break;
            }
        }
        pending.clear();
    }

    commands
}

fn parse_makefile_targets(makefile: &str) -> Vec<String> {
    let mut targets = Vec::new();
    for line in makefile.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty()
            || trimmed.starts_with('#')
            || trimmed.starts_with('.')
            || trimmed.contains(":=")
            || trimmed.contains("?=")
            || trimmed.contains("+=")
            || trimmed.contains('=')
        {
            continue;
        }
        let Some((target, _rest)) = trimmed.split_once(':') else {
            continue;
        };
        if target.contains('%') || target.contains(' ') || target.is_empty() {
            continue;
        }
        if target
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.'))
        {
            push_unique(&mut targets, target.to_string());
        }
    }
    targets
}

fn maybe_dir_suffix(dir: &Path, name: &str) -> String {
    let path = dir.join(name);
    if path.is_dir() {
        format!("{name}/")
    } else {
        name.to_string()
    }
}

fn collect_present(dir: &Path, names: &[&str]) -> Vec<String> {
    names
        .iter()
        .filter(|name| dir.join(name).exists())
        .map(|name| (*name).to_string())
        .collect()
}

fn entry_has_source(dir: &Path, entry: &ProjectCatalogEntry, options: &ProjectScanOptions) -> bool {
    if entry.source_globs.is_empty() {
        return false;
    }
    scan_sources(dir, dir, entry, options, 0)
}

fn scan_sources(
    root: &Path,
    dir: &Path,
    entry: &ProjectCatalogEntry,
    options: &ProjectScanOptions,
    depth: usize,
) -> bool {
    if let Some(max_depth) = options.depth {
        if depth > max_depth.saturating_add(2) {
            return false;
        }
    }
    let Ok(read_dir) = std::fs::read_dir(dir) else {
        return false;
    };
    for child in read_dir.flatten() {
        let Ok(file_type) = child.file_type() else {
            continue;
        };
        let name = child.file_name().to_string_lossy().into_owned();
        if !options.include_hidden && name.starts_with('.') {
            continue;
        }
        if file_type.is_dir() {
            if !options.include_vendor && STANDARD_VENDOR_DIRS.contains(&name.as_str()) {
                continue;
            }
            if child.path() != root && is_project_root_candidate(&child.path()) {
                continue;
            }
            if scan_sources(root, &child.path(), entry, options, depth + 1) {
                return true;
            }
            continue;
        }
        let rel = relative_posix(root, &child.path());
        if entry
            .source_globs
            .iter()
            .any(|pattern| simple_glob_match(pattern, &rel))
        {
            return true;
        }
    }
    false
}

fn entry_confidence(
    entry: &ProjectCatalogEntry,
    has_anchor: bool,
    has_lockfile: bool,
    has_source: bool,
) -> f64 {
    let mut score: f64 = 0.0;
    if has_anchor {
        score = score.max(entry.anchor_score);
    }
    if has_source {
        score = score.max(0.5);
    }
    if has_lockfile {
        score = if has_source {
            1.0
        } else {
            (score + 0.45).min(1.0)
        };
    }
    score
}

fn detect_vcs(dir: &Path) -> Option<String> {
    let mut cursor = Some(dir);
    while let Some(path) = cursor {
        if path.join(".git").exists() {
            return Some("git".to_string());
        }
        if path.join(".hg").exists() {
            return Some("hg".to_string());
        }
        cursor = path.parent();
    }
    None
}

fn is_project_root_candidate(dir: &Path) -> bool {
    if dir.join(".git").exists() || dir.join("harn.toml").is_file() {
        return true;
    }
    project_catalog().iter().any(|entry| {
        entry
            .anchors
            .iter()
            .chain(entry.lockfiles.iter())
            .any(|name| dir.join(name).exists())
    })
}

fn relative_posix(base: &Path, path: &Path) -> String {
    match path.strip_prefix(base) {
        Ok(rel) if rel.as_os_str().is_empty() => ".".to_string(),
        Ok(rel) => rel.to_string_lossy().replace('\\', "/"),
        Err(_) => path.to_string_lossy().replace('\\', "/"),
    }
}

fn simple_glob_match(pattern: &str, candidate: &str) -> bool {
    if let Some(suffix) = pattern.strip_prefix("**/") {
        return candidate == suffix || candidate.ends_with(&format!("/{suffix}"));
    }
    if let Some(suffix) = pattern.strip_prefix("*.") {
        return candidate
            .rsplit('/')
            .next()
            .is_some_and(|name| name.ends_with(&format!(".{suffix}")));
    }
    if pattern.contains("/**/*.") {
        let Some((prefix, ext)) = pattern.split_once("/**/*.") else {
            return false;
        };
        return candidate.starts_with(prefix)
            && candidate
                .rsplit('/')
                .next()
                .is_some_and(|name| name.ends_with(&format!(".{ext}")));
    }
    candidate == pattern
}

fn record_score(scores: &mut BTreeMap<String, f64>, label: &str, score: f64) {
    scores
        .entry(label.to_string())
        .and_modify(|current| *current = current.max(score))
        .or_insert(score);
}

fn confidence_value(evidence: &ProjectEvidence) -> VmValue {
    let mut confidence = BTreeMap::new();
    for (label, score) in evidence
        .language_scores
        .iter()
        .chain(evidence.framework_scores.iter())
    {
        confidence.insert(label.clone(), VmValue::Float(*score));
    }
    VmValue::Dict(std::sync::Arc::new(confidence))
}

fn sorted_confident_labels(scores: &BTreeMap<String, f64>) -> Vec<String> {
    let mut items = scores
        .iter()
        .filter(|(_label, score)| **score >= 0.5)
        .map(|(label, score)| (label.clone(), *score))
        .collect::<Vec<_>>();
    items.sort_by(|left, right| {
        right
            .1
            .partial_cmp(&left.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.0.cmp(&right.0))
    });
    items.into_iter().map(|(label, _score)| label).collect()
}

fn push_unique_option(values: &mut Vec<String>, value: Option<&str>) {
    if let Some(value) = value {
        push_unique(values, value.to_string());
    }
}

fn push_unique(values: &mut Vec<String>, value: String) {
    if !values.contains(&value) {
        values.push(value);
    }
}

fn catalog_entry_value(entry: &ProjectCatalogEntry) -> VmValue {
    let mut value = BTreeMap::new();
    value.insert(
        "id".to_string(),
        VmValue::String(std::sync::Arc::from(entry.id.to_string())),
    );
    value.insert(
        "languages".to_string(),
        VmValue::List(std::sync::Arc::new(
            entry
                .languages
                .iter()
                .map(|item| VmValue::String(std::sync::Arc::from((*item).to_string())))
                .collect(),
        )),
    );
    value.insert(
        "frameworks".to_string(),
        VmValue::List(std::sync::Arc::new(
            entry
                .frameworks
                .iter()
                .map(|item| VmValue::String(std::sync::Arc::from((*item).to_string())))
                .collect(),
        )),
    );
    value.insert(
        "build_systems".to_string(),
        VmValue::List(std::sync::Arc::new(
            entry
                .build_systems
                .iter()
                .map(|item| VmValue::String(std::sync::Arc::from((*item).to_string())))
                .collect(),
        )),
    );
    value.insert(
        "anchors".to_string(),
        VmValue::List(std::sync::Arc::new(
            entry
                .anchors
                .iter()
                .map(|item| VmValue::String(std::sync::Arc::from((*item).to_string())))
                .collect(),
        )),
    );
    value.insert(
        "lockfiles".to_string(),
        VmValue::List(std::sync::Arc::new(
            entry
                .lockfiles
                .iter()
                .map(|item| VmValue::String(std::sync::Arc::from((*item).to_string())))
                .collect(),
        )),
    );
    value.insert(
        "source_globs".to_string(),
        VmValue::List(std::sync::Arc::new(
            entry
                .source_globs
                .iter()
                .map(|item| VmValue::String(std::sync::Arc::from((*item).to_string())))
                .collect(),
        )),
    );
    value.insert(
        "default_build_cmd".to_string(),
        entry
            .default_build_cmd
            .map(|value| VmValue::String(std::sync::Arc::from(value.to_string())))
            .unwrap_or(VmValue::Nil),
    );
    value.insert(
        "default_test_cmd".to_string(),
        entry
            .default_test_cmd
            .map(|value| VmValue::String(std::sync::Arc::from(value.to_string())))
            .unwrap_or(VmValue::Nil),
    );
    VmValue::Dict(std::sync::Arc::new(value))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(label: &str) -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix(&format!("harn-project-{label}-"))
            .tempdir()
            .expect("tempdir")
    }

    #[test]
    fn scan_detects_rust_workspace_root_without_nested_source_walk() {
        let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("crate dir")
            .parent()
            .expect("workspace root")
            .to_path_buf();
        let evidence = scan_exact_directory(&repo_root, &ProjectScanOptions::default());
        assert!(sorted_confident_labels(&evidence.language_scores).contains(&"rust".to_string()));
        assert!(
            evidence
                .language_scores
                .get("rust")
                .copied()
                .unwrap_or_default()
                >= 0.95
        );
    }

    #[test]
    fn package_name_detection_matches_manifest_priority() {
        let dir = temp_dir("package-name");
        std::fs::write(
            dir.path().join("pyproject.toml"),
            "[project]\nname = \"python-name\"\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("package.json"), "{\"name\":\"node-name\"}").unwrap();
        std::fs::write(
            dir.path().join("go.mod"),
            "module github.com/acme/go-name\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"cargo-name\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();

        assert_eq!(
            detect_package_name(dir.path()).as_deref(),
            Some("python-name")
        );
    }

    fn context_options_without_env() -> ContextProfileOptions {
        ContextProfileOptions {
            include_env_credentials: false,
            ..ContextProfileOptions::default()
        }
    }

    fn context_profile_ids(resolution: &ContextProfileResolution) -> Vec<String> {
        resolution
            .profiles
            .iter()
            .map(|profile| profile.id.clone())
            .collect()
    }

    fn flatten_profile_field(
        resolution: &ContextProfileResolution,
        field: fn(&ContextProfileActivation) -> Vec<String>,
    ) -> Vec<String> {
        unique_flatten(resolution.profiles.iter().flat_map(field))
    }

    #[test]
    fn context_profile_resolves_github_remote_with_credentials() {
        let dir = temp_dir("context-github");
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        std::fs::write(
            dir.path().join(".git/config"),
            "[remote \"origin\"]\n\turl = https://github.com/burin-labs/harn.git\n",
        )
        .unwrap();

        let mut options = context_options_without_env();
        options.credentials = BTreeSet::from(["github".to_string()]);
        let resolution = resolve_context_profile(dir.path(), options);

        assert_eq!(
            context_profile_ids(&resolution),
            vec!["git".to_string(), "github".to_string()]
        );
        assert_eq!(
            flatten_profile_field(&resolution, |profile| profile.skills.clone()),
            vec!["git".to_string(), "github".to_string()]
        );
        assert_eq!(
            flatten_profile_field(&resolution, |profile| profile.mcp_presets.clone()),
            vec!["github".to_string()]
        );
        assert_eq!(
            resolution
                .signals
                .remote
                .as_ref()
                .and_then(|remote| remote.slug.as_ref()),
            Some(&"burin-labs/harn".to_string())
        );
    }

    #[test]
    fn context_profile_marks_github_preset_as_needing_credentials() {
        let dir = temp_dir("context-github-no-credentials");
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        std::fs::write(
            dir.path().join(".git/config"),
            "[remote \"origin\"]\n\turl = git@github.com:burin-labs/harn.git\n",
        )
        .unwrap();

        let resolution = resolve_context_profile(dir.path(), context_options_without_env());
        let github = resolution
            .profiles
            .iter()
            .find(|profile| profile.id == "github")
            .expect("github profile");
        assert!(github.mcp_presets.is_empty());
        assert_eq!(github.mcp_preset_candidates[0].status, "needs_credentials");
        assert_eq!(
            github.mcp_preset_candidates[0].missing_credentials,
            vec!["GITHUB_PERSONAL_ACCESS_TOKEN".to_string()]
        );
    }

    #[test]
    fn context_profile_does_not_treat_non_github_slug_as_github() {
        let dir = temp_dir("context-non-github-slug");
        let mut options = context_options_without_env();
        options.remote = Some(GitRemoteSignal {
            name: "origin".to_string(),
            host: "gitlab.com".to_string(),
            slug: Some("burin-labs/harn".to_string()),
            redacted_url: "https://gitlab.com/burin-labs/harn.git".to_string(),
        });

        let resolution = resolve_context_profile(dir.path(), options);

        assert_eq!(context_profile_ids(&resolution), vec!["git".to_string()]);
    }

    #[test]
    fn context_profile_resolves_rust_crate_without_extra_profiles() {
        let dir = temp_dir("context-rust");
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"demo\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("Cargo.lock"), "# lock\n").unwrap();

        let resolution = resolve_context_profile(dir.path(), context_options_without_env());

        assert_eq!(context_profile_ids(&resolution), vec!["rust".to_string()]);
        assert_eq!(
            flatten_profile_field(&resolution, |profile| profile.tool_groups.clone()),
            vec!["cargo".to_string()]
        );
        assert_eq!(resolution.profiles[0].prompt_fragment.id, "profile:rust");
    }

    #[test]
    fn context_profile_resolves_package_json_workspace() {
        let dir = temp_dir("context-node");
        std::fs::create_dir_all(dir.path().join("packages/web/src")).unwrap();
        std::fs::write(
            dir.path().join("package.json"),
            "{\n  \"private\": true,\n  \"workspaces\": [\"packages/*\"],\n  \"packageManager\": \"pnpm@9.0.0\"\n}\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("packages/web/src/app.ts"),
            "export const app = 1;\n",
        )
        .unwrap();

        let resolution = resolve_context_profile(dir.path(), context_options_without_env());

        assert_eq!(context_profile_ids(&resolution), vec!["node".to_string()]);
        assert_eq!(
            flatten_profile_field(&resolution, |profile| profile.tool_groups.clone()),
            vec!["node".to_string()]
        );
        assert!(resolution.profiles[0].mcp_presets.is_empty());
    }

    #[test]
    fn context_profile_bare_directory_has_no_active_profiles() {
        let dir = temp_dir("context-bare");
        let resolution = resolve_context_profile(dir.path(), context_options_without_env());

        assert!(resolution.profiles.is_empty());
        assert_eq!(resolution.activated_prompt_tokens, 0);
        assert!(resolution.always_on_prompt_tokens > 0);
    }

    #[test]
    fn context_profile_consumes_supplied_code_librarian_signals_without_scanning() {
        let dir = temp_dir("context-supplied");
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"local-rust\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        let mut options = context_options_without_env();
        options.signal_source = Some("code_librarian".to_string());
        options.credentials = BTreeSet::from(["github".to_string()]);
        options.fingerprint = Some(ProjectFingerprint {
            primary_language: "python".to_string(),
            languages: vec!["python".to_string()],
            frameworks: Vec::new(),
            package_manager: Some("uv".to_string()),
            package_managers: vec!["uv".to_string()],
            test_runner: Some("pytest".to_string()),
            build_tool: None,
            vcs: Some("git".to_string()),
            ci: Vec::new(),
            has_tests: false,
            has_ci: false,
            lockfile_paths: Vec::new(),
        });
        options.remote = remote_signal_from_value(&VmValue::String(std::sync::Arc::from(
            "https://github.com/burin-labs/harn.git",
        )));

        let resolution = resolve_context_profile(dir.path(), options);

        assert_eq!(resolution.signals.source, "code_librarian");
        assert_eq!(
            context_profile_ids(&resolution),
            vec![
                "git".to_string(),
                "github".to_string(),
                "python".to_string()
            ]
        );
        assert_eq!(
            flatten_profile_field(&resolution, |profile| profile.mcp_presets.clone()),
            vec!["github".to_string()]
        );
    }

    #[test]
    fn context_profile_scans_fingerprint_when_supplied_signals_only_include_remote() {
        let dir = temp_dir("context-remote-only");
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"demo\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();

        let mut signals = BTreeMap::new();
        signals.insert(
            "remote".to_string(),
            string_value("https://github.com/burin-labs/harn.git"),
        );
        let mut raw_options = BTreeMap::new();
        raw_options.insert("include_env_credentials".to_string(), VmValue::Bool(false));
        raw_options.insert(
            "signals".to_string(),
            VmValue::Dict(std::sync::Arc::new(signals)),
        );
        let options =
            parse_context_profile_options(Some(&VmValue::Dict(std::sync::Arc::new(raw_options))));

        assert!(options.fingerprint.is_none());
        let resolution = resolve_context_profile(dir.path(), options);

        assert_eq!(
            context_profile_ids(&resolution),
            vec!["git".to_string(), "github".to_string(), "rust".to_string()]
        );
    }

    #[test]
    fn context_profile_redacts_remote_userinfo_and_query() {
        assert_eq!(
            redact_remote_url(
                "https://user:secret@github.com/burin-labs/harn.git?token=secret#frag"
            ),
            "https://<redacted>@github.com/burin-labs/harn.git"
        );
        assert_eq!(
            github_slug_from_remote(
                "https://user:secret@github.com/burin-labs/harn.git?token=secret#frag"
            )
            .as_deref(),
            Some("burin-labs/harn")
        );
    }

    #[test]
    fn scan_tree_respects_gitignore_and_vendor_dirs_by_default() {
        let dir = temp_dir("tree-ignore");
        std::fs::create_dir_all(dir.path().join("frontend/src")).unwrap();
        std::fs::create_dir_all(dir.path().join("ignored/src")).unwrap();
        std::fs::create_dir_all(dir.path().join("node_modules/pkg")).unwrap();
        std::fs::write(dir.path().join(".gitignore"), "ignored/\n").unwrap();
        std::fs::write(
            dir.path().join("frontend/package.json"),
            "{\"name\":\"frontend\"}",
        )
        .unwrap();
        std::fs::write(dir.path().join("frontend/package-lock.json"), "{}").unwrap();
        std::fs::write(
            dir.path().join("frontend/src/app.ts"),
            "export const x = 1;\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("ignored/go.mod"), "module ignored\n").unwrap();
        std::fs::write(
            dir.path().join("node_modules/pkg/package.json"),
            "{\"name\":\"pkg\"}",
        )
        .unwrap();

        let tree = scan_project_tree(dir.path(), &ProjectScanOptions::default()).unwrap();
        assert!(tree.contains_key("."));
        assert!(tree.contains_key("frontend"));
        assert!(!tree.contains_key("ignored"));
        assert!(!tree.contains_key("node_modules"));
    }

    #[test]
    fn walk_tree_includes_all_directories_and_hashes_local_content_only() {
        let dir = temp_dir("walk");
        std::fs::create_dir_all(dir.path().join("src/auth")).unwrap();
        std::fs::create_dir_all(dir.path().join("src/api")).unwrap();
        std::fs::write(
            dir.path().join("src/auth/lib.rs"),
            "pub fn login() -> bool { true }\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("src/api/lib.rs"), "pub fn handle() {}\n").unwrap();

        let first = walk_project_tree(dir.path(), &ProjectScanOptions::default()).unwrap();
        let src = first
            .iter()
            .find(|entry| entry.relative_path == "src")
            .expect("src entry");
        let auth = first
            .iter()
            .find(|entry| entry.relative_path == "src/auth")
            .expect("auth entry");
        assert_eq!(
            first
                .iter()
                .map(|entry| entry.relative_path.as_str())
                .collect::<Vec<_>>(),
            vec![".", "src", "src/api", "src/auth"]
        );

        std::fs::write(
            dir.path().join("src/auth/lib.rs"),
            "pub fn login() -> bool { false }\n",
        )
        .unwrap();

        let second = walk_project_tree(dir.path(), &ProjectScanOptions::default()).unwrap();
        let src_after = second
            .iter()
            .find(|entry| entry.relative_path == "src")
            .expect("src entry");
        let auth_after = second
            .iter()
            .find(|entry| entry.relative_path == "src/auth")
            .expect("auth entry");

        assert_eq!(src.content_hash, src_after.content_hash);
        assert_ne!(auth.content_hash, auth_after.content_hash);
    }

    #[test]
    fn project_fingerprint_detects_polyglot_repo_shape() {
        let dir = temp_dir("fingerprint-polyglot");
        std::fs::create_dir_all(dir.path().join("backend")).unwrap();
        std::fs::create_dir_all(dir.path().join("portal/tests")).unwrap();
        std::fs::create_dir_all(dir.path().join(".github/workflows")).unwrap();
        std::fs::write(
            dir.path().join("backend/Cargo.toml"),
            "[package]\nname = \"backend\"\nversion = \"0.1.0\"\n[dependencies]\naxum = \"0.8\"\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("backend/Cargo.lock"), "# lock\n").unwrap();
        std::fs::write(
            dir.path().join("portal/package.json"),
            "{\n  \"name\": \"portal\",\n  \"packageManager\": \"pnpm@9.0.0\",\n  \"dependencies\": {\n    \"next\": \"15.0.0\",\n    \"react\": \"19.0.0\"\n  }\n}\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("portal/next.config.ts"),
            "export default {}\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("portal/pnpm-lock.yaml"),
            "lockfileVersion: '9.0'\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join(".github/workflows/ci.yml"),
            "name: ci\non: push\n",
        )
        .unwrap();

        let fingerprint = detect_project_fingerprint(dir.path());
        assert_eq!(fingerprint.primary_language, "mixed");
        assert_eq!(
            fingerprint.languages,
            vec!["rust".to_string(), "typescript".to_string()]
        );
        assert_eq!(
            fingerprint.frameworks,
            vec!["axum".to_string(), "next".to_string(), "react".to_string()]
        );
        assert_eq!(
            fingerprint.package_managers,
            vec!["cargo".to_string(), "pnpm".to_string()]
        );
        assert_eq!(fingerprint.package_manager.as_deref(), Some("cargo"));
        assert_eq!(fingerprint.test_runner.as_deref(), Some("cargo-test"));
        assert_eq!(fingerprint.build_tool.as_deref(), Some("cargo"));
        assert_eq!(fingerprint.vcs, None);
        assert_eq!(fingerprint.ci, vec!["github-actions".to_string()]);
        assert!(fingerprint.has_tests);
        assert!(fingerprint.has_ci);
        assert_eq!(
            fingerprint.lockfile_paths,
            vec![
                "backend/Cargo.lock".to_string(),
                "portal/pnpm-lock.yaml".to_string()
            ]
        );
    }

    #[test]
    fn project_fingerprint_detects_python_package_managers() {
        let dir = temp_dir("fingerprint-python");
        std::fs::write(
            dir.path().join("pyproject.toml"),
            "[project]\nname = \"api\"\ndependencies = [\"fastapi>=0.110\"]\n[tool.uv]\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("uv.lock"), "# lock\n").unwrap();
        std::fs::create_dir_all(dir.path().join("tests")).unwrap();

        let fingerprint = detect_project_fingerprint(dir.path());
        assert_eq!(fingerprint.primary_language, "python");
        assert_eq!(fingerprint.languages, vec!["python".to_string()]);
        assert!(fingerprint.frameworks.contains(&"fastapi".to_string()));
        assert_eq!(fingerprint.package_managers, vec!["uv".to_string()]);
        assert_eq!(fingerprint.package_manager.as_deref(), Some("uv"));
        assert_eq!(fingerprint.test_runner.as_deref(), Some("pytest"));
        assert_eq!(fingerprint.build_tool.as_deref(), Some("uv"));
        assert!(fingerprint.has_tests);
        assert!(!fingerprint.has_ci);
        assert_eq!(fingerprint.lockfile_paths, vec!["uv.lock".to_string()]);
    }

    #[test]
    fn project_fingerprint_detects_rust_nextest_profile() {
        let dir = temp_dir("fingerprint-rust-nextest");
        std::fs::create_dir_all(dir.path().join(".config")).unwrap();
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"runner\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join(".config/nextest.toml"),
            "[profile.default]\n",
        )
        .unwrap();

        let fingerprint = detect_project_fingerprint(dir.path());
        assert_eq!(fingerprint.primary_language, "rust");
        assert_eq!(fingerprint.package_manager.as_deref(), Some("cargo"));
        assert_eq!(fingerprint.test_runner.as_deref(), Some("nextest"));
        assert_eq!(fingerprint.build_tool.as_deref(), Some("cargo"));
        assert_eq!(fingerprint.vcs.as_deref(), Some("git"));
    }

    #[test]
    fn project_fingerprint_detects_swift_spm_profile() {
        let dir = temp_dir("fingerprint-swift");
        std::fs::create_dir_all(dir.path().join("Sources/App")).unwrap();
        std::fs::create_dir_all(dir.path().join("Tests/AppTests")).unwrap();
        std::fs::write(
            dir.path().join("Package.swift"),
            "import PackageDescription\nlet package = Package(name: \"App\")\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("Sources/App/main.swift"),
            "__io_print(\"hi\")\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("Tests/AppTests/AppTests.swift"),
            "import XCTest\n",
        )
        .unwrap();

        let fingerprint = detect_project_fingerprint(dir.path());
        assert_eq!(fingerprint.primary_language, "swift");
        assert_eq!(fingerprint.package_manager.as_deref(), Some("spm"));
        assert_eq!(fingerprint.test_runner.as_deref(), Some("xctest"));
        assert_eq!(fingerprint.build_tool.as_deref(), Some("spm"));
        assert!(fingerprint.has_tests);
    }

    #[test]
    fn project_fingerprint_detects_npm_workspace_profile() {
        let dir = temp_dir("fingerprint-npm");
        std::fs::create_dir_all(dir.path().join("packages/web/src")).unwrap();
        std::fs::create_dir_all(dir.path().join("packages/web/tests")).unwrap();
        std::fs::write(
            dir.path().join("package.json"),
            "{\n  \"name\": \"workspace\",\n  \"private\": true,\n  \"workspaces\": [\"packages/*\"],\n  \"packageManager\": \"npm@10.8.0\",\n  \"devDependencies\": {\n    \"vite\": \"5.0.0\",\n    \"vitest\": \"2.0.0\"\n  },\n  \"scripts\": {\n    \"build\": \"vite build\",\n    \"test\": \"vitest run\"\n  }\n}\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("package-lock.json"), "{}\n").unwrap();
        std::fs::write(
            dir.path().join("packages/web/src/app.ts"),
            "export const app = 1;\n",
        )
        .unwrap();

        let fingerprint = detect_project_fingerprint(dir.path());
        assert_eq!(fingerprint.primary_language, "typescript");
        assert_eq!(fingerprint.package_manager.as_deref(), Some("npm"));
        assert_eq!(fingerprint.test_runner.as_deref(), Some("vitest"));
        assert_eq!(fingerprint.build_tool.as_deref(), Some("vite"));
        assert!(fingerprint.has_tests);
    }

    #[test]
    fn project_fingerprint_detects_poetry_profile() {
        let dir = temp_dir("fingerprint-poetry");
        std::fs::create_dir_all(dir.path().join("tests")).unwrap();
        std::fs::write(
            dir.path().join("pyproject.toml"),
            "[tool.poetry]\nname = \"svc\"\nversion = \"0.1.0\"\n[tool.poetry.dependencies]\npython = \"^3.12\"\nfastapi = \"^0.110\"\n[tool.poetry.group.dev.dependencies]\npytest = \"^8.0\"\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("poetry.lock"), "# lock\n").unwrap();

        let fingerprint = detect_project_fingerprint(dir.path());
        assert_eq!(fingerprint.package_manager.as_deref(), Some("poetry"));
        assert_eq!(fingerprint.test_runner.as_deref(), Some("pytest"));
        assert_eq!(fingerprint.build_tool.as_deref(), Some("poetry"));
        assert!(fingerprint.frameworks.contains(&"fastapi".to_string()));
    }

    #[test]
    fn project_fingerprint_detects_go_module_profile() {
        let dir = temp_dir("fingerprint-go");
        std::fs::create_dir_all(dir.path().join("pkg")).unwrap();
        std::fs::write(
            dir.path().join("go.mod"),
            "module github.com/acme/service\n\ngo 1.23\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("go.sum"), "example v0.0.0 h1:abc\n").unwrap();
        std::fs::write(dir.path().join("pkg/service.go"), "package pkg\n").unwrap();

        let fingerprint = detect_project_fingerprint(dir.path());
        assert_eq!(fingerprint.primary_language, "go");
        assert_eq!(fingerprint.package_manager.as_deref(), Some("go-mod"));
        assert_eq!(fingerprint.test_runner.as_deref(), Some("go-test"));
        assert_eq!(fingerprint.build_tool.as_deref(), Some("go"));
    }

    #[test]
    fn project_fingerprint_handles_empty_directory() {
        let dir = temp_dir("fingerprint-empty");
        let fingerprint = detect_project_fingerprint(dir.path());
        assert_eq!(fingerprint.primary_language, "unknown");
        assert!(fingerprint.languages.is_empty());
        assert!(fingerprint.frameworks.is_empty());
        assert!(fingerprint.package_managers.is_empty());
        assert_eq!(fingerprint.package_manager, None);
        assert_eq!(fingerprint.test_runner, None);
        assert_eq!(fingerprint.build_tool, None);
        assert_eq!(fingerprint.vcs, None);
        assert!(fingerprint.ci.is_empty());
        assert!(!fingerprint.has_tests);
        assert!(!fingerprint.has_ci);
        assert!(fingerprint.lockfile_paths.is_empty());
    }
}
