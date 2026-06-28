//! mise / asdf toolchain PATH normalizer for the `run()` tool family.
//!
//! Background (an IDE host's env-resilience R&D, "Pick B"): when the agent runs a
//! shell command, the child process should resolve the interpreter the *repo
//! declares* (`.tool-versions`, `.mise.toml`, `.ruby-version`, `.nvmrc`), not
//! whatever stale system version happens to be first on `PATH`. The motivating
//! failure was a keg-only Ruby 2.6.10 shadowing a repo-declared Ruby 3.2 — the
//! agent burned ~32 turns chasing a phantom. This generalizes an IDE host's
//! hardcoded `/opt/homebrew/opt/ruby/bin` fix into a principled,
//! language-agnostic, declaration-gated mechanism.
//!
//! Guardrails (these are why the design passed adversarial review):
//!
//! - **Declaration-gated.** Absent a recognized version file at (or above) the
//!   command's cwd, the child's `PATH` is byte-identical to today. Healthy
//!   repos stay silent.
//! - **Process-scoped.** Only the `run()` child-process environment is
//!   touched — never the user's interactive shell, never a global mutation.
//! - **No version guessing.** Resolution is delegated to `mise` / `asdf`. There
//!   is no per-language version table in harn to rot. If neither tool is on
//!   `PATH`, or it cannot resolve the declared tool, `PATH` is left untouched —
//!   we surface a diagnosis, we do not invent a phantom shadow.
//! - **Visible + disable-able.** Each override emits a one-line `tracing::info!`
//!   ("using ruby 3.2 from .tool-versions for run()"). The whole feature is
//!   gated behind `HARN_RUN_TOOLCHAIN_PATH` (default OFF), so a developer whose
//!   working interpreter deliberately differs from the repo declaration — the
//!   one real false-positive — can leave it off per session.
//!
//! The detection + resolution machinery is split from the I/O (filesystem read,
//! resolver shell-out, env lookup) via the [`Env`] trait so unit tests are
//! deterministic and never touch the host.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::process::EnvMode;

/// Environment variable that gates the whole feature. Default OFF: the host
/// (e.g. an IDE host) opts in per agent-session by setting it, and can disable it for
/// a single session by unsetting it. Recognized truthy values: `1`, `on`,
/// `true`, `auto`, `yes` (case-insensitive). Anything else (or unset) is off.
pub(crate) const ENABLE_ENV: &str = "HARN_RUN_TOOLCHAIN_PATH";

/// Version-declaration files we recognize, in priority order. Each maps to the
/// resolver query we will issue. `.tool-versions` / `.mise.toml` are
/// multi-tool; the single-tool files pin one well-known tool.
const RUBY_VERSION_FILE: &str = ".ruby-version";
const NODE_VERSION_FILE: &str = ".nvmrc";
const TOOL_VERSIONS_FILE: &str = ".tool-versions";
const MISE_TOML_FILE: &str = ".mise.toml";

/// A single declared `(tool, version)` pair plus the file that declared it (for
/// the visible log line).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Declaration {
    pub(crate) tool: String,
    pub(crate) version: String,
    pub(crate) source_file: String,
}

/// Indirection over the host so tests are deterministic. Production wires this
/// to the real filesystem, real `mise`/`asdf` shell-out, and `std::env::var`.
pub(crate) trait Env {
    /// Read a file's contents as UTF-8, returning `None` if it does not exist
    /// or cannot be read/decoded.
    fn read_file(&self, path: &Path) -> Option<String>;

    /// Returns `true` if the named resolver (`"mise"` / `"asdf"`) is on `PATH`.
    fn resolver_available(&self, resolver: &str) -> bool;

    /// Resolve a declared `(tool, version)` to its install bin directory using
    /// the given resolver. Returns the bin dir only if the resolver succeeds
    /// *and* the directory exists on disk. `None` otherwise (unresolved or
    /// phantom path — we never prepend a path that does not exist).
    fn resolve_bin_dir(&self, resolver: &str, decl: &Declaration) -> Option<PathBuf>;

    /// Read an environment variable (used for the feature gate and current
    /// `PATH`).
    fn var(&self, key: &str) -> Option<String>;
}

