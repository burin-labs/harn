//! `.harn`-authored custom lint rules (#2850) — the ESLint-plugin equivalent,
//! but in Harn.
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
//! pub fn lint(source) -> list {
//!   // Inspect `source` (the raw text of the file being linted) and return a
//!   // list of findings. The structural rule engine is available read-only,
//!   // so a rule can delegate to `rules.diagnostics` / `rules.search` and
//!   // return their output directly:
//!   return rules_diagnostics(source, "<rule toml>") ?? []
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
//! ## Sandbox + fail-safe
//!
//! Rules run in a dedicated VM with the language, the standard library, and the
//! **read-only** structural rule engine — but *not* the default host I/O
//! (filesystem / network / process). A lint rule inspects source and returns
//! findings; it has no business touching the host.
//!
//! A buggy rule **fails safe**: a load error, a runtime throw, or a malformed
//! return becomes a diagnostic attributed to the rule, never a linter crash
//! (the C1 acceptance criterion).

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
        let int = |key: &str, default: usize| {
            dict.get(key)
                .and_then(VmValue::as_int)
                .filter(|n| *n >= 0)
                .map(|n| n as usize)
                .unwrap_or(default)
        };
        let line = int("line", 1).max(1);
        let column = int("column", 1).max(1);
        let start = int("start_byte", 0);
        let end = int("end_byte", start).max(start);
        Some(LintDiagnostic {
            code: DiagnosticCode::LintRuleEngine,
            rule: std::borrow::Cow::Owned(id.to_string()),
            message,
            span: Span {
                start,
                end,
                line,
                column,
                end_line: line,
            },
            severity: severity_from(dict.get("severity")),
            suggestion: None,
            fix: None,
        })
    }

    /// Map a rule's return value (expected: a list of finding dicts) onto
    /// diagnostics. A non-list return (e.g. `nil`) yields nothing.
    fn map_return(id: &str, value: &VmValue, out: &mut Vec<LintDiagnostic>) {
        if let VmValue::List(items) = value {
            for item in items.iter() {
                if let Some(diag) = finding_to_diagnostic(id, item) {
                    out.push(diag);
                }
            }
        }
    }

    pub(crate) async fn run(files: &[PathBuf]) -> HashMap<PathBuf, Vec<LintDiagnostic>> {
        let mut out: HashMap<PathBuf, Vec<LintDiagnostic>> = HashMap::new();

        // The union of rule files across all targets. Most targets share a
        // manifest, so this is usually a single small directory listing.
        let mut rule_paths: Vec<PathBuf> = Vec::new();
        let mut seen = HashSet::new();
        for file in files {
            for path in discover_rule_paths(file) {
                if seen.insert(path.clone()) {
                    rule_paths.push(path);
                }
            }
        }
        if rule_paths.is_empty() {
            return out; // Common path: no project rules — near-zero cost.
        }

        let mut vm = sandbox_vm();

        // Load each rule's `lint` closure once. A `*.lint.harn` without a `lint`
        // export is a no-op (skipped); a module that won't load fails safe.
        let mut rules: Vec<LoadedRule> = Vec::new();
        for path in &rule_paths {
            let id = rule_id_for(path);
            let Ok(source) = std::fs::read_to_string(path) else {
                continue;
            };
            match vm
                .load_module_exports_from_source(path.clone(), &source)
                .await
            {
                Ok(exports) => {
                    if let Some(lint) = exports.get("lint") {
                        rules.push(LoadedRule::Ok {
                            id,
                            lint: lint.clone(),
                        });
                    }
                }
                Err(error) => rules.push(LoadedRule::Failed {
                    id,
                    error: error.to_string(),
                }),
            }
        }
        if rules.is_empty() {
            return out;
        }

        for file in files {
            let Ok(source) = std::fs::read_to_string(file) else {
                continue;
            };
            let mut diagnostics = Vec::new();
            for rule in &rules {
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
            VmValue::Dict(Arc::new(map))
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
            assert_eq!(d.rule.as_ref(), "no-todo");
            assert_eq!((d.span.line, d.span.column), (7, 3));
            assert_eq!((d.span.start, d.span.end), (10, 14));
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
        fn non_list_return_yields_nothing() {
            let mut out = Vec::new();
            map_return("r", &VmValue::Nil, &mut out);
            assert!(out.is_empty());
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
