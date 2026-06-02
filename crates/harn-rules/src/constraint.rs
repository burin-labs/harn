//! `where` constraints: predicates on captured metavars (Semgrep
//! `metavariable-regex` / `metavariable-comparison` / `metavariable-pattern`).
//!
//! A match survives only when every constraint holds. Constraints are
//! compiled once (regex compiled, sub-pattern lowered to a tree-sitter
//! query) and evaluated against each match's metavar bindings.

use regex::Regex;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Query, QueryCursor};

use harn_hostlib::ast::{api, Language};

use crate::engine::{Binding, ResolvedBinding};
use crate::error::RulesError;
use crate::model::{Constraint, ResolvedBindingConstraint};
use crate::pattern::compile_pattern;

/// A compiled `where` constraint bound to one metavar.
pub struct CompiledConstraint {
    /// The metavar this constraint filters on (without `$`).
    pub metavar: String,
    kind: Kind,
}

enum Kind {
    Regex(Regex),
    Comparison { op: CmpOp, value: toml::Value },
    SubPattern { language: Language, query: Query },
    ResolvesTo(ResolvedBindingConstraint),
    Type(String),
}

#[derive(Clone, Copy)]
enum CmpOp {
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
    Ne,
}

impl CmpOp {
    fn parse(op: &str) -> Option<Self> {
        Some(match op {
            "<" => CmpOp::Lt,
            "<=" => CmpOp::Le,
            ">" => CmpOp::Gt,
            ">=" => CmpOp::Ge,
            "==" => CmpOp::Eq,
            "!=" => CmpOp::Ne,
            _ => return None,
        })
    }
}

impl CompiledConstraint {
    /// Compile a constraint. `default_language` is the rule's language,
    /// used for a sub-pattern that does not name its own.
    pub fn compile(
        rule_id: &str,
        default_language: Language,
        constraint: &Constraint,
    ) -> Result<Self, RulesError> {
        let err = |message: String| RulesError::PatternCompile {
            rule: rule_id.to_string(),
            message,
        };

        let set = [
            constraint.regex.is_some(),
            constraint.comparison.is_some(),
            constraint.pattern.is_some(),
            constraint.resolves_to.is_some(),
            constraint.type_.is_some(),
        ]
        .into_iter()
        .filter(|b| *b)
        .count();
        if set != 1 {
            return Err(err(format!(
                "where-constraint on `{}` must set exactly one of `regex` / `comparison` / `pattern` / `resolves_to` / `type`",
                constraint.metavar
            )));
        }

        let kind = if let Some(re) = &constraint.regex {
            Kind::Regex(
                Regex::new(re)
                    .map_err(|e| err(format!("constraint regex `{re}` is invalid: {e}")))?,
            )
        } else if let Some(cmp) = &constraint.comparison {
            let op = CmpOp::parse(&cmp.op)
                .ok_or_else(|| err(format!("unknown comparison operator `{}`", cmp.op)))?;
            Kind::Comparison {
                op,
                value: cmp.value.clone(),
            }
        } else if let Some(snippet) = &constraint.pattern {
            let language = match &constraint.language {
                Some(name) => Language::from_name(name)
                    .ok_or_else(|| err(format!("unknown sub-pattern language `{name}`")))?,
                None => default_language,
            };
            let ts_language = language
                .ts_language()
                .ok_or_else(|| err(format!("grammar for `{}` is unavailable", language.name())))?;
            let compiled = compile_pattern(snippet, language)
                .map_err(|m| err(format!("sub-pattern on `{}`: {m}", constraint.metavar)))?;
            let query = Query::new(&ts_language, &compiled.query)
                .map_err(|e| err(format!("sub-pattern query rejected: {e}")))?;
            Kind::SubPattern { language, query }
        } else if let Some(resolves_to) = &constraint.resolves_to {
            if default_language != Language::Harn {
                return Err(err(format!(
                    "`resolves_to` on `{}` is only supported for Harn rules",
                    constraint.metavar
                )));
            }
            if resolves_to.id.is_none()
                && resolves_to.name.is_none()
                && resolves_to.kind.is_none()
                && resolves_to.line.is_none()
                && resolves_to.column.is_none()
            {
                return Err(err(format!(
                    "`resolves_to` on `{}` must set at least one identity field",
                    constraint.metavar
                )));
            }
            Kind::ResolvesTo(resolves_to.clone())
        } else {
            if default_language != Language::Harn {
                return Err(err(format!(
                    "`type` on `{}` is only supported for Harn rules",
                    constraint.metavar
                )));
            }
            let expected = constraint.type_.as_ref().unwrap();
            if expected.trim().is_empty() {
                return Err(err(format!(
                    "`type` on `{}` must not be empty",
                    constraint.metavar
                )));
            }
            Kind::Type(expected.clone())
        };

        Ok(CompiledConstraint {
            metavar: constraint.metavar.clone(),
            kind,
        })
    }

    /// Evaluate the constraint against a metavar binding.
    pub fn evaluate(&self, binding: &Binding) -> bool {
        match &self.kind {
            Kind::Regex(re) => re.is_match(&binding.text),
            Kind::Comparison { op, value } => evaluate_comparison(*op, &binding.text, value),
            Kind::SubPattern { language, query } => {
                let Ok(tree) = api::parse_tree(&binding.text, *language) else {
                    return false;
                };
                let mut cursor = QueryCursor::new();
                let mut it = cursor.matches(query, tree.root_node(), binding.text.as_bytes());
                it.next().is_some()
            }
            Kind::ResolvesTo(expected) => binding
                .metadata
                .resolved
                .as_ref()
                .is_some_and(|actual| resolved_matches(expected, actual)),
            Kind::Type(expected) => binding
                .metadata
                .ty
                .as_ref()
                .is_some_and(|actual| actual == expected),
        }
    }
}