/// Returns `true` if the feature gate is enabled for this process/session.
pub(crate) fn enabled(env: &dyn Env) -> bool {
    match env.var(ENABLE_ENV) {
        Some(value) => matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "on" | "true" | "auto" | "yes"
        ),
        None => false,
    }
}

/// Walk from `start` up to the filesystem root, returning the first directory
/// that contains any recognized version-declaration file. `None` if none is
/// found. This mirrors how `mise`/`asdf` themselves resolve the nearest config.
fn find_declaration_root(env: &dyn Env, start: &Path) -> Option<PathBuf> {
    let mut current = Some(start);
    while let Some(dir) = current {
        for file in [
            TOOL_VERSIONS_FILE,
            MISE_TOML_FILE,
            RUBY_VERSION_FILE,
            NODE_VERSION_FILE,
        ] {
            if env.read_file(&dir.join(file)).is_some() {
                return Some(dir.to_path_buf());
            }
        }
        current = dir.parent();
    }
    None
}

/// Parse every recognized declaration file in `root` into a flat, de-duplicated
/// list of declarations (first declaration of a given tool wins). Pure string
/// parsing — no resolution, no guessing.
fn parse_declarations(env: &dyn Env, root: &Path) -> Vec<Declaration> {
    let mut out: Vec<Declaration> = Vec::new();
    let mut seen_tools: Vec<String> = Vec::new();

    let mut push = |tool: String, version: String, source: &str| {
        if tool.is_empty() || version.is_empty() {
            return;
        }
        if seen_tools.iter().any(|t| t == &tool) {
            return;
        }
        seen_tools.push(tool.clone());
        out.push(Declaration {
            tool,
            version,
            source_file: source.to_string(),
        });
    };

    // .tool-versions: `tool version [version...]` per line, `#` comments.
    if let Some(contents) = env.read_file(&root.join(TOOL_VERSIONS_FILE)) {
        for line in contents.lines() {
            let line = line.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            let mut parts = line.split_whitespace();
            if let (Some(tool), Some(version)) = (parts.next(), parts.next()) {
                push(tool.to_string(), version.to_string(), TOOL_VERSIONS_FILE);
            }
        }
    }

    // .mise.toml: minimal `[tools]` table parse — `tool = "version"`.
    // We intentionally do NOT pull a full TOML parser in: mise's tool pins are
    // simple `name = "version"` lines, and anything more exotic is left to mise
    // itself (we still resolve via `mise where`).
    if let Some(contents) = env.read_file(&root.join(MISE_TOML_FILE)) {
        let mut in_tools = false;
        for line in contents.lines() {
            let trimmed = line.split('#').next().unwrap_or("").trim();
            if trimmed.is_empty() {
                continue;
            }
            if trimmed.starts_with('[') {
                in_tools = trimmed == "[tools]";
                continue;
            }
            if !in_tools {
                continue;
            }
            if let Some((tool, rest)) = trimmed.split_once('=') {
                let tool = tool.trim();
                let version = rest.trim().trim_matches(['"', '\'']).trim();
                push(tool.to_string(), version.to_string(), MISE_TOML_FILE);
            }
        }
    }

    // .ruby-version / .nvmrc: single bare version, well-known tool.
    if let Some(contents) = env.read_file(&root.join(RUBY_VERSION_FILE)) {
        let version = first_bare_version(&contents);
        push("ruby".to_string(), version, RUBY_VERSION_FILE);
    }
    if let Some(contents) = env.read_file(&root.join(NODE_VERSION_FILE)) {
        let version = first_bare_version(&contents);
        // .nvmrc allows a leading `v` (e.g. `v20.11.0`); strip it for the
        // resolver, which expects a bare version.
        let version = version.trim_start_matches('v').to_string();
        push("node".to_string(), version, NODE_VERSION_FILE);
    }

    out
}

/// First non-empty, non-comment token of a single-version file.
fn first_bare_version(contents: &str) -> String {
    contents
        .lines()
        .map(|line| line.split('#').next().unwrap_or("").trim())
        .find(|line| !line.is_empty())
        .map(|line| line.split_whitespace().next().unwrap_or("").to_string())
        .unwrap_or_default()
}

