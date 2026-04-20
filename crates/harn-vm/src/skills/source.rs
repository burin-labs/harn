//! `SkillSource` trait + concrete filesystem / host implementations.
//!
//! A `SkillSource` is anything that can enumerate skills (metadata-only)
//! and fetch a fully-populated [`Skill`] on demand. The layered
//! discovery code ([`super::discovery::LayeredDiscovery`]) stacks
//! multiple sources on top of each other — filesystem walks for
//! `--skill-dir`, `$HARN_SKILLS_PATH`, `.harn/skills/`, `harn.toml`,
//! `~/.harn/skills`, `.harn/packages/**/skills`, `/etc/harn/skills`,
//! `$XDG_CONFIG_HOME/harn/skills`, plus a host-backed source for
//! bridge-mode runs. Each layer tags every manifest with the layer
//! label so higher-priority layers can shadow lower ones cleanly and
//! `harn doctor` can report where each skill came from.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::frontmatter::{parse_frontmatter, split_frontmatter, SkillManifest};

/// A single layer label. Top-level layer numbering matches the priority
/// table in the spec: `Cli` (1) wins over `Env` (2) which wins over
/// `Project` (3) and so on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Layer {
    Cli,
    Env,
    Project,
    Manifest,
    User,
    Package,
    System,
    Host,
}

impl Layer {
    pub fn label(self) -> &'static str {
        match self {
            Layer::Cli => "cli",
            Layer::Env => "env",
            Layer::Project => "project",
            Layer::Manifest => "manifest",
            Layer::User => "user",
            Layer::Package => "package",
            Layer::System => "system",
            Layer::Host => "host",
        }
    }

    pub fn from_label(label: &str) -> Option<Layer> {
        match label {
            "cli" => Some(Layer::Cli),
            "env" => Some(Layer::Env),
            "project" => Some(Layer::Project),
            "manifest" => Some(Layer::Manifest),
            "user" => Some(Layer::User),
            "package" => Some(Layer::Package),
            "system" => Some(Layer::System),
            "host" => Some(Layer::Host),
            _ => None,
        }
    }

    pub const fn all() -> &'static [Layer] {
        &[
            Layer::Cli,
            Layer::Env,
            Layer::Project,
            Layer::Manifest,
            Layer::User,
            Layer::Package,
            Layer::System,
            Layer::Host,
        ]
    }
}

/// The fully loaded form of a skill: manifest + markdown body + context
/// needed to substitute `${HARN_SKILL_DIR}` and surface diagnostics.
#[derive(Debug, Clone)]
pub struct Skill {
    pub manifest: SkillManifest,
    /// SKILL.md body after the closing frontmatter delimiter. Not yet
    /// substituted — callers apply [`super::substitute::substitute_skill_body`]
    /// at invocation time so per-run args / session ids can vary.
    pub body: String,
    /// Absolute directory the SKILL.md lives in. `None` for host-provided
    /// skills where the host owns the underlying storage.
    pub skill_dir: Option<PathBuf>,
    /// Which layer produced this skill.
    pub layer: Layer,
    /// If set, points to the fully-qualified skill id (e.g. `acme/ops`).
    pub namespace: Option<String>,
    /// Field names found in the frontmatter but not recognized by the
    /// current build. Displayed as warnings by `harn doctor`.
    pub unknown_fields: Vec<String>,
}

impl Skill {
    /// `"<namespace>/<name>"` when the skill has a namespace, otherwise
    /// just `name`. This is the key layered discovery uses for collision
    /// detection.
    pub fn id(&self) -> String {
        match &self.namespace {
            Some(ns) if !ns.is_empty() => format!("{ns}/{}", self.manifest.name),
            _ => self.manifest.name.clone(),
        }
    }
}

/// Abstract skill source. Implementations are [`Send`] so we can hand
/// them to async code paths in the future; today everything is sync.
pub trait SkillSource: Send + Sync {
    /// Enumerate skills without loading bodies. Callers use this to
    /// produce the shadowing table before paying to read every file.
    fn list(&self) -> Vec<SkillManifestRef>;

    /// Load a specific skill by id. Must be deterministic for the id
    /// returned by `list()`.
    fn fetch(&self, id: &str) -> Result<Skill, String>;

