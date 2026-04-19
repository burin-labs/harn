//! Filesystem-and-host skill discovery for Harn.
//!
//! See `docs/src/skills.md` for the user-facing reference. At a glance:
//!
//! - [`frontmatter`] parses SKILL.md YAML frontmatter into
//!   [`SkillManifest`](frontmatter::SkillManifest).
//! - [`source`] defines the [`SkillSource`] trait and the concrete
//!   filesystem / host implementations.
//! - [`discovery`] stacks multiple sources in priority order, handles
//!   name collisions, and reports shadowed skills for `harn doctor`.
//! - [`substitute`] implements the `$ARGUMENTS` / `$N` / `${HARN_*}`
//!   escapes that run over SKILL.md bodies at invocation time.
//!
//! The `default_sources` helper wires together the seven non-host
//! filesystem layers. Hosts add a bridge-backed [`HostSkillSource`]
//! on top.

pub mod discovery;
pub mod frontmatter;
pub mod runtime;
pub mod source;
pub mod substitute;

use std::path::{Path, PathBuf};

pub use discovery::{DiscoveryOptions, DiscoveryReport, LayeredDiscovery, Shadowed};
pub use frontmatter::{parse_frontmatter, split_frontmatter, ParsedFrontmatter, SkillManifest};
pub use runtime::{
    clear_current_skill_registry, current_skill_registry, install_current_skill_registry,
    load_bound_skill_by_name, load_skill_from_registry, resolve_skill_entry, skill_entry_id,
    vm_error as skill_vm_error, BoundSkillRegistry, LoadedSkill, SkillFetcher,
};
pub use source::{
    skill_entry_to_vm, skill_manifest_ref_to_vm, FsSkillSource, HostSkillSource, Layer, Skill,
    SkillManifestRef, SkillSource,
};
pub use substitute::{substitute_skill_body, SubstitutionContext};

/// Inputs controlling the seven non-host filesystem layers.
#[derive(Debug, Clone, Default)]
pub struct FsLayerConfig {
    /// `--skill-dir` paths. First has highest priority, but inside the
    /// CLI layer there is no further ordering — unqualified names
    /// collide and the first one loaded wins.
    pub cli_dirs: Vec<PathBuf>,
    /// `$HARN_SKILLS_PATH` entries in the order they appeared.
    pub env_dirs: Vec<PathBuf>,
    /// Project root (directory holding `.harn/skills/`), if one was
    /// found by walking up from the executing script.
    pub project_root: Option<PathBuf>,
    /// `[skills] paths` entries from harn.toml, pre-resolved to
    /// absolute directories.
    pub manifest_paths: Vec<PathBuf>,
    /// `[[skill.source]]` entries from harn.toml, pre-resolved.
    pub manifest_sources: Vec<ManifestSource>,
    /// `$HOME/.harn/skills` (or the platform equivalent).
    pub user_dir: Option<PathBuf>,
    /// Walk target for `.harn/packages/**/skills/*/SKILL.md`.
    pub packages_dir: Option<PathBuf>,
    /// `/etc/harn/skills` + `$XDG_CONFIG_HOME/harn/skills` combined.
    pub system_dirs: Vec<PathBuf>,
}

/// A `[[skill.source]]` entry resolved to something the VM can load.
/// `fs` and `git` are active today; `registry` is reserved and inert
/// until a marketplace exists (per issue #73).
#[derive(Debug, Clone)]
pub enum ManifestSource {
    Fs {
        path: PathBuf,
        namespace: Option<String>,
    },
    Git {
        path: PathBuf,
        namespace: Option<String>,
    },
}

impl ManifestSource {
    pub fn path(&self) -> &Path {
        match self {
            ManifestSource::Fs { path, .. } | ManifestSource::Git { path, .. } => path,
        }
    }
    pub fn namespace(&self) -> Option<&str> {
        match self {
            ManifestSource::Fs { namespace, .. } | ManifestSource::Git { namespace, .. } => {
                namespace.as_deref()
            }
        }
    }
}