/// Outcome of a normalization attempt, returned so callers (and tests) can see
/// what happened without parsing logs.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Normalization {
    /// Bin directories to prepend to the child `PATH`, in declaration order.
    pub(crate) prepend_dirs: Vec<PathBuf>,
    /// Declarations that were found but could not be resolved (resolver
    /// missing, or resolved path absent). Kept for diagnostics.
    pub(crate) unresolved: Vec<Declaration>,
}

impl Normalization {
    pub(crate) fn is_noop(&self) -> bool {
        self.prepend_dirs.is_empty()
    }
}

/// Core (host-free) computation: given the effective cwd, detect declarations,
/// resolve them, and return the bin dirs to prepend. Pure over the [`Env`]
/// trait. Returns an empty (no-op) [`Normalization`] when the feature is
/// disabled or no declaration is found.
pub(crate) fn compute(env: &dyn Env, cwd: &Path) -> Normalization {
    let mut result = Normalization::default();
    if !enabled(env) {
        return result;
    }
    let Some(root) = find_declaration_root(env, cwd) else {
        return result;
    };
    let declarations = parse_declarations(env, &root);
    if declarations.is_empty() {
        return result;
    }

    // Prefer mise, then asdf. Resolution is fully delegated; harn never maps a
    // tool name to a version directory itself.
    let resolver = if env.resolver_available("mise") {
        Some("mise")
    } else if env.resolver_available("asdf") {
        Some("asdf")
    } else {
        None
    };

    let Some(resolver) = resolver else {
        // Declarations exist but no resolver is installed — surface a diagnosis,
        // leave PATH untouched.
        for decl in &declarations {
            tracing::info!(
                tool = %decl.tool,
                version = %decl.version,
                source = %decl.source_file,
                "run() toolchain-path: {} {} declared in {} but no mise/asdf on PATH to resolve it; leaving PATH untouched",
                decl.tool,
                decl.version,
                decl.source_file,
            );
        }
        result.unresolved = declarations;
        return result;
    };

    for decl in declarations {
        match env.resolve_bin_dir(resolver, &decl) {
            Some(bin_dir) => {
                tracing::info!(
                    tool = %decl.tool,
                    version = %decl.version,
                    source = %decl.source_file,
                    bin_dir = %bin_dir.display(),
                    "run() toolchain-path: using {} {} from {} ({} bin {})",
                    decl.tool,
                    decl.version,
                    decl.source_file,
                    resolver,
                    bin_dir.display(),
                );
                result.prepend_dirs.push(bin_dir);
            }
            None => {
                tracing::info!(
                    tool = %decl.tool,
                    version = %decl.version,
                    source = %decl.source_file,
                    "run() toolchain-path: {} {} declared in {} but {} could not resolve an existing install; leaving PATH untouched",
                    decl.tool,
                    decl.version,
                    decl.source_file,
                    resolver,
                );
                result.unresolved.push(decl);
            }
        }
    }

    result
}

