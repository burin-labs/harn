//! API-shape guidance for capability attenuation and explicit call sites.
//!
//! These are deliberately conservative diagnostics. A root
//! `Harness` is right at entry and orchestration boundaries; an ordinary
//! helper whose entire observed authority is one or two direct sub-handles
//! should advertise those narrower nominal types instead. Public APIs with
//! four or more same-typed positional values should use a named closed record
//! so call sites state which value is which.

use std::collections::BTreeSet;

use harn_parser::{visit, DiagnosticCode as Code, Node, SNode, TypeExpr, TypedParam};
use harn_vm::HarnessKind;

use crate::diagnostic::{LintDiagnostic, LintSeverity};

const ATTENUATION_RULE: &str = "capability-attenuation";
const POSITIONAL_RULE: &str = "homogeneous-positional-api";
const POSITIONAL_THRESHOLD: usize = 4;

pub(crate) fn check_api_design(program: &[SNode], diagnostics: &mut Vec<LintDiagnostic>) {
    // A named function installed as a handler is entered by the runtime or a
    // host framework. That callback boundary receives the root Harness by
    // contract even when today's body happens to use one capability. Treat
    // this structural registration as stronger evidence than local body-use
    // counting; narrowing it would make the callback uncallable.
    let mut boundary_callbacks = BTreeSet::new();
    let mut public_functions = BTreeSet::new();
    visit::walk_program(program, &mut |node| {
        if let Node::FnDecl {
            name, is_pub: true, ..
        } = &node.node
        {
            public_functions.insert(name.clone());
        }
        let Node::DictLiteral(entries) = &node.node else {
            return;
        };
        for entry in entries {
            let is_handler = matches!(
                &entry.key.node,
                Node::Identifier(key) | Node::StringLiteral(key) if key == "handler"
            );
            if is_handler {
                if let Node::Identifier(name) = &entry.value.node {
                    boundary_callbacks.insert(name.clone());
                }
            }
        }
    });
    let connector_module = harn_vm::connectors::harn_module::abi::metadata_exports()
        .iter()
        .all(|name| public_functions.contains(*name));

    for declaration in program {
        let (declaration, attributed_boundary) = match &declaration.node {
            Node::AttributedDecl { attributes, inner } => (
                inner.as_ref(),
                attributes.iter().any(|attribute| attribute.name == "job"),
            ),
            _ => (declaration, false),
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

        let trigger_boundary = params.len() >= 2
            && matches!(
                params[0].type_expr.as_ref(),
                Some(TypeExpr::Named(type_name)) if type_name == HarnessKind::Root.type_name()
            )
            && matches!(
                params[1].type_expr.as_ref(),
                Some(TypeExpr::Named(type_name)) if type_name == "TriggerEvent"
            );
        let connector_boundary = connector_module
            && *is_pub
            && harn_vm::connectors::harn_module::abi::is_runtime_export(name);
        if !attributed_boundary
            && !trigger_boundary
            && !connector_boundary
            && !boundary_callbacks.contains(name)
        {
            check_capability_attenuation(name, params, body, declaration, diagnostics);
        }
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
        let mut shadowed_in_nested_callable = false;
        let mut record_use = |node: &SNode| match &node.node {
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
            Node::FnDecl { params, .. } | Node::Closure { params, .. }
                if params.iter().any(|nested| nested.name == parameter.name) =>
            {
                shadowed_in_nested_callable = true;
            }
            _ => {}
        };
        // Defaults execute in the callable's scope and may use authority just
        // like the body. Ignoring them can falsely attenuate a root parameter,
        // leaving the default with an unreachable grant.
        for candidate in params {
            if let Some(default) = &candidate.default_value {
                visit::walk_node(default, &mut record_use);
            }
        }
        visit::walk_program(body, &mut record_use);

        // Suppress when the root escapes, is forwarded, or touches an unknown
        // member: local syntax no longer proves attenuation is safe.
        if identifier_uses == 0
            || identifier_uses != direct_subhandle_uses
            || unknown_member
            || shadowed_in_nested_callable
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
            suggestion: Some(if subhandles.len() == 1 {
                format!(
                    "accept the narrow capability parameter `{signature}` and pass the sub-handle at call sites; keep root `Harness` for entrypoints and genuine multi-capability orchestration"
                )
            } else {
                format!(
                    "accept one closed capability record `{{{signature}}}` and construct it from the two sub-handles at call sites; keep root `Harness` for entrypoints and genuine multi-capability orchestration"
                )
            }),
            // Narrowing a signature is only safe when every call site moves
            // with it, and a caller can live in a module this rule never sees.
            // Until the fixer resolves capability requirements across module
            // boundaries, this stays advisory.
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
    let mut groups: Vec<(TypeExpr, Vec<&str>)> = Vec::new();
    for parameter in params {
        let Some(ty) = parameter.type_expr.as_ref() else {
            continue;
        };
        if parameter.rest
            || matches!(ty, TypeExpr::Named(name) if name == HarnessKind::Root.type_name())
        {
            continue;
        }
        if let Some((_, names)) = groups.iter_mut().find(|(candidate, _)| candidate == ty) {
            names.push(parameter.name.as_str());
        } else {
            groups.push((ty.clone(), vec![parameter.name.as_str()]));
        }
    }
    let Some(names) = groups
        .iter()
        .map(|(_, names)| names)
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
        let source = "fn load(harness: Harness, path: string) { harness.fs.exists(path); return harness.fs.read_text(path) }";
        let diagnostics = lint(source);
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
        assert_eq!(
            diagnostics[0].repair().expect("repair").safety,
            harn_parser::RepairSafety::SurfaceChanging
        );
        // Advisory only: a caller can live in a module this rule never sees,
        // so narrowing the signature is not safe to apply automatically.
        assert!(diagnostics[0].fix.is_none());
    }

    #[test]
    fn recommends_a_closed_capability_record_for_two_capability_helpers() {
        let multi = lint(
            "fn copy(harness: Harness, path: string) { harness.obs.log_info(path); return harness.fs.read_text(path) }",
        );
        assert_eq!(multi.len(), 1);
        let suggestion = multi[0].suggestion.as_deref().expect("suggestion");
        assert!(
            suggestion.contains("{fs: HarnessFs, obs: HarnessObs}"),
            "{suggestion}"
        );
        assert!(multi[0].fix.is_none());
    }

    #[test]
    fn does_not_fix_shadowed_receivers() {
        let shadowed = lint(
            "fn load(harness: Harness) { const callback = { harness: Harness -> harness.fs.cwd() }; return harness.fs.cwd() }",
        );
        assert!(shadowed
            .iter()
            .all(|diagnostic| diagnostic.rule != ATTENUATION_RULE));
    }

    #[test]
    fn parameter_defaults_participate_in_authority_analysis() {
        let multi = lint(
            "fn load(harness: Harness, root: string = harness.fs.cwd()) { return harness.agent.current_id() }",
        );
        assert_eq!(multi.len(), 1);
        // The default executes in the callable's scope, so its `harness.fs`
        // use has to widen the recommendation alongside the body's agent use.
        let suggestion = multi[0].suggestion.as_deref().expect("suggestion");
        assert!(
            suggestion.contains("{agent: HarnessAgent, fs: HarnessFs}"),
            "{suggestion}"
        );

        let escaped = lint(
            "fn load(harness: Harness, root: string = project_root(harness)) { return harness.fs.read_text(root) }",
        );
        assert!(escaped
            .iter()
            .all(|diagnostic| diagnostic.rule != ATTENUATION_RULE));
    }

    #[test]
    fn preserves_entrypoints_or_root_values_that_escape() {
        let diagnostics = lint(
            "fn main(harness: Harness) { harness.fs.cwd() }\nfn orchestrate(harness: Harness) { delegate(harness) }",
        );
        assert!(diagnostics.iter().all(|d| d.rule != ATTENUATION_RULE));
    }

    #[test]
    fn preserves_runtime_registered_handler_boundaries() {
        let diagnostics = lint(
            "fn on_event(harness: Harness, event) { harness.channels.append(\"seen\", event) }\n\
             fn install(runtime: HarnessRuntime) { runtime.trigger_register({handler: on_event}) }",
        );
        assert!(diagnostics.iter().all(|d| d.rule != ATTENUATION_RULE));
    }

    #[test]
    fn preserves_job_entrypoint_boundaries() {
        let diagnostics =
            lint("@job(\"scan\")\npub fn scan(harness: Harness, event) { return event.kind }");
        assert!(diagnostics.iter().all(|d| d.rule != ATTENUATION_RULE));
    }

    #[test]
    fn preserves_nominal_trigger_handler_boundaries() {
        let diagnostics =
            lint("pub fn on_event(harness: Harness, event: TriggerEvent) { return event.kind }");
        assert!(diagnostics.iter().all(|d| d.rule != ATTENUATION_RULE));
    }

    #[test]
    fn preserves_connector_runtime_export_boundaries() {
        let diagnostics = lint(
            "pub fn provider_id() { return \"example\" }\n\
             pub fn kinds() { return [\"webhook\"] }\n\
             pub fn payload_schema() { return {} }\n\
             pub fn init(harness: Harness, ctx) { harness.runtime.store_set(\"ctx\", ctx) }\n\
             pub fn normalize_inbound(harness: Harness, raw) { return {raw: raw, secret: harness.secrets.read(\"hook\")} }\n\
             pub fn helper(harness: Harness) { harness.runtime.store_get(\"ctx\") }",
        );
        let attenuation = diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.rule == ATTENUATION_RULE)
            .collect::<Vec<_>>();
        assert_eq!(attenuation.len(), 1);
        assert!(attenuation[0].message.contains("helper `helper`"));
    }

    #[test]
    fn ordinary_public_function_named_like_connector_export_is_not_exempt() {
        let diagnostics = lint(
            "pub fn call(harness: Harness, method, args) { harness.net.request(method, args.url) }",
        );
        assert!(diagnostics
            .iter()
            .any(|diagnostic| diagnostic.rule == ATTENUATION_RULE));
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
    fn counts_defaulted_parameters_that_remain_positional_at_call_sites() {
        let diagnostics = lint(
            "pub fn connect(host: string, user: string, password: string = \"\", database: string = \"\") {}",
        );
        assert!(diagnostics
            .iter()
            .any(|diagnostic| diagnostic.rule == POSITIONAL_RULE));
    }

    #[test]
    fn private_or_heterogeneous_signatures_are_not_flagged() {
        let diagnostics = lint(
            "fn private(a: int, b: int, c: int, d: int) {}\npub fn mixed(a: int, b: int, c: int, label: string) -> nil {}",
        );
        assert!(diagnostics.iter().all(|d| d.rule != POSITIONAL_RULE));
    }
}
