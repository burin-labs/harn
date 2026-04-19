use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::{fs, process};

use serde::Deserialize;

const PKG_DIR: &str = ".harn/packages";
const MANIFEST: &str = "harn.toml";
const LOCK_FILE: &str = "harn.lock";

#[derive(Debug, Clone, Deserialize)]
pub struct Manifest {
    #[allow(dead_code)]
    pub package: Option<PackageInfo>,
    #[serde(default)]
    pub dependencies: HashMap<String, Dependency>,
    #[serde(default)]
    pub mcp: Vec<McpServerConfig>,
    #[serde(default)]
    pub check: CheckConfig,
    #[serde(default)]
    pub workspace: WorkspaceConfig,
    /// `[skills]` table — per-project skill discovery configuration
    /// (paths, lookup_order, disable).
    #[serde(default)]
    pub skills: SkillsConfig,
    /// `[[skill.source]]` array-of-tables — declared skill sources
    /// (filesystem, git, reserved registry).
    #[serde(default)]
    pub skill: SkillTables,
    /// `[capabilities]` section — per-provider-per-model override of
    /// the shipped capability matrix (`defer_loading`, `tool_search`,
    /// `prompt_caching`, etc.). Entries under `[[capabilities.provider.<name>]]`
    /// are prepended to the built-in rules for the same provider so
    /// early adopters can flag proxied endpoints as supporting tool
    /// search without waiting for a Harn release. See
    /// `harn_vm::llm::capabilities` for the rule schema.
    #[serde(default)]
    pub capabilities: Option<harn_vm::llm::capabilities::CapabilitiesFile>,
    /// Stable exported package modules. Keys are the logical import
    /// suffixes (e.g. `providers/openai`) and values are package-root-
    /// relative file paths. Consumers import them via `<package>/<key>`.
    #[allow(dead_code)]
    #[serde(default)]
    pub exports: HashMap<String, String>,
    /// `[llm]` section — packaged provider definitions, aliases,
    /// inference rules, tier rules, and model defaults. Uses the same
    /// schema as `providers.toml`, but merges into the current run
    /// instead of replacing the global config file.
    #[serde(default)]
    pub llm: harn_vm::llm_config::ProvidersConfig,
    /// `[[hooks]]` array-of-tables — declarative runtime hooks installed
    /// once per process/thread before execution starts. Matches the
    /// manifest-extension ABI shape added by `[exports]` / `[llm]`, but
    /// the handlers themselves live in Harn modules.
    #[serde(default)]
    pub hooks: Vec<HookConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct HookConfig {
    pub event: harn_vm::orchestration::HookEvent,
    #[serde(default = "default_hook_pattern")]
    pub pattern: String,
    pub handler: String,
}

fn default_hook_pattern() -> String {
    "*".to_string()
}

/// `[skills]` table body.
#[derive(Debug, Default, Clone, Deserialize)]
#[allow(dead_code)] // `defaults` is parsed now and consumed by a follow-up CLI wiring PR.
pub struct SkillsConfig {
    /// Additional filesystem roots to scan. Each entry may be a
    /// literal directory or a glob (`packages/*/skills`). Resolved
    /// relative to the directory holding harn.toml.
    #[serde(default)]
    pub paths: Vec<String>,
    /// Override priority order. Values are layer labels —
    /// `cli`, `env`, `project`, `manifest`, `user`, `package`,
    /// `system`, `host`. Unlisted layers fall through to default
    /// priority after listed ones.
    #[serde(default)]
    pub lookup_order: Vec<String>,
    /// Disable entire layers. Same label set as `lookup_order`.
    #[serde(default)]
    pub disable: Vec<String>,
    /// `[skills.defaults]` inline sub-table — applied to every
    /// discovered skill when the field is unset in its SKILL.md
    /// frontmatter.
    #[serde(default)]
    pub defaults: SkillDefaults,
}

#[derive(Debug, Default, Clone, Deserialize)]
#[allow(dead_code)] // Wired in the follow-up that threads defaults into the loader.
pub struct SkillDefaults {
    #[serde(default)]
    pub tool_search: Option<String>,
    #[serde(default)]
    pub always_loaded: Vec<String>,
}

/// Container for `[[skill.source]]` array-of-tables.
#[derive(Debug, Default, Clone, Deserialize)]
pub struct SkillTables {
    #[serde(default, rename = "source")]
    pub sources: Vec<SkillSourceEntry>,
}

/// One `[[skill.source]]` entry. The `registry` variant is accepted
/// for forward-compat but inert — see issue #73 and `docs/src/skills.md`
/// for the marketplace timeline.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
#[allow(dead_code)]
pub enum SkillSourceEntry {
    Fs {
        path: String,
        #[serde(default)]
        namespace: Option<String>,
    },
    Git {
        url: String,
        #[serde(default)]
        tag: Option<String>,
        #[serde(default)]
        namespace: Option<String>,
    },
    Registry {
        #[serde(default)]
        url: Option<String>,
        #[serde(default)]
        name: Option<String>,
    },
}

/// Severity override for preflight diagnostics. `error` (default) fails
/// `harn check`; `warning` reports but does not fail; `off` suppresses
/// entirely. Accepted via `[check].preflight_severity` in harn.toml so
/// repos with hosts that do not expose every capability statically can
/// keep the checker running on genuine type errors.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum PreflightSeverity {
    #[default]
    Error,
    Warning,
    Off,
}

