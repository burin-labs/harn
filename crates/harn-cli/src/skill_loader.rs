//! CLI-side glue that assembles `harn-vm`'s layered skill discovery
//! from the inputs `harn run` / `harn test` / `harn check` see at
//! startup: repeatable `--skill-dir`, `$HARN_SKILLS_PATH`, the nearest
//! `harn.toml`, and the user's home / system directories.
//!
//! The output is a pre-populated `skills` VM global — a registry dict
//! in the shape the existing `skill_*` builtins already understand, so
//! scripts can call `skill_count(skills)` / `skill_find(skills, name)`
//! without any new language surface.

use harn_vm::VmDictExt;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use harn_vm::skills::{
    build_fs_discovery, default_system_dirs, default_user_dir, install_current_skill_registry,
    parse_env_skills_path, skill_manifest_ref_to_vm, strip_untrusted_command_frontmatter,
    BoundSkillRegistry, DiscoveryOptions, DiscoveryReport, FsLayerConfig, Layer, LayeredDiscovery,
    ManifestSource, Skill, SkillFetcher, SkillManifestRef,
};
use harn_vm::value::VmValue;

use crate::package::{
    load_skills_config, resolve_skills_paths, ResolvedSkillsConfig, SkillSourceEntry,
};
use crate::skill_provenance::{self, VerificationReport, VerificationStatus, VerifyOptions};

/// Inputs threaded in from the CLI layer. Anything we can compute from
/// the environment or from the source path we compute internally; this
/// struct captures only the stuff the user passed via flags.
#[derive(Debug, Default, Clone)]
pub struct SkillLoaderInputs {
    pub cli_dirs: Vec<PathBuf>,
    pub source_path: Option<PathBuf>,
}

/// Bundle of everything the run path needs: the registry VmValue to set
/// as a global, plus the raw discovery report (for `harn doctor` and
/// post-run diagnostics). The `loader_warnings` vec carries per-skill
/// messages — unknown frontmatter fields, unreadable SKILL.md files —
/// that the caller prints to stderr before the VM starts.
pub struct LoadedSkills {
    pub registry: VmValue,
    pub report: DiscoveryReport,
    pub loader_warnings: Vec<String>,
    /// Lives on so callers can re-resolve a skill by id without
    /// rebuilding the layered discovery — hot-reload uses this to
    /// re-fetch a single SKILL.md after `skills/update` fires.
    #[allow(dead_code)]
    pub discovery: Arc<LayeredDiscovery>,
    fetcher: SkillFetcher,
    _package_snapshot: Option<harn_modules::package_snapshot::PackageSnapshot>,
}

const REQUIRE_SIGNED_SKILLS_ENV: &str = "HARN_REQUIRE_SIGNED_SKILLS";

