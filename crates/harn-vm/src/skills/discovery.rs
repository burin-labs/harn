//! Layered discovery across multiple [`SkillSource`]s.
//!
//! Priority (highest wins):
//!
//! 1. CLI          — `--skill-dir <path>` (repeatable)
//! 2. Env          — `$HARN_SKILLS_PATH` (colon-separated)
//! 3. Project      — `.harn/skills/<name>/SKILL.md` walking up
//! 4. Manifest     — `[skills] paths` + `[[skill.source]]`
//! 5. User         — `~/.harn/skills/<name>/SKILL.md`
//! 6. Package      — `.harn/packages/**/skills/*/SKILL.md`
//! 7. System       — `/etc/harn/skills/` + `$XDG_CONFIG_HOME/harn/skills`
//! 8. Host         — Bridge-registered
//!
//! Unqualified names collide across layers; higher layer wins, the
//! loser is recorded in [`Shadowed`] so `harn doctor` can surface it.
//! Fully-qualified `<namespace>/<skill>` ids bypass collision and
//! live alongside any unqualified name.

use std::collections::BTreeMap;

use super::source::{Layer, Skill, SkillManifestRef, SkillSource};

/// Per-source lookup_order override: `disable` kicks a layer out.
#[derive(Debug, Clone, Default)]
pub struct DiscoveryOptions {
    pub disabled_layers: Vec<Layer>,
    pub lookup_order: Option<Vec<Layer>>,
}

impl DiscoveryOptions {
    pub fn is_enabled(&self, layer: Layer) -> bool {
        !self.disabled_layers.contains(&layer)
    }
}

#[derive(Debug, Clone)]
pub struct Shadowed {
    pub id: String,
    pub winner: Layer,
    pub loser: Layer,
    pub loser_origin: String,
}

#[derive(Debug, Clone)]
pub struct DiscoveryReport {
    pub winners: Vec<SkillManifestRef>,
    pub shadowed: Vec<Shadowed>,
    pub disabled_layers: Vec<Layer>,
    pub unknown_fields: Vec<(String, Vec<String>)>,
}

pub struct LayeredDiscovery {
    sources: Vec<Box<dyn SkillSource>>,
    options: DiscoveryOptions,
}

impl LayeredDiscovery {
    pub fn new() -> Self {
        Self {
            sources: Vec::new(),
            options: DiscoveryOptions::default(),
        }
    }

    pub fn with_options(mut self, options: DiscoveryOptions) -> Self {
        self.options = options;
        self
    }