/// Apply the normalization to a run() child environment map, honoring the
/// [`EnvMode`] semantics:
///
/// - `InheritClean` / `Patch`: the child inherits the parent `PATH` unless the
///   caller already set one in `env`. We compute the base `PATH` (caller's
///   override if present, else the parent's `PATH`) and prepend our dirs.
/// - `Replace`: the child only sees `env`. We prepend to whatever `PATH` the
///   caller supplied in `env`; if none was supplied, prepending would create a
///   `PATH` that omits the system dirs the command likely needs, so we instead
///   only set the toolchain dirs (still correct — the declared interpreter
///   resolves first — and matches the caller's explicit "replace" intent).
///
/// `env` is mutated in place. A no-op normalization leaves it byte-identical.
pub(crate) fn apply_to_env(
    env_map: &mut BTreeMap<String, String>,
    env_mode: EnvMode,
    parent_path: Option<&str>,
    normalization: &Normalization,
) {
    if normalization.is_noop() {
        return;
    }

    // The child's PATH env key is `PATH` on every platform std::process::Command
    // targets (Windows env keys are case-insensitive, but std normalizes the
    // common name). The caller's env map, however, may carry a differently-cased
    // key (e.g. `Path` on Windows) — find it case-insensitively so we prepend to
    // the base the child would actually see instead of leaving a stale duplicate.
    let caller_path_key = find_path_key(env_map);
    let caller_path = caller_path_key
        .as_deref()
        .and_then(|key| env_map.get(key).cloned());

    // Determine the base PATH the child would otherwise see.
    let base = match env_mode {
        EnvMode::Replace => caller_path,
        EnvMode::InheritClean | EnvMode::Patch => {
            caller_path.or_else(|| parent_path.map(str::to_string))
        }
    };

    // Build the new PATH with the platform-correct separator/quoting rules via
    // std, so Windows gets `;`-joined entries and unix gets `:`-joined entries
    // without hardcoding either. `join_paths` only fails if an entry itself
    // contains the platform separator (e.g. a `;` inside a Windows path), which
    // a resolver-reported install dir will not; if it ever does we fall back to
    // the un-prefixed base rather than corrupting PATH.
    let mut entries: Vec<PathBuf> = normalization.prepend_dirs.clone();
    if let Some(base) = base.as_deref().filter(|b| !b.is_empty()) {
        entries.extend(std::env::split_paths(base));
    }
    let new_path = match std::env::join_paths(entries.iter().map(|p| p.as_os_str())) {
        Ok(joined) => joined.to_string_lossy().into_owned(),
        Err(_) => match base {
            Some(base) if !base.is_empty() => base,
            _ => return,
        },
    };

    // Reuse the caller's existing key casing when present so we overwrite it in
    // place (avoiding a `Path` + `PATH` duplicate on Windows); otherwise use the
    // canonical `PATH`.
    let key = caller_path_key.unwrap_or_else(|| "PATH".to_string());
    env_map.insert(key, new_path);
}

/// Find the PATH key in `env_map`, matching case-insensitively on Windows (where
/// `Path` and `PATH` are the same variable) and exactly elsewhere. Returns the
/// key as it is actually stored so the caller can update it in place.
fn find_path_key(env_map: &BTreeMap<String, String>) -> Option<String> {
    if env_map.contains_key("PATH") {
        return Some("PATH".to_string());
    }
    if cfg!(windows) {
        return env_map
            .keys()
            .find(|k| k.eq_ignore_ascii_case("PATH"))
            .cloned();
    }
    None
}

/// Production [`Env`] implementation: real filesystem, real `mise`/`asdf`
/// shell-out via `which`-style PATH probing, real `std::env`.
pub(crate) struct RealEnv;

impl Env for RealEnv {
    fn read_file(&self, path: &Path) -> Option<String> {
        std::fs::read_to_string(path).ok()
    }

    fn resolver_available(&self, resolver: &str) -> bool {
        which_on_path(resolver).is_some()
    }

    fn resolve_bin_dir(&self, resolver: &str, decl: &Declaration) -> Option<PathBuf> {
        let resolver_bin = which_on_path(resolver)?;
        let tool_arg = format!("{}@{}", decl.tool, decl.version);
        // mise: `mise where ruby@3.2` -> install dir; bin is `<dir>/bin`.
        // asdf: `asdf where ruby 3.2` -> install dir; bin is `<dir>/bin`.
        let output = match resolver {
            "mise" => std::process::Command::new(&resolver_bin)
                .arg("where")
                .arg(&tool_arg)
                .output()
                .ok()?,
            "asdf" => std::process::Command::new(&resolver_bin)
                .arg("where")
                .arg(&decl.tool)
                .arg(&decl.version)
                .output()
                .ok()?,
            _ => return None,
        };
        if !output.status.success() {
            return None;
        }
        let install_dir = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if install_dir.is_empty() {
            return None;
        }
        let bin_dir = PathBuf::from(install_dir).join("bin");
        // Never prepend a phantom path: only return it if it exists on disk.
        if bin_dir.is_dir() {
            Some(bin_dir)
        } else {
            None
        }
    }

    fn var(&self, key: &str) -> Option<String> {
        std::env::var(key).ok()
    }
}