/// Build a [`LayeredDiscovery`] for the seven non-host layers from
/// [`FsLayerConfig`]. Callers extend it with a [`HostSkillSource`] when
/// they have a bridge handle.
pub fn build_fs_discovery(cfg: &FsLayerConfig, options: DiscoveryOptions) -> LayeredDiscovery {
    let mut discovery = LayeredDiscovery::new().with_options(options);

    for path in &cfg.cli_dirs {
        discovery = discovery.push(FsSkillSource::new(path.clone(), Layer::Cli));
    }
    for path in &cfg.env_dirs {
        discovery = discovery.push(FsSkillSource::new(path.clone(), Layer::Env));
    }
    if let Some(root) = &cfg.project_root {
        let proj_skills = root.join(".harn").join("skills");
        if proj_skills.exists() {
            discovery = discovery.push(FsSkillSource::new(proj_skills, Layer::Project));
        }
    }
    for path in &cfg.manifest_paths {
        discovery = discovery.push(FsSkillSource::new(path.clone(), Layer::Manifest));
    }
    for entry in &cfg.manifest_sources {
        let source = FsSkillSource::new(entry.path().to_path_buf(), Layer::Manifest);
        let source = if let Some(ns) = entry.namespace() {
            source.with_namespace(ns)
        } else {
            source
        };
        discovery = discovery.push(source);
    }
    if let Some(path) = &cfg.user_dir {
        if path.exists() {
            discovery = discovery.push(FsSkillSource::new(path.clone(), Layer::User));
        }
    }
    if let Some(root) = &cfg.packages_dir {
        for skills_root in walk_packages_skills(root) {
            discovery = discovery.push(FsSkillSource::new(skills_root, Layer::Package));
        }
    }
    for path in &cfg.system_dirs {
        if path.exists() {
            discovery = discovery.push(FsSkillSource::new(path.clone(), Layer::System));
        }
    }

    discovery
}

/// Walk `<packages>/*/skills` and return each concrete skills root.
/// Does not recurse more than two levels — package authors are expected
/// to place their bundled skills one level deep.
fn walk_packages_skills(packages_dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(packages_dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let pkg_skills = entry.path().join("skills");
        if pkg_skills.is_dir() {
            out.push(pkg_skills);
        }
    }
    out.sort();
    out
}

/// Parse `$HARN_SKILLS_PATH` into absolute directory candidates.
/// The separator is `:` on Unix and `;` on Windows (matches `PATH`).
pub fn parse_env_skills_path(raw: &str) -> Vec<PathBuf> {
    #[cfg(unix)]
    let sep = ':';
    #[cfg(not(unix))]
    let sep = ';';
    raw.split(sep)
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .collect()
}

/// Canonical system-level search paths. We read `$XDG_CONFIG_HOME` with
/// the usual `$HOME/.config` fallback and always include `/etc/harn/skills`
/// on Unix.
pub fn default_system_dirs() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        if !xdg.is_empty() {
            out.push(PathBuf::from(xdg).join("harn").join("skills"));
        }
    } else if let Some(home) = dirs_home() {
        out.push(home.join(".config").join("harn").join("skills"));
    }
    #[cfg(unix)]
    {
        out.push(PathBuf::from("/etc/harn/skills"));
    }
    out
}

/// The conventional user-level skill directory (`~/.harn/skills`).
pub fn default_user_dir() -> Option<PathBuf> {
    dirs_home().map(|h| h.join(".harn").join("skills"))
}

fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from).or_else(|| {
        // Windows fallback without pulling in the `dirs` crate.
        std::env::var_os("USERPROFILE").map(PathBuf::from)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn env_skills_path_parses_and_skips_empties() {
        let raw = if cfg!(unix) {
            "/a/b::/c/d"
        } else {
            "C:\\a\\b;;C:\\c\\d"
        };
        let parsed = parse_env_skills_path(raw);
        assert_eq!(parsed.len(), 2);
    }

    #[test]
    fn default_system_dirs_respects_xdg() {
        let tmp = tempfile::tempdir().unwrap();
        let xdg = tmp.path().to_path_buf();
        // SAFETY for test isolation: each test process has its own env.
        std::env::set_var("XDG_CONFIG_HOME", &xdg);
        let dirs = default_system_dirs();
        assert!(dirs.iter().any(|p| p.starts_with(&xdg)));
        std::env::remove_var("XDG_CONFIG_HOME");
    }

    #[test]
    fn walks_packages_skills_one_level_deep() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir_all(tmp.path().join("pkg-a").join("skills")).unwrap();
        fs::create_dir_all(tmp.path().join("pkg-b").join("skills")).unwrap();
        fs::create_dir_all(tmp.path().join("pkg-c")).unwrap(); // no skills/
        let skills = walk_packages_skills(tmp.path());
        assert_eq!(skills.len(), 2);
    }
}
