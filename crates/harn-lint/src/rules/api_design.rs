//! API-shape guidance for capability attenuation and explicit call sites.
//!
//! These are deliberately conservative, non-fixing diagnostics. A root
//! `Harness` is right at entry and orchestration boundaries; an ordinary
//! helper whose entire observed authority is one or two direct sub-handles
//! should advertise those narrower nominal types instead. Public APIs with
//! four or more same-typed positional values should use a named closed record
//! so call sites state which value is which.

use std::collections::{BTreeSet, HashMap};

use harn_parser::{visit, DiagnosticCode as Code, Node, SNode, TypeExpr, TypedParam};
use harn_vm::HarnessKind;

use crate::diagnostic::{LintDiagnostic, LintSeverity};

const ATTENUATION_RULE: &str = "capability-attenuation";
const POSITIONAL_RULE: &str = "homogeneous-positional-api";
const POSITIONAL_THRESHOLD: usize = 4;

pub(crate) fn check_api_design(program: &[SNode], diagnostics: &mut Vec<LintDiagnostic>) {
    for declaration in program {
        let declaration = match &declaration.node {
            Node::AttributedDecl { inner, .. } => inner.as_ref(),
            _ => declaration,
        };
        let Node::FnDecl {
            name,
            params,
            body,
            is_pub,
            ..
        } = &declaration.node
        else {
            continue;
        };

        check_capability_attenuation(name, params, body, declaration, diagnostics);
        if *is_pub {
            check_homogeneous_positionals(name, params, declaration, diagnostics);
        }
    }
}

fn check_capability_attenuation(
    function_name: &str,
    params: &[TypedParam],
    body: &[SNode],
    declaration: &SNode,
    diagnostics: &mut Vec<LintDiagnostic>,
) {
    // `main` is an execution boundary. Pipelines are excluded structurally
    // above because they are orchestration boundaries by definition.
    if function_name == "main" {
        return;
    }
    for parameter in params {
        if !matches!(
            parameter.type_expr.as_ref(),
            Some(TypeExpr::Named(name)) if name == HarnessKind::Root.type_name()
        ) {
            continue;
        }

        let mut identifier_uses = 0usize;
        let mut direct_subhandle_uses = 0usize;
        let mut unknown_member = false;
        let mut subhandles = BTreeSet::new();
        visit::walk_program(body, &mut |node| match &node.node {
            Node::Identifier(name) if name == &parameter.name => identifier_uses += 1,
            Node::PropertyAccess { object, property }
            | Node::OptionalPropertyAccess { object, property }
                if matches!(&object.node, Node::Identifier(name) if name == &parameter.name) =>
            {
                direct_subhandle_uses += 1;
                if let Some(kind) = HarnessKind::from_field_name(property) {
                    subhandles.insert((property.clone(), kind.type_name()));
                } else {
                    unknown_member = true;
                }
            }
            _ => {}
        });

        // Suppress when the root escapes, is forwarded, or touches an unknown
        // member: local syntax no longer proves attenuation is safe.
        if identifier_uses == 0
            || identifier_uses != direct_subhandle_uses
            || unknown_member
            || !(1..=2).contains(&subhandles.len())
        {
            continue;
        }

        let signature = subhandles
            .iter()
            .map(|(field, ty)| format!("{field}: {ty}"))
            .collect::<Vec<_>>()
            .join(", ");
        let capabilities = subhandles
            .iter()
            .map(|(field, _)| format!("`harness.{field}`"))
            .collect::<Vec<_>>()
            .join(" and ");
        diagnostics.push(LintDiagnostic {
            code: Code::LintBroadHarnessParameter,
            rule: ATTENUATION_RULE.into(),
            message: format!(
                "helper `{function_name}` accepts root `Harness` but uses only {capabilities}"
            ),
            span: declaration.span,
            severity: if subhandles.len() == 1 {
                LintSeverity::Warning
            } else {
                LintSeverity::Info
            },
            suggestion: Some(format!(
                "accept the narrow capability parameter{} `{signature}` and pass the sub-handle{} at call sites; keep root `Harness` for entrypoints and genuine multi-capability orchestration",
                if subhandles.len() == 1 { "" } else { "s" },
                if subhandles.len() == 1 { "" } else { "s" },
            )),
            fix: None,
        });
    }
}