    pub fn push<S: SkillSource + 'static>(mut self, source: S) -> Self {
        self.sources.push(Box::new(source));
        self
    }

    pub fn push_boxed(&mut self, source: Box<dyn SkillSource>) {
        self.sources.push(source);
    }

    pub fn sources(&self) -> &[Box<dyn SkillSource>] {
        &self.sources
    }

    pub fn options(&self) -> &DiscoveryOptions {
        &self.options
    }

    /// Produce the shadowing table — each id maps to the winning layer
    /// and every shadowed competitor is recorded. When `options.lookup_order`
    /// is set, layers outside the list fall through to their default
    /// priority but with lower precedence than explicitly-listed layers.
    pub fn build_report(&self) -> DiscoveryReport {
        let mut by_layer: BTreeMap<Layer, Vec<SkillManifestRef>> = BTreeMap::new();
        for source in &self.sources {
            let layer = source.layer();
            if !self.options.is_enabled(layer) {
                continue;
            }
            by_layer.entry(layer).or_default().extend(source.list());
        }

        let order: Vec<Layer> = if let Some(explicit) = &self.options.lookup_order {
            let mut seen = std::collections::BTreeSet::new();
            let mut result: Vec<Layer> = Vec::new();
            for layer in explicit {
                if !self.options.is_enabled(*layer) {
                    continue;
                }
                if seen.insert(*layer) {
                    result.push(*layer);
                }
            }
            for layer in Layer::all() {
                if !self.options.is_enabled(*layer) {
                    continue;
                }
                if seen.insert(*layer) {
                    result.push(*layer);
                }
            }
            result
        } else {
            Layer::all()
                .iter()
                .copied()
                .filter(|l| self.options.is_enabled(*l))
                .collect()
        };

        let mut winners_by_id: BTreeMap<String, SkillManifestRef> = BTreeMap::new();
        let mut shadowed: Vec<Shadowed> = Vec::new();
        let mut unknown_fields: Vec<(String, Vec<String>)> = Vec::new();

        for layer in &order {
            let Some(refs) = by_layer.get(layer) else {
                continue;
            };
            for m in refs {
                let existing = winners_by_id.get(&m.id).cloned();
                match existing {
                    None => {
                        winners_by_id.insert(m.id.clone(), m.clone());
                    }
                    Some(existing) => {
                        // Winner is whichever layer is earlier in `order`. If the
                        // incumbent was resolved earlier, the newcomer loses; we
                        // record the shadow.
                        shadowed.push(Shadowed {
                            id: m.id.clone(),
                            winner: existing.layer,
                            loser: m.layer,
                            loser_origin: m.origin.clone(),
                        });
                    }
                }
            }
        }

        let mut winners: Vec<SkillManifestRef> = winners_by_id.into_values().collect();
        winners.sort_by(|a, b| a.id.cmp(&b.id));

        for winner in &winners {
            if !winner.unknown_fields.is_empty() {
                unknown_fields.push((winner.id.clone(), winner.unknown_fields.clone()));
            }
        }

        DiscoveryReport {
            winners,
            shadowed,
            disabled_layers: self.options.disabled_layers.clone(),
            unknown_fields,
        }
    }

    /// Resolve a skill id to its winning [`Skill`], using the current
    /// discovery report to short-circuit lookups at the correct layer.
    pub fn fetch(&self, id: &str) -> Result<Skill, String> {
        let report = self.build_report();
        self.fetch_impl(id, &report.winners)
    }

    fn fetch_impl(&self, id: &str, winners: &[SkillManifestRef]) -> Result<Skill, String> {
        let target = winners
            .iter()
            .find(|m| m.id == id)
            .ok_or_else(|| format!("skill '{id}' not found"))?;
        for source in &self.sources {
            if source.layer() != target.layer {
                continue;
            }
            if let Ok(skill) = source.fetch(id) {
                if skill.id() == id {
                    return Ok(skill);
                }
            }
        }
        Err(format!("skill '{id}' not found"))
    }
}

impl Default for LayeredDiscovery {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skills::frontmatter::SkillManifest;
    use crate::skills::source::{FsSkillSource, HostSkillSource};
    use std::fs;