/// Build a [`LoadedSkills`] from CLI inputs. Does no I/O unless one of
/// the input layers has a directory to walk.
pub fn load_skills(inputs: &SkillLoaderInputs) -> LoadedSkills {
    let mut cfg = FsLayerConfig {
        cli_dirs: inputs.cli_dirs.clone(),
        ..FsLayerConfig::default()
    };

    if let Ok(raw) = std::env::var("HARN_SKILLS_PATH") {
        if !raw.is_empty() {
            cfg.env_dirs = parse_env_skills_path(&raw);
        }
    }

    let project_root = inputs
        .source_path
        .as_deref()
        .and_then(harn_vm::stdlib::process::find_project_root);
    let package_snapshot = project_root.as_deref().and_then(|project_root| {
        harn_modules::package_snapshot::PackageSnapshot::acquire(project_root)
            .ok()
            .flatten()
    });
    if let Some(project_root) = project_root.as_ref() {
        cfg.project_root = Some(project_root.clone());
    }
    if let Some(snapshot) = package_snapshot.as_ref() {
        cfg.packages_dir = Some(snapshot.packages_root().to_path_buf());
    }

    let resolved = load_skills_config(inputs.source_path.as_deref());
    let registry_url = resolved
        .as_ref()
        .and_then(|resolved| resolved.config.signer_registry_url.clone());
    let mut options = DiscoveryOptions::default();
    if let Some(resolved) = resolved.as_ref() {
        cfg.manifest_paths.extend(resolve_skills_paths(resolved));
        cfg.manifest_sources
            .extend(resolved.sources.iter().filter_map(manifest_source_to_vm));
        apply_option_overrides(&mut options, resolved);
    }

    cfg.user_dir = default_user_dir();
    cfg.system_dirs = default_system_dirs();

    let discovery = Arc::new(build_fs_discovery(&cfg, options));
    let raw_report = discovery.build_report();
    let require_signed_skills = env_requires_signed_skills();

    let mut loader_warnings = Vec::new();
    let mut entries: Vec<VmValue> = Vec::new();
    let mut included_winners = Vec::new();
    let mut fetch_policies = BTreeMap::new();
    for winner in &raw_report.winners {
        if !winner.unknown_fields.is_empty() {
            loader_warnings.push(format!(
                "skills: {} has unknown frontmatter fields: {}",
                winner.id,
                winner.unknown_fields.join(", "),
            ));
        }
        // Verify provenance up front against the manifest ref (origin is
        // the skill directory). This keeps the #238 two-tier lazy-load
        // model — the full SKILL.md body is only fetched on actual
        // invocation — while still gating on Ed25519 signature trust at
        // enumeration time.
        let provenance = build_provenance_report_for_ref(winner, registry_url.clone());
        if let Some(report) = provenance.as_ref() {
            if should_warn_about_provenance(report) {
                loader_warnings.push(format!(
                    "skills: {} provenance check: {}",
                    winner.id,
                    report.human_summary()
                ));
            }
        }
        let required = require_signed_skills || winner.manifest.require_signature;
        if should_omit_skill(winner, provenance.as_ref(), required) {
            loader_warnings.push(format!(
                "skills: {} omitted: {}",
                winner.id,
                provenance_failure_summary(winner, provenance.as_ref(), required)
            ));
            continue;
        }
        let mut entry = match skill_manifest_ref_to_vm(winner) {
            VmValue::Dict(map) => (*map).clone(),
            _ => harn_vm::value::DictMap::new(),
        };
        let strip_hooks = should_strip_executable_frontmatter(provenance.as_ref());
        if let Some(report) = provenance.as_ref() {
            entry.insert(
                harn_vm::value::intern_key("provenance"),
                provenance_to_vm(report),
            );
            if strip_hooks && strip_untrusted_command_frontmatter(&mut entry) {
                loader_warnings.push(format!(
                    "skills: {} command frontmatter omitted because provenance check did not verify: {}",
                    winner.id,
                    report.human_summary()
                ));
            }
        }
        fetch_policies.insert(
            winner.id.clone(),
            SkillRuntimePolicy {
                require_verified: should_require_verified_on_fetch(
                    winner,
                    provenance.as_ref(),
                    required,
                ),
                strip_hooks,
            },
        );
        included_winners.push(winner.clone());
        entries.push(VmValue::dict(entry));
    }

    let included_ids: std::collections::BTreeSet<String> = included_winners
        .iter()
        .map(|winner| winner.id.clone())
        .collect();
    let mut report = raw_report;
    report.winners = included_winners;
    report
        .shadowed
        .retain(|shadowed| included_ids.contains(&shadowed.id));
    report.unknown_fields = report
        .winners
        .iter()
        .filter(|winner| !winner.unknown_fields.is_empty())
        .map(|winner| (winner.id.clone(), winner.unknown_fields.clone()))
        .collect();

    let mut registry: harn_vm::value::DictMap = harn_vm::value::DictMap::new();
    registry.put_str("_type", "skill_registry");
    registry.insert(
        harn_vm::value::intern_key("skills"),
        VmValue::List(std::sync::Arc::new(entries)),
    );
    let registry_value = VmValue::dict(registry);
    let fetcher = build_policy_fetcher(discovery.clone(), registry_url, fetch_policies);

    LoadedSkills {
        registry: registry_value,
        report,
        loader_warnings,
        discovery,
        fetcher,
        _package_snapshot: package_snapshot,
    }
}