impl PreflightSeverity {
    pub fn from_opt(raw: Option<&str>) -> Self {
        match raw.map(|s| s.to_ascii_lowercase()) {
            Some(v) if v == "warning" || v == "warn" => Self::Warning,
            Some(v) if v == "off" || v == "allow" || v == "silent" => Self::Off,
            _ => Self::Error,
        }
    }
}

#[derive(Debug, Default, Clone, Deserialize)]
pub struct CheckConfig {
    #[serde(default)]
    pub strict: bool,
    #[serde(default)]
    pub strict_types: bool,
    #[serde(default)]
    pub disable_rules: Vec<String>,
    #[serde(default)]
    pub host_capabilities: HashMap<String, Vec<String>>,
    #[serde(default, alias = "host_capabilities_file")]
    pub host_capabilities_path: Option<String>,
    #[serde(default)]
    pub bundle_root: Option<String>,
    /// Downgrade or suppress preflight diagnostics. See
    /// [`PreflightSeverity`].
    #[serde(default, alias = "preflight-severity")]
    pub preflight_severity: Option<String>,
    /// List of `"capability.operation"` strings that should be accepted
    /// by preflight without emitting a diagnostic, even if the operation
    /// is not in the default or loaded capability manifest.
    #[serde(default, alias = "preflight-allow")]
    pub preflight_allow: Vec<String>,
}

#[derive(Debug, Default, Clone, Deserialize)]
pub struct WorkspaceConfig {
    /// Directory or file globs (repo-relative) that `harn check --workspace`
    /// walks to collect the full pipeline tree in one invocation.
    #[serde(default)]
    pub pipelines: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct McpServerConfig {
    pub name: String,
    #[serde(default)]
    pub transport: Option<String>,
    #[serde(default)]
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub auth_token: Option<String>,
    #[serde(default)]
    pub client_id: Option<String>,
    #[serde(default)]
    pub client_secret: Option<String>,
    #[serde(default)]
    pub scopes: Option<String>,
    #[serde(default)]
    pub protocol_version: Option<String>,
    #[serde(default)]
    pub proxy_server_name: Option<String>,
    /// When `true`, the server is NOT booted up-front. It boots on the
    /// first `mcp_call` or on skill activation that declares it in
    /// `requires_mcp`. See harn#75.
    #[serde(default)]
    pub lazy: bool,
    /// Optional pointer to a Server Card — either an HTTP(S) URL or a
    /// local filesystem path. When set, `mcp_server_card("name")` reads
    /// the card from this source (cached per-process with a TTL).
    #[serde(default)]
    pub card: Option<String>,
    /// How long (milliseconds) to keep a lazy server's process alive
    /// after its last binder releases. 0 / unset → disconnect
    /// immediately. Ignored for non-lazy servers.
    #[serde(default, alias = "keep-alive-ms", alias = "keep_alive")]
    pub keep_alive_ms: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct PackageInfo {
    pub name: Option<String>,
    pub version: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum Dependency {
    Table(DepTable),
    Path(String),
}

#[derive(Debug, Clone, Deserialize)]
pub struct DepTable {
    pub git: Option<String>,
    pub tag: Option<String>,
    pub path: Option<String>,
}

impl Dependency {
    fn git_url(&self) -> Option<&str> {
        match self {
            Dependency::Table(t) => t.git.as_deref(),
            Dependency::Path(_) => None,
        }
    }

    fn tag(&self) -> Option<&str> {
        match self {
            Dependency::Table(t) => t.tag.as_deref(),
            Dependency::Path(_) => None,
        }
    }

    fn local_path(&self) -> Option<&str> {
        match self {
            Dependency::Table(t) => t.path.as_deref(),
            Dependency::Path(p) => Some(p.as_str()),
        }
    }
}

#[derive(Debug, Default, Clone)]
pub struct RuntimeExtensions {
    pub root_manifest: Option<Manifest>,
    pub llm: Option<harn_vm::llm_config::ProvidersConfig>,
    pub capabilities: Option<harn_vm::llm::capabilities::CapabilitiesFile>,
    pub hooks: Vec<ResolvedHookConfig>,
}

#[derive(Debug, Clone)]
pub struct ResolvedHookConfig {
    pub event: harn_vm::orchestration::HookEvent,
    pub pattern: String,
    pub handler: String,
    pub manifest_dir: PathBuf,
    pub package_name: Option<String>,
}

#[derive(Debug, Clone)]
struct LocatedManifest {
    manifest: Manifest,
    dir: PathBuf,
}

type HookModuleCacheKey = (PathBuf, Option<String>, String);
type HookModuleExports = std::collections::BTreeMap<String, std::rc::Rc<harn_vm::VmClosure>>;

#[derive(Debug, Default)]
struct LockFile {
    entries: HashMap<String, LockEntry>,
}

#[derive(Debug, Clone)]
struct LockEntry {
    git: Option<String>,
    tag: Option<String>,
    commit: Option<String>,
    path: Option<String>,
}

impl LockFile {
    fn load(path: &Path) -> Self {
        let content = match fs::read_to_string(path) {
            Ok(s) => s,
            Err(_) => return Self::default(),
        };

        let mut entries = HashMap::new();
        let mut current_name: Option<String> = None;
        let mut current = LockEntry {
            git: None,
            tag: None,
            commit: None,
            path: None,
        };

        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with("[[package]]") {
                if let Some(name) = current_name.take() {
                    entries.insert(name, current.clone());
                }
                current = LockEntry {
                    git: None,
                    tag: None,
                    commit: None,
                    path: None,
                };
            } else if let Some((key, value)) = trimmed.split_once('=') {
                let key = key.trim();
                let value = value.trim().trim_matches('"');
                match key {
                    "name" => current_name = Some(value.to_string()),
                    "git" => current.git = Some(value.to_string()),
                    "tag" => current.tag = Some(value.to_string()),
                    "commit" => current.commit = Some(value.to_string()),
                    "path" => current.path = Some(value.to_string()),
                    _ => {}
                }
            }
        }
        if let Some(name) = current_name {
            entries.insert(name, current);
        }

        LockFile { entries }
    }

    fn save(&self, path: &Path) {
        let mut out =
            String::from("# This file is auto-generated by `harn install`. Do not edit.\n\n");
        let mut names: Vec<&String> = self.entries.keys().collect();
        names.sort();
        for name in names {
            let entry = &self.entries[name];
            out.push_str("[[package]]\n");
            out.push_str(&format!("name = \"{name}\"\n"));
            if let Some(git) = &entry.git {
                out.push_str(&format!("git = \"{git}\"\n"));
            }
            if let Some(tag) = &entry.tag {
                out.push_str(&format!("tag = \"{tag}\"\n"));
            }
            if let Some(commit) = &entry.commit {
                out.push_str(&format!("commit = \"{commit}\"\n"));
            }
            if let Some(path) = &entry.path {
                out.push_str(&format!("path = \"{path}\"\n"));
            }
            out.push('\n');
        }
        if let Err(e) = fs::write(path, &out) {
            eprintln!("Failed to write lock file: {e}");
        }
    }
}

pub fn read_manifest() -> Manifest {
    let content = match fs::read_to_string(MANIFEST) {
        Ok(s) => s,
        Err(_) => {
            eprintln!("No harn.toml found in current directory.");
            eprintln!("Create one with `harn init` or manually.");
            process::exit(1);
        }
    };
    match toml::from_str::<Manifest>(&content) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("Failed to parse harn.toml: {e}");
            process::exit(1);
        }
    }
}