/// Minimal `which`: probe each `PATH` entry for an executable named `name`.
/// Avoids a new crate dependency for a single resolver-presence check.
fn which_on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
        #[cfg(windows)]
        {
            let exe = dir.join(format!("{name}.exe"));
            if exe.is_file() {
                return Some(exe);
            }
        }
    }
    None
}

/// Convenience entry point used by the spawn sites: compute + apply against the
/// real host in one call. `cwd` is the command's effective working directory
/// (the run() `cwd` arg, or the process cwd when none was supplied).
pub(crate) fn normalize_child_env(
    cwd: &Path,
    env_map: &mut BTreeMap<String, String>,
    env_mode: EnvMode,
) {
    let env = RealEnv;
    if !enabled(&env) {
        return;
    }
    let normalization = compute(&env, cwd);
    let parent_path = std::env::var("PATH").ok();
    apply_to_env(env_map, env_mode, parent_path.as_deref(), &normalization);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;

    /// Deterministic fake host. `files` maps absolute paths to contents;
    /// `resolvers` lists installed resolvers; `bins` maps
    /// `"<resolver>:<tool>@<version>"` to a resolved bin dir.
    #[derive(Default)]
    struct FakeEnv {
        files: HashMap<PathBuf, String>,
        resolvers: Vec<String>,
        bins: HashMap<String, PathBuf>,
        vars: HashMap<String, String>,
        // Records resolver lookups so tests can assert delegation happened.
        resolve_calls: RefCell<Vec<String>>,
    }

    impl FakeEnv {
        fn with_gate_on() -> Self {
            let mut env = FakeEnv::default();
            env.vars.insert(ENABLE_ENV.to_string(), "1".to_string());
            env
        }
        fn file(mut self, path: &str, contents: &str) -> Self {
            self.files.insert(PathBuf::from(path), contents.to_string());
            self
        }
        fn resolver(mut self, name: &str) -> Self {
            self.resolvers.push(name.to_string());
            self
        }
        fn bin(mut self, key: &str, dir: &str) -> Self {
            self.bins.insert(key.to_string(), PathBuf::from(dir));
            self
        }
    }

    impl Env for FakeEnv {
        fn read_file(&self, path: &Path) -> Option<String> {
            self.files.get(path).cloned()
        }
        fn resolver_available(&self, resolver: &str) -> bool {
            self.resolvers.iter().any(|r| r == resolver)
        }
        fn resolve_bin_dir(&self, resolver: &str, decl: &Declaration) -> Option<PathBuf> {
            let key = format!("{resolver}:{}@{}", decl.tool, decl.version);
            self.resolve_calls.borrow_mut().push(key.clone());
            self.bins.get(&key).cloned()
        }
        fn var(&self, key: &str) -> Option<String> {
            self.vars.get(key).cloned()
        }
    }

    #[test]
    fn no_declaration_is_a_byte_for_byte_noop() {
        let env = FakeEnv::with_gate_on().resolver("mise");
        let norm = compute(&env, Path::new("/work/clean-repo"));
        assert!(norm.is_noop());
        assert!(norm.prepend_dirs.is_empty());
        assert!(norm.unresolved.is_empty());

        // And applying it leaves the env map untouched.
        let mut env_map: BTreeMap<String, String> = BTreeMap::new();
        env_map.insert("FOO".to_string(), "bar".to_string());
        let before = env_map.clone();
        apply_to_env(
            &mut env_map,
            EnvMode::InheritClean,
            Some("/usr/bin:/bin"),
            &norm,
        );
        assert_eq!(env_map, before, "no declaration must not touch PATH");
        assert!(!env_map.contains_key("PATH"));
    }

    #[test]
    fn disabled_gate_is_a_noop_even_with_declaration() {
        // Gate off (default): a declared+installed tool must still no-op.
        let env = FakeEnv::default()
            .file("/work/app/.tool-versions", "ruby 3.2.2\n")
            .resolver("mise")
            .bin("mise:ruby@3.2.2", "/opt/mise/ruby/3.2.2");
        let norm = compute(&env, Path::new("/work/app"));
        assert!(norm.is_noop());
        assert!(env.resolve_calls.borrow().is_empty());
    }

    /// Join PATH components with the platform separator (`;` on Windows, `:` on
    /// unix) via the same `std::env::join_paths` that `apply_to_env` uses, so
    /// fixtures and expectations match the child PATH on every OS instead of
    /// hardcoding a unix `:` that Windows treats as part of a single entry.
    fn join_path_str(parts: &[&str]) -> String {
        std::env::join_paths(parts.iter().map(std::ffi::OsStr::new))
            .expect("clean ASCII path components join")
            .into_string()
            .expect("ASCII path joins to valid UTF-8")
    }

    #[test]
    fn declared_and_installed_prepends_resolved_bin_dir() {
        let env = FakeEnv::with_gate_on()
            .file("/work/app/.tool-versions", "ruby 3.2.2\nnodejs 20.11.0\n")
            .resolver("mise")
            .bin("mise:ruby@3.2.2", "/opt/mise/installs/ruby/3.2.2")
            .bin("mise:nodejs@20.11.0", "/opt/mise/installs/node/20.11.0");
        let norm = compute(&env, Path::new("/work/app"));
        assert_eq!(
            norm.prepend_dirs,
            vec![
                PathBuf::from("/opt/mise/installs/ruby/3.2.2"),
                PathBuf::from("/opt/mise/installs/node/20.11.0"),
            ]
        );
        assert!(norm.unresolved.is_empty());

        let mut env_map = BTreeMap::new();
        let base = join_path_str(&["/usr/bin", "/bin"]);
        apply_to_env(
            &mut env_map,
            EnvMode::InheritClean,
            Some(base.as_str()),
            &norm,
        );
        let expected = join_path_str(&[
            "/opt/mise/installs/ruby/3.2.2",
            "/opt/mise/installs/node/20.11.0",
            "/usr/bin",
            "/bin",
        ]);
        assert_eq!(
            env_map.get("PATH").map(String::as_str),
            Some(expected.as_str())
        );
    }

    #[test]
    fn declared_but_not_installed_is_noop_with_diagnostic() {
        // Declared, resolver present, but resolver returns no install dir
        // (e.g. version not installed). PATH must be left untouched and the
        // declaration recorded as unresolved.
        let env = FakeEnv::with_gate_on()
            .file("/work/app/.ruby-version", "3.2.2\n")
            .resolver("mise");
        let norm = compute(&env, Path::new("/work/app"));
        assert!(norm.is_noop(), "phantom path must not be prepended");
        assert_eq!(
            norm.unresolved,
            vec![Declaration {
                tool: "ruby".to_string(),
                version: "3.2.2".to_string(),
                source_file: ".ruby-version".to_string(),
            }]
        );
        // Delegation actually happened.
        assert_eq!(
            env.resolve_calls.borrow().as_slice(),
            &["mise:ruby@3.2.2".to_string()]
        );

        let mut env_map = BTreeMap::new();
        apply_to_env(&mut env_map, EnvMode::Patch, Some("/usr/bin"), &norm);
        assert!(!env_map.contains_key("PATH"));
    }

    #[test]
    fn no_resolver_installed_is_noop_with_diagnostic() {
        let env = FakeEnv::with_gate_on().file("/work/app/.nvmrc", "v20.11.0\n");
        let norm = compute(&env, Path::new("/work/app"));
        assert!(norm.is_noop());
        assert_eq!(norm.unresolved.len(), 1);
        assert_eq!(norm.unresolved[0].tool, "node");
        assert_eq!(norm.unresolved[0].version, "20.11.0", "nvmrc `v` stripped");
        // Never attempted resolution because no resolver is present.
        assert!(env.resolve_calls.borrow().is_empty());
    }

    #[test]
    fn walks_up_to_find_declaration_root() {
        let env = FakeEnv::with_gate_on()
            .file("/work/app/.tool-versions", "ruby 3.2.2\n")
            .resolver("mise")
            .bin("mise:ruby@3.2.2", "/opt/ruby/3.2.2");
        // cwd is a nested subdirectory; declaration lives at the repo root.
        let norm = compute(&env, Path::new("/work/app/lib/deep/nested"));
        assert_eq!(norm.prepend_dirs, vec![PathBuf::from("/opt/ruby/3.2.2")]);
    }

    #[test]
    fn asdf_used_when_mise_absent() {
        let env = FakeEnv::with_gate_on()
            .file("/work/app/.tool-versions", "ruby 3.2.2\n")
            .resolver("asdf")
            .bin("asdf:ruby@3.2.2", "/home/u/.asdf/installs/ruby/3.2.2");
        let norm = compute(&env, Path::new("/work/app"));
        assert_eq!(
            norm.prepend_dirs,
            vec![PathBuf::from("/home/u/.asdf/installs/ruby/3.2.2")]
        );
        assert_eq!(
            env.resolve_calls.borrow().as_slice(),
            &["asdf:ruby@3.2.2".to_string()]
        );
    }

    #[test]
    fn mise_toml_tools_table_is_parsed() {
        let toml =
            "[settings]\nexperimental = true\n\n[tools]\nruby = \"3.2.2\"\nnode = '20.11.0'\n";
        let env = FakeEnv::with_gate_on()
            .file("/work/app/.mise.toml", toml)
            .resolver("mise")
            .bin("mise:ruby@3.2.2", "/opt/ruby/3.2.2")
            .bin("mise:node@20.11.0", "/opt/node/20.11.0");
        let norm = compute(&env, Path::new("/work/app"));
        assert_eq!(
            norm.prepend_dirs,
            vec![
                PathBuf::from("/opt/ruby/3.2.2"),
                PathBuf::from("/opt/node/20.11.0"),
            ]
        );
    }

    #[test]
    fn replace_mode_with_caller_path_prepends() {
        let norm = Normalization {
            prepend_dirs: vec![PathBuf::from("/opt/ruby/bin")],
            unresolved: vec![],
        };
        let mut env_map = BTreeMap::new();
        env_map.insert("PATH".to_string(), "/sbin".to_string());
        apply_to_env(
            &mut env_map,
            EnvMode::Replace,
            Some("/parent/ignored"),
            &norm,
        );
        // Replace mode ignores the parent PATH; prepends to the caller's.
        let expected = join_path_str(&["/opt/ruby/bin", "/sbin"]);
        assert_eq!(
            env_map.get("PATH").map(String::as_str),
            Some(expected.as_str())
        );
    }

    #[test]
    fn replace_mode_without_caller_path_sets_only_toolchain() {
        let norm = Normalization {
            prepend_dirs: vec![PathBuf::from("/opt/ruby/bin")],
            unresolved: vec![],
        };
        let mut env_map = BTreeMap::new();
        apply_to_env(
            &mut env_map,
            EnvMode::Replace,
            Some("/parent/ignored"),
            &norm,
        );
        assert_eq!(
            env_map.get("PATH").map(String::as_str),
            Some("/opt/ruby/bin")
        );
    }

    #[test]
    fn caller_path_override_wins_over_parent_in_inherit_mode() {
        let norm = Normalization {
            prepend_dirs: vec![PathBuf::from("/opt/ruby/bin")],
            unresolved: vec![],
        };
        let mut env_map = BTreeMap::new();
        env_map.insert("PATH".to_string(), "/caller/path".to_string());
        apply_to_env(&mut env_map, EnvMode::Patch, Some("/parent/path"), &norm);
        let expected = join_path_str(&["/opt/ruby/bin", "/caller/path"]);
        assert_eq!(
            env_map.get("PATH").map(String::as_str),
            Some(expected.as_str())
        );
    }

    #[test]
    fn tool_versions_comments_and_blank_lines_ignored() {
        let contents = "# comment\n\nruby 3.2.2 # inline\n  \npython 3.11.0\n";
        let env = FakeEnv::with_gate_on()
            .file("/r/.tool-versions", contents)
            .resolver("mise")
            .bin("mise:ruby@3.2.2", "/opt/ruby/3.2.2")
            .bin("mise:python@3.11.0", "/opt/python/3.11.0");
        let norm = compute(&env, Path::new("/r"));
        assert_eq!(
            norm.prepend_dirs,
            vec![
                PathBuf::from("/opt/ruby/3.2.2"),
                PathBuf::from("/opt/python/3.11.0"),
            ]
        );
    }
}