    fn write_skill(root: &std::path::Path, sub: &str, name: &str, desc: &str) {
        let dir = root.join(sub);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("SKILL.md"),
            format!("---\nname: {name}\nshort: {desc}\ndescription: {desc}\n---\nbody of {name}"),
        )
        .unwrap();
    }

    #[test]
    fn cli_layer_shadows_user_layer() {
        let cli_root = tempfile::tempdir().unwrap();
        let user_root = tempfile::tempdir().unwrap();
        write_skill(cli_root.path(), "deploy", "deploy", "cli version");
        write_skill(user_root.path(), "deploy", "deploy", "user version");
        write_skill(user_root.path(), "review", "review", "review here");

        let discovery = LayeredDiscovery::new()
            .push(FsSkillSource::new(cli_root.path(), Layer::Cli))
            .push(FsSkillSource::new(user_root.path(), Layer::User));

        let report = discovery.build_report();
        assert_eq!(report.winners.len(), 2);
        let deploy = report.winners.iter().find(|s| s.id == "deploy").unwrap();
        assert_eq!(deploy.layer, Layer::Cli);
        assert_eq!(deploy.manifest.description, "cli version");

        let review = report.winners.iter().find(|s| s.id == "review").unwrap();
        assert_eq!(review.layer, Layer::User);

        assert_eq!(report.shadowed.len(), 1);
        assert_eq!(report.shadowed[0].id, "deploy");
        assert_eq!(report.shadowed[0].winner, Layer::Cli);
        assert_eq!(report.shadowed[0].loser, Layer::User);
    }

    #[test]
    fn qualified_name_bypasses_collision() {
        let proj = tempfile::tempdir().unwrap();
        let user = tempfile::tempdir().unwrap();
        write_skill(proj.path(), "deploy", "deploy", "proj deploy");
        write_skill(user.path(), "deploy", "deploy", "user deploy");

        let discovery = LayeredDiscovery::new()
            .push(FsSkillSource::new(proj.path(), Layer::Project))
            .push(FsSkillSource::new(user.path(), Layer::User).with_namespace("personal"));

        let report = discovery.build_report();
        let ids: Vec<_> = report.winners.iter().map(|s| s.id.clone()).collect();
        assert!(ids.contains(&"deploy".to_string()));
        assert!(ids.contains(&"personal/deploy".to_string()));
        // No shadowing because ids differ.
        assert!(report.shadowed.is_empty(), "{:#?}", report.shadowed);
    }

    #[test]
    fn disabled_layer_is_excluded() {
        let proj = tempfile::tempdir().unwrap();
        let sys = tempfile::tempdir().unwrap();
        write_skill(proj.path(), "alpha", "alpha", "proj");
        write_skill(sys.path(), "beta", "beta", "sys");

        let discovery = LayeredDiscovery::new()
            .push(FsSkillSource::new(proj.path(), Layer::Project))
            .push(FsSkillSource::new(sys.path(), Layer::System))
            .with_options(DiscoveryOptions {
                disabled_layers: vec![Layer::System],
                lookup_order: None,
            });

        let report = discovery.build_report();
        assert_eq!(report.winners.len(), 1);
        assert_eq!(report.winners[0].id, "alpha");
    }

    #[test]
    fn custom_lookup_order_inverts_priority() {
        let cli = tempfile::tempdir().unwrap();
        let user = tempfile::tempdir().unwrap();
        write_skill(cli.path(), "deploy", "deploy", "cli");
        write_skill(user.path(), "deploy", "deploy", "user");

        let discovery = LayeredDiscovery::new()
            .push(FsSkillSource::new(cli.path(), Layer::Cli))
            .push(FsSkillSource::new(user.path(), Layer::User))
            .with_options(DiscoveryOptions {
                disabled_layers: Vec::new(),
                lookup_order: Some(vec![Layer::User, Layer::Cli]),
            });

        let report = discovery.build_report();
        let deploy = report.winners.iter().find(|s| s.id == "deploy").unwrap();
        assert_eq!(deploy.layer, Layer::User);
        assert_eq!(report.shadowed[0].winner, Layer::User);
        assert_eq!(report.shadowed[0].loser, Layer::Cli);
    }

    #[test]
    fn host_source_participates_in_discovery() {
        let proj = tempfile::tempdir().unwrap();
        write_skill(proj.path(), "alpha", "alpha", "proj");

        let host = HostSkillSource::new(
            || {
                vec![SkillManifestRef {
                    id: "gamma".into(),
                    manifest: SkillManifest {
                        name: "gamma".into(),
                        short: "host-only".into(),
                        description: "host-only".into(),
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
                        short: "host-only".into(),
                        description: "host-only".into(),
                        ..Default::default()
                    },
                    body: "from host".into(),
                    skill_dir: None,
                    layer: Layer::Host,
                    namespace: None,
                    unknown_fields: Vec::new(),
                })
            },
        );

        let discovery = LayeredDiscovery::new()
            .push(FsSkillSource::new(proj.path(), Layer::Project))
            .push(host);

        let report = discovery.build_report();
        let ids: Vec<_> = report.winners.iter().map(|s| s.id.clone()).collect();
        assert_eq!(ids, vec!["alpha".to_string(), "gamma".to_string()]);
        let gamma = discovery.fetch("gamma").unwrap();
        assert_eq!(gamma.body, "from host");
    }
}
