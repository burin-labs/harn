//! Which receivers in this file are the host `Harness`, and the
//! spelling-independent call recognizer built on that.
//!
//! `HARN-LNT-071` asks call sites to move from an ambient builtin to the typed
//! `harness.<capability>.<method>` that replaced it. A rule that identifies a
//! call by matching `Node::FunctionCall` against a builtin's name stops
//! applying the moment a call site does so. It does not fail — it reports
//! nothing, which is indistinguishable from a clean result (harn#7280).
//!
//! Both homes for a rule need the same answer to "which builtin does this call
//! name?": the stateful [`Linter`][super::Linter] walk, and the pure-function
//! rules in [`crate::rule`], which reach it through
//! [`RuleCtx::harness`][crate::rule::RuleCtx]. Owning the question here keeps
//! one answer for both.

use std::collections::{HashMap, HashSet};

use harn_parser::lexical::BindingId;
use harn_parser::{Node, SNode};

use super::Linter;

/// Harness receiver facts for one file, collected in a prepass before the
/// lint walk.
#[derive(Default)]
pub(crate) struct HarnessFacts {
    /// Exact identifier-use resolution for Harness receiver policy. The
    /// declaration identity prevents nested parameters and locals with the
    /// same spelling from inheriting host authority.
    resolved_identifier_bindings: HashMap<(usize, usize), BindingId>,
    harness_bindings: HashSet<BindingId>,
}

impl HarnessFacts {
    /// Resolve every callable's parameters and record which bindings carry the
    /// typed `Harness`.
    pub(crate) fn collect(nodes: &[SNode]) -> Self {
        let mut facts = Self::default();
        harn_parser::visit::walk_program(nodes, &mut |node| {
            let (params, body) = match &node.node {
                Node::Pipeline { params, body, .. }
                | Node::FnDecl { params, body, .. }
                | Node::ToolDecl { params, body, .. }
                | Node::Closure { params, body, .. } => (params.as_slice(), body.as_slice()),
                _ => return,
            };
            facts.resolved_identifier_bindings.extend(
                harn_parser::lexical::resolved_identifier_bindings(params, body),
            );
            let Some(harness_name) = Linter::callable_harness_param(params) else {
                return;
            };
            if let Some(param) = params.iter().find(|param| param.name == harness_name) {
                facts
                    .harness_bindings
                    .insert(BindingId::from_declaration(&param.name, param.span));
            }
        });
        facts
    }

    /// The capability segment of a `harness.<capability>` method receiver, so
    /// `harness.interaction.request_approval(...)` yields `"interaction"`.
    ///
    /// The root is compared against the enclosing binding rather than the
    /// literal name `harness`, because the parameter may be declared
    /// `_harness`, and a local named `harness` that is not the host handle
    /// must not be mistaken for one.
    pub(crate) fn capability_of<'a>(&self, object: &'a SNode) -> Option<&'a str> {
        let (root, properties) = harn_parser::lexical::resolved_receiver_path(
            object,
            &self.resolved_identifier_bindings,
        )?;
        self.harness_bindings
            .contains(root)
            .then(|| properties.first().copied())
            .flatten()
    }

    /// The arguments of `node` when it calls `builtin`, in either spelling:
    /// the ambient global itself, or the typed `harness.<capability>.<method>`
    /// that replaced it.
    ///
    /// Rules identify a call by which builtin it names. Matching the syntax
    /// instead makes them stop applying the moment a call site adopts the
    /// spelling `HARN-LNT-071` asks for, and a rule that quietly stops
    /// applying reads exactly like one that found nothing. Routing the
    /// question here keeps one owner for it — the migration recipe already
    /// derived from the capability surface — instead of a second name table
    /// that would drift from it.
    ///
    /// A migration that reshapes arguments resolves to `None`: the caller is
    /// about to read the ambient call's argument positions, and a request
    /// record or a call-then-property projection no longer has them.
    pub(crate) fn call_names_builtin<'node>(
        &self,
        node: &'node SNode,
        builtin: &str,
    ) -> Option<&'node [SNode]> {
        match &node.node {
            Node::FunctionCall { name, args, .. } => (name == builtin).then_some(args.as_slice()),
            Node::MethodCall {
                object,
                method,
                args,
            }
            | Node::OptionalMethodCall {
                object,
                method,
                args,
            } => {
                let migration = harn_vm::stdlib::harness_migration_for_builtin(builtin)?;
                if !matches!(
                    migration.arguments,
                    harn_vm::stdlib::HarnessBuiltinArgumentMigration::Forward
                ) {
                    return None;
                }
                (migration.method == method
                    && self.capability_of(object) == Some(migration.capability.field_name()))
                .then_some(args.as_slice())
            }
            _ => None,
        }
    }

    /// Whether `node` is a method call on a `harness.<capability>` receiver.
    ///
    /// A rule that scans several builtin names per node uses this to reject
    /// the ordinary method call — `items.map(...)`, `client.send(...)` — with
    /// one receiver resolution instead of one migration lookup per name.
    pub(crate) fn is_capability_method_call(&self, node: &SNode) -> bool {
        match &node.node {
            Node::MethodCall { object, .. } | Node::OptionalMethodCall { object, .. } => {
                self.capability_of(object).is_some()
            }
            _ => false,
        }
    }
}
