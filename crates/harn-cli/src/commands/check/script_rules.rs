//! `.harn`-authored custom lint rules — the ESLint-plugin equivalent, but in
//! Harn.
//!
//! A project drops a `*.lint.harn` module into a `[rules] ruleDirs` directory.
//! `harn lint` discovers it, runs its exported `pub fn lint(source)` over each
//! linted `.harn` file, and merges the returned findings into the normal lint
//! output — indistinguishable from a built-in rule (same exit code, same
//! report).
//!
//! ## The rule contract
//!
//! ```harn
//! // rules/no-foo.lint.harn
//! pub fn lint(source) {
//!   // Inspect `source` (the raw text of the file being linted) and return
//!   // findings. The structural rule engine is available read-only, so a rule
//!   // can delegate to `rules_diagnostics` and return its diagnostics envelope
//!   // directly:
//!   return rules_diagnostics({rule: rule_src, source: source, language: "harn"})
//! }
//! ```
//!
//! Each finding is a dict; the recognised fields mirror what `rules.diagnostics`
//! emits, so a rule can pass that output straight through:
//!
//! - `message` (required) — the diagnostic text. A finding without it is skipped.
//! - `severity` — `"error"` / `"warning"` / `"info"` (default `"warning"`).
//! - `line` / `column` — 1-based location (default `1` / `1`).
//! - `start_byte` / `end_byte` — byte span for the underline (default `0`).
//!
//! A rule can also return the dict emitted by `rules_diagnostics(...)`; its
//! `diagnostics` list is mapped onto the same lint output.
//!
//! ## Sandbox + fail-safe
//!
//! Rules run in a dedicated VM with the language, the standard library, and the
//! **read-only** structural rule engine — but *not* the default host I/O
//! (filesystem / network / process). A lint rule inspects source and returns
//! findings; it has no business touching the host.
//!
//! A buggy rule **fails safe**: a load error, a runtime throw, or a malformed
//! return becomes a diagnostic attributed to the rule, never a linter crash.

#[cfg(feature = "hostlib")]
mod imp {
    use std::collections::{HashMap, HashSet};
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    use harn_lexer::Span;
    use harn_lint::{LintDiagnostic, LintSeverity};
    use harn_parser::DiagnosticCode;
    use harn_vm::{VmClosure, VmValue};

    /// Suffix that marks a `.harn` module as an imperative lint rule.
    const RULE_SUFFIX: &str = ".lint.harn";

    /// A discovered rule, either loaded or failed-to-load. Both variants carry
    /// the rule id (the file stem minus `.lint`) so findings and load errors are
    /// attributable.
    enum LoadedRule {
        Ok { id: String, lint: Arc<VmClosure> },
        Failed { id: String, error: String },
    }