    /// Layer this source represents. Used for shadowing + provenance.
    fn layer(&self) -> Layer;

    /// Human-readable label for diagnostics (e.g. the root directory).
    fn describe(&self) -> String;
}

/// Light-weight handle returned by `list()` so callers can decide which
/// layer wins before re-reading the SKILL.md.
#[derive(Debug, Clone)]
pub struct SkillManifestRef {
    pub id: String,
    pub manifest: SkillManifest,
    pub layer: Layer,
    pub namespace: Option<String>,
    pub origin: String,
    pub unknown_fields: Vec<String>,
}

/// Filesystem source — walks one root directory looking for
/// `SKILL.md` files two levels deep (`<root>/<name>/SKILL.md`) or a
/// single flat file (`<root>/SKILL.md` when `<root>` itself is the
/// skill dir). The single-root shape keeps CLI `--skill-dir`
/// behavior predictable; users who want multi-root share-pools layer
/// them via the manifest `[skills] paths`.
#[derive(Debug, Clone)]
pub struct FsSkillSource {
    pub root: PathBuf,
    pub layer: Layer,
    /// Optional namespace prefix. When set, every discovered skill is
    /// registered as `<namespace>/<name>` and shadowing only happens on
    /// the fully-qualified id. Powers the `[[skill.source]] name =
    /// "acme/ops"` escape hatch for multi-tenant setups.
    pub namespace: Option<String>,
}

impl FsSkillSource {
    pub fn new(root: impl Into<PathBuf>, layer: Layer) -> Self {
        Self {
            root: root.into(),
            layer,
            namespace: None,
        }
    }

    pub fn with_namespace(mut self, namespace: impl Into<String>) -> Self {
        let ns = namespace.into();
        self.namespace = if ns.is_empty() { None } else { Some(ns) };
        self
    }

    fn iter_skill_dirs(&self) -> Vec<PathBuf> {
        let mut results = Vec::new();
        if !self.root.is_dir() {
            return results;
        }
        // Accept `<root>/SKILL.md` as a single-skill bundle (unusual but
        // convenient for `--skill-dir /path/to/one-skill`).
        if self.root.join("SKILL.md").is_file() {
            results.push(self.root.clone());
            return results;
        }
        // Otherwise walk one level deep.
        let Ok(entries) = fs::read_dir(&self.root) else {
            return results;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            if path.join("SKILL.md").is_file() {
                results.push(path);
            }
        }
        results.sort();
        results
    }

    fn finalize_manifest(
        &self,
        dir: &Path,
        skill_file: &Path,
        manifest: &mut SkillManifest,
    ) -> Result<(), String> {
        if manifest.name.is_empty() {
            if let Some(name) = dir.file_name().and_then(|n| n.to_str()) {
                manifest.name = name.to_string();
            }
        }
        if manifest.name.is_empty() {
            return Err(format!(
                "{}: SKILL.md has no `name` field and directory has no basename",
                skill_file.display()
            ));
        }
        if manifest.short.trim().is_empty() {
            return Err(format!(
                "{}: SKILL.md requires a non-empty `short` field",
                skill_file.display()
            ));
        }
        Ok(())
    }

    fn load_manifest_from_dir(&self, dir: &Path) -> Result<SkillManifestRef, String> {
        let skill_file = dir.join("SKILL.md");
        let source = fs::read_to_string(&skill_file)
            .map_err(|e| format!("failed to read {}: {e}", skill_file.display()))?;
        let (fm, _) = split_frontmatter(&source);
        let parsed = parse_frontmatter(fm).map_err(|e| format!("{}: {e}", skill_file.display()))?;
        let mut manifest = parsed.manifest;
        self.finalize_manifest(dir, &skill_file, &mut manifest)?;
        let id = match &self.namespace {
            Some(ns) if !ns.is_empty() => format!("{ns}/{}", manifest.name),
            _ => manifest.name.clone(),
        };
        Ok(SkillManifestRef {
            id,
            manifest,
            layer: self.layer,
            namespace: self.namespace.clone(),
            origin: dir.display().to_string(),
            unknown_fields: parsed.unknown_fields,
        })
    }