fn resolved_matches(expected: &ResolvedBindingConstraint, actual: &ResolvedBinding) -> bool {
    expected.id.as_ref().is_none_or(|id| id == &actual.id)
        && expected
            .name
            .as_ref()
            .is_none_or(|name| name == &actual.name)
        && expected
            .kind
            .as_ref()
            .is_none_or(|kind| kind == &actual.kind)
        && expected
            .line
            .is_none_or(|line| line == actual.span.start_row + 1)
        && expected
            .column
            .is_none_or(|column| column == actual.span.start_col + 1)
}

fn evaluate_comparison(op: CmpOp, text: &str, value: &toml::Value) -> bool {
    // Numeric comparison when the RHS is a number and the captured text
    // parses as one; otherwise fall back to string equality for `==` / `!=`.
    if let Some(rhs) = value
        .as_float()
        .or_else(|| value.as_integer().map(|i| i as f64))
    {
        if let Ok(lhs) = text.trim().parse::<f64>() {
            return match op {
                CmpOp::Lt => lhs < rhs,
                CmpOp::Le => lhs <= rhs,
                CmpOp::Gt => lhs > rhs,
                CmpOp::Ge => lhs >= rhs,
                CmpOp::Eq => (lhs - rhs).abs() < f64::EPSILON,
                CmpOp::Ne => (lhs - rhs).abs() >= f64::EPSILON,
            };
        }
        // RHS numeric but LHS not a number: only `!=` can be satisfied.
        return matches!(op, CmpOp::Ne);
    }

    let rhs = match value {
        toml::Value::String(s) => s.clone(),
        toml::Value::Boolean(b) => b.to_string(),
        other => other.to_string(),
    };
    match op {
        CmpOp::Eq => text == rhs,
        CmpOp::Ne => text != rhs,
        // Ordering on non-numbers falls back to lexicographic compare.
        CmpOp::Lt => text < rhs.as_str(),
        CmpOp::Le => text <= rhs.as_str(),
        CmpOp::Gt => text > rhs.as_str(),
        CmpOp::Ge => text >= rhs.as_str(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{BindingMetadata, Span};
    use crate::model::Comparison;

    fn binding(text: &str) -> Binding {
        Binding {
            text: text.into(),
            span: Span {
                start_byte: 0,
                end_byte: text.len(),
                start_row: 0,
                start_col: 0,
                end_row: 0,
                end_col: text.len(),
            },
            metadata: BindingMetadata::default(),
        }
    }

    fn regex_constraint(metavar: &str, re: &str) -> CompiledConstraint {
        let c = Constraint {
            metavar: metavar.into(),
            regex: Some(re.into()),
            comparison: None,
            pattern: None,
            resolves_to: None,
            type_: None,
            language: None,
        };
        CompiledConstraint::compile("r", Language::Rust, &c).unwrap()
    }

    #[test]
    fn regex_constraint_matches() {
        let c = regex_constraint("KEY", "^[a-z][a-zA-Z]*$");
        assert!(c.evaluate(&binding("userId")));
        assert!(!c.evaluate(&binding("0bad")));
    }

    #[test]
    fn numeric_comparison() {
        let c = Constraint {
            metavar: "N".into(),
            regex: None,
            comparison: Some(Comparison {
                op: ">".into(),
                value: toml::Value::Integer(0),
            }),
            pattern: None,
            resolves_to: None,
            type_: None,
            language: None,
        };
        let c = CompiledConstraint::compile("r", Language::Rust, &c).unwrap();
        assert!(c.evaluate(&binding("5")));
        assert!(!c.evaluate(&binding("0")));
        assert!(!c.evaluate(&binding("-3")));
    }

    #[test]
    fn string_equality_comparison() {
        let c = Constraint {
            metavar: "S".into(),
            regex: None,
            comparison: Some(Comparison {
                op: "!=".into(),
                value: toml::Value::String("nil".into()),
            }),
            pattern: None,
            resolves_to: None,
            type_: None,
            language: None,
        };
        let c = CompiledConstraint::compile("r", Language::Rust, &c).unwrap();
        assert!(c.evaluate(&binding("something")));
        assert!(!c.evaluate(&binding("nil")));
    }

    #[test]
    fn sub_pattern_constraint() {
        // The captured metavar text must itself be a call expression.
        let c = Constraint {
            metavar: "VALUE".into(),
            regex: None,
            comparison: None,
            pattern: Some("$FN($ARG)".into()),
            resolves_to: None,
            type_: None,
            language: Some("typescript".into()),
        };
        let c = CompiledConstraint::compile("r", Language::TypeScript, &c).unwrap();
        assert!(c.evaluate(&binding("compute(x)")));
        assert!(!c.evaluate(&binding("42")));
    }

    #[test]
    fn rejects_zero_or_multiple_kinds() {
        let none = Constraint {
            metavar: "X".into(),
            regex: None,
            comparison: None,
            pattern: None,
            resolves_to: None,
            type_: None,
            language: None,
        };
        assert!(CompiledConstraint::compile("r", Language::Rust, &none).is_err());
    }
}
