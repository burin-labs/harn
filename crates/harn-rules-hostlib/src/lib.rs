//! Host capability exposing the `harn-rules` declarative rule engine to
//! Harn as `rules.search` / `rules.report` / `rules.apply`.
//!
//! This crate lives outside `harn-hostlib` on purpose: `harn-rules` already
//! depends on `harn-hostlib` (for the tree-sitter grammars), so the rules
//! builtins would form a dependency cycle if they lived there. An embedder
//! (harn-cli, harn-serve) calls [`install`] alongside `harn_hostlib::install_default`.
//!
//! ## Builtins
//!
//! - `rules.search` (read-only) — run a rule and return its matches.
//! - `rules.report` (read-only) — run a rule in report-only mode and return
//!   a [`harn_rules::DataTable`] (counts + per-match rows).
//! - `rules.apply` (write-gated) — apply a codemod rule's `fix`; writes only
//!   when `dry_run: false` *and* the rule is safe to auto-apply (or
//!   `allow_unsafe: true`). Shares the deterministic-tools gate with the
//!   other mutating builtins.
//!
//! A rule is passed as its TOML source (`rule`), so an agent can author and
//! run a rule entirely from `.harn` without recompiling the binary. The
//! richer **imperative** `.harn` visitor (`on_match($node, ctx)`) needs a
//! synchronous closure-callback from a Rust builtin, which the VM does not
//! support today (only async builtins can call back) — tracked separately.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use harn_hostlib::ast::Language;
use harn_hostlib::tools::permissions::gated_handler;
use harn_hostlib::{
    BuiltinRegistry, HostlibCapability, HostlibError, HostlibRegistry, RegisteredBuiltin,
};
use harn_vm::{Vm, VmValue};

use harn_rules::{data_table, CompiledRule, Rule, RuleMatch, SourceFile};

const SEARCH: &str = "hostlib_rules_search";
const REPORT: &str = "hostlib_rules_report";
const APPLY: &str = "hostlib_rules_apply";

/// The `rules` host capability.
#[derive(Default)]
pub struct RulesCapability;

impl HostlibCapability for RulesCapability {
    fn module_name(&self) -> &'static str {
        "rules"
    }

    fn register_builtins(&self, registry: &mut BuiltinRegistry) {
        registry.register(RegisteredBuiltin {
            name: SEARCH,
            module: "rules",
            method: "search",
            handler: Arc::new(search_run),
        });
        registry.register(RegisteredBuiltin {
            name: REPORT,
            module: "rules",
            method: "report",
            handler: Arc::new(report_run),
        });
        // `apply` writes files, so it shares the deterministic-tools gate.
        registry.register(RegisteredBuiltin {
            name: APPLY,
            module: "rules",
            method: "apply",
            handler: gated_handler(APPLY, apply_run),
        });
    }
}

/// Install the `rules` capability into a VM. Call this alongside
/// `harn_hostlib::install_default`.
pub fn install(vm: &mut Vm) {
    HostlibRegistry::new()
        .with(RulesCapability)
        .register_into_vm(vm);
}

// ---------------------------------------------------------------------------
// Builtins
// ---------------------------------------------------------------------------

fn search_run(args: &[VmValue]) -> Result<VmValue, HostlibError> {
    let dict = first_dict(SEARCH, args)?;
    let rule = compile_rule(SEARCH, &dict)?;
    let files = load_files(SEARCH, &dict)?;

    let mut matches = Vec::new();
    for file in &files {
        for m in rule.run(&file.source).map_err(|e| backend(SEARCH, &e))? {
            matches.push(match_to_vm(&file.path, &m));
        }
    }
    Ok(dict_vm([
        ("result", str_vm("ok")),
        ("match_count", VmValue::Int(matches.len() as i64)),
        ("matches", VmValue::List(Arc::new(matches))),
    ]))
}

fn report_run(args: &[VmValue]) -> Result<VmValue, HostlibError> {
    let dict = first_dict(REPORT, args)?;
    let rule = compile_rule(REPORT, &dict)?;
    let files = load_files(REPORT, &dict)?;
    let table = data_table(&rule, &files).map_err(|e| backend(REPORT, &e))?;
    Ok(json_to_vm(&table.to_json_value()))
}

fn apply_run(args: &[VmValue]) -> Result<VmValue, HostlibError> {
    let dict = first_dict(APPLY, args)?;
    let rule = compile_rule(APPLY, &dict)?;
    let dry_run = optional_bool(&dict, "dry_run", true);
    let allow_unsafe = optional_bool(&dict, "allow_unsafe", false);
    let files = load_files(APPLY, &dict)?;

    let auto_applicable = rule.safety().is_auto_applicable();
    let mut entries = Vec::new();
    for file in &files {
        let outcome = rule.apply(&file.source).map_err(|e| backend(APPLY, &e))?;
        // Write only on a real apply, when the edit is safe to auto-apply
        // (or explicitly allowed), and the rule actually changed the file.
        let applied = !dry_run && outcome.changed && (auto_applicable || allow_unsafe);
        if applied {
            std::fs::write(&file.path, &outcome.rewritten).map_err(|e| HostlibError::Backend {
                builtin: APPLY,
                message: format!("write `{}`: {e}", file.path.display()),
            })?;
        }
        entries.push(dict_vm([
            ("path", str_vm(file.path.display().to_string())),
            ("changed", VmValue::Bool(outcome.changed)),
            ("applied", VmValue::Bool(applied)),
            ("idempotent", VmValue::Bool(outcome.idempotent)),
            ("safety", str_vm(format!("{:?}", outcome.safety))),
            ("preview", str_vm(outcome.rewritten)),
        ]));
    }
    Ok(dict_vm([
        ("result", str_vm("ok")),
        ("dry_run", VmValue::Bool(dry_run)),
        ("auto_applicable", VmValue::Bool(auto_applicable)),
        ("files", VmValue::List(Arc::new(entries))),
    ]))
}

