//! `harn scan` dispatches to the embedded `cli/scan.harn` handler. The shim
//! resolves the rule(s) and the fileset in Rust — where the tree-sitter
//! `Language` registry and the gitignore-aware walker live — then hands the
//! `.harn` handler a per-rule *plan* (rule TOML + the matching files). The
//! handler runs the engine (`std/rules`) and formats the output.

use crate::cli::ScanArgs;

#[cfg(not(feature = "hostlib"))]
pub(crate) async fn run(_args: ScanArgs) {
    eprintln!(
        "`harn scan` requires the `hostlib` feature (default-on); it is unavailable in this build"
    );
    std::process::exit(2);
}

#[cfg(feature = "hostlib")]
pub(crate) async fn run(args: ScanArgs) {
    use crate::dispatch;
    use crate::env_guard::ScopedEnvVar;

    let plan = match build_plan(&args) {
        Ok(plan) => plan,
        Err(message) => {
            eprintln!("scan: {message}");
            std::process::exit(2);
        }
    };

    let _plan = ScopedEnvVar::set("HARN_SCAN_PLAN_JSON", &plan);
    let _report = ScopedEnvVar::set(
        "HARN_SCAN_REPORT_ONLY",
        if args.report_only { "1" } else { "0" },
    );
    let exit = dispatch::dispatch_to_embedded_script("scan", Vec::new(), args.json).await;
    if exit != 0 {
        std::process::exit(exit);
    }
}

#[cfg(feature = "hostlib")]
fn build_plan(args: &ScanArgs) -> Result<String, String> {
    use harn_hostlib::ast::Language;

    // With a saved rule/pack every positional is a path; otherwise the first
    // positional is the inline pattern and the rest are paths.
    let saved_rule = args.rule.is_some() || args.rule_pack.is_some();
    let (pattern, paths): (Option<&str>, &[String]) = if saved_rule {
        (None, &args.args)
    } else {
        (
            args.args.first().map(String::as_str),
            args.args.get(1..).unwrap_or(&[]),
        )
    };

    let specs = resolve_rules(args, pattern)?;
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

#[cfg(feature = "hostlib")]
struct RuleSpec {
    toml: String,
    language: harn_hostlib::ast::Language,
}

#[cfg(feature = "hostlib")]
fn resolve_rules(args: &ScanArgs, pattern: Option<&str>) -> Result<Vec<RuleSpec>, String> {
    use harn_hostlib::ast::Language;
    use std::fs;

    if let Some(pattern) = pattern {
        let lang_name = args
            .lang
            .as_deref()
            .ok_or("an inline pattern requires `--lang <language>`")?;
        let language = Language::from_name(lang_name)
            .ok_or_else(|| format!("unknown language `{lang_name}`"))?;
        let toml = format!(
            "id = \"scan\"\nlanguage = \"{}\"\n[rule]\npattern = \"{}\"\n",
            toml_escape(lang_name),
            toml_escape(pattern),
        );
        Ok(vec![RuleSpec { toml, language }])
    } else if let Some(rule_file) = &args.rule {
        let toml = fs::read_to_string(rule_file).map_err(|e| format!("read `{rule_file}`: {e}"))?;
        let language = rule_language(&toml)?;
        Ok(vec![RuleSpec { toml, language }])
    } else if let Some(dir) = &args.rule_pack {
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

/// Parse a rule TOML's declared `language` into a [`Language`].
#[cfg(feature = "hostlib")]
fn rule_language(src: &str) -> Result<harn_hostlib::ast::Language, String> {
    use harn_hostlib::ast::Language;

    let value: toml::Value = toml::from_str(src).map_err(|e| format!("invalid rule TOML: {e}"))?;
    let name = value
        .get("language")
        .and_then(|v| v.as_str())
        .ok_or("rule TOML is missing a top-level `language`")?;
    Language::from_name(name).ok_or_else(|| format!("unknown language `{name}`"))
}

/// Collect candidate files from `paths` (default: the current directory),
/// recursing directories with the gitignore-aware walker.
#[cfg(feature = "hostlib")]
fn collect_files(paths: &[String]) -> Vec<std::path::PathBuf> {
    use ignore::WalkBuilder;
    use std::path::{Path, PathBuf};

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
#[cfg(feature = "hostlib")]
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
