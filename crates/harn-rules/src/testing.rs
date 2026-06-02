//! A small rule-test harness (#2842).
//!
//! Run a rule against an **annotated fixture** and check that its matches line
//! up with inline `// ruleid:` / `// ok:` comments — the Semgrep convention,
//! adapted to be language-agnostic:
//!
//! ```text
//!   // ruleid: no-foo
//!   foo();              // <- must match `no-foo`
//!   // ok: no-foo
//!   bar();              // <- must NOT match `no-foo`
//!   baz();              // <- no annotation: must NOT match either
//! ```
//!
//! An annotation comment sits on its **own line** and targets the **next**
//! line. The check is strict: every match must be covered by a `// ruleid:`
//! (an un-annotated match is a false positive), and every `// ruleid:` line
//! must match (a missing match is a false negative).

use std::collections::{BTreeMap, BTreeSet};

use crate::engine::CompiledRule;
use crate::error::RulesError;

/// What an annotation asserts about the line it targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Expectation {
    /// `// ruleid: <id>` — the targeted line must match the rule.
    Match,
    /// `// ok: <id>` — the targeted line must not match.
    NoMatch,
}

/// Why a fixture line failed its expectation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureKind {
    /// `// ruleid:` annotated this line, but the rule did not match.
    ExpectedMatch,
    /// `// ok:` annotated this line, but the rule matched it.
    UnexpectedMatch,
    /// The rule matched this line, but nothing annotated it (false positive).
    Unannotated,
}

/// One failed expectation in a fixture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TestFailure {
    /// 0-based fixture line.
    pub line: usize,
    pub kind: FailureKind,
}

impl TestFailure {
    /// A human-readable, 1-based description.
    pub fn describe(&self) -> String {
        let what = match self.kind {
            FailureKind::ExpectedMatch => "expected a match (// ruleid:) but found none",
            FailureKind::UnexpectedMatch => "matched, but // ok: said it should not",
            FailureKind::Unannotated => "matched, but no // ruleid: annotated it",
        };
        format!("line {}: {what}", self.line + 1)
    }
}

/// The outcome of running one rule against one annotated fixture.
#[derive(Debug, Clone)]
pub struct InlineTestReport {
    pub rule_id: String,
    /// True when there are no failures.
    pub passed: bool,
    /// Number of annotations checked.
    pub checked: usize,
    /// Number of matches the rule produced.
    pub matches: usize,
    pub failures: Vec<TestFailure>,
}

/// Run `rule` against `source` and compare its matches with the fixture's
/// inline `// ruleid:` / `// ok:` annotations.
pub fn run_inline_test(rule: &CompiledRule, source: &str) -> Result<InlineTestReport, RulesError> {
    let expectations = parse_annotations(source, rule.id());

    let matched_lines: BTreeSet<usize> =
        rule.run(source)?.iter().map(|m| m.span.start_row).collect();

    let mut failures = Vec::new();

    // Every annotation must hold.
    for (&line, &expectation) in &expectations {
        match expectation {
            Expectation::Match if !matched_lines.contains(&line) => failures.push(TestFailure {
                line,
                kind: FailureKind::ExpectedMatch,
            }),
            Expectation::NoMatch if matched_lines.contains(&line) => failures.push(TestFailure {
                line,
                kind: FailureKind::UnexpectedMatch,
            }),
            _ => {}
        }
    }

    // Every match must be acknowledged by a `// ruleid:` (no false positives).
    for &line in &matched_lines {
        if expectations.get(&line) != Some(&Expectation::Match) {
            // A `// ok:` match is already reported as UnexpectedMatch above;
            // only flag the genuinely un-annotated ones here.
            if !expectations.contains_key(&line) {
                failures.push(TestFailure {
                    line,
                    kind: FailureKind::Unannotated,
                });
            }
        }
    }

    failures.sort_by_key(|f| (f.line, f.kind as u8));

    Ok(InlineTestReport {
        rule_id: rule.id().to_string(),
        passed: failures.is_empty(),
        checked: expectations.len(),
        matches: matched_lines.len(),
        failures,
    })
}