fn merge_capability_overrides(
    target: &mut harn_vm::llm::capabilities::CapabilitiesFile,
    source: &harn_vm::llm::capabilities::CapabilitiesFile,
) {
    for (provider, rules) in &source.provider {
        target
            .provider
            .entry(provider.clone())
            .or_default()
            .extend(rules.clone());
    }
    target
        .provider_family
        .extend(source.provider_family.clone());
}

fn collect_package_manifests(packages_dir: &Path) -> Vec<LocatedManifest> {
    let mut manifests = Vec::new();
    let Ok(entries) = fs::read_dir(packages_dir) else {
        return manifests;
    };
    let mut dirs: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.is_dir())
        .collect();
    dirs.sort();
    for dir in dirs {
        let manifest_path = dir.join(MANIFEST);
        let Ok(content) = fs::read_to_string(&manifest_path) else {
            continue;
        };
        match toml::from_str::<Manifest>(&content) {
            Ok(manifest) => manifests.push(LocatedManifest { manifest, dir }),
            Err(e) => eprintln!("warning: failed to parse {}: {e}", manifest_path.display()),
        }
    }
    manifests
}

fn resolved_hooks_from_manifest(
    manifest: &Manifest,
    manifest_dir: &Path,
) -> Vec<ResolvedHookConfig> {
    manifest
        .hooks
        .iter()
        .map(|hook| ResolvedHookConfig {
            event: hook.event,
            pattern: hook.pattern.clone(),
            handler: hook.handler.clone(),
            manifest_dir: manifest_dir.to_path_buf(),
            package_name: manifest.package.as_ref().and_then(|pkg| pkg.name.clone()),
        })
        .collect()
}

fn manifest_capabilities(
    manifest: &Manifest,
) -> Option<&harn_vm::llm::capabilities::CapabilitiesFile> {
    manifest.capabilities.as_ref()
}

fn is_empty_capabilities(file: &harn_vm::llm::capabilities::CapabilitiesFile) -> bool {
    file.provider.is_empty() && file.provider_family.is_empty()
}