    fn load_from_dir(&self, dir: &Path) -> Result<Skill, String> {
        let skill_file = dir.join("SKILL.md");
        let source = fs::read_to_string(&skill_file)
            .map_err(|e| format!("failed to read {}: {e}", skill_file.display()))?;
        let (fm, body) = split_frontmatter(&source);
        let parsed = parse_frontmatter(fm).map_err(|e| format!("{}: {e}", skill_file.display()))?;
        let mut manifest = parsed.manifest;
        self.finalize_manifest(dir, &skill_file, &mut manifest)?;
        let skill = Skill {
            body: body.to_string(),
            skill_dir: Some(dir.to_path_buf()),
            layer: self.layer,
            namespace: self.namespace.clone(),
            unknown_fields: parsed.unknown_fields,
            manifest,
        };
        Ok(skill)
    }
}

impl SkillSource for FsSkillSource {
    fn list(&self) -> Vec<SkillManifestRef> {
        let mut out = Vec::new();
        for dir in self.iter_skill_dirs() {
            match self.load_manifest_from_dir(&dir) {
                Ok(skill) => {
                    out.push(skill);
                }
                Err(err) => {
                    eprintln!("warning: skills: {err}");
                }
            }
        }
        out
    }

    fn fetch(&self, id: &str) -> Result<Skill, String> {
        let target_name = match id.rsplit_once('/') {
            Some((_, n)) => n,
            None => id,
        };
        for dir in self.iter_skill_dirs() {
            let skill = self.load_from_dir(&dir)?;
            if skill.id() == id || skill.manifest.name == target_name {
                return Ok(skill);
            }
        }
        Err(format!(
            "skill '{id}' not found under {}",
            self.root.display()
        ))
    }

    fn layer(&self) -> Layer {
        self.layer
    }

    fn describe(&self) -> String {
        match &self.namespace {
            Some(ns) => format!("{} [{}] ns={ns}", self.root.display(), self.layer.label()),
            None => format!("{} [{}]", self.root.display(), self.layer.label()),
        }
    }
}

/// Callable the bridge adapter hands to [`HostSkillSource`] to
/// enumerate skills via `skills/list`.
pub type HostSkillLister = Arc<dyn Fn() -> Vec<SkillManifestRef> + Send + Sync>;

/// Callable the bridge adapter hands to [`HostSkillSource`] to fetch
/// one skill via `skills/fetch`.
pub type HostSkillFetcher = Arc<dyn Fn(&str) -> Result<Skill, String> + Send + Sync>;

/// Bridge-backed skill source. Calls the `skills/list` / `skills/fetch`
/// RPCs defined in `crates/harn-vm/src/bridge.rs` so a host can expose
/// its own managed skill store to the VM.
pub struct HostSkillSource {
    loader: HostSkillLister,
    fetcher: HostSkillFetcher,
}

impl HostSkillSource {
    pub fn new<L, F>(loader: L, fetcher: F) -> Self
    where
        L: Fn() -> Vec<SkillManifestRef> + Send + Sync + 'static,
        F: Fn(&str) -> Result<Skill, String> + Send + Sync + 'static,
    {
        Self {
            loader: Arc::new(loader),
            fetcher: Arc::new(fetcher),
        }
    }
}

impl std::fmt::Debug for HostSkillSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostSkillSource").finish_non_exhaustive()
    }
}

impl SkillSource for HostSkillSource {
    fn list(&self) -> Vec<SkillManifestRef> {
        (self.loader)()
    }

    fn fetch(&self, id: &str) -> Result<Skill, String> {
        (self.fetcher)(id)
    }

    fn layer(&self) -> Layer {
        Layer::Host
    }

    fn describe(&self) -> String {
        "host-provided [host]".to_string()
    }
}

