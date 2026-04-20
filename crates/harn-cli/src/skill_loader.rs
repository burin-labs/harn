//! CLI-side glue that assembles `harn-vm`'s layered skill discovery
//! from the inputs `harn run` / `harn test` / `harn check` see at
//! startup: repeatable `--skill-dir`, `$HARN_SKILLS_PATH`, the nearest
//! `harn.toml`, and the user's home / system directories.
//!
//! The output is a pre-populated `skills` VM global — a registry dict
//! in the shape the existing `skill_*` builtins already understand, so
//! scripts can call `skill_count(skills)` / `skill_find(skills, name)`
//! without any new language surface.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use harn_vm::skills::{
    build_fs_discovery, default_system_dirs, default_user_dir, install_current_skill_registry,
    parse_env_skills_path, skill_manifest_ref_to_vm, BoundSkillRegistry, DiscoveryOptions,
    DiscoveryReport, FsLayerConfig, Layer, LayeredDiscovery, ManifestSource, SkillManifestRef,
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
}

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

    if let Some(project_root) = inputs
        .source_path
        .as_deref()
        .and_then(harn_vm::stdlib::process::find_project_root)
    {
        cfg.project_root = Some(project_root.clone());
        cfg.packages_dir = Some(project_root.join(".harn").join("packages"));
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
    let report = discovery.build_report();

    let mut loader_warnings = Vec::new();
    let mut entries: Vec<VmValue> = Vec::new();
    for winner in &report.winners {
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
            if matches!(
                report.status,
                VerificationStatus::InvalidSignature
                    | VerificationStatus::MissingSigner
                    | VerificationStatus::UntrustedSigner
            ) {
                loader_warnings.push(format!(
                    "skills: {} provenance check: {}",
                    winner.id,
                    report.human_summary()
                ));
            }
        }
        let mut entry = match skill_manifest_ref_to_vm(winner) {
            VmValue::Dict(map) => (*map).clone(),
            _ => BTreeMap::new(),
        };
        if let Some(report) = provenance {
            entry.insert("provenance".to_string(), provenance_to_vm(&report));
        }
        entries.push(VmValue::Dict(Rc::new(entry)));
    }

    let mut registry: BTreeMap<String, VmValue> = BTreeMap::new();
    registry.insert(
        "_type".to_string(),
        VmValue::String(Rc::from("skill_registry")),
    );
    registry.insert("skills".to_string(), VmValue::List(Rc::new(entries)));
    let registry_value = VmValue::Dict(Rc::new(registry));

    LoadedSkills {
        registry: registry_value,
        report,
        loader_warnings,
        discovery,
    }
}

fn build_provenance_report_for_ref(
    winner: &SkillManifestRef,
    registry_url: Option<String>,
) -> Option<VerificationReport> {
    if winner.origin.is_empty() {
        return None;
    }
    let skill_path = PathBuf::from(&winner.origin).join("SKILL.md");
    let options = VerifyOptions {
        registry_url,
        allowed_signers: winner.manifest.trusted_signers.clone(),
    };
    match skill_provenance::verify_skill(&skill_path, &options) {
        Ok(report) => Some(report),
        Err(error) => Some(VerificationReport {
            skill_path: skill_path.clone(),
            signature_path: skill_provenance::signature_path_for(&skill_path),
            skill_sha256: String::new(),
            signer_fingerprint: None,
            signed: false,
            trusted: false,
            status: VerificationStatus::InvalidSignature,
            error: Some(error),
        }),
    }
}

fn provenance_to_vm(report: &VerificationReport) -> VmValue {
    let mut dict = BTreeMap::new();
    dict.insert(
        "skill_sha256".to_string(),
        VmValue::String(Rc::from(report.skill_sha256.as_str())),
    );
    dict.insert("signed".to_string(), VmValue::Bool(report.signed));
    dict.insert("trusted".to_string(), VmValue::Bool(report.trusted));
    dict.insert(
        "status".to_string(),
        VmValue::String(Rc::from(match report.status {
            VerificationStatus::Verified => "verified",
            VerificationStatus::MissingSignature => "missing_signature",
            VerificationStatus::InvalidSignature => "invalid_signature",
            VerificationStatus::MissingSigner => "missing_signer",
            VerificationStatus::UntrustedSigner => "untrusted_signer",
        })),
    );
    dict.insert(
        "signature_path".to_string(),
        VmValue::String(Rc::from(report.signature_path.display().to_string())),
    );
    if let Some(fingerprint) = report.signer_fingerprint.as_deref() {
        dict.insert(
            "signer_fingerprint".to_string(),
            VmValue::String(Rc::from(fingerprint)),
        );
    }
    if let Some(error) = report.error.as_deref() {
        dict.insert("error".to_string(), VmValue::String(Rc::from(error)));
    }
    VmValue::Dict(Rc::new(dict))
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
            // Git deps are materialized by `harn install` under
            // `.harn/packages/<name>`. We can't know the name from just
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
    let discovery = loaded.discovery.clone();
    install_current_skill_registry(Some(BoundSkillRegistry {
        registry: loaded.registry.clone(),
        fetcher: Arc::new(move |id| discovery.fetch(id)),
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

    #[test]
    fn cli_dirs_produce_registry_entries() {
        let tmp = tempfile::tempdir().unwrap();
        write_skill(tmp.path(), "deploy", "deploy", "body A");
        let loaded = load_skills(&SkillLoaderInputs {
            cli_dirs: vec![tmp.path().to_path_buf()],
            source_path: None,
        });
        assert_eq!(loaded.report.winners.len(), 1);
        assert!(loaded.loader_warnings.is_empty());
        let VmValue::Dict(registry) = &loaded.registry else {
            panic!("registry should be a dict");
        };
        let VmValue::List(entries) = registry.get("skills").unwrap() else {
            panic!("skills should be a list");
        };
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
    fn loader_attaches_verified_provenance_metadata() {
        let _cwd = lock_cwd();
        let _env = lock_env().blocking_lock();
        let tmp = tempfile::tempdir().unwrap();
        std::env::set_var("HOME", tmp.path());

        let skill_dir = tmp.path().join("deploy");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: deploy\nshort: deploy short card\nrequire_signature: true\n---\nbody",
        )
        .unwrap();

        let keys = skill_provenance::generate_keypair(tmp.path().join("signer.pem")).unwrap();
        skill_provenance::sign_skill(skill_dir.join("SKILL.md"), &keys.private_key_path).unwrap();
        skill_provenance::trust_add(keys.public_key_path.to_str().unwrap()).unwrap();

        let loaded = load_skills(&SkillLoaderInputs {
            cli_dirs: vec![tmp.path().to_path_buf()],
            source_path: None,
        });
        let VmValue::Dict(registry) = &loaded.registry else {
            panic!("registry should be a dict");
        };
        let VmValue::List(entries) = registry.get("skills").unwrap() else {
            panic!("skills should be a list");
        };
        let Some(provenance) = entries[0]
            .as_dict()
            .and_then(|entry| entry.get("provenance"))
            .and_then(VmValue::as_dict)
        else {
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
        std::env::set_var("HOME", tmp.path());

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
}
