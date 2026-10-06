use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use ignore::gitignore::{Gitignore, GitignoreBuilder};
use ignore::WalkBuilder;
use sha2::{Digest, Sha256};

use crate::runtime_limits::RuntimeLimits;
use crate::stdlib::process::resolve_source_relative_path;
use crate::stdlib::project_catalog::{project_catalog, ProjectCatalogEntry};
use crate::value::{VmDictExt, VmError, VmValue};

// One shared list owns "what counts as build output" for every Harn walk.
use super::*;
use crate::stdlib::fs::ignore_policy::{
    IgnorePolicy, BUILTIN_IGNORED_DIRS, PROJECT_IGNORE_FILENAMES,
};
mod fingerprint;
pub(super) use fingerprint::detect_project_fingerprint;
use fingerprint::*;

const PROJECT_FINGERPRINT_MAX_DEPTH: usize = RuntimeLimits::DEFAULT.max_project_fingerprint_depth;
const PROJECT_LANGUAGE_ORDER: &[&str] = &[
    "rust",
    "typescript",
    "python",
    "go",
    "swift",
    "ruby",
    "scala",
    "kotlin",
    "java",
    "elixir",
    "zig",
    "php",
    "csharp",
    "c",
    "cpp",
];
const PROJECT_FRAMEWORK_ORDER: &[&str] = &[
    "axum", "next", "react", "laravel", "django", "fastapi", "rails",
];
const PROJECT_PACKAGE_MANAGER_ORDER: &[&str] = &[
    "cargo", "spm", "composer", "pnpm", "npm", "yarn", "uv", "poetry", "pip", "go-mod", "bundler",
];
const PROJECT_TEST_RUNNER_ORDER: &[&str] = &[
    "nextest",
    "cargo-test",
    "pest",
    "phpunit",
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
    "cargo", "spm", "composer", "next", "vite", "uv", "poetry", "pnpm", "npm", "yarn", "go",
    "bundler", "pip",
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
    ("composer.lock", Some("composer")),
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

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct ProjectFingerprint {
    pub(super) primary_language: String,
    pub(super) languages: Vec<String>,
    pub(super) frameworks: Vec<String>,
    pub(super) package_manager: Option<String>,
    pub(super) package_managers: Vec<String>,
    pub(super) test_runner: Option<String>,
    pub(super) build_tool: Option<String>,
    pub(super) vcs: Option<String>,
    pub(super) ci: Vec<String>,
    pub(super) has_tests: bool,
    pub(super) has_ci: bool,
    pub(super) lockfile_paths: Vec<String>,
}

impl ProjectFingerprint {
    pub(super) fn into_vm_value(self) -> VmValue {
        let mut value = BTreeMap::new();
        value.put_str("primary_language", self.primary_language);
        value.insert(
            "languages".to_string(),
            VmValue::List(std::sync::Arc::new(
                self.languages
                    .into_iter()
                    .map(|item| VmValue::String(arcstr::ArcStr::from(item)))
                    .collect(),
            )),
        );
        value.insert(
            "frameworks".to_string(),
            VmValue::List(std::sync::Arc::new(
                self.frameworks
                    .into_iter()
                    .map(|item| VmValue::String(arcstr::ArcStr::from(item)))
                    .collect(),
            )),
        );
        value.insert(
            "package_manager".to_string(),
            self.package_manager
                .map(|value| VmValue::String(arcstr::ArcStr::from(value)))
                .unwrap_or(VmValue::Nil),
        );
        value.insert(
            "package_managers".to_string(),
            VmValue::List(std::sync::Arc::new(
                self.package_managers
                    .into_iter()
                    .map(|item| VmValue::String(arcstr::ArcStr::from(item)))
                    .collect(),
            )),
        );
        value.insert(
            "test_runner".to_string(),
            self.test_runner
                .map(|value| VmValue::String(arcstr::ArcStr::from(value)))
                .unwrap_or(VmValue::Nil),
        );
        value.insert(
            "build_tool".to_string(),
            self.build_tool
                .map(|value| VmValue::String(arcstr::ArcStr::from(value)))
                .unwrap_or(VmValue::Nil),
        );
        value.insert(
            "vcs".to_string(),
            self.vcs
                .map(|value| VmValue::String(arcstr::ArcStr::from(value)))
                .unwrap_or(VmValue::Nil),
        );
        value.insert(
            "ci".to_string(),
            VmValue::List(std::sync::Arc::new(
                self.ci
                    .into_iter()
                    .map(|item| VmValue::String(arcstr::ArcStr::from(item)))
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
                    .map(|item| VmValue::String(arcstr::ArcStr::from(item)))
                    .collect(),
            )),
        );
        VmValue::dict(value)
    }
}

#[derive(Debug, Default)]
struct FingerprintSignals {
    languages: BTreeSet<String>,
    source_language_counts: BTreeMap<String, usize>,
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
    php_project: bool,
    ruby_project: bool,
    has_next_dep: bool,
    has_next_config: bool,
    has_vite_dep: bool,
    has_vite_config: bool,
    has_pytest_signal: bool,
    has_unittest_signal: bool,
}

impl FingerprintSignals {
    fn add_language_signal(&mut self, language: &str) {
        self.languages.insert(language.to_string());
    }

    fn add_source_language(&mut self, language: &str) {
        self.add_language_signal(language);
        *self
            .source_language_counts
            .entry(language.to_string())
            .or_default() += 1;
    }
}

#[derive(Debug, Clone, Copy, Default, Eq, Ord, PartialEq, PartialOrd)]
pub(super) enum ScanTier {
    #[default]
    Ambient,
    Config,
}

#[derive(Debug, Clone)]
pub(super) struct ProjectScanOptions {
    pub(super) tiers: BTreeSet<ScanTier>,
    depth: Option<usize>,
    include_hidden: bool,
    include_vendor: bool,
    ignore_policy: IgnorePolicy,
}

impl Default for ProjectScanOptions {
    fn default() -> Self {
        Self {
            tiers: BTreeSet::from([ScanTier::Ambient]),
            depth: Some(3),
            include_hidden: false,
            include_vendor: false,
            ignore_policy: IgnorePolicy::default(),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub(super) struct ProjectTreeEntry {
    pub(super) relative_path: String,
    pub(super) metadata_path: String,
    pub(super) structure_hash: String,
    pub(super) content_hash: String,
}

impl ProjectTreeEntry {
    pub(super) fn into_vm_value(self) -> VmValue {
        let mut value = BTreeMap::new();
        value.put_str("path", self.relative_path);
        value.put_str("dir", self.metadata_path);
        value.put_str("structure_hash", self.structure_hash);
        value.put_str("content_hash", self.content_hash);
        VmValue::dict(value)
    }
}

#[derive(Debug, Clone, Default)]
pub(super) struct ProjectEvidence {
    path: PathBuf,
    pub(super) language_scores: BTreeMap<String, f64>,
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
    pub(super) fn into_vm_value(self) -> VmValue {
        let confidence = confidence_value(&self);
        let mut result = BTreeMap::new();
        result.put_str("path", self.path.to_string_lossy());
        result.insert(
            "languages".to_string(),
            VmValue::List(std::sync::Arc::new(
                sorted_confident_labels(&self.language_scores)
                    .into_iter()
                    .map(|name| VmValue::String(arcstr::ArcStr::from(name)))
                    .collect(),
            )),
        );
        result.insert(
            "frameworks".to_string(),
            VmValue::List(std::sync::Arc::new(
                sorted_confident_labels(&self.framework_scores)
                    .into_iter()
                    .map(|name| VmValue::String(arcstr::ArcStr::from(name)))
                    .collect(),
            )),
        );
        result.insert(
            "build_systems".to_string(),
            VmValue::List(std::sync::Arc::new(
                self.build_systems
                    .into_iter()
                    .map(|name| VmValue::String(arcstr::ArcStr::from(name)))
                    .collect(),
            )),
        );
        result.insert(
            "vcs".to_string(),
            self.vcs
                .map(|value| VmValue::String(arcstr::ArcStr::from(value)))
                .unwrap_or(VmValue::Nil),
        );
        result.insert(
            "lockfiles".to_string(),
            VmValue::List(std::sync::Arc::new(
                self.lockfiles
                    .into_iter()
                    .map(|name| VmValue::String(arcstr::ArcStr::from(name)))
                    .collect(),
            )),
        );
        result.insert(
            "anchors".to_string(),
            VmValue::List(std::sync::Arc::new(
                self.anchors
                    .into_iter()
                    .map(|name| VmValue::String(arcstr::ArcStr::from(name)))
                    .collect(),
            )),
        );
        result.insert("confidence".to_string(), confidence);
        result.insert(
            "package_name".to_string(),
            self.package_name
                .map(|value| VmValue::String(arcstr::ArcStr::from(value)))
                .unwrap_or(VmValue::Nil),
        );
        result.insert(
            "build_commands".to_string(),
            VmValue::List(std::sync::Arc::new(
                self.build_commands
                    .into_iter()
                    .map(|cmd| VmValue::String(arcstr::ArcStr::from(cmd)))
                    .collect(),
            )),
        );
        result.insert(
            "declared_scripts".to_string(),
            VmValue::dict(
                self.declared_scripts
                    .into_iter()
                    .map(|(k, v)| (k, VmValue::String(arcstr::ArcStr::from(v))))
                    .collect::<crate::value::DictMap>(),
            ),
        );
        result.insert(
            "readme_code_fences".to_string(),
            VmValue::List(std::sync::Arc::new(
                self.readme_code_fences
                    .into_iter()
                    .map(|lang| VmValue::String(arcstr::ArcStr::from(lang)))
                    .collect(),
            )),
        );
        result.insert(
            "dockerfile_commands".to_string(),
            VmValue::List(std::sync::Arc::new(
                self.dockerfile_commands
                    .into_iter()
                    .map(|cmd| VmValue::String(arcstr::ArcStr::from(cmd)))
                    .collect(),
            )),
        );
        result.insert(
            "makefile_targets".to_string(),
            VmValue::List(std::sync::Arc::new(
                self.makefile_targets
                    .into_iter()
                    .map(|target| VmValue::String(arcstr::ArcStr::from(target)))
                    .collect(),
            )),
        );
        VmValue::dict(result)
    }
}

pub(super) fn parse_project_options(value: Option<&VmValue>) -> ProjectScanOptions {
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
    // An unknown level falls back to the default: this parser has no error
    // channel, and every other option here is equally lenient.
    if let Some(policy) = dict
        .get(IgnorePolicy::OPTION_KEY)
        .map(VmValue::display)
        .and_then(|raw| IgnorePolicy::parse(&raw))
    {
        options.ignore_policy = policy;
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

pub(super) fn project_fingerprint_from_value(value: &VmValue) -> Option<ProjectFingerprint> {
    project_fingerprint_from_dict(value.as_dict()?)
}

pub(super) fn project_fingerprint_from_dict(
    dict: &crate::value::DictMap,
) -> Option<ProjectFingerprint> {
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

fn dict_has_project_fingerprint_shape(dict: &crate::value::DictMap) -> bool {
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

fn string_list_field(dict: &crate::value::DictMap, key: &str) -> Vec<String> {
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

fn value_as_list(value: &VmValue) -> Option<&[VmValue]> {
    match value {
        VmValue::List(items) => Some(items.as_slice()),
        _ => None,
    }
}

pub(super) fn scan_project_tree(
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

pub(super) fn walk_project_tree(
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
    let gitignore = build_gitignore(base, options.ignore_policy);
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

    let skip_builtin_dirs = skips_builtin_dirs(options);
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
        if !skip_builtin_dirs {
            return true;
        }
        let name = entry.file_name().to_string_lossy();
        !BUILTIN_IGNORED_DIRS.contains(&name.as_ref())
    });

    builder
}

/// Whether the built-in directory names are skipped for this scan.
///
/// `include_vendor` is the long-standing per-scan escape hatch and stays
/// independent of the ignore level, so a caller can keep project ignore files
/// while still descending into build output.
fn skips_builtin_dirs(options: &ProjectScanOptions) -> bool {
    !options.include_vendor && options.ignore_policy != IgnorePolicy::None
}

/// Project ignore files at `base`, merged in stack order: within one
/// [`Gitignore`] the last matching pattern wins, so appending
/// [`PROJECT_IGNORE_FILENAMES`] in order reproduces the layer precedence every
/// other Harn walk gets. The built-in directory names stay out of this matcher
/// so `include_vendor` can still switch them off on their own.
fn build_gitignore(base: &Path, policy: IgnorePolicy) -> Gitignore {
    let mut builder = GitignoreBuilder::new(base);
    if policy.reads_project_files() {
        for name in PROJECT_IGNORE_FILENAMES {
            let _ = builder.add(base.join(name));
        }
    }
    builder.build().unwrap_or_else(|_| Gitignore::empty())
}

fn compute_directory_structure_hash(
    base: &Path,
    dir: &Path,
    options: &ProjectScanOptions,
) -> String {
    let gitignore = build_gitignore(base, options.ignore_policy);
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
    let gitignore = build_gitignore(base, options.ignore_policy);
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
        && skips_builtin_dirs(options)
        && BUILTIN_IGNORED_DIRS.contains(&name.as_str())
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

pub(super) fn scan_exact_directory(dir: &Path, options: &ProjectScanOptions) -> ProjectEvidence {
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

    if let Some(composer_json) = read_json_object(dir.join("composer.json")) {
        if evidence.package_name.is_none() {
            evidence.package_name = composer_json
                .get("name")
                .and_then(|value| value.as_str())
                .map(str::to_string);
        }
        if let Some(scripts) = composer_json
            .get("scripts")
            .and_then(|value| value.as_object())
        {
            for (name, value) in scripts {
                let commands = composer_script_commands(value);
                if commands.is_empty() {
                    continue;
                }
                evidence
                    .declared_scripts
                    .insert(format!("composer:{name}"), commands.join(" && "));
            }
            for key in ["test", "pest", "phpunit"] {
                if scripts.contains_key(key) {
                    push_unique(build_commands, format!("composer {key}"));
                }
            }
        }
        let deps = collect_composer_dependency_names(&composer_json);
        if deps.contains("pestphp/pest") {
            push_unique(build_commands, "vendor/bin/pest".to_string());
        }
        if deps.contains("phpunit/phpunit") {
            push_unique(build_commands, "vendor/bin/phpunit".to_string());
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

pub(super) fn detect_package_name(dir: &Path) -> Option<String> {
    package_name_from_pyproject(dir)
        .or_else(|| package_name_from_package_json(dir))
        .or_else(|| package_name_from_composer_json(dir))
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

fn package_name_from_composer_json(dir: &Path) -> Option<String> {
    let parsed = read_json_object(dir.join("composer.json"))?;
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
                #[expect(
                    clippy::string_slice,
                    reason = "keyword is ASCII and to_ascii_uppercase preserves byte offsets"
                )]
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
            if skips_builtin_dirs(options) && BUILTIN_IGNORED_DIRS.contains(&name.as_str()) {
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
    let temp_root = std::env::temp_dir().canonicalize().ok();
    let mut cursor = Some(dir);
    while let Some(path) = cursor {
        if path.join(".git").exists() {
            return Some("git".to_string());
        }
        if path.join(".hg").exists() {
            return Some("hg".to_string());
        }
        if temp_root.as_deref().is_some_and(|root| path == root) {
            return None;
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
    VmValue::dict(confidence)
}

pub(super) fn sorted_confident_labels(scores: &BTreeMap<String, f64>) -> Vec<String> {
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