/// Convert a [`Skill`] into the `{_type: "skill_registry", skills: [...]}`
/// dict form used by the existing skill_* VM builtins. Returns the entry
/// dict only — callers assemble the outer registry.
pub fn skill_entry_to_vm(skill: &Skill) -> crate::value::VmValue {
    use crate::value::VmValue;
    use std::rc::Rc;

    let mut entry: BTreeMap<String, VmValue> = BTreeMap::new();
    entry.insert(
        "name".to_string(),
        VmValue::String(Rc::from(skill.manifest.name.as_str())),
    );
    entry.insert(
        "short".to_string(),
        VmValue::String(Rc::from(skill.manifest.short.as_str())),
    );
    entry.insert(
        "description".to_string(),
        VmValue::String(Rc::from(if skill.manifest.description.is_empty() {
            skill.manifest.short.as_str()
        } else {
            skill.manifest.description.as_str()
        })),
    );
    if let Some(when) = &skill.manifest.when_to_use {
        entry.insert(
            "when_to_use".to_string(),
            VmValue::String(Rc::from(when.as_str())),
        );
    }
    if skill.manifest.disable_model_invocation {
        entry.insert("disable_model_invocation".to_string(), VmValue::Bool(true));
    }
    if !skill.manifest.allowed_tools.is_empty() {
        entry.insert(
            "allowed_tools".to_string(),
            VmValue::List(Rc::new(
                skill
                    .manifest
                    .allowed_tools
                    .iter()
                    .map(|t| VmValue::String(Rc::from(t.as_str())))
                    .collect(),
            )),
        );
    }
    if skill.manifest.user_invocable {
        entry.insert("user_invocable".to_string(), VmValue::Bool(true));
    }
    if !skill.manifest.paths.is_empty() {
        entry.insert(
            "paths".to_string(),
            VmValue::List(Rc::new(
                skill
                    .manifest
                    .paths
                    .iter()
                    .map(|p| VmValue::String(Rc::from(p.as_str())))
                    .collect(),
            )),
        );
    }
    if let Some(context) = &skill.manifest.context {
        entry.insert(
            "context".to_string(),
            VmValue::String(Rc::from(context.as_str())),
        );
    }
    if let Some(agent) = &skill.manifest.agent {
        entry.insert(
            "agent".to_string(),
            VmValue::String(Rc::from(agent.as_str())),
        );
    }
    if !skill.manifest.hooks.is_empty() {
        let mut hooks: BTreeMap<String, VmValue> = BTreeMap::new();
        for (k, v) in &skill.manifest.hooks {
            hooks.insert(k.clone(), VmValue::String(Rc::from(v.as_str())));
        }
        entry.insert("hooks".to_string(), VmValue::Dict(Rc::new(hooks)));
    }
    if let Some(model) = &skill.manifest.model {
        entry.insert(
            "model".to_string(),
            VmValue::String(Rc::from(model.as_str())),
        );
    }
    if let Some(effort) = &skill.manifest.effort {
        entry.insert(
            "effort".to_string(),
            VmValue::String(Rc::from(effort.as_str())),
        );
    }
    if skill.manifest.require_signature {
        entry.insert("require_signature".to_string(), VmValue::Bool(true));
    }
    if !skill.manifest.trusted_signers.is_empty() {
        entry.insert(
            "trusted_signers".to_string(),
            VmValue::List(Rc::new(
                skill
                    .manifest
                    .trusted_signers
                    .iter()
                    .map(|fingerprint| VmValue::String(Rc::from(fingerprint.as_str())))
                    .collect(),
            )),
        );
    }
    if let Some(shell) = &skill.manifest.shell {
        entry.insert(
            "shell".to_string(),
            VmValue::String(Rc::from(shell.as_str())),
        );
    }
    if let Some(hint) = &skill.manifest.argument_hint {
        entry.insert(
            "argument_hint".to_string(),
            VmValue::String(Rc::from(hint.as_str())),
        );
    }
    entry.insert(
        "body".to_string(),
        VmValue::String(Rc::from(skill.body.as_str())),
    );
    if let Some(dir) = &skill.skill_dir {
        entry.insert(
            "skill_dir".to_string(),
            VmValue::String(Rc::from(dir.display().to_string().as_str())),
        );
    }
    entry.insert(
        "source".to_string(),
        VmValue::String(Rc::from(skill.layer.label())),
    );
    if let Some(ns) = &skill.namespace {
        entry.insert(
            "namespace".to_string(),
            VmValue::String(Rc::from(ns.as_str())),
        );
    }
    VmValue::Dict(Rc::new(entry))
}