// ---------------------------------------------------------------------------
// Shared parsing / conversion
// ---------------------------------------------------------------------------

fn compile_rule(
    builtin: &'static str,
    dict: &BTreeMap<String, VmValue>,
) -> Result<CompiledRule, HostlibError> {
    let toml = require_string(builtin, dict, "rule")?;
    let rule = Rule::from_toml_str(&toml).map_err(|e| HostlibError::InvalidParameter {
        builtin,
        param: "rule",
        message: format!("invalid rule TOML: {e}"),
    })?;
    CompiledRule::compile(&rule).map_err(|e| HostlibError::InvalidParameter {
        builtin,
        param: "rule",
        message: e.to_string(),
    })
}

/// Load the fileset: inline `source` (+ `language`) for a single buffer, or
/// `paths` read from disk (language inferred per file; undetectable files
/// are skipped).
fn load_files(
    builtin: &'static str,
    dict: &BTreeMap<String, VmValue>,
) -> Result<Vec<SourceFile>, HostlibError> {
    if let Some(source) = optional_string(dict, "source") {
        let language_name = require_string(builtin, dict, "language")?;
        let language =
            Language::from_name(&language_name).ok_or_else(|| HostlibError::InvalidParameter {
                builtin,
                param: "language",
                message: format!("unknown language `{language_name}`"),
            })?;
        let path = optional_string(dict, "path").unwrap_or_else(|| "<inline>".to_string());
        return Ok(vec![SourceFile {
            path: PathBuf::from(path),
            language,
            source,
        }]);
    }

    let paths = optional_string_list(dict, "paths");
    if paths.is_empty() {
        return Err(HostlibError::MissingParameter {
            builtin,
            param: "paths",
        });
    }
    let mut files = Vec::new();
    for path in paths {
        let contents = std::fs::read_to_string(&path).map_err(|e| HostlibError::Backend {
            builtin,
            message: format!("read `{path}`: {e}"),
        })?;
        if let Some(file) = SourceFile::detect(&path, contents) {
            files.push(file);
        }
    }
    Ok(files)
}

fn match_to_vm(path: &std::path::Path, m: &RuleMatch) -> VmValue {
    let captures: BTreeMap<String, VmValue> = m
        .bindings
        .iter()
        .map(|(name, b)| (name.clone(), str_vm(&b.text)))
        .collect();
    dict_vm([
        ("path", str_vm(path.display().to_string())),
        ("text", str_vm(&m.text)),
        ("start_row", VmValue::Int(m.span.start_row as i64)),
        ("start_col", VmValue::Int(m.span.start_col as i64)),
        ("end_row", VmValue::Int(m.span.end_row as i64)),
        ("end_col", VmValue::Int(m.span.end_col as i64)),
        ("captures", VmValue::Dict(Arc::new(captures))),
    ])
}

fn backend(builtin: &'static str, err: &harn_rules::RulesError) -> HostlibError {
    HostlibError::Backend {
        builtin,
        message: err.to_string(),
    }
}

fn json_to_vm(value: &serde_json::Value) -> VmValue {
    match value {
        serde_json::Value::Null => VmValue::Nil,
        serde_json::Value::Bool(b) => VmValue::Bool(*b),
        serde_json::Value::Number(n) => n
            .as_i64()
            .map(VmValue::Int)
            .unwrap_or_else(|| VmValue::Float(n.as_f64().unwrap_or(0.0))),
        serde_json::Value::String(s) => str_vm(s),
        serde_json::Value::Array(items) => {
            VmValue::List(Arc::new(items.iter().map(json_to_vm).collect()))
        }
        serde_json::Value::Object(map) => VmValue::Dict(Arc::new(
            map.iter()
                .map(|(k, v)| (k.clone(), json_to_vm(v)))
                .collect(),
        )),
    }
}

// ---------------------------------------------------------------------------
// Minimal arg/value helpers (harn-hostlib's `tools::args` is crate-private)
// ---------------------------------------------------------------------------

fn first_dict(
    builtin: &'static str,
    args: &[VmValue],
) -> Result<Arc<BTreeMap<String, VmValue>>, HostlibError> {
    match args.first() {
        Some(VmValue::Dict(dict)) => Ok(dict.clone()),
        Some(VmValue::Nil) | None => Ok(Arc::new(BTreeMap::new())),
        Some(_) => Err(HostlibError::InvalidParameter {
            builtin,
            param: "params",
            message: "expected a dict argument".into(),
        }),
    }
}