    /// Collect `*.lint.harn` files declared by `file`'s nearest manifest's
    /// `[rules] ruleDirs`. Mirrors `lint::project_engine_rule_sources` (the
    /// declarative-rule discovery) but for imperative `.harn` modules.
    fn discover_rule_paths(file: &Path) -> Vec<PathBuf> {
        let Some((manifest, dir)) = crate::package::find_nearest_manifest(file) else {
            return Vec::new();
        };
        let mut paths = Vec::new();
        for rel in &manifest.rules.rule_dirs {
            let Ok(entries) = std::fs::read_dir(dir.join(rel)) else {
                continue;
            };
            let mut files: Vec<_> = entries
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .filter(|p| {
                    p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.ends_with(RULE_SUFFIX))
                })
                .collect();
            files.sort();
            paths.extend(files);
        }
        paths
    }

    fn rule_id_for(path: &Path) -> String {
        path.file_name()
            .and_then(|n| n.to_str())
            .map(|n| n.trim_end_matches(RULE_SUFFIX).to_string())
            .filter(|stem| !stem.is_empty())
            .unwrap_or_else(|| "harn-script-rule".to_string())
    }

    /// Build the read-only sandbox VM: language + stdlib + the structural rule
    /// engine, but no default host I/O.
    fn sandbox_vm() -> harn_vm::Vm {
        let mut vm = harn_vm::Vm::new();
        harn_vm::register_vm_stdlib(&mut vm);
        harn_rules_hostlib::install(&mut vm);
        vm
    }

    /// A fail-safe diagnostic attributing an error to a rule. Anchored at the
    /// start of the file so it always renders.
    fn rule_error_diagnostic(id: &str, error: &str) -> LintDiagnostic {
        LintDiagnostic {
            code: DiagnosticCode::LintRuleEngine,
            rule: std::borrow::Cow::Owned(id.to_string()),
            message: format!("lint rule '{id}' failed: {error}"),
            span: Span {
                start: 0,
                end: 0,
                line: 1,
                column: 1,
                end_line: 1,
            },
            severity: LintSeverity::Error,
            suggestion: None,
            fix: None,
        }
    }

    fn severity_from(value: Option<&VmValue>) -> LintSeverity {
        match value.map(VmValue::as_str_cow).as_deref() {
            Some("error") => LintSeverity::Error,
            Some("info") => LintSeverity::Info,
            _ => LintSeverity::Warning,
        }
    }

    /// Map one finding dict onto a [`LintDiagnostic`], or `None` if it has no
    /// `message` (the one required field).
    fn finding_to_diagnostic(id: &str, finding: &VmValue) -> Option<LintDiagnostic> {
        let dict = finding.as_dict()?;
        let message = dict.get("message")?.as_str_cow().into_owned();
        if message.is_empty() {
            return None;
        }
        let optional_int = |key: &str| {
            dict.get(key)
                .and_then(VmValue::as_int)
                .filter(|n| *n >= 0)
                .map(|n| n as usize)
        };
        let line = optional_int("line")
            .or_else(|| optional_int("start_row").map(|row| row + 1))
            .unwrap_or(1)
            .max(1);
        let column = optional_int("column")
            .or_else(|| optional_int("start_col").map(|col| col + 1))
            .unwrap_or(1)
            .max(1);
        let end_line = optional_int("end_line")
            .or_else(|| optional_int("end_row").map(|row| row + 1))
            .unwrap_or(line)
            .max(line);
        let start = optional_int("start_byte").unwrap_or(0);
        let end = optional_int("end_byte").unwrap_or(start).max(start);
        let rule_id = dict
            .get("rule")
            .or_else(|| dict.get("rule_id"))
            .map(VmValue::as_str_cow)
            .filter(|rule| !rule.is_empty())
            .map(|rule| rule.into_owned())
            .unwrap_or_else(|| id.to_string());
        Some(LintDiagnostic {
            code: DiagnosticCode::LintRuleEngine,
            rule: std::borrow::Cow::Owned(rule_id),
            message,
            span: Span {
                start,
                end,
                line,
                column,
                end_line,
            },
            severity: severity_from(dict.get("severity")),
            suggestion: None,
            fix: None,
        })
    }

    fn map_findings(id: &str, items: &[VmValue], out: &mut Vec<LintDiagnostic>) {
        for item in items {
            if let Some(diag) = finding_to_diagnostic(id, item) {
                out.push(diag);
            }
        }
    }

    /// Map a rule's return value onto diagnostics. Rules can return a direct
    /// finding dict, a list of finding dicts, or the `rules_diagnostics(...)`
    /// envelope.
    fn map_return(id: &str, value: &VmValue, out: &mut Vec<LintDiagnostic>) {
        match value {
            VmValue::List(items) => map_findings(id, items, out),
            VmValue::Dict(dict) => {
                if dict.contains_key("message") {
                    if let Some(diag) = finding_to_diagnostic(id, value) {
                        out.push(diag);
                    }
                } else if let Some(VmValue::List(items)) = dict.get("diagnostics") {
                    map_findings(id, items, out);
                } else {
                    out.push(rule_error_diagnostic(
                        id,
                        "lint(source) returned a dict without `message` or a `diagnostics` list",
                    ));
                }
            }
            _ => out.push(rule_error_diagnostic(
                id,
                "lint(source) must return a finding, a list of findings, or a rules_diagnostics result",
            )),
        }
    }

    pub(crate) async fn run(files: &[PathBuf]) -> HashMap<PathBuf, Vec<LintDiagnostic>> {
        let mut out: HashMap<PathBuf, Vec<LintDiagnostic>> = HashMap::new();

        // Discover rules per target file. A single `harn lint` invocation can
        // span multiple packages, and a package's conventions must not leak
        // into sibling packages just because their files were linted together.
        let mut file_rule_paths: HashMap<PathBuf, Vec<PathBuf>> = HashMap::new();
        let mut unique_rule_paths: Vec<PathBuf> = Vec::new();
        let mut seen = HashSet::new();
        for file in files {
            let paths = discover_rule_paths(file);
            if paths.is_empty() {
                continue;
            }
            for path in &paths {
                if seen.insert(path.clone()) {
                    unique_rule_paths.push(path.clone());
                }
            }
            file_rule_paths.insert(file.clone(), paths);
        }
        if unique_rule_paths.is_empty() {
            return out; // Common path: no project rules — near-zero cost.
        }

        let mut vm = sandbox_vm();

        // Load each rule's `lint` closure once. A `*.lint.harn` without a `lint`
        // export is a no-op (skipped); a module that won't load fails safe.
        let mut loaded_rules: HashMap<PathBuf, LoadedRule> = HashMap::new();
        for path in &unique_rule_paths {
            let id = rule_id_for(path);
            let source = match std::fs::read_to_string(path) {
                Ok(source) => source,
                Err(error) => {
                    loaded_rules.insert(
                        path.clone(),
                        LoadedRule::Failed {
                            id,
                            error: error.to_string(),
                        },
                    );
                    continue;
                }
            };
            match vm
                .load_module_exports_from_source(path.clone(), &source)
                .await
            {
                Ok(exports) => {
                    if let Some(lint) = exports.get("lint") {
                        loaded_rules.insert(
                            path.clone(),
                            LoadedRule::Ok {
                                id,
                                lint: lint.clone(),
                            },
                        );
                    }
                }
                Err(error) => {
                    loaded_rules.insert(
                        path.clone(),
                        LoadedRule::Failed {
                            id,
                            error: error.to_string(),
                        },
                    );
                }
            }
        }
        if loaded_rules.is_empty() {
            return out;
        }

        for file in files {
            let Some(rule_paths) = file_rule_paths.get(file) else {
                continue;
            };
            let Ok(source) = std::fs::read_to_string(file) else {
                continue;
            };
            let mut diagnostics = Vec::new();
            for path in rule_paths {
                let Some(rule) = loaded_rules.get(path) else {
                    continue;
                };
                match rule {
                    LoadedRule::Failed { id, error } => {
                        diagnostics.push(rule_error_diagnostic(id, error));
                    }
                    LoadedRule::Ok { id, lint } => {
                        let arg = VmValue::String(Arc::from(source.as_str()));
                        match vm.call_closure_pub(lint, &[arg]).await {
                            Ok(value) => map_return(id, &value, &mut diagnostics),
                            Err(error) => {
                                diagnostics.push(rule_error_diagnostic(id, &error.to_string()));
                            }
                        }
                    }
                }
            }
            if !diagnostics.is_empty() {
                out.insert(file.clone(), diagnostics);
            }
        }
        out
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::collections::BTreeMap;

        fn finding(pairs: &[(&str, VmValue)]) -> VmValue {
            let mut map = BTreeMap::new();
            for (k, v) in pairs {
                map.insert((*k).to_string(), v.clone());
            }
            VmValue::dict(map)
        }

        fn s(text: &str) -> VmValue {
            VmValue::String(Arc::from(text))
        }

        #[test]
        fn maps_a_full_finding() {
            let d = finding_to_diagnostic(
                "no-todo",
                &finding(&[
                    ("message", s("nope")),
                    ("rule_id", s("delegated-rule")),
                    ("severity", s("error")),
                    ("line", VmValue::Int(7)),
                    ("column", VmValue::Int(3)),
                    ("start_byte", VmValue::Int(10)),
                    ("end_byte", VmValue::Int(14)),
                ]),
            )
            .expect("maps");
            assert_eq!(d.message, "nope");
            assert_eq!(d.severity, LintSeverity::Error);
            assert_eq!(d.rule.as_ref(), "delegated-rule");
            assert_eq!((d.span.line, d.span.column), (7, 3));
            assert_eq!((d.span.start, d.span.end), (10, 14));
        }

        #[test]
        fn maps_a_rules_diagnostics_envelope() {
            let diagnostics = VmValue::List(Arc::new(vec![finding(&[
                ("message", s("delegated")),
                ("rule_id", s("structural-rule")),
                ("severity", s("info")),
                ("start_row", VmValue::Int(4)),
                ("start_col", VmValue::Int(2)),
                ("end_row", VmValue::Int(5)),
            ])]));
            let mut out = Vec::new();
            map_return(
                "script-rule",
                &finding(&[("diagnostics", diagnostics)]),
                &mut out,
            );
            assert_eq!(out.len(), 1);
            assert_eq!(out[0].rule.as_ref(), "structural-rule");
            assert_eq!(out[0].severity, LintSeverity::Info);
            assert_eq!(
                (out[0].span.line, out[0].span.column, out[0].span.end_line),
                (5, 3, 6)
            );
        }

        #[test]
        fn maps_a_single_finding_dict() {
            let mut out = Vec::new();
            map_return(
                "script-rule",
                &finding(&[("message", s("single"))]),
                &mut out,
            );
            assert_eq!(out.len(), 1);
            assert_eq!(out[0].message, "single");
            assert_eq!(out[0].rule.as_ref(), "script-rule");
        }

        #[test]
        fn defaults_location_and_severity() {
            let d = finding_to_diagnostic("r", &finding(&[("message", s("m"))])).expect("maps");
            assert_eq!(d.severity, LintSeverity::Warning);
            assert_eq!((d.span.line, d.span.column), (1, 1));
        }

        #[test]
        fn a_finding_without_a_message_is_skipped() {
            assert!(finding_to_diagnostic("r", &finding(&[("severity", s("error"))])).is_none());
            assert!(finding_to_diagnostic("r", &finding(&[("message", s(""))])).is_none());
        }

        #[test]
        fn non_list_return_is_rule_error() {
            let mut out = Vec::new();
            map_return("r", &VmValue::Nil, &mut out);
            assert_eq!(out.len(), 1);
            assert_eq!(out[0].severity, LintSeverity::Error);
            assert!(out[0].message.contains("must return a finding"));
        }

        #[test]
        fn rule_error_is_an_error_diagnostic() {
            let d = rule_error_diagnostic("boom", "kaboom");
            assert_eq!(d.severity, LintSeverity::Error);
            assert!(d.message.contains("boom") && d.message.contains("kaboom"));
        }
    }
}

#[cfg(feature = "hostlib")]
pub(crate) use imp::run as run_project_script_rules;

/// Without the `hostlib` feature there is no VM to host `.harn` rules, so the
/// linter simply has none. Keeps the call site in `harn lint` unconditional.
#[cfg(not(feature = "hostlib"))]
pub(crate) async fn run_project_script_rules(
    _files: &[std::path::PathBuf],
) -> std::collections::HashMap<std::path::PathBuf, Vec<harn_lint::LintDiagnostic>> {
    std::collections::HashMap::new()
}