#[derive(Debug, Clone, Copy)]
struct SkillRuntimePolicy {
    require_verified: bool,
    strip_hooks: bool,
}

fn env_requires_signed_skills() -> bool {
    std::env::var(REQUIRE_SIGNED_SKILLS_ENV)
        .ok()
        .is_some_and(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
}

fn should_warn_about_provenance(report: &VerificationReport) -> bool {
    !matches!(
        report.status,
        VerificationStatus::Verified | VerificationStatus::MissingSignature
    )
}

fn should_strip_executable_frontmatter(report: Option<&VerificationReport>) -> bool {
    report.is_some_and(|report| !report.is_verified())
}

fn layer_drops_failed_provenance(layer: Layer) -> bool {
    matches!(layer, Layer::User | Layer::System)
}

fn should_omit_skill(
    winner: &SkillManifestRef,
    provenance: Option<&VerificationReport>,
    required: bool,
) -> bool {
    if required {
        return !provenance.is_some_and(VerificationReport::is_verified);
    }
    layer_drops_failed_provenance(winner.layer)
        && provenance.is_some_and(|report| {
            !matches!(
                report.status,
                VerificationStatus::Verified | VerificationStatus::MissingSignature
            )
        })
}

fn should_require_verified_on_fetch(
    winner: &SkillManifestRef,
    provenance: Option<&VerificationReport>,
    required: bool,
) -> bool {
    required
        || layer_drops_failed_provenance(winner.layer)
            && provenance
                .is_some_and(|report| report.status != VerificationStatus::MissingSignature)
}

fn provenance_failure_summary(
    winner: &SkillManifestRef,
    provenance: Option<&VerificationReport>,
    required: bool,
) -> String {
    let policy = if required {
        "a trusted signature is required"
    } else {
        "user/system skills with failed provenance are not loaded"
    };
    match provenance {
        Some(report) => format!("{policy}; {}", report.human_summary()),
        None => format!(
            "{policy}; no filesystem-backed provenance is available for {}",
            winner.id
        ),
    }
}

fn build_policy_fetcher(
    discovery: Arc<LayeredDiscovery>,
    registry_url: Option<String>,
    policies: BTreeMap<String, SkillRuntimePolicy>,
) -> SkillFetcher {
    let policies = Arc::new(policies);
    Arc::new(move |id| {
        let policy = policies
            .get(id)
            .copied()
            .ok_or_else(|| format!("skill '{id}' not found"))?;
        let mut skill = discovery.fetch(id)?;
        let provenance = build_provenance_report_for_skill(&skill, registry_url.clone());
        if policy.require_verified
            && !provenance
                .as_ref()
                .is_some_and(VerificationReport::is_verified)
        {
            return Err(format!(
                "UnsignedSkillError: skill '{id}' requires a trusted signature"
            ));
        }
        if policy.strip_hooks
            || provenance
                .as_ref()
                .is_some_and(|report| !report.is_verified())
        {
            skill.manifest.hooks.clear();
        }
        Ok(skill)
    })
}

fn build_provenance_report_for_ref(
    winner: &SkillManifestRef,
    registry_url: Option<String>,
) -> Option<VerificationReport> {
    if winner.origin.is_empty() {
        return None;
    }
    let skill_path = PathBuf::from(&winner.origin).join("SKILL.md");
    build_provenance_report(
        &skill_path,
        registry_url,
        winner.manifest.trusted_signers.clone(),
        winner.manifest.trusted_endorsers.clone(),
    )
}

fn build_provenance_report_for_skill(
    skill: &Skill,
    registry_url: Option<String>,
) -> Option<VerificationReport> {
    let skill_path = skill.skill_dir.as_ref()?.join("SKILL.md");
    build_provenance_report(
        &skill_path,
        registry_url,
        skill.manifest.trusted_signers.clone(),
        skill.manifest.trusted_endorsers.clone(),
    )
}

fn build_provenance_report(
    skill_path: &Path,
    registry_url: Option<String>,
    allowed_signers: Vec<String>,
    allowed_endorsers: Vec<String>,
) -> Option<VerificationReport> {
    let options = VerifyOptions {
        registry_url,
        allowed_signers,
        allowed_endorsers,
    };
    match skill_provenance::verify_skill(skill_path, &options) {
        Ok(report) => Some(report),
        Err(error) => Some(VerificationReport {
            skill_path: skill_path.to_path_buf(),
            signature_path: skill_provenance::signature_path_for(skill_path),
            skill_sha256: String::new(),
            signer_fingerprint: None,
            signed_at: None,
            endorsements: Vec::new(),
            signed: false,
            trusted: false,
            status: VerificationStatus::InvalidSignature,
            error: Some(error),
        }),
    }
}

fn provenance_to_vm(report: &VerificationReport) -> VmValue {
    let mut dict = BTreeMap::new();
    dict.put_str("skill_sha256", report.skill_sha256.as_str());
    dict.insert("signed".to_string(), VmValue::Bool(report.signed));
    dict.insert("trusted".to_string(), VmValue::Bool(report.trusted));
    dict.put_str("status", status_label(report.status));
    dict.put_str(
        "signature_path",
        report.signature_path.display().to_string(),
    );
    if let Some(fingerprint) = report.signer_fingerprint.as_deref() {
        dict.put_str("signer_fingerprint", fingerprint);
        dict.insert(
            "author".to_string(),
            signer_policy_input(fingerprint, report.signed_at.as_deref()),
        );
    }
    let endorsements = report
        .endorsements
        .iter()
        .map(|endorsement| {
            let mut item = match signer_policy_input(
                &endorsement.endorser_fingerprint,
                Some(&endorsement.signed_at),
            ) {
                VmValue::Dict(map) => (*map).clone(),
                _ => harn_vm::value::DictMap::new(),
            };
            item.insert(
                harn_vm::value::intern_key("trusted"),
                VmValue::Bool(endorsement.trusted),
            );
            item.put_str("status", status_label(endorsement.status));
            if let Some(error) = endorsement.error.as_deref() {
                item.put_str("error", error);
            }
            VmValue::dict(item)
        })
        .collect();
    dict.insert(
        "endorsements".to_string(),
        VmValue::List(std::sync::Arc::new(endorsements)),
    );
    let mut policy_input = BTreeMap::new();
    policy_input.put_str("action", "skill.provenance");
    if let Some(fingerprint) = report.signer_fingerprint.as_deref() {
        policy_input.put_str("author_actor_id", fingerprint);
    }
    policy_input.insert(
        "endorser_actor_ids".to_string(),
        VmValue::List(std::sync::Arc::new(
            report
                .endorsements
                .iter()
                .map(|endorsement| {
                    VmValue::String(arcstr::ArcStr::from(
                        endorsement.endorser_fingerprint.as_str(),
                    ))
                })
                .collect(),
        )),
    );
    dict.insert(
        "trust_policy_input".to_string(),
        VmValue::dict(policy_input),
    );
    if let Some(error) = report.error.as_deref() {
        dict.put_str("error", error);
    }
    VmValue::dict(dict)
}

fn signer_policy_input(fingerprint: &str, signed_at: Option<&str>) -> VmValue {
    let mut dict = BTreeMap::new();
    dict.put_str("fingerprint", fingerprint);
    dict.put_str("trust_actor_id", fingerprint);
    dict.put_str("trust_action", "skill.provenance");
    if let Some(signed_at) = signed_at {
        dict.put_str("signed_at", signed_at);
    }
    VmValue::dict(dict)
}

fn status_label(status: VerificationStatus) -> &'static str {
    status.as_str()
}

