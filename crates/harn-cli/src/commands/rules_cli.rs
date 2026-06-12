//! Shared shim helpers for the rule-engine CLI surfaces (`harn scan`,
//! `harn codemod`).
//!
//! Both surfaces resolve a rule (an inline pattern, a `--rule` TOML file, or a
//! `--rule-pack` directory/package) and walk a fileset in Rust — where the
//! tree-sitter `Language` registry and the gitignore-aware walker live — then
//! hand the `.harn` handler a per-rule *plan* (the rule TOML plus the files
//! that match its language). The handler runs `std/rules` over the plan.

#![cfg(feature = "hostlib")]

use std::path::PathBuf;

use harn_hostlib::ast::Language;

/// A resolved rule: its TOML source and the language it targets.
pub(crate) struct RuleSpec {
    pub toml: String,
    pub language: Language,
}

/// Resolve the rule(s) to run. Exactly one source is used, in priority order:
/// an inline `pattern` (requires `lang`), a `rule_file`, or a `rule_pack`
/// directory/package of `*.toml` rules.
pub(crate) fn resolve_rules(
    inline_pattern: Option<&str>,
    lang: Option<&str>,
    rule_file: Option<&str>,
    rule_pack: Option<&str>,
) -> Result<Vec<RuleSpec>, String> {
    use std::fs;

    if let Some(pattern) = inline_pattern {
        let lang_name = lang.ok_or("an inline pattern requires `--lang <language>`")?;
        let language = Language::from_name(lang_name)
            .ok_or_else(|| format!("unknown language `{lang_name}`"))?;
        let toml = format!(
            "id = \"scan\"\nlanguage = \"{}\"\n[rule]\npattern = \"{}\"\n",
            crate::format::escape_toml_basic_string(lang_name),
            crate::format::escape_toml_basic_string(pattern),
        );
        Ok(vec![RuleSpec { toml, language }])
    } else if let Some(rule_file) = rule_file {
        let toml = fs::read_to_string(rule_file).map_err(|e| format!("read `{rule_file}`: {e}"))?;
        let language = rule_language(&toml)?;
        Ok(vec![RuleSpec { toml, language }])
    } else if let Some(pack) = rule_pack {
        let specs = resolve_rule_pack(pack)?;
        if specs.is_empty() {
            return Err(format!("rule pack `{pack}` has no `*.toml` rules"));
        }
        Ok(specs)
    } else {
        // No explicit rule given: fall back to the project's `[rules] ruleDirs`.
        let discovered = discover_project_rules()?;
        if discovered.is_empty() {
            Err("no rule given: pass an inline `<pattern> --lang <lang>`, \
                 `--rule <file>`, `--rule-pack <pack>`, or declare \
                 `[rules] ruleDirs` in harn.toml"
                .into())
        } else {
            Ok(discovered)
        }
    }
}

/// Return the top-level `*.toml` rule files in `dir`, sorted by path.
///
/// Rule-pack `ruleDirs` are intentionally not recursive: nested directories
/// are reserved for utility rules, fixtures, and other pack-local support
/// files unless the manifest names them explicitly.
pub(crate) fn collect_rule_files_in_dir(
    dir: &std::path::Path,
) -> Result<Vec<std::path::PathBuf>, String> {
    use std::fs;

    let mut paths: Vec<_> = fs::read_dir(dir)
        .map_err(|e| format!("read rule dir `{}`: {e}", dir.display()))?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().and_then(|e| e.to_str()) == Some("toml"))
        .collect();
    paths.sort();
    Ok(paths)
}

/// Load the top-level `*.toml` rules in `dir` as [`RuleSpec`]s, sorted by path.
fn load_rule_dir_specs(dir: &std::path::Path) -> Result<Vec<RuleSpec>, String> {
    use std::fs;

    let mut specs = Vec::new();
    for path in collect_rule_files_in_dir(dir)? {
        let toml =
            fs::read_to_string(&path).map_err(|e| format!("read `{}`: {e}", path.display()))?;
        let language = rule_language(&toml)?;
        specs.push(RuleSpec { toml, language });
    }
    Ok(specs)
}

/// Resolve a `--rule-pack` value to its rules. The value is either a local
/// directory, or the name of an **installed package** (#2846) — a package
/// fetched with `harn add` and materialized under `<project>/.harn/packages/`.
fn resolve_rule_pack(pack: &str) -> Result<Vec<RuleSpec>, String> {
    let local = std::path::Path::new(pack);
    if local.is_dir() {
        return load_pack_rules(local);
    }
    if let Some(installed) = installed_package_dir(pack) {
        return load_pack_rules(&installed);
    }
    Err(format!(
        "rule pack `{pack}` is not a directory or an installed package \
         (run `harn add {pack}` first?)"
    ))
}

/// The local directory of an installed package named by dependency alias or
/// canonical registry name, if it exists.
fn installed_package_dir(name: &str) -> Option<std::path::PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    let (_, project_dir) = crate::package::find_nearest_manifest(&cwd)?;
    let packages_dir = project_dir.join(".harn").join("packages");
    if crate::package::validate_package_alias(name).is_ok() {
        let dir = packages_dir.join(name);
        if dir.is_dir() {
            return Some(dir);
        }
    }

    let (registry_name, requested_version) = crate::package::parse_registry_package_spec(name)?;
    let lock = crate::package::LockFile::load(&project_dir.join("harn.lock"))
        .ok()
        .flatten()?;
    let entry = lock.packages.iter().find(|entry| {
        entry.registry.as_ref().is_some_and(|registry| {
            registry.name == registry_name
                && requested_version.is_none_or(|version| registry.version == version)
        })
    })?;
    let dir = packages_dir.join(&entry.name);
    dir.is_dir().then_some(dir)
}

