//! Shared shim helpers for the rule-engine CLI surfaces (`harn scan`,
//! `harn codemod`).
//!
//! Both surfaces resolve a rule (an inline pattern, a `--rule` TOML file, or a
//! `--rule-pack` directory) and walk a fileset in Rust — where the tree-sitter
//! `Language` registry and the gitignore-aware walker live — then hand the
//! `.harn` handler a per-rule *plan* (the rule TOML plus the files that match
//! its language). The handler runs `std/rules` over the plan.

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
/// directory of `*.toml` rules.
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
            toml_escape(lang_name),
            toml_escape(pattern),
        );
        Ok(vec![RuleSpec { toml, language }])
    } else if let Some(rule_file) = rule_file {
        let toml = fs::read_to_string(rule_file).map_err(|e| format!("read `{rule_file}`: {e}"))?;
        let language = rule_language(&toml)?;
        Ok(vec![RuleSpec { toml, language }])
    } else if let Some(dir) = rule_pack {
        let mut specs = Vec::new();
        let entries = fs::read_dir(dir).map_err(|e| format!("read rule pack `{dir}`: {e}"))?;
        let mut paths: Vec<_> = entries
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("toml"))
            .collect();
        paths.sort();
        for path in paths {
            let toml =
                fs::read_to_string(&path).map_err(|e| format!("read `{}`: {e}", path.display()))?;
            let language = rule_language(&toml)?;
            specs.push(RuleSpec { toml, language });
        }
        if specs.is_empty() {
            return Err(format!("rule pack `{dir}` has no `*.toml` rules"));
        }
        Ok(specs)
    } else {
        Err("provide an inline <pattern>, `--rule <file>`, or `--rule-pack <dir>`".into())
    }
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

/// Escape a string for a TOML basic (double-quoted) string.
fn toml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            _ => out.push(ch),
        }
    }
    out
}