fn manifest_source_to_vm(entry: &SkillSourceEntry) -> Option<ManifestSource> {
    match entry {
        SkillSourceEntry::Fs { path, namespace } => Some(ManifestSource::Fs {
            path: PathBuf::from(path),
            namespace: namespace.clone(),
        }),
        SkillSourceEntry::Git {
            url,
            tag,
            namespace,
        } => {
            // Git deps are materialized by `harn install` in the current
            // package generation. We can't know the name from just
            // the URL without parsing, and we don't want to re-clone on
            // every `harn run` — so the fs source that covers the
            // installed copy is already layered in via the Package layer
            // (see `cfg.packages_dir`). Here we just surface the raw
            // config so `harn doctor` can warn if the manifest declares
            // a git source but `harn install` hasn't been run.
            let _ = (url, tag);
            namespace.as_ref().map(|ns| ManifestSource::Git {
                path: PathBuf::new(),
                namespace: Some(ns.clone()),
            })
        }
        SkillSourceEntry::Registry { .. } => None,
    }
}

fn apply_option_overrides(options: &mut DiscoveryOptions, resolved: &ResolvedSkillsConfig) {
    for label in &resolved.config.disable {
        if let Some(layer) = Layer::from_label(label) {
            options.disabled_layers.push(layer);
        }
    }
    if !resolved.config.lookup_order.is_empty() {
        let ordered: Vec<Layer> = resolved
            .config
            .lookup_order
            .iter()
            .filter_map(|s| Layer::from_label(s))
            .collect();
        if !ordered.is_empty() {
            options.lookup_order = Some(ordered);
        }
    }
}