/// Load a rule pack's rules: from the pack's own `[rules] ruleDirs` when it
/// ships a `harn.toml` that declares them, else from `*.toml` at the pack root.
fn load_pack_rules(dir: &std::path::Path) -> Result<Vec<RuleSpec>, String> {
    let manifest_path = dir.join("harn.toml");
    if manifest_path.is_file() {
        if let Ok(manifest) = crate::package::read_manifest_from_path(&manifest_path) {
            if !manifest.rules.rule_dirs.is_empty() {
                let mut specs = Vec::new();
                for rel in &manifest.rules.rule_dirs {
                    specs.extend(load_rule_dir_specs(&dir.join(rel))?);
                }
                return Ok(specs);
            }
        }
    }
    load_rule_dir_specs(dir)
}

/// Discover rules from the nearest project manifest's `[rules] ruleDirs`
/// (resolved relative to the manifest's directory). Returns an empty vec when
/// there is no manifest or it declares no `ruleDirs`, so callers fall through
/// to their usual "no rule given" error.
fn discover_project_rules() -> Result<Vec<RuleSpec>, String> {
    let mut specs = Vec::new();
    for rule_dir in project_rule_dirs()? {
        specs.extend(load_rule_dir_specs(&rule_dir)?);
    }
    Ok(specs)
}

/// Resolve the nearest project manifest's `[rules] ruleDirs` and validate that
/// each entry names a directory. Returns an empty vec when no manifest or no
/// rule directories are declared.
pub(crate) fn project_rule_dirs() -> Result<Vec<PathBuf>, String> {
    let cwd = std::env::current_dir().map_err(|e| format!("current dir: {e}"))?;
    let Some((manifest, manifest_dir)) = crate::package::find_nearest_manifest(&cwd) else {
        return Ok(Vec::new());
    };

    let mut dirs = Vec::new();
    for rel in &manifest.rules.rule_dirs {
        let rule_dir = manifest_dir.join(rel);
        if !rule_dir.is_dir() {
            return Err(format!(
                "`[rules] ruleDirs` entry `{rel}` is not a directory ({})",
                rule_dir.display()
            ));
        }
        dirs.push(rule_dir);
    }
    Ok(dirs)
}

/// Build the per-rule plan JSON: each rule paired with the files (recursively
/// collected from `paths`, gitignore-aware) that match its language.
pub(crate) fn build_plan(specs: Vec<RuleSpec>, paths: &[String]) -> Result<String, String> {
    let files = collect_files(paths);
    let plan: Vec<serde_json::Value> = specs
        .into_iter()
        .map(|spec| {
            let lang_name = spec.language.name();
            let matching: Vec<String> = files
                .iter()
                .filter(|path| Language::detect(path, None).map(|l| l.name()) == Some(lang_name))
                .map(|path| path.display().to_string())
                .collect();
            serde_json::json!({
                "rule": spec.toml,
                "language": lang_name,
                "files": matching,
            })
        })
        .collect();
    serde_json::to_string(&plan).map_err(|e| format!("serialize plan: {e}"))
}

/// Collect files matching one language with the same gitignore-aware walker the
/// rule-pack paths use.
pub(crate) fn collect_files_for_language(paths: &[String], language: Language) -> Vec<String> {
    let lang_name = language.name();
    collect_files(paths)
        .into_iter()
        .filter(|path| Language::detect(path, None).map(|l| l.name()) == Some(lang_name))
        .map(|path| path.display().to_string())
        .collect()
}

/// True if a rule TOML declares a top-level `fix` (i.e. it is a codemod, not a
/// lint/search). Used to filter discovered packs down to applicable rules.
pub(crate) fn rule_has_fix(src: &str) -> bool {
    toml::from_str::<toml::Value>(src)
        .ok()
        .and_then(|v| v.get("fix").map(toml::Value::is_str))
        .unwrap_or(false)
}

/// Parse a rule TOML's declared `language` into a [`Language`].
fn rule_language(src: &str) -> Result<Language, String> {
    let value: toml::Value = toml::from_str(src).map_err(|e| format!("invalid rule TOML: {e}"))?;
    let name = value
        .get("language")
        .and_then(|v| v.as_str())
        .ok_or("rule TOML is missing a top-level `language`")?;
    Language::from_name(name).ok_or_else(|| format!("unknown language `{name}`"))
}

/// Collect candidate files from `paths` (default: the current directory),
/// recursing directories with the gitignore-aware walker.
fn collect_files(paths: &[String]) -> Vec<PathBuf> {
    use ignore::WalkBuilder;
    use std::path::Path;

    let roots: Vec<String> = if paths.is_empty() {
        vec![".".to_string()]
    } else {
        paths.to_vec()
    };

    let mut out: Vec<PathBuf> = Vec::new();
    for root in &roots {
        let path = Path::new(root);
        if path.is_dir() {
            let mut walker = WalkBuilder::new(path);
            walker
                .hidden(false)
                .git_ignore(true)
                .git_global(true)
                .git_exclude(true)
                .require_git(false);
            for entry in walker.build().filter_map(Result::ok) {
                if entry.file_type().is_some_and(|t| t.is_file()) {
                    out.push(entry.path().to_path_buf());
                }
            }
        } else if path.is_file() {
            out.push(path.to_path_buf());
        }
    }
    out.sort();
    out.dedup();
    out
}