pub fn skill_manifest_ref_to_vm(skill: &SkillManifestRef) -> crate::value::VmValue {
    use crate::value::VmValue;
    use std::rc::Rc;

    let mut entry: BTreeMap<String, VmValue> = BTreeMap::new();
    entry.insert(
        "name".to_string(),
        VmValue::String(Rc::from(skill.manifest.name.as_str())),
    );
    entry.insert(
        "short".to_string(),
        VmValue::String(Rc::from(skill.manifest.short.as_str())),
    );
    entry.insert(
        "description".to_string(),
        VmValue::String(Rc::from(if skill.manifest.description.is_empty() {
            skill.manifest.short.as_str()
        } else {
            skill.manifest.description.as_str()
        })),
    );
    if let Some(when) = &skill.manifest.when_to_use {
        entry.insert(
            "when_to_use".to_string(),
            VmValue::String(Rc::from(when.as_str())),
        );
    }
    if skill.manifest.disable_model_invocation {
        entry.insert("disable_model_invocation".to_string(), VmValue::Bool(true));
    }
    if !skill.manifest.allowed_tools.is_empty() {
        entry.insert(
            "allowed_tools".to_string(),
            VmValue::List(Rc::new(
                skill
                    .manifest
                    .allowed_tools
                    .iter()
                    .map(|tool| VmValue::String(Rc::from(tool.as_str())))
                    .collect(),
            )),
        );
    }
    if skill.manifest.user_invocable {
        entry.insert("user_invocable".to_string(), VmValue::Bool(true));
    }
    if !skill.manifest.paths.is_empty() {
        entry.insert(
            "paths".to_string(),
            VmValue::List(Rc::new(
                skill
                    .manifest
                    .paths
                    .iter()
                    .map(|path| VmValue::String(Rc::from(path.as_str())))
                    .collect(),
            )),
        );
    }
    if let Some(context) = &skill.manifest.context {
        entry.insert(
            "context".to_string(),
            VmValue::String(Rc::from(context.as_str())),
        );
    }
    if let Some(agent) = &skill.manifest.agent {
        entry.insert(
            "agent".to_string(),
            VmValue::String(Rc::from(agent.as_str())),
        );
    }
    if !skill.manifest.hooks.is_empty() {
        let mut hooks: BTreeMap<String, VmValue> = BTreeMap::new();
        for (key, value) in &skill.manifest.hooks {
            hooks.insert(key.clone(), VmValue::String(Rc::from(value.as_str())));
        }
        entry.insert("hooks".to_string(), VmValue::Dict(Rc::new(hooks)));
    }
    if let Some(model) = &skill.manifest.model {
        entry.insert(
            "model".to_string(),
            VmValue::String(Rc::from(model.as_str())),
        );
    }
    if let Some(effort) = &skill.manifest.effort {
        entry.insert(
            "effort".to_string(),
            VmValue::String(Rc::from(effort.as_str())),
        );
    }
    if let Some(shell) = &skill.manifest.shell {
        entry.insert(
            "shell".to_string(),
            VmValue::String(Rc::from(shell.as_str())),
        );
    }
    if let Some(hint) = &skill.manifest.argument_hint {
        entry.insert(
            "argument_hint".to_string(),
            VmValue::String(Rc::from(hint.as_str())),
        );
    }
    entry.insert(
        "source".to_string(),
        VmValue::String(Rc::from(skill.layer.label())),
    );
    if let Some(ns) = &skill.namespace {
        entry.insert(
            "namespace".to_string(),
            VmValue::String(Rc::from(ns.as_str())),
        );
    }
    VmValue::Dict(Rc::new(entry))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write(tmp: &Path, rel: &str, body: &str) {
        let p = tmp.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, body).unwrap();
    }

    #[test]
    fn fs_source_walks_one_level_deep() {
        let tmp = tempfile::tempdir().unwrap();
        write(
            tmp.path(),
            "deploy/SKILL.md",
            "---\nname: deploy\nshort: deploy the service\ndescription: ship it\n---\nrun deploy",
        );
        write(
            tmp.path(),
            "review/SKILL.md",
            "---\nname: review\nshort: review a pull request\n---\nbody",
        );
        write(tmp.path(), "not-a-skill.txt", "no");

        let src = FsSkillSource::new(tmp.path(), Layer::Project);
        let listed = src.list();
        assert_eq!(listed.len(), 2);
        let names: Vec<_> = listed.iter().map(|s| s.manifest.name.clone()).collect();
        assert!(names.contains(&"deploy".to_string()));
        assert!(names.contains(&"review".to_string()));

        let skill = src.fetch("deploy").unwrap();
        assert_eq!(skill.manifest.short, "deploy the service");
        assert_eq!(skill.manifest.description, "ship it");
        assert_eq!(skill.body, "run deploy");
    }

    #[test]
    fn fs_source_accepts_root_as_single_skill() {
        let tmp = tempfile::tempdir().unwrap();
        write(
            tmp.path(),
            "SKILL.md",
            "---\nname: solo\nshort: single skill bundle\n---\n(body)",
        );
        let src = FsSkillSource::new(tmp.path(), Layer::Cli);
        let listed = src.list();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].manifest.name, "solo");
    }

    #[test]
    fn fs_source_defaults_name_to_directory() {
        let tmp = tempfile::tempdir().unwrap();
        write(
            tmp.path(),
            "nameless/SKILL.md",
            "---\nshort: fallback to the directory name\n---\nbody only",
        );
        let src = FsSkillSource::new(tmp.path(), Layer::User);
        let skill = src.fetch("nameless").unwrap();
        assert_eq!(skill.manifest.name, "nameless");
    }

    #[test]
    fn fs_source_namespace_prefixes_id() {
        let tmp = tempfile::tempdir().unwrap();
        write(
            tmp.path(),
            "deploy/SKILL.md",
            "---\nname: deploy\nshort: deploy the service\n---\nbody",
        );
        let src = FsSkillSource::new(tmp.path(), Layer::Manifest).with_namespace("acme/ops");
        let listed = src.list();
        assert_eq!(listed[0].id, "acme/ops/deploy");
        let skill = src.fetch("acme/ops/deploy").unwrap();
        assert_eq!(skill.id(), "acme/ops/deploy");
    }

    #[test]
    fn fs_source_missing_root_is_empty_not_error() {
        let src = FsSkillSource::new("/does/not/exist/anywhere", Layer::System);
        assert!(src.list().is_empty());
        assert!(src.fetch("nope").is_err());
    }

    #[test]
    fn fs_source_requires_short_card() {
        let tmp = tempfile::tempdir().unwrap();
        write(
            tmp.path(),
            "broken/SKILL.md",
            "---\nname: broken\n---\nbody",
        );
        let src = FsSkillSource::new(tmp.path(), Layer::Project);
        assert!(src.list().is_empty());
        let err = src.fetch("broken").unwrap_err();
        assert!(err.contains("`short`"), "{err}");
    }

    #[test]
    fn host_source_wraps_closures() {
        let host = HostSkillSource::new(
            || {
                vec![SkillManifestRef {
                    id: "h1".into(),
                    manifest: SkillManifest {
                        name: "h1".into(),
                        short: "host-provided skill".into(),
                        ..Default::default()
                    },
                    layer: Layer::Host,
                    namespace: None,
                    origin: "host".into(),
                    unknown_fields: Vec::new(),
                }]
            },
            |id| {
                Ok(Skill {
                    manifest: SkillManifest {
                        name: id.to_string(),
                        short: "host-provided skill".into(),
                        ..Default::default()
                    },
                    body: "host body".into(),
                    skill_dir: None,
                    layer: Layer::Host,
                    namespace: None,
                    unknown_fields: Vec::new(),
                })
            },
        );
        assert_eq!(host.list().len(), 1);
        let s = host.fetch("h1").unwrap();
        assert_eq!(s.body, "host body");
        assert_eq!(s.layer, Layer::Host);
    }

    #[test]
    fn layer_label_roundtrips() {
        for layer in Layer::all() {
            assert_eq!(Layer::from_label(layer.label()), Some(*layer));
        }
    }
}
