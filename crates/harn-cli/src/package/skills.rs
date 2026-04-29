use super::*;

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

    let mut config = manifest.skills;
    if let Some(raw) = config.signer_registry_url.as_deref() {
        if !raw.is_empty() && Url::parse(raw).is_err() && !PathBuf::from(raw).is_absolute() {
            config.signer_registry_url = Some(dir.join(raw).display().to_string());
        }
    }

    Some(ResolvedSkillsConfig {
        config,
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

pub(crate) fn expand_single_star_glob(path: &Path) -> Vec<PathBuf> {
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
    signer_registry_url = "https://skills.harnlang.com/signers/"

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
    url = "https://skills.harnlang.com"
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
            resolved.config.signer_registry_url.as_deref(),
            Some("https://skills.harnlang.com/signers/")
        );
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
}