/// Load the nearest project manifest plus any installed package manifests and
/// merge their runtime extensions. Installed packages load first; the root
/// project manifest wins on conflicts.
pub fn load_runtime_extensions(anchor: &Path) -> RuntimeExtensions {
    let Some((root_manifest, manifest_dir)) = find_nearest_manifest(anchor) else {
        return RuntimeExtensions::default();
    };

    let mut llm = harn_vm::llm_config::ProvidersConfig::default();
    let mut capabilities = harn_vm::llm::capabilities::CapabilitiesFile::default();
    let mut hooks = Vec::new();

    for located in collect_package_manifests(&manifest_dir.join(PKG_DIR)) {
        llm.merge_from(&located.manifest.llm);
        if let Some(file) = manifest_capabilities(&located.manifest) {
            merge_capability_overrides(&mut capabilities, file);
        }
        hooks.extend(resolved_hooks_from_manifest(
            &located.manifest,
            &located.dir,
        ));
    }

    llm.merge_from(&root_manifest.llm);
    if let Some(file) = manifest_capabilities(&root_manifest) {
        merge_capability_overrides(&mut capabilities, file);
    }
    hooks.extend(resolved_hooks_from_manifest(&root_manifest, &manifest_dir));

    RuntimeExtensions {
        root_manifest: Some(root_manifest),
        llm: (!llm.is_empty()).then_some(llm),
        capabilities: (!is_empty_capabilities(&capabilities)).then_some(capabilities),
        hooks,
    }
}

/// Install merged runtime extensions on the current thread.
pub fn install_runtime_extensions(extensions: &RuntimeExtensions) {
    harn_vm::llm_config::set_user_overrides(extensions.llm.clone());
    harn_vm::llm::capabilities::set_user_overrides(extensions.capabilities.clone());
}

async fn resolve_hook_exports(
    vm: &mut harn_vm::Vm,
    hook: &ResolvedHookConfig,
    module_name: &str,
) -> Result<HookModuleExports, String> {
    let self_name_matches = hook
        .package_name
        .as_deref()
        .is_some_and(|name| name == module_name);
    if self_name_matches {
        let lib_path = hook.manifest_dir.join("lib.harn");
        if lib_path.exists() {
            return vm
                .load_module_exports(&lib_path)
                .await
                .map_err(|error| error.to_string());
        }
    }

    vm.load_module_exports_from_import(module_name)
        .await
        .map_err(|error| error.to_string())
}

pub async fn install_manifest_hooks(
    vm: &mut harn_vm::Vm,
    extensions: &RuntimeExtensions,
) -> Result<(), String> {
    harn_vm::orchestration::clear_runtime_hooks();
    let mut loaded_exports: HashMap<HookModuleCacheKey, HookModuleExports> = HashMap::new();
    for hook in &extensions.hooks {
        let Some((module_name, function_name)) = hook.handler.rsplit_once("::") else {
            return Err(format!(
                "invalid hook handler '{}': expected <module>::<function>",
                hook.handler
            ));
        };
        let cache_key = (
            hook.manifest_dir.clone(),
            hook.package_name.clone(),
            module_name.to_string(),
        );
        if !loaded_exports.contains_key(&cache_key) {
            let exports = resolve_hook_exports(vm, hook, module_name).await?;
            loaded_exports.insert(cache_key.clone(), exports);
        }
        let exports = loaded_exports
            .get(&cache_key)
            .expect("manifest hook exports cached");
        let Some(closure) = exports.get(function_name) else {
            return Err(format!(
                "hook handler '{}' is not exported by module '{}'",
                function_name, module_name
            ));
        };
        harn_vm::orchestration::register_vm_hook(
            hook.event,
            hook.pattern.clone(),
            hook.handler.clone(),
            closure.clone(),
        );
    }
    Ok(())
}

fn absolutize_check_config_paths(mut config: CheckConfig, manifest_dir: &Path) -> CheckConfig {
    if let Some(path) = config.host_capabilities_path.clone() {
        let candidate = PathBuf::from(&path);
        if !candidate.is_absolute() {
            config.host_capabilities_path =
                Some(manifest_dir.join(candidate).display().to_string());
        }
    }
    if let Some(path) = config.bundle_root.clone() {
        let candidate = PathBuf::from(&path);
        if !candidate.is_absolute() {
            config.bundle_root = Some(manifest_dir.join(candidate).display().to_string());
        }
    }
    config
}

/// Walk upward from `start` (or its parent if it's a file path that
/// does not yet exist) looking for the nearest `harn.toml`. Stops at
/// a `.git` boundary so a stray manifest in `$HOME` or a parent
/// project is never silently picked up. Returns `(manifest, manifest_dir)`
/// when found.
fn find_nearest_manifest(start: &Path) -> Option<(Manifest, PathBuf)> {
    const MAX_PARENT_DIRS: usize = 16;
    let base = if start.is_absolute() {
        start.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(start)
    };
    let mut cursor: Option<PathBuf> = if base.is_dir() {
        Some(base)
    } else {
        base.parent().map(Path::to_path_buf)
    };
    let mut steps = 0usize;
    while let Some(dir) = cursor {
        if steps >= MAX_PARENT_DIRS {
            break;
        }
        steps += 1;
        let candidate = dir.join(MANIFEST);
        if candidate.is_file() {
            if let Ok(content) = fs::read_to_string(&candidate) {
                match toml::from_str::<Manifest>(&content) {
                    Ok(manifest) => return Some((manifest, dir)),
                    Err(e) => {
                        eprintln!("warning: failed to parse {}: {e}", candidate.display());
                        return None;
                    }
                }
            }
        }
        if dir.join(".git").exists() {
            break;
        }
        cursor = dir.parent().map(Path::to_path_buf);
    }
    None
}

