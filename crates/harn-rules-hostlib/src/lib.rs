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
//! - `rules.diagnostics` (read-only) — run a **declarative** rule and return
//!   its [`harn_rules::Diagnostic`]s (message + severity + span + fix).
//! - `rules.visit` (read-only, **async**) — the **imperative** escape hatch:
//!   run a rule's matcher, then invoke a `.harn` visitor
//!   `on_match($node, $ctx)` once per match. The visitor returns its
//!   report(s) — `nil`/`false` to skip, a `{message, fix, safety}` dict, or
//!   a list of them — which the engine turns into diagnostics of the same
//!   shape `rules.diagnostics` emits. The visitor has full programmatic
//!   control (compute a message/fix from the captured metavars), which the
//!   declarative form cannot.
//! - `rules.apply` (write-gated) — apply a codemod rule's `fix`; writes only
//!   when `dry_run: false` *and* the rule is safe to auto-apply (or
//!   `allow_unsafe: true`). Shares the deterministic-tools gate with the
//!   other mutating builtins.
//!
//! A rule is passed as its TOML source (`rule`), so an agent can author and
//! run a rule — declarative *or* imperative — entirely from `.harn` without
//! recompiling the binary.
//!
//! ### Why `rules.visit` is async, and why it returns rather than mutates
//!
//! A *synchronous* hostlib builtin cannot call a `.harn` closure: the VM's
//! [`Vm::call_closure_pub`] is async-only. So the visitor is registered as an
//! **async** builtin (directly on the VM via [`Vm::register_async_builtin`],
//! bypassing the sync [`HostlibRegistry`]), which can obtain a child VM from
//! its [`AsyncBuiltinCtx`] and call back per match.
//!
//! The visitor **returns** its reports instead of calling a mutating
//! `ctx.report(...)`: Harn closures capture by value, so a Harn-side
//! accumulator could not collect across calls, and `VmValue` has no callable
//! variant that carries captured Rust state to embed a stateful `report`
//! method in `ctx`. Returning is both the sound option and the simpler one.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use harn_hostlib::ast::Language;
use harn_hostlib::tools::permissions::gated_handler;
use harn_hostlib::{
    BuiltinRegistry, HostlibCapability, HostlibError, HostlibRegistry, RegisteredBuiltin,
};
use harn_vm::{AsyncBuiltinCtx, Vm, VmError, VmValue};

use harn_rules::{
    data_table, Applicability, CompiledRule, Diagnostic, Rule, RuleMatch, Safety, Severity,
    SourceFile, Span,
};