/// Parse `// ruleid:` / `// ok:` annotations that apply to `rule_id`. Each
/// annotation is on its own line and targets the **next** line (0-based). An
/// id list (`// ruleid: a, b`) applies if `rule_id` is among them.
fn parse_annotations(source: &str, rule_id: &str) -> BTreeMap<usize, Expectation> {
    let mut out = BTreeMap::new();
    for (i, line) in source.lines().enumerate() {
        let trimmed = line.trim_start();
        // Accept `//` (C-family) or `#` (Python/Ruby/shell) line comments.
        let Some(rest) = trimmed
            .strip_prefix("//")
            .or_else(|| trimmed.strip_prefix('#'))
        else {
            continue;
        };
        let rest = rest.trim_start();
        let (expectation, ids) = if let Some(ids) = rest.strip_prefix("ruleid:") {
            (Expectation::Match, ids)
        } else if let Some(ids) = rest.strip_prefix("ok:") {
            (Expectation::NoMatch, ids)
        } else {
            continue;
        };
        if ids.split(',').map(str::trim).any(|id| id == rule_id) {
            // Targets the next line; a trailing annotation at EOF targets
            // nothing and is harmless.
            out.insert(i + 1, expectation);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Rule;

    fn rule(toml: &str) -> CompiledRule {
        CompiledRule::compile(&Rule::from_toml_str(toml).unwrap()).unwrap()
    }

    const NO_FOO: &str = r#"
        id = "no-foo"
        language = "typescript"
        message = "no foo"
        [rule]
        pattern = "foo()"
    "#;

    #[test]
    fn passing_fixture_reports_no_failures() {
        let r = rule(NO_FOO);
        let src = "// ruleid: no-foo\nfoo();\n// ok: no-foo\nbar();\n";
        let report = run_inline_test(&r, src).unwrap();
        assert!(report.passed, "failures: {:?}", report.failures);
        assert_eq!(report.checked, 2);
        assert_eq!(report.matches, 1);
    }

    #[test]
    fn false_negative_is_reported() {
        // `// ruleid:` on a line that does not actually match.
        let r = rule(NO_FOO);
        let src = "// ruleid: no-foo\nbar();\n";
        let report = run_inline_test(&r, src).unwrap();
        assert!(!report.passed);
        assert_eq!(report.failures[0].kind, FailureKind::ExpectedMatch);
    }

    #[test]
    fn false_positive_on_ok_line_is_reported() {
        let r = rule(NO_FOO);
        let src = "// ok: no-foo\nfoo();\n";
        let report = run_inline_test(&r, src).unwrap();
        assert!(!report.passed);
        assert_eq!(report.failures[0].kind, FailureKind::UnexpectedMatch);
    }

    #[test]
    fn unannotated_match_is_a_false_positive() {
        let r = rule(NO_FOO);
        let src = "foo();\n";
        let report = run_inline_test(&r, src).unwrap();
        assert!(!report.passed);
        assert_eq!(report.failures[0].kind, FailureKind::Unannotated);
    }

    #[test]
    fn annotations_for_other_rules_are_ignored() {
        let r = rule(NO_FOO);
        // The annotation names a different rule, so it does not apply here;
        // and the matching line below is annotated for `no-foo`.
        let src = "// ruleid: other-rule\nbar();\n// ruleid: no-foo\nfoo();\n";
        let report = run_inline_test(&r, src).unwrap();
        assert!(report.passed, "failures: {:?}", report.failures);
        assert_eq!(report.checked, 1);
    }

    #[test]
    fn python_hash_comments_work() {
        let r = rule(
            r#"
            id = "call-print"
            language = "python"
            [rule]
            pattern = "print($X)"
        "#,
        );
        let src = "# ruleid: call-print\nprint(x)\n# ok: call-print\ny = 1\n";
        let report = run_inline_test(&r, src).unwrap();
        assert!(report.passed, "failures: {:?}", report.failures);
    }
}