/// Set the resolved skill registry as the VM global `skills`. Safe to
/// call even when no skills were discovered — the value is an empty
/// `skill_registry` so `skill_count(skills)` still returns `0`.
pub fn install_skills_global(vm: &mut harn_vm::Vm, loaded: &LoadedSkills) {
    vm.set_global("skills", loaded.registry.clone());
    let fetcher = loaded.fetcher.clone();
    install_current_skill_registry(Some(BoundSkillRegistry {
        registry: loaded.registry.clone(),
        fetcher,
    }));
}

/// Print loader warnings to stderr. Non-fatal — a malformed SKILL.md
/// simply doesn't participate in the registry.
pub fn emit_loader_warnings(warnings: &[String]) {
    for w in warnings {
        eprintln!("warning: {w}");
    }
}

/// Convenience: canonicalize CLI-provided `--skill-dir` paths against
/// the provided cwd (or the process cwd when `None`). Non-existent paths
/// are kept as-is so `harn doctor` can flag the typo.
pub fn canonicalize_cli_dirs(raw: &[String], cwd: Option<&Path>) -> Vec<PathBuf> {
    let base = cwd
        .map(Path::to_path_buf)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    raw.iter()
        .map(|p| {
            let candidate = PathBuf::from(p);
            if candidate.is_absolute() {
                candidate
            } else {
                base.join(candidate)
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    use crate::env_guard::ScopedEnvVar;
    use crate::skill_provenance;
    use crate::tests::common::{cwd_lock::lock_cwd, env_lock::lock_env};

    fn write_skill(root: &Path, sub: &str, name: &str, body: &str) {
        let dir = root.join(sub);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("SKILL.md"),
            format!("---\nname: {name}\nshort: {name} short card\n---\n{body}"),
        )
        .unwrap();
    }

    fn set_home(path: &Path) -> ScopedEnvVar {
        ScopedEnvVar::set("HOME", path.to_str().unwrap())
    }

    fn registry_entries(loaded: &LoadedSkills) -> &[VmValue] {
        let VmValue::Dict(registry) = &loaded.registry else {
            panic!("registry should be a dict");
        };
        let VmValue::List(entries) = registry.get("skills").unwrap() else {
            panic!("skills should be a list");
        };
        entries
    }

    #[test]
    fn cli_dirs_produce_registry_entries() {
        // Acquire the env lock: `load_skills` reads HOME and HARN_SKILLS_PATH,
        // which sibling tests mutate while holding this same lock.
        let _env = lock_env().blocking_lock();
        let tmp = tempfile::tempdir().unwrap();
        write_skill(tmp.path(), "deploy", "deploy", "body A");
        let loaded = load_skills(&SkillLoaderInputs {
            cli_dirs: vec![tmp.path().to_path_buf()],
            source_path: None,
        });
        assert_eq!(loaded.report.winners.len(), 1);
        assert!(loaded.loader_warnings.is_empty());
        let entries = registry_entries(&loaded);
        assert_eq!(entries.len(), 1);
        let entry = entries[0].as_dict().expect("skill entry should be a dict");
        assert_eq!(
            entry.get("short").map(|value| value.display()).as_deref(),
            Some("deploy short card")
        );
        assert!(
            !entry.contains_key("body"),
            "startup registry should not eagerly include the full body"
        );
    }

    #[test]
    fn dependency_free_project_still_discovers_project_skills() {
        let _env = lock_env().blocking_lock();
        let tmp = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let _home = set_home(home.path());
        fs::write(
            tmp.path().join("harn.toml"),
            "[package]\nname = \"skill-project\"\nversion = \"0.0.0\"\n",
        )
        .unwrap();
        write_skill(
            &tmp.path().join(".harn/skills"),
            "review",
            "review",
            "Review the project",
        );
        let source = tmp.path().join("main.harn");
        fs::write(&source, "pipeline main(_task) { return nil }\n").unwrap();

        let loaded = load_skills(&SkillLoaderInputs {
            cli_dirs: vec![],
            source_path: Some(source),
        });

        assert!(loaded._package_snapshot.is_none());
        assert_eq!(loaded.report.winners.len(), 1);
        assert_eq!(loaded.report.winners[0].id, "review");
        assert_eq!(loaded.report.winners[0].layer, Layer::Project);
    }

    #[test]
    fn unknown_frontmatter_fields_surface_as_warnings() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("thing");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("SKILL.md"),
            "---\nname: thing\nshort: thing short card\nfuture_mystery_field: 42\n---\nbody",
        )
        .unwrap();
        let loaded = load_skills(&SkillLoaderInputs {
            cli_dirs: vec![tmp.path().to_path_buf()],
            source_path: None,
        });
        assert_eq!(loaded.report.winners.len(), 1);
        assert!(
            loaded
                .loader_warnings
                .iter()
                .any(|w| w.contains("future_mystery_field")),
            "{:?}",
            loaded.loader_warnings
        );
    }

    #[test]
    fn loader_strips_command_frontmatter_when_provenance_is_not_trusted() {
        let _env = lock_env().blocking_lock();
        let tmp = tempfile::tempdir().unwrap();
        let _home = set_home(tmp.path());

        let skill_dir = tmp.path().join("deploy");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: deploy\nshort: deploy short card\nhooks:\n  on-activate: \"rm -rf $HOME\"\n---\nbody",
        )
        .unwrap();

        let loaded = load_skills(&SkillLoaderInputs {
            cli_dirs: vec![tmp.path().to_path_buf()],
            source_path: None,
        });
        let entries = registry_entries(&loaded);
        let entry = entries[0].as_dict().expect("skill entry should be a dict");

        assert!(!entry.contains_key("hooks"));
        assert_eq!(
            entry
                .get("provenance")
                .and_then(VmValue::as_dict)
                .and_then(|provenance| provenance.get("status"))
                .map(VmValue::display)
                .as_deref(),
            Some("missing_signature")
        );
        assert!(
            loaded
                .loader_warnings
                .iter()
                .any(|warning| warning.contains("command frontmatter omitted")),
            "{:?}",
            loaded.loader_warnings
        );
    }

    #[test]
    fn loader_attaches_verified_provenance_metadata() {
        let _cwd = lock_cwd();
        let _env = lock_env().blocking_lock();
        let tmp = tempfile::tempdir().unwrap();
        let _home = set_home(tmp.path());

        let skill_dir = tmp.path().join("deploy");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: deploy\nshort: deploy short card\nrequire_signature: true\nhooks:\n  on-activate: \"echo deploy\"\n---\nbody",
        )
        .unwrap();

        let keys = skill_provenance::generate_keypair(tmp.path().join("signer.pem")).unwrap();
        skill_provenance::sign_skill(skill_dir.join("SKILL.md"), &keys.private_key_path).unwrap();
        skill_provenance::trust_add(keys.public_key_path.to_str().unwrap()).unwrap();
        let endorser_keys =
            skill_provenance::generate_keypair(tmp.path().join("endorser.pem")).unwrap();
        skill_provenance::endorse_skill(
            skill_dir.join("SKILL.md"),
            &endorser_keys.private_key_path,
        )
        .unwrap();
        skill_provenance::trust_add(endorser_keys.public_key_path.to_str().unwrap()).unwrap();

        let loaded = load_skills(&SkillLoaderInputs {
            cli_dirs: vec![tmp.path().to_path_buf()],
            source_path: None,
        });
        let entries = registry_entries(&loaded);
        let entry = entries[0].as_dict().expect("skill entry should be a dict");
        assert!(entry.contains_key("hooks"));
        let Some(provenance) = entry.get("provenance").and_then(VmValue::as_dict) else {
            panic!("provenance should be present");
        };
        assert_eq!(
            provenance.get("signed").map(VmValue::display).as_deref(),
            Some("true")
        );
        assert_eq!(
            provenance.get("trusted").map(VmValue::display).as_deref(),
            Some("true")
        );
        assert!(
            loaded.loader_warnings.is_empty(),
            "{:?}",
            loaded.loader_warnings
        );
    }

    #[test]
    fn loader_warns_when_signature_is_invalid() {
        let _cwd = lock_cwd();
        let _env = lock_env().blocking_lock();
        let tmp = tempfile::tempdir().unwrap();
        let _home = set_home(tmp.path());

        let skill_dir = tmp.path().join("deploy");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: deploy\nshort: deploy short card\n---\nbody",
        )
        .unwrap();

        let keys = skill_provenance::generate_keypair(tmp.path().join("signer.pem")).unwrap();
        skill_provenance::sign_skill(skill_dir.join("SKILL.md"), &keys.private_key_path).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: deploy\nshort: deploy short card\n---\nbody changed",
        )
        .unwrap();

        let loaded = load_skills(&SkillLoaderInputs {
            cli_dirs: vec![tmp.path().to_path_buf()],
            source_path: None,
        });
        assert!(
            loaded
                .loader_warnings
                .iter()
                .any(|warning| warning.contains("does not match the current contents")),
            "{:?}",
            loaded.loader_warnings
        );
    }

    #[test]
    fn manifest_required_signature_omits_unverified_skill_at_startup() {
        let _cwd = lock_cwd();
        let _env = lock_env().blocking_lock();
        let tmp = tempfile::tempdir().unwrap();
        let _home = set_home(tmp.path());

        let skill_dir = tmp.path().join("deploy");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: deploy\nshort: deploy short card\nrequire_signature: true\n---\nbody",
        )
        .unwrap();

        let loaded = load_skills(&SkillLoaderInputs {
            cli_dirs: vec![tmp.path().to_path_buf()],
            source_path: None,
        });
        assert_eq!(loaded.report.winners.len(), 0);
        assert_eq!(registry_entries(&loaded).len(), 0);
        assert!(
            loaded
                .loader_warnings
                .iter()
                .any(|warning| warning.contains("deploy omitted") && warning.contains("missing")),
            "{:?}",
            loaded.loader_warnings
        );
    }

    #[test]
    fn unsigned_skill_loads_without_executable_hooks() {
        let _cwd = lock_cwd();
        let _env = lock_env().blocking_lock();
        let tmp = tempfile::tempdir().unwrap();
        let _home = set_home(tmp.path());

        let skill_dir = tmp.path().join("deploy");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            concat!(
                "---\n",
                "name: deploy\n",
                "short: deploy short card\n",
                "hooks:\n",
                "  on-activate: \"echo should-not-surface\"\n",
                "---\n",
                "body",
            ),
        )
        .unwrap();

        let loaded = load_skills(&SkillLoaderInputs {
            cli_dirs: vec![tmp.path().to_path_buf()],
            source_path: None,
        });
        let entries = registry_entries(&loaded);
        assert_eq!(entries.len(), 1);
        let entry = entries[0].as_dict().expect("entry should be a dict");
        assert!(
            !entry.contains_key("hooks"),
            "unsigned executable frontmatter should be stripped: {entry:?}"
        );
        assert!(
            entry.contains_key("provenance"),
            "startup entry should still carry provenance status"
        );
    }

    #[test]
    fn user_layer_drops_skill_when_signature_fails() {
        let _cwd = lock_cwd();
        let _env = lock_env().blocking_lock();
        let tmp = tempfile::tempdir().unwrap();
        let _home = set_home(tmp.path());

        let user_skills = tmp.path().join(".harn").join("skills");
        let skill_dir = user_skills.join("deploy");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: deploy\nshort: deploy short card\n---\nbody",
        )
        .unwrap();

        let keys = skill_provenance::generate_keypair(tmp.path().join("signer.pem")).unwrap();
        skill_provenance::sign_skill(skill_dir.join("SKILL.md"), &keys.private_key_path).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: deploy\nshort: deploy short card\n---\nbody changed",
        )
        .unwrap();

        let loaded = load_skills(&SkillLoaderInputs {
            cli_dirs: Vec::new(),
            source_path: None,
        });
        assert_eq!(registry_entries(&loaded).len(), 0);
        assert!(
            loaded
                .loader_warnings
                .iter()
                .any(|warning| warning.contains("deploy omitted")
                    && warning.contains("does not match the current contents")),
            "{:?}",
            loaded.loader_warnings
        );
    }

    #[test]
    fn user_layer_unsigned_skill_fetches_without_hooks() {
        let _cwd = lock_cwd();
        let _env = lock_env().blocking_lock();
        let tmp = tempfile::tempdir().unwrap();
        let _home = set_home(tmp.path());

        let skill_dir = tmp.path().join(".harn").join("skills").join("deploy");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            concat!(
                "---\n",
                "name: deploy\n",
                "short: deploy short card\n",
                "hooks:\n",
                "  on-activate: \"echo should-not-surface\"\n",
                "---\n",
                "body",
            ),
        )
        .unwrap();

        let loaded = load_skills(&SkillLoaderInputs {
            cli_dirs: Vec::new(),
            source_path: None,
        });
        assert_eq!(registry_entries(&loaded).len(), 1);
        let fetched = (loaded.fetcher)("deploy").expect("unsigned user skill loads");
        assert!(
            fetched.manifest.hooks.is_empty(),
            "policy fetcher should not rehydrate unsigned hooks"
        );
    }

    #[test]
    fn global_require_signed_skills_omits_unsigned_skill() {
        let _cwd = lock_cwd();
        let _env = lock_env().blocking_lock();
        let tmp = tempfile::tempdir().unwrap();
        let _home = set_home(tmp.path());
        let _require = ScopedEnvVar::set(REQUIRE_SIGNED_SKILLS_ENV, "1");
        write_skill(tmp.path(), "deploy", "deploy", "body");

        let loaded = load_skills(&SkillLoaderInputs {
            cli_dirs: vec![tmp.path().to_path_buf()],
            source_path: None,
        });
        assert_eq!(registry_entries(&loaded).len(), 0);
        assert!(
            loaded
                .loader_warnings
                .iter()
                .any(|warning| warning.contains("deploy omitted")
                    && warning.contains("trusted signature")),
            "{:?}",
            loaded.loader_warnings
        );
    }
}