/// Load the `[check]` config from the nearest `harn.toml`.
/// Walks up from the given file (or from cwd if no file is given),
/// stopping at a `.git` boundary.
pub fn load_check_config(harn_file: Option<&std::path::Path>) -> CheckConfig {
    let anchor = harn_file
        .map(Path::to_path_buf)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    if let Some((manifest, dir)) = find_nearest_manifest(&anchor) {
        return absolutize_check_config_paths(manifest.check, &dir);
    }
    CheckConfig::default()
}

/// Load the `[workspace]` config and the directory of the `harn.toml`
/// it came from. Paths in the returned config are left as-is (callers
/// resolve them against the returned `manifest_dir`).
pub fn load_workspace_config(anchor: Option<&Path>) -> Option<(WorkspaceConfig, PathBuf)> {
    let anchor = anchor
        .map(Path::to_path_buf)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let (manifest, dir) = find_nearest_manifest(&anchor)?;
    Some((manifest.workspace, dir))
}

/// `harn install` — install all dependencies from harn.toml.
pub fn install_packages() {
    let manifest = read_manifest();

    if manifest.dependencies.is_empty() {
        println!("No dependencies to install.");
        return;
    }

    let has_git_deps = manifest
        .dependencies
        .values()
        .any(|d| d.git_url().is_some());
    if has_git_deps
        && process::Command::new("git")
            .arg("--version")
            .output()
            .is_err()
    {
        eprintln!("Error: git is required to install git dependencies but was not found.");
        eprintln!("Install git and ensure it's in your PATH.");
        process::exit(1);
    }

    let pkg_dir = PathBuf::from(PKG_DIR);
    if let Err(e) = fs::create_dir_all(&pkg_dir) {
        eprintln!("Failed to create {PKG_DIR}: {e}");
        process::exit(1);
    }

    let mut lock = LockFile::load(Path::new(LOCK_FILE));
    let mut installed = 0;
    let mut visiting = HashSet::new();

    for (name, dep) in &manifest.dependencies {
        install_one(
            name,
            dep,
            &pkg_dir,
            &mut lock,
            &mut visiting,
            &mut installed,
        );
    }

    lock.save(Path::new(LOCK_FILE));
    println!("\nInstalled {installed} package(s) to {PKG_DIR}/");
}

fn install_one(
    name: &str,
    dep: &Dependency,
    pkg_dir: &Path,
    lock: &mut LockFile,
    visiting: &mut HashSet<String>,
    installed: &mut usize,
) {
    if !visiting.insert(name.to_string()) {
        eprintln!("  warning: circular dependency detected for '{name}', skipping");
        return;
    }

    let dest = pkg_dir.join(name);

    if let Some(git_url) = dep.git_url() {
        install_git_dep(name, git_url, dep.tag(), &dest, lock);
        *installed += 1;
    } else if let Some(local_path) = dep.local_path() {
        install_local_dep(name, local_path, &dest);
        *installed += 1;
        lock.entries.insert(
            name.to_string(),
            LockEntry {
                git: None,
                tag: None,
                commit: None,
                path: Some(local_path.to_string()),
            },
        );
    } else {
        eprintln!("  {name}: no git or path specified, skipping");
        visiting.remove(name);
        return;
    }

    let sub_manifest_path = dest.join("harn.toml");
    if sub_manifest_path.exists() {
        if let Ok(content) = fs::read_to_string(&sub_manifest_path) {
            if let Ok(sub_manifest) = toml::from_str::<Manifest>(&content) {
                for (sub_name, sub_dep) in &sub_manifest.dependencies {
                    let sub_dest = pkg_dir.join(sub_name);
                    if !sub_dest.exists() {
                        install_one(sub_name, sub_dep, pkg_dir, lock, visiting, installed);
                    }
                }
            }
        }
    }

    visiting.remove(name);
}

fn install_git_dep(name: &str, git_url: &str, tag: Option<&str>, dest: &Path, lock: &mut LockFile) {
    if let Some(entry) = lock.entries.get(name) {
        if entry.git.as_deref() == Some(git_url)
            && entry.tag.as_deref() == tag
            && entry.commit.is_some()
            && dest.exists()
        {
            println!("  {name}: up to date (locked)");
            return;
        }
    }

    if dest.exists() {
        println!("  updating {name} from {git_url}");
        let _ = fs::remove_dir_all(dest);
    } else {
        println!("  installing {name} from {git_url}");
    }

    let mut cmd = process::Command::new("git");
    cmd.args(["clone", "--depth", "1"]);
    if let Some(t) = tag {
        cmd.args(["--branch", t]);
    }
    cmd.arg(git_url);
    cmd.arg(dest.as_os_str());
    cmd.stdout(process::Stdio::null());
    cmd.stderr(process::Stdio::piped());

    match cmd.output() {
        Ok(output) if output.status.success() => {
            let commit = get_git_commit(dest);
            // Drop .git to save disk space.
            let _ = fs::remove_dir_all(dest.join(".git"));
            lock.entries.insert(
                name.to_string(),
                LockEntry {
                    git: Some(git_url.to_string()),
                    tag: tag.map(|t| t.to_string()),
                    commit,
                    path: None,
                },
            );
        }
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            eprintln!("  failed to clone {name}: {stderr}");
        }
        Err(e) => {
            eprintln!("  failed to run git for {name}: {e}");
            eprintln!("  hint: make sure git is installed and in PATH");
        }
    }
}

