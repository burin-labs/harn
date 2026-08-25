//! Recognizing a `harness.<capability>.<method>(...)` call site.
//!
//! Every `check` pass that keys a rule on an ambient global name has to
//! recognize the capability-method spelling too, because that is the spelling
//! the migration diagnostics tell authors to write. One definition of "this
//! receiver is a harness handle" lives here so the passes cannot drift apart.
//!
//! The reference model for this question is
//! `harn_ir::classify::capability_method_for`: a `harness.<capability>`
//! property access, or a bare sub-handle identifier that a caller destructured
//! out of one. The difference is what each crate can see. `harn-ir` resolves a
//! bare identifier against the capability handles it tracked while walking
//! scopes; a `check` pass has no scope model, so it can only ask whether the
//! identifier's *name* is a capability field name — which a local binding could
//! also satisfy. The two entry points below split on exactly that, and the
//! split is deliberate rather than an oversight:
//!
//! * [`harness_handle_field`] keeps the permissive spelling, because that is
//!   what `bundle.rs` already shipped for collecting bundle entries. Narrowing
//!   it would silently drop entries that are being collected today.
//! * [`harness_method_receiver`] accepts only the unambiguous `harness.<cap>`
//!   receiver, because its callers raise errors. A destructured handle is
//!   therefore missed; that under-flags, which is the right way to be wrong for
//!   a diagnostic that tells an author their code is broken. Widening it needs
//!   the scope tracking `harn-ir` has, not a looser name test.

use harn_parser::{Node, SNode};

/// The capability field of a harness handle receiver, so `harness.process`
/// yields `"process"` — and so does a bare `process` identifier, which may be a
/// destructured sub-handle or may be an unrelated local of the same name.
pub(super) fn harness_handle_field(node: &SNode) -> Option<&str> {
    match &node.node {
        Node::PropertyAccess { object, property } if matches!(&object.node, Node::Identifier(root) if root == "harness") => {
            Some(property)
        }
        Node::Identifier(name)
            if harn_builtin_meta::CapabilityId::from_field_name(name).is_some() =>
        {
            Some(name)
        }
        _ => None,
    }
}

/// The typed capability behind an explicit `harness.<capability>` receiver.
///
/// This classifies only the receiver. Some portable Harness methods are
/// registry-backed builtins, while native methods such as
/// `harness.process.exec_at` are dispatched directly by the VM. Requiring a
/// builtin entry here would silently exclude that second, equally public
/// method family.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct HarnessMethodReceiver<'a> {
    pub(super) field: &'a str,
    pub(super) capability: harn_builtin_meta::CapabilityId,
}

pub(super) fn harness_method_receiver(object: &SNode) -> Option<HarnessMethodReceiver<'_>> {
    let Node::PropertyAccess {
        object: root,
        property,
    } = &object.node
    else {
        return None;
    };
    if !matches!(&root.node, Node::Identifier(name) if name == "harness") {
        return None;
    }
    let capability = harn_builtin_meta::CapabilityId::from_field_name(property)?;
    Some(HarnessMethodReceiver {
        field: property,
        capability,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use harn_lexer::Lexer;
    use harn_parser::Parser;

    /// The receiver and method of the first method call in `source`.
    fn first_method_call(source: &str) -> (SNode, String) {
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let program = Parser::new(tokens).parse().expect("parse");
        let mut found = None;
        harn_parser::visit::walk_program(&program, &mut |node| {
            if found.is_some() {
                return;
            }
            if let Node::MethodCall { object, method, .. } = &node.node {
                found = Some(((**object).clone(), method.clone()));
            }
        });
        found.expect("a method call")
    }

    fn receiver_of(source: &str) -> Option<harn_builtin_meta::CapabilityId> {
        let (object, _) = first_method_call(source);
        harness_method_receiver(&object).map(|receiver| receiver.capability)
    }

    #[test]
    fn resolves_explicit_harness_capability_receivers() {
        assert_eq!(
            receiver_of("fn main(harness: Harness) {\n    harness.llm.call(\"u\", \"s\")\n}\n"),
            Some(harn_builtin_meta::CapabilityId::Llm)
        );
        assert_eq!(
            receiver_of(
                "fn main(harness: Harness) {\n    harness.process.exec_at(\"d\", \"ls\")\n}\n"
            ),
            Some(harn_builtin_meta::CapabilityId::Process)
        );
    }

    /// The point of the receiver check: an unrelated object that happens to
    /// have a colliding method name is not a capability call.
    #[test]
    fn rejects_a_non_harness_receiver_with_a_colliding_method_name() {
        assert_eq!(
            receiver_of("fn main(harness: Harness) {\n    client.exec_at(\"d\", \"ls\")\n}\n"),
            None
        );
        assert_eq!(
            receiver_of("fn main(harness: Harness) {\n    client.call(\"u\", \"s\")\n}\n"),
            None
        );
    }

    /// A bare capability-named local is ambiguous without scope tracking, so
    /// the error-raising entry point declines it. `harness_handle_field` still
    /// accepts it for `bundle.rs`.
    #[test]
    fn declines_a_bare_capability_named_receiver_but_handle_field_accepts_it() {
        assert_eq!(
            receiver_of("fn main(harness: Harness) {\n    process.exec_at(\"d\", \"ls\")\n}\n"),
            None
        );
        let (object, _) =
            first_method_call("fn main(harness: Harness) {\n    process.exec_at(\"d\")\n}\n");
        assert_eq!(harness_handle_field(&object), Some("process"));
    }
}