const SEARCH: &str = "hostlib_rules_search";
const REPORT: &str = "hostlib_rules_report";
const DIAGNOSTICS: &str = "hostlib_rules_diagnostics";
const VISIT: &str = "hostlib_rules_visit";
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
        registry.register(RegisteredBuiltin {
            name: DIAGNOSTICS,
            module: "rules",
            method: "diagnostics",
            handler: Arc::new(diagnostics_run),
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
    // `rules.visit` invokes a `.harn` closure per match, which only an async
    // builtin can do (`call_closure_pub` is async). It is therefore registered
    // directly on the VM rather than through the sync `HostlibRegistry`.
    vm.register_async_builtin(VISIT, visit_run);
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

fn diagnostics_run(args: &[VmValue]) -> Result<VmValue, HostlibError> {
    let dict = first_dict(DIAGNOSTICS, args)?;
    let rule = compile_rule(DIAGNOSTICS, &dict)?;
    let files = load_files(DIAGNOSTICS, &dict)?;

    let mut diagnostics = Vec::new();
    for file in &files {
        for d in rule
            .diagnostics(&file.source)
            .map_err(|e| backend(DIAGNOSTICS, &e))?
        {
            diagnostics.push(diagnostic_vm(&file.path, &d));
        }
    }
    Ok(dict_vm([
        ("result", str_vm("ok")),
        ("diagnostic_count", VmValue::Int(diagnostics.len() as i64)),
        ("diagnostics", VmValue::List(Arc::new(diagnostics))),
    ]))
}

/// The imperative escape hatch (#2878): run the rule's matcher, then call the
/// `.harn` visitor `on_match($node, $ctx)` once per match. The visitor's
/// return value becomes diagnostics of the same shape `rules.diagnostics`
/// emits. Read-only — it never writes; the agent applies fixes itself.
async fn visit_run(ctx: AsyncBuiltinCtx, args: Vec<VmValue>) -> Result<VmValue, VmError> {
    let dict = first_dict(VISIT, &args).map_err(host_err)?;
    let rule = compile_rule(VISIT, &dict).map_err(host_err)?;
    let files = load_files(VISIT, &dict).map_err(host_err)?;
    let visitor = match dict.get("on_match") {
        Some(VmValue::Closure(c)) => c.clone(),
        _ => {
            return Err(VmError::Runtime(format!(
                "{VISIT}: `on_match` must be a function `fn(node, ctx)`"
            )))
        }
    };

    let default_severity = rule.severity();
    let default_safety = rule.safety();
    let rule_id = rule.id().to_string();

    let mut vm = ctx.child_vm();
    let mut diagnostics = Vec::new();
    for file in &files {
        let matches = rule
            .run(&file.source)
            .map_err(|e| host_err(backend(VISIT, &e)))?;
        let file_ctx = ctx_vm(&file.path, file.language, &file.source, &rule_id);
        for m in &matches {
            let node = node_vm(m);
            let ret = vm
                .call_closure_pub(&visitor, &[node, file_ctx.clone()])
                .await?;
            ctx.forward_output(&vm.take_output());
            for report in reports_from_return(ret) {
                diagnostics.push(report_to_diagnostic_vm(
                    &file.path,
                    &rule_id,
                    m.span,
                    report,
                    default_severity,
                    default_safety,
                ));
            }
        }
    }
    Ok(dict_vm([
        ("result", str_vm("ok")),
        ("diagnostic_count", VmValue::Int(diagnostics.len() as i64)),
        ("diagnostics", VmValue::List(Arc::new(diagnostics))),
    ]))
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

/// Lower a `HostlibError` into a `VmError` for the async `rules.visit` path
/// (which must return `VmError`, not `HostlibError`).
fn host_err(err: HostlibError) -> VmError {
    VmError::Runtime(err.to_string())
}

/// One report a `.harn` visitor returned for a single match. Every field is
/// optional: an empty report (e.g. the visitor returned `true`) flags the
/// match using the rule's own defaults.
#[derive(Default)]
struct ReportSpec {
    message: Option<String>,
    fix: Option<String>,
    safety: Option<Safety>,
    severity: Option<Severity>,
}

/// The `node` value handed to a visitor: the matched text, its metavar
/// captures, and its span.
fn node_vm(m: &RuleMatch) -> VmValue {
    let captures: BTreeMap<String, VmValue> = m
        .bindings
        .iter()
        .map(|(name, b)| (name.clone(), str_vm(&b.text)))
        .collect();
    dict_vm([
        ("text", str_vm(&m.text)),
        ("captures", VmValue::Dict(Arc::new(captures))),
        ("start_row", VmValue::Int(m.span.start_row as i64)),
        ("start_col", VmValue::Int(m.span.start_col as i64)),
        ("end_row", VmValue::Int(m.span.end_row as i64)),
        ("end_col", VmValue::Int(m.span.end_col as i64)),
    ])
}

/// The read-only `ctx` value handed to a visitor: where the match lives and
/// what produced it.
fn ctx_vm(path: &Path, language: Language, source: &str, rule_id: &str) -> VmValue {
    dict_vm([
        ("path", str_vm(path.display().to_string())),
        ("language", str_vm(language.name())),
        ("source", str_vm(source)),
        ("rule_id", str_vm(rule_id)),
    ])
}

/// Build a diagnostic dict — the one shape both `rules.diagnostics` and
/// `rules.visit` emit, so an equivalent declarative and imperative rule
/// produce identical output.
fn diagnostic_dict(
    path: &Path,
    rule_id: &str,
    message: &str,
    severity: Severity,
    span: Span,
    fix: Option<String>,
    applicability: Applicability,
) -> VmValue {
    dict_vm([
        ("path", str_vm(path.display().to_string())),
        ("rule_id", str_vm(rule_id)),
        ("message", str_vm(message)),
        ("severity", str_vm(severity.as_str())),
        ("start_row", VmValue::Int(span.start_row as i64)),
        ("start_col", VmValue::Int(span.start_col as i64)),
        ("end_row", VmValue::Int(span.end_row as i64)),
        ("end_col", VmValue::Int(span.end_col as i64)),
        ("applicability", str_vm(applicability.as_str())),
        ("fix", fix.map(str_vm).unwrap_or(VmValue::Nil)),
    ])
}

fn diagnostic_vm(path: &Path, d: &Diagnostic) -> VmValue {
    diagnostic_dict(
        path,
        &d.rule_id,
        &d.message,
        d.severity,
        d.span,
        d.fix.clone(),
        d.applicability,
    )
}

/// Turn a visitor's [`ReportSpec`] into the same diagnostic dict, located at
/// the match's span and falling back to the rule's defaults.
fn report_to_diagnostic_vm(
    path: &Path,
    rule_id: &str,
    span: Span,
    report: ReportSpec,
    default_severity: Severity,
    default_safety: Safety,
) -> VmValue {
    let severity = report.severity.unwrap_or(default_severity);
    let safety = report.safety.unwrap_or(default_safety);
    diagnostic_dict(
        path,
        rule_id,
        report.message.as_deref().unwrap_or(""),
        severity,
        span,
        report.fix,
        safety.applicability(),
    )
}

/// Interpret a visitor's return value: `nil`/`false` skips, `true` flags with
/// rule defaults, a dict is one report, a list is many (skipping `nil`/`false`
/// entries).
fn reports_from_return(ret: VmValue) -> Vec<ReportSpec> {
    match ret {
        VmValue::Nil | VmValue::Bool(false) => Vec::new(),
        VmValue::Bool(true) => vec![ReportSpec::default()],
        VmValue::Dict(d) => vec![report_from_dict(&d)],
        VmValue::List(items) => items.iter().filter_map(report_from_item).collect(),
        _ => Vec::new(),
    }
}

fn report_from_item(v: &VmValue) -> Option<ReportSpec> {
    match v {
        VmValue::Nil | VmValue::Bool(false) => None,
        VmValue::Bool(true) => Some(ReportSpec::default()),
        VmValue::Dict(d) => Some(report_from_dict(d)),
        _ => None,
    }
}

fn report_from_dict(d: &BTreeMap<String, VmValue>) -> ReportSpec {
    ReportSpec {
        message: optional_string(d, "message"),
        fix: optional_string(d, "fix"),
        safety: optional_string(d, "safety").and_then(|s| parse_safety(&s)),
        severity: optional_string(d, "severity").and_then(|s| parse_severity(&s)),
    }
}

fn parse_severity(s: &str) -> Option<Severity> {
    match s {
        "info" => Some(Severity::Info),
        "warning" => Some(Severity::Warning),
        "error" => Some(Severity::Error),
        _ => None,
    }
}

fn parse_safety(s: &str) -> Option<Safety> {
    match s {
        "format-only" => Some(Safety::FormatOnly),
        "behavior-preserving" => Some(Safety::BehaviorPreserving),
        "scope-local" => Some(Safety::ScopeLocal),
        "surface-changing" => Some(Safety::SurfaceChanging),
        "capability-changing" => Some(Safety::CapabilityChanging),
        "needs-human" => Some(Safety::NeedsHuman),
        _ => None,
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
    fn diagnostics_returns_lint_findings() {
        let lint = r#"
            id = "calls"
            language = "typescript"
            message = "function call"
            [rule]
            pattern = "$FN()"
        "#;
        let result = diagnostics_run(&[dict(&[
            ("rule", str_vm(lint)),
            ("source", str_vm("foo();\nbar();\n")),
            ("language", str_vm("typescript")),
            ("path", str_vm("a.ts")),
        ])])
        .unwrap();
        assert_eq!(int(get(&result, "diagnostic_count")), 2);
        let diags = match get(&result, "diagnostics") {
            VmValue::List(l) => l.clone(),
            _ => panic!(),
        };
        assert_eq!(s(get(&diags[0], "message")), "function call");
        assert_eq!(s(get(&diags[0], "severity")), "warning");
        // No `fix` and default safety → a suggestion, not machine-applicable.
        assert_eq!(s(get(&diags[0], "applicability")), "suggestion");
        assert_eq!(int(get(&diags[1], "start_row")), 1);
        assert!(matches!(get(&diags[0], "fix"), VmValue::Nil));
    }

    #[test]
    fn report_helpers_round_trip_severity_and_safety() {
        // The string<->enum mapping used by `rules.visit` reports.
        assert_eq!(parse_severity("error"), Some(Severity::Error));
        assert_eq!(parse_severity("bogus"), None);
        assert_eq!(parse_safety("format-only"), Some(Safety::FormatOnly));
        assert_eq!(parse_safety("needs-human"), Some(Safety::NeedsHuman));
        assert_eq!(parse_safety("nope"), None);
        // `true` flags with defaults; nil/false skip; a dict carries fields.
        assert_eq!(reports_from_return(VmValue::Bool(true)).len(), 1);
        assert_eq!(reports_from_return(VmValue::Nil).len(), 0);
        assert_eq!(reports_from_return(VmValue::Bool(false)).len(), 0);
        let list = VmValue::List(Arc::new(vec![
            dict(&[("message", str_vm("a"))]),
            VmValue::Nil,
            dict(&[("message", str_vm("b"))]),
        ]));
        assert_eq!(reports_from_return(list).len(), 2);
    }

    #[test]
    fn capability_does_not_register_the_async_visitor() {
        // `rules.visit` is async, so it is installed directly on the VM in
        // `install`, not through the sync capability registry.
        let mut registry = BuiltinRegistry::new();
        RulesCapability.register_builtins(&mut registry);
        let names: Vec<_> = registry.iter().map(|b| b.name).collect();
        assert!(!names.contains(&VISIT));
        assert!(names.contains(&DIAGNOSTICS));
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
    fn capability_registers_the_sync_builtins() {
        let mut registry = BuiltinRegistry::new();
        RulesCapability.register_builtins(&mut registry);
        let names: Vec<_> = registry.iter().map(|b| b.name).collect();
        assert_eq!(names, vec![SEARCH, REPORT, DIAGNOSTICS, APPLY]);
    }
}