fn get_git_commit(repo_dir: &Path) -> Option<String> {
    let output = process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(repo_dir)
        .output()
        .ok()?;
    if output.status.success() {
        Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        None
    }
}

fn install_local_dep(name: &str, source_path: &str, dest: &Path) {
    let source = Path::new(source_path);

    if source.is_dir() {
        if dest.exists() {
            println!("  updating {name} from {source_path}");
            let _ = fs::remove_dir_all(dest);
        } else {
            println!("  installing {name} from {source_path}");
        }
        if let Err(e) = copy_dir_recursive(source, dest) {
            eprintln!("  failed to install {name}: {e}");
        }
    } else if source.is_file() {
        let dest_file = dest.with_extension("harn");
        if dest_file.exists() {
            println!("  updating {name} from {source_path}");
        } else {
            println!("  installing {name} from {source_path}");
        }
        if let Some(parent) = dest_file.parent() {
            fs::create_dir_all(parent).ok();
        }
        if let Err(e) = fs::copy(source, &dest_file) {
            eprintln!("  failed to install {name}: {e}");
        }
    } else {
        let harn_source = PathBuf::from(format!("{source_path}.harn"));
        if harn_source.exists() {
            let dest_file = dest.with_extension("harn");
            println!("  installing {name} from {}", harn_source.display());
            if let Some(parent) = dest_file.parent() {
                fs::create_dir_all(parent).ok();
            }
            if let Err(e) = fs::copy(&harn_source, &dest_file) {
                eprintln!("  failed to install {name}: {e}");
            }
        } else {
            eprintln!("  package source not found: {source_path}");
        }
    }
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let dest_path = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_recursive(&entry.path(), &dest_path)?;
        } else {
            fs::copy(entry.path(), &dest_path)?;
        }
    }
    Ok(())
}

/// `harn add <name> --git <url> [--tag <tag>]` — add a dependency to harn.toml.
pub fn add_package(name: &str, git_url: Option<&str>, tag: Option<&str>, local_path: Option<&str>) {
    if git_url.is_none() && local_path.is_none() {
        eprintln!("Must specify --git <url> or --path <local-path>");
        process::exit(1);
    }

    let manifest_path = Path::new(MANIFEST);
    let mut content = if manifest_path.exists() {
        fs::read_to_string(manifest_path).unwrap_or_default()
    } else {
        "[package]\nname = \"my-project\"\nversion = \"0.1.0\"\n".to_string()
    };

    if !content.contains("[dependencies]") {
        content.push_str("\n[dependencies]\n");
    }

    let dep_line = if let Some(url) = git_url {
        if let Some(t) = tag {
            format!("{name} = {{ git = \"{url}\", tag = \"{t}\" }}")
        } else {
            format!("{name} = {{ git = \"{url}\" }}")
        }
    } else {
        let p = local_path.unwrap();
        format!("{name} = {{ path = \"{p}\" }}")
    };

    let mut lines: Vec<String> = content.lines().map(|l| l.to_string()).collect();
    let mut replaced = false;
    for line in &mut lines {
        if line.starts_with(name) && line.contains('=') {
            // Avoid prefix matches (e.g. `foo_bar` when looking for `foo`).
            let before_eq = line.split('=').next().unwrap_or("").trim();
            if before_eq == name {
                *line = dep_line.clone();
                replaced = true;
                break;
            }
        }
    }

    if !replaced {
        let dep_idx = lines
            .iter()
            .position(|l| l.trim() == "[dependencies]")
            .unwrap_or_else(|| {
                lines.push("[dependencies]".to_string());
                lines.len() - 1
            });
        lines.insert(dep_idx + 1, dep_line);
    }

    let new_content = lines.join("\n") + "\n";
    if let Err(e) = fs::write(manifest_path, &new_content) {
        eprintln!("Failed to write harn.toml: {e}");
        process::exit(1);
    }

    println!("Added {name} to harn.toml");
    println!("Run `harn install` to fetch the package.");
}

/// Resolved `[skills]` section plus the directory the manifest came
/// from. Paths in `skills.paths` are joined against `manifest_dir`;
/// `[[skill.source]]` fs entries get absolutized here too.
pub struct ResolvedSkillsConfig {
    pub config: SkillsConfig,
    pub sources: Vec<SkillSourceEntry>,
    pub manifest_dir: PathBuf,
}

/// Load the `[skills]` + `[[skill.source]]` tables from the nearest
/// harn.toml, walking up from `anchor` like [`load_check_config`].
/// Returns `None` when there is no manifest on the walk path.
pub fn load_skills_config(anchor: Option<&Path>) -> Option<ResolvedSkillsConfig> {
    let anchor = anchor
        .map(Path::to_path_buf)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let (manifest, dir) = find_nearest_manifest(&anchor)?;

    // Absolutize `[[skill.source]]` fs paths relative to manifest_dir.
    let sources = manifest
        .skill
        .sources
        .into_iter()
        .map(|s| match s {
            SkillSourceEntry::Fs { path, namespace } => {
                let abs = if PathBuf::from(&path).is_absolute() {
                    path
                } else {
                    dir.join(&path).display().to_string()
                };
                SkillSourceEntry::Fs {
                    path: abs,
                    namespace,
                }
            }
            other => other,
        })
        .collect();

    Some(ResolvedSkillsConfig {
        config: manifest.skills,
        sources,
        manifest_dir: dir,
    })
}