fn require_string(
    builtin: &'static str,
    dict: &BTreeMap<String, VmValue>,
    key: &'static str,
) -> Result<String, HostlibError> {
    match dict.get(key) {
        Some(VmValue::String(s)) => Ok(s.to_string()),
        _ => Err(HostlibError::MissingParameter {
            builtin,
            param: key,
        }),
    }
}

fn optional_string(dict: &BTreeMap<String, VmValue>, key: &str) -> Option<String> {
    match dict.get(key) {
        Some(VmValue::String(s)) => Some(s.to_string()),
        _ => None,
    }
}

fn optional_string_list(dict: &BTreeMap<String, VmValue>, key: &str) -> Vec<String> {
    match dict.get(key) {
        Some(VmValue::List(items)) => items
            .iter()
            .filter_map(|v| match v {
                VmValue::String(s) => Some(s.to_string()),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn optional_bool(dict: &BTreeMap<String, VmValue>, key: &str, default: bool) -> bool {
    match dict.get(key) {
        Some(VmValue::Bool(b)) => *b,
        _ => default,
    }
}

fn str_vm(s: impl AsRef<str>) -> VmValue {
    VmValue::String(Arc::from(s.as_ref()))
}

fn dict_vm<const N: usize>(entries: [(&str, VmValue); N]) -> VmValue {
    let map: BTreeMap<String, VmValue> = entries
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
    VmValue::Dict(Arc::new(map))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dict(pairs: &[(&str, VmValue)]) -> VmValue {
        let map: BTreeMap<String, VmValue> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect();
        VmValue::Dict(Arc::new(map))
    }

    fn get<'a>(v: &'a VmValue, key: &str) -> &'a VmValue {
        match v {
            VmValue::Dict(d) => d.get(key).unwrap_or_else(|| panic!("missing {key}")),
            _ => panic!("not a dict"),
        }
    }

    fn int(v: &VmValue) -> i64 {
        match v {
            VmValue::Int(i) => *i,
            other => panic!("not int: {other:?}"),
        }
    }

    fn s(v: &VmValue) -> String {
        match v {
            VmValue::String(s) => s.to_string(),
            other => panic!("not string: {other:?}"),
        }
    }

    fn b(v: &VmValue) -> bool {
        match v {
            VmValue::Bool(b) => *b,
            other => panic!("not bool: {other:?}"),
        }
    }

    const SEARCH_RULE: &str = r#"
        id = "find-calls"
        language = "typescript"
        [rule]
        pattern = "$FN()"
    "#;

    #[test]
    fn search_returns_matches_with_captures() {
        let result = search_run(&[dict(&[
            ("rule", str_vm(SEARCH_RULE)),
            ("source", str_vm("foo();\nbar();\n")),
            ("language", str_vm("typescript")),
        ])])
        .unwrap();
        assert_eq!(int(get(&result, "match_count")), 2);
        let matches = match get(&result, "matches") {
            VmValue::List(l) => l.clone(),
            _ => panic!(),
        };
        assert_eq!(s(get(get(&matches[0], "captures"), "FN")), "foo");
    }

    #[test]
    fn report_returns_a_data_table() {
        let result = report_run(&[dict(&[
            ("rule", str_vm(SEARCH_RULE)),
            ("source", str_vm("foo();\nbar();\n")),
            ("language", str_vm("typescript")),
            ("path", str_vm("a.ts")),
        ])])
        .unwrap();
        assert_eq!(int(get(get(&result, "summary"), "total_rows")), 2);
        assert_eq!(s(get(&result, "rule_id")), "find-calls");
    }

    #[test]
    fn apply_dry_run_previews_without_writing() {
        let rule = r#"
            id = "rename"
            language = "typescript"
            safety = "behavior-preserving"
            fix = "bar()"
            [rule]
            pattern = "foo()"
        "#;
        let result = apply_run(&[dict(&[
            ("rule", str_vm(rule)),
            ("source", str_vm("foo();\n")),
            ("language", str_vm("typescript")),
            ("dry_run", VmValue::Bool(true)),
        ])])
        .unwrap();
        let files = match get(&result, "files") {
            VmValue::List(l) => l.clone(),
            _ => panic!(),
        };
        assert!(b(get(&files[0], "changed")));
        assert!(!b(get(&files[0], "applied")));
        assert_eq!(s(get(&files[0], "preview")), "bar();\n");
    }

    #[test]
    fn missing_rule_is_an_error() {
        let err = search_run(&[dict(&[
            ("source", str_vm("x")),
            ("language", str_vm("rust")),
        ])]);
        assert!(matches!(
            err,
            Err(HostlibError::MissingParameter { param: "rule", .. })
        ));
    }

    #[test]
    fn capability_registers_three_builtins() {
        let mut registry = BuiltinRegistry::new();
        RulesCapability.register_builtins(&mut registry);
        let names: Vec<_> = registry.iter().map(|b| b.name).collect();
        assert_eq!(names, vec![SEARCH, REPORT, APPLY]);
    }
}
