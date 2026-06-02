//! Adapter (#2849) that runs a declarative `harn-rules` engine rule as a
//! `harn-lint` [`Rule`], so structural rules loaded from a project's
//! `[rules] ruleDirs` show up in `harn lint` output indistinguishably from
//! built-in rules — same `disable_rules` filtering, same `--fix` plumbing.
//!
//! An engine rule matches via tree-sitter over the raw source, so the adapter
//! runs in the whole-program phase: given the source text (`RuleCtx::source`),
//! it calls [`harn_rules::CompiledRule::diagnostics`] and maps each engine
//! [`harn_rules::Diagnostic`] onto a [`LintDiagnostic`].

use std::borrow::Cow;

use harn_lexer::{FixEdit, Span};
use harn_parser::{DiagnosticCode, SNode};
use harn_rules::{CompiledRule, Rule as EngineRuleModel, Severity as EngineSeverity};

use crate::diagnostic::{LintDiagnostic, LintSeverity};
use crate::rule::{Rule, RuleCtx};

/// A compiled engine rule wrapped as a lint rule.
pub(crate) struct EngineRule {
    id: String,
    compiled: CompiledRule,
}

impl EngineRule {
    /// Compile an engine rule from its TOML source. Returns `None` when the
    /// source is not a valid rule, so a malformed project rule degrades to
    /// "not loaded" rather than breaking the whole lint run.
    pub(crate) fn from_toml(source: &str) -> Option<Self> {
        let model = EngineRuleModel::from_toml_str(source).ok()?;
        let compiled = CompiledRule::compile(&model).ok()?;
        let id = compiled.id().to_string();
        Some(Self { id, compiled })
    }
}

/// Convert an engine span (byte offsets + 0-based row/col) to a lexer span
/// (byte offsets + 1-based line/column).
fn to_lexer_span(span: &harn_rules::Span) -> Span {
    Span {
        start: span.start_byte,
        end: span.end_byte,
        line: span.start_row + 1,
        column: span.start_col + 1,
        end_line: span.end_row + 1,
    }
}

impl Rule for EngineRule {
    fn id(&self) -> &str {
        &self.id
    }

    fn check_program(
        &mut self,
        _program: &[SNode],
        ctx: &RuleCtx<'_>,
        out: &mut Vec<LintDiagnostic>,
    ) {
        // Engine rules match raw source; without it there is nothing to do.
        let Some(source) = ctx.source else {
            return;
        };
        // A parse/match error means this rule simply contributes nothing —
        // the parser reports real syntax errors through its own channel.
        let Ok(diagnostics) = self.compiled.diagnostics(source) else {
            return;
        };
        for diag in diagnostics {
            let span = to_lexer_span(&diag.span);
            out.push(LintDiagnostic {
                code: DiagnosticCode::LintRuleEngine,
                rule: Cow::Owned(diag.rule_id),
                message: diag.message,
                span,
                severity: match diag.severity {
                    EngineSeverity::Info => LintSeverity::Info,
                    EngineSeverity::Warning => LintSeverity::Warning,
                    EngineSeverity::Error => LintSeverity::Error,
                },
                suggestion: None,
                fix: diag
                    .fix
                    .map(|replacement| vec![FixEdit { span, replacement }]),
            });
        }
    }
}