/// Expand `skills.paths` (which may include simple `*` globs) into
/// concrete directories relative to `manifest_dir`. We implement just
/// enough globbing for the documented `packages/*/skills` pattern so
/// we don't force a `glob`-crate dep on harn-cli.
pub fn resolve_skills_paths(cfg: &ResolvedSkillsConfig) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for entry in &cfg.config.paths {
        let raw = PathBuf::from(entry);
        let absolute = if raw.is_absolute() {
            raw
        } else {
            cfg.manifest_dir.join(raw)
        };
        out.extend(expand_single_star_glob(&absolute));
    }
    out
}

fn expand_single_star_glob(path: &Path) -> Vec<PathBuf> {
    let as_str = path.to_string_lossy().to_string();
    if !as_str.contains('*') {
        return vec![path.to_path_buf()];
    }
    let components: Vec<&str> = as_str.split('/').collect();
    let mut results: Vec<PathBuf> = vec![PathBuf::new()];
    for comp in components {
        let mut next: Vec<PathBuf> = Vec::new();
        if comp == "*" {
            for parent in &results {
                if let Ok(entries) = fs::read_dir(parent) {
                    for entry in entries.flatten() {
                        let path = entry.path();
                        if path.is_dir() {
                            next.push(path);
                        }
                    }
                }
            }
        } else if comp.is_empty() {
            for parent in &results {
                if parent.as_os_str().is_empty() {
                    next.push(PathBuf::from("/"));
                } else {
                    next.push(parent.clone());
                }
            }
        } else {
            for parent in &results {
                let joined = parent.join(comp);
                // Filter branches whose literal suffix does not exist on
                // disk so downstream FS sources don't iterate over phantom
                // directories (one Rust round-trip cheaper than discovering
                // them at load time).
                if joined.exists() || parent.as_os_str().is_empty() {
                    next.push(joined);
                }
            }
        }
        results = next;
    }
    results
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preflight_severity_parsing_accepts_synonyms() {
        assert_eq!(
            PreflightSeverity::from_opt(Some("warning")),
            PreflightSeverity::Warning
        );
        assert_eq!(
            PreflightSeverity::from_opt(Some("WARN")),
            PreflightSeverity::Warning
        );
        assert_eq!(
            PreflightSeverity::from_opt(Some("off")),
            PreflightSeverity::Off
        );
        assert_eq!(
            PreflightSeverity::from_opt(Some("allow")),
            PreflightSeverity::Off
        );
        assert_eq!(
            PreflightSeverity::from_opt(Some("error")),
            PreflightSeverity::Error
        );
        assert_eq!(PreflightSeverity::from_opt(None), PreflightSeverity::Error);
        // Unknown values fall back to the safe default (error).
        assert_eq!(
            PreflightSeverity::from_opt(Some("bogus")),
            PreflightSeverity::Error
        );
    }

    #[test]
    fn load_check_config_walks_up_from_nested_file() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        // Mark root as project boundary so walk-up terminates here.
        std::fs::create_dir_all(root.join(".git")).unwrap();
        fs::write(
            root.join(MANIFEST),
            r#"
[check]
preflight_severity = "warning"
preflight_allow = ["custom.scan", "runtime.*"]
host_capabilities_path = "./schemas/host-caps.json"

[workspace]
pipelines = ["pipelines", "scripts"]
"#,
        )
        .unwrap();
        let nested = root.join("src").join("deep");
        std::fs::create_dir_all(&nested).unwrap();
        let harn_file = nested.join("pipeline.harn");
        fs::write(&harn_file, "pipeline main() {}\n").unwrap();

        let cfg = load_check_config(Some(&harn_file));
        assert_eq!(cfg.preflight_severity.as_deref(), Some("warning"));
        assert_eq!(cfg.preflight_allow, vec!["custom.scan", "runtime.*"]);
        let caps_path = cfg.host_capabilities_path.expect("host caps path");
        assert!(
            caps_path.ends_with("schemas/host-caps.json")
                || caps_path.ends_with("schemas\\host-caps.json"),
            "unexpected absolutized path: {caps_path}"
        );

        let (workspace, manifest_dir) =
            load_workspace_config(Some(&harn_file)).expect("workspace manifest");
        assert_eq!(workspace.pipelines, vec!["pipelines", "scripts"]);
        // Walk-up lands on the directory containing the harn.toml.
        assert_eq!(manifest_dir, root);
    }

    #[test]
    fn load_skills_config_parses_tables_and_sources() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        fs::write(
            root.join(MANIFEST),
            r#"
[skills]
paths = ["packages/*/skills", "../shared-skills"]
lookup_order = ["cli", "project", "host"]
disable = ["system"]

[skills.defaults]
tool_search = "bm25"
always_loaded = ["look", "edit"]

[[skill.source]]
type = "fs"
path = "../shared"

[[skill.source]]
type = "git"
url = "https://github.com/acme/harn-skills"
tag = "v1.2.0"

[[skill.source]]
type = "registry"
url = "https://skills.harn.burincode.com"
name = "acme/ops"
"#,
        )
        .unwrap();
        let harn_file = root.join("main.harn");
        fs::write(&harn_file, "pipeline main() {}\n").unwrap();

        let resolved = load_skills_config(Some(&harn_file)).expect("skills config should load");
        assert_eq!(resolved.config.paths.len(), 2);
        assert_eq!(resolved.config.lookup_order, vec!["cli", "project", "host"]);
        assert_eq!(resolved.config.disable, vec!["system"]);
        assert_eq!(
            resolved.config.defaults.tool_search.as_deref(),
            Some("bm25")
        );
        assert_eq!(resolved.config.defaults.always_loaded, vec!["look", "edit"]);

        assert_eq!(resolved.sources.len(), 3);
        match &resolved.sources[0] {
            SkillSourceEntry::Fs { path, .. } => {
                assert!(path.ends_with("shared"), "fs path absolutized: {path}");
            }
            other => panic!("expected fs source, got {other:?}"),
        }
        match &resolved.sources[1] {
            SkillSourceEntry::Git { url, tag, .. } => {
                assert!(url.contains("harn-skills"));
                assert_eq!(tag.as_deref(), Some("v1.2.0"));
            }
            other => panic!("expected git source, got {other:?}"),
        }
        assert!(matches!(
            &resolved.sources[2],
            SkillSourceEntry::Registry { .. }
        ));
    }

    #[test]
    fn expand_single_star_glob_handles_packages_pattern() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::create_dir_all(root.join("packages/pkg-a/skills")).unwrap();
        fs::create_dir_all(root.join("packages/pkg-b/skills")).unwrap();
        fs::create_dir_all(root.join("packages/pkg-c")).unwrap();

        let raw = root.join("packages").join("*").join("skills");
        let expanded = expand_single_star_glob(&raw);
        assert_eq!(expanded.len(), 2);
    }

    #[test]
    fn load_check_config_stops_at_git_boundary() {
        let tmp = tempfile::tempdir().unwrap();
        // An ancestor harn.toml above .git must NOT be picked up.
        fs::write(
            tmp.path().join(MANIFEST),
            "[check]\npreflight_severity = \"off\"\n",
        )
        .unwrap();
        let project = tmp.path().join("project");
        std::fs::create_dir_all(project.join(".git")).unwrap();
        let inner = project.join("src");
        std::fs::create_dir_all(&inner).unwrap();
        let harn_file = inner.join("main.harn");
        fs::write(&harn_file, "pipeline main() {}\n").unwrap();
        let cfg = load_check_config(Some(&harn_file));
        assert!(
            cfg.preflight_severity.is_none(),
            "must not inherit harn.toml from outside the .git boundary"
        );
    }

    #[test]
    fn load_runtime_extensions_merges_package_and_root_llm_config() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::create_dir_all(root.join(".harn/packages/acme")).unwrap();
        fs::write(
            root.join(MANIFEST),
            r#"
[llm.aliases]
project-fast = { id = "project/model", provider = "project" }

[llm.providers.project]
base_url = "https://project.test/v1"
chat_endpoint = "/chat/completions"
"#,
        )
        .unwrap();
        fs::write(
            root.join(".harn/packages/acme/harn.toml"),
            r#"
[llm.aliases]
acme-fast = { id = "acme/model", provider = "acme" }

[llm.providers.acme]
base_url = "https://acme.test/v1"
chat_endpoint = "/chat/completions"
"#,
        )
        .unwrap();
        let harn_file = root.join("main.harn");
        fs::write(&harn_file, "pipeline main() {}\n").unwrap();

        let extensions = load_runtime_extensions(&harn_file);
        let llm = extensions.llm.expect("merged llm config");
        assert!(llm.providers.contains_key("acme"));
        assert!(llm.providers.contains_key("project"));
        assert!(llm.aliases.contains_key("acme-fast"));
        assert!(llm.aliases.contains_key("project-fast"));
    }

    #[test]
    fn load_runtime_extensions_collects_manifest_hooks_in_order() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::create_dir_all(root.join(".harn/packages/acme")).unwrap();
        fs::write(
            root.join(MANIFEST),
            r#"
[package]
name = "workspace"

[[hooks]]
event = "PostToolUse"
pattern = "tool.name =~ \"read\""
handler = "workspace::after_read"
"#,
        )
        .unwrap();
        fs::write(
            root.join(".harn/packages/acme/harn.toml"),
            r#"
[package]
name = "acme"

[[hooks]]
event = "PreToolUse"
pattern = "tool.name =~ \"edit|write\""
handler = "acme::audit_edit"
"#,
        )
        .unwrap();
        let harn_file = root.join("main.harn");
        fs::write(&harn_file, "pipeline main() {}\n").unwrap();

        let extensions = load_runtime_extensions(&harn_file);
        assert_eq!(extensions.hooks.len(), 2);
        assert_eq!(extensions.hooks[0].handler, "acme::audit_edit");
        assert_eq!(extensions.hooks[1].handler, "workspace::after_read");
    }
}