fn check_homogeneous_positionals(
    function_name: &str,
    params: &[TypedParam],
    declaration: &SNode,
    diagnostics: &mut Vec<LintDiagnostic>,
) {
    let mut groups: HashMap<String, Vec<&str>> = HashMap::new();
    for parameter in params {
        let Some(ty) = parameter.type_expr.as_ref() else {
            continue;
        };
        if parameter.rest
            || parameter.default_value.is_some()
            || matches!(ty, TypeExpr::Named(name) if name == HarnessKind::Root.type_name())
        {
            continue;
        }
        groups
            .entry(format!("{ty:?}"))
            .or_default()
            .push(parameter.name.as_str());
    }
    let Some(names) = groups
        .values()
        .filter(|names| names.len() >= POSITIONAL_THRESHOLD)
        .max_by_key(|names| names.len())
    else {
        return;
    };

    diagnostics.push(LintDiagnostic {
        code: Code::LintHomogeneousPositionalApi,
        rule: POSITIONAL_RULE.into(),
        message: format!(
            "public function `{function_name}` has {} same-typed positional parameters ({})",
            names.len(),
            names.join(", ")
        ),
        span: declaration.span,
        severity: LintSeverity::Info,
        suggestion: Some(
            "replace the ambiguous positional group with one named closed-record parameter; construct it with named fields at call sites and destructure it inside the function"
                .to_string(),
        ),
        fix: None,
    });
}

#[cfg(test)]
mod tests {
    use harn_lexer::Lexer;
    use harn_parser::Parser;

    use super::*;

    fn lint(source: &str) -> Vec<LintDiagnostic> {
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let program = Parser::new(tokens).parse().expect("parse");
        let mut diagnostics = Vec::new();
        check_api_design(&program, &mut diagnostics);
        diagnostics
    }

    #[test]
    fn recommends_one_narrow_handle_for_an_ordinary_helper() {
        let diagnostics =
            lint("fn load(harness: Harness, path: string) { return harness.fs.read_text(path) }");
        assert_eq!(
            diagnostics
                .iter()
                .filter(|d| d.rule == ATTENUATION_RULE)
                .count(),
            1
        );
        assert!(diagnostics[0]
            .suggestion
            .as_deref()
            .unwrap()
            .contains("HarnessFs"));
    }

    #[test]
    fn preserves_entrypoints_or_root_values_that_escape() {
        let diagnostics = lint(
            "fn main(harness: Harness) { harness.fs.cwd() }\nfn orchestrate(harness: Harness) { delegate(harness) }",
        );
        assert!(diagnostics.iter().all(|d| d.rule != ATTENUATION_RULE));
    }

    #[test]
    fn allows_genuine_multi_capability_orchestration() {
        let diagnostics = lint(
            "fn coordinate(harness: Harness) { harness.fs.cwd(); harness.net.get(\"x\"); harness.clock.now() }",
        );
        assert!(diagnostics.iter().all(|d| d.rule != ATTENUATION_RULE));
    }

    #[test]
    fn recommends_a_closed_record_for_ambiguous_public_positionals() {
        let diagnostics = lint(
            "pub fn bounds(left: int, top: int, right: int, bottom: int) -> int { return left }",
        );
        assert_eq!(
            diagnostics
                .iter()
                .filter(|d| d.rule == POSITIONAL_RULE)
                .count(),
            1
        );
        assert!(diagnostics[0]
            .suggestion
            .as_deref()
            .unwrap()
            .contains("closed-record"));
    }

    #[test]
    fn private_or_heterogeneous_signatures_are_not_flagged() {
        let diagnostics = lint(
            "fn private(a: int, b: int, c: int, d: int) {}\npub fn mixed(a: int, b: int, c: int, label: string) -> nil {}",
        );
        assert!(diagnostics.iter().all(|d| d.rule != POSITIONAL_RULE));
    }
}
