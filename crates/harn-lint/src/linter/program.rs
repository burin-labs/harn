//! Whole-program prepasses and persona/step metadata validation.

use harn_lexer::Span;
use harn_parser::{DiagnosticCode as Code, Node, SNode};

use super::Linter;
use crate::diagnostic::{LintDiagnostic, LintSeverity};

impl Linter<'_> {
    pub(crate) fn lint_program(&mut self, nodes: &[SNode]) {
        self.collect_hoisted_callable_names(nodes);
        self.collect_persona_step_metadata(nodes);
        self.collect_impl_method_names(nodes);
        self.run_program_rules(nodes);
        for node in nodes {
            self.lint_node(node);
        }
    }

    /// Collect source-level callables before the order-sensitive lint walk.
    /// Harn declarations are hoisted, so ambient-migration name resolution
    /// must see a callable even when its declaration follows its first call.
    fn collect_hoisted_callable_names(&mut self, nodes: &[SNode]) {
        for node in nodes {
            let node = match &node.node {
                Node::AttributedDecl { inner, .. } => &inner.node,
                node => node,
            };
            let name = match node {
                Node::Pipeline { name, .. }
                | Node::FnDecl { name, .. }
                | Node::ToolDecl { name, .. }
                | Node::SkillDecl { name, .. }
                | Node::StructDecl { name, .. }
                | Node::EnumDecl { name, .. } => Some(name),
                Node::EvalPackDecl { binding_name, .. } => Some(binding_name),
                _ => None,
            };
            if let Some(name) = name {
                self.known_functions.insert(name.clone());
            }
        }
    }

    fn collect_persona_step_metadata(&mut self, nodes: &[SNode]) {
        for node in nodes {
            let Node::AttributedDecl { attributes, inner } = &node.node else {
                continue;
            };
            if attributes.iter().any(|attribute| attribute.name == "step") {
                if let Node::FnDecl { name, .. } = &inner.node {
                    self.step_functions.insert(name.clone());
                    let step_name = attributes
                        .iter()
                        .find(|attribute| attribute.name == "step")
                        .and_then(|attribute| attribute.named_arg("name"))
                        .and_then(Self::string_literal_value)
                        .unwrap_or(name)
                        .to_string();
                    self.step_names_by_function.insert(name.clone(), step_name);
                }
            }
            if attributes
                .iter()
                .any(|attribute| attribute.name == "persona")
            {
                if let Node::FnDecl { name, body, .. } = &inner.node {
                    let persona_name = attributes
                        .iter()
                        .find(|attribute| attribute.name == "persona")
                        .and_then(|attribute| attribute.named_arg("name"))
                        .and_then(Self::string_literal_value)
                        .unwrap_or(name)
                        .to_string();
                    self.collect_persona_calls(&persona_name, body);
                }
            }
        }
        for (call, _, persona) in &self.persona_body_calls {
            if let Some(step_name) = self.step_names_by_function.get(call) {
                self.persona_steps
                    .entry(persona.clone())
                    .or_default()
                    .insert(step_name.clone());
            }
        }
    }

    fn collect_persona_calls(&mut self, persona_name: &str, body: &[SNode]) {
        for node in body {
            self.collect_persona_calls_node(persona_name, node);
        }
    }

    fn collect_persona_calls_node(&mut self, persona_name: &str, node: &SNode) {
        if let Node::FunctionCall { name, .. } = &node.node {
            self.persona_body_calls
                .push((name.clone(), node.span, persona_name.to_string()));
        }
        for child in harn_parser::visit::immediate_children(node) {
            self.collect_persona_calls_node(persona_name, child);
        }
    }

    pub(super) fn validate_step_hook_target(&mut self, args: &[SNode], span: Span) {
        let Some(persona_pattern) = args.first().and_then(Self::string_literal_value) else {
            return;
        };
        let Some(step_name) = args.get(1).and_then(Self::string_literal_value) else {
            return;
        };
        let matching_personas: Vec<_> = self
            .persona_steps
            .iter()
            .filter(|(persona, _)| harn_glob::match_name(persona_pattern, persona))
            .collect();
        if matching_personas.is_empty() {
            self.diagnostics.push(LintDiagnostic {
                code: Code::LintPersonaHookTarget,
                rule: "persona-hook-target".into(),
                message: format!("`register_step_hook` pattern `{persona_pattern}` does not match a statically declared `@persona`"),
                span,
                severity: LintSeverity::Error,
                suggestion: Some("register hooks against a declared persona name or glob".to_string()),
                fix: None,
            });
            return;
        }
        let missing: Vec<_> = matching_personas
            .into_iter()
            .filter_map(|(persona, steps)| (!steps.contains(step_name)).then_some(persona.clone()))
            .collect();
        if !missing.is_empty() {
            self.diagnostics.push(LintDiagnostic {
                code: Code::LintPersonaHookTarget,
                rule: "persona-hook-target".into(),
                message: format!("`register_step_hook` targets step `{step_name}`, but it is not declared by persona(s): {}", missing.join(", ")),
                span,
                severity: LintSeverity::Error,
                suggestion: Some("use a step name declared with `@step(name: ...)` and called by the matching `@persona`".to_string()),
                fix: None,
            });
        }
    }
}
