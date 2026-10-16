//! The single owner of builtin value methods.
//!
//! [`BUILTIN_METHODS`] names every method a builtin receiver supports, with
//! its signature, whether it mutates its receiver, and its reference doc.
//! [`REMOVED_METHODS`] records retired spellings and how to repair them. The
//! VM and portable-kernel dispatch tables are drift-checked against these
//! rows, and the typechecker, linter, language server, and generated docs
//! derive from them.

mod reference;
mod table;

use crate::ast::{Node, SNode};

pub use harn_builtin_meta::{
    MethodDescriptor, MethodEffect, MethodMigration, MethodReceiver, RemovedMethod,
};
pub use reference::{
    render_reference, splice_reference, REFERENCE_BEGIN_MARKER, REFERENCE_END_MARKER,
};
pub use table::{BUILTIN_METHODS, REMOVED_METHODS};

/// The method `name` on `receiver`, if it exists.
pub fn method(receiver: MethodReceiver, name: &str) -> Option<&'static MethodDescriptor> {
    BUILTIN_METHODS
        .iter()
        .find(|row| row.receiver == receiver && row.name() == name)
}

/// Every method on `receiver`, in table order.
pub fn methods_of(receiver: MethodReceiver) -> impl Iterator<Item = &'static MethodDescriptor> {
    BUILTIN_METHODS
        .iter()
        .filter(move |row| row.receiver == receiver)
}

/// Method names on `receiver`, in table order.
pub fn method_names(receiver: MethodReceiver) -> Vec<&'static str> {
    methods_of(receiver).map(MethodDescriptor::name).collect()
}

/// True when some builtin receiver has a method called `name`.
pub fn is_method_name(name: &str) -> bool {
    BUILTIN_METHODS.iter().any(|row| row.name() == name)
}

/// The retired spelling `name` on `receiver`, if it was removed.
pub fn removed_method(receiver: MethodReceiver, name: &str) -> Option<&'static RemovedMethod> {
    REMOVED_METHODS
        .iter()
        .find(|row| row.receiver == receiver && row.name == name)
}

/// Retired spellings that are also common verbs on host and connector
/// objects (`store.delete(key)`), so a call through an unknown receiver is not
/// evidence of a stale builtin call.
const HOST_VERBS: &[&str] = &["delete"];

/// Every removed row for a retired spelling that no live method on any
/// receiver still uses, so a call to it is stale whatever the receiver's type.
/// `count` is not one (iterators keep it); `to_lower` is. Empty otherwise.
pub fn removed_method_on_any_receiver(name: &str) -> Vec<&'static RemovedMethod> {
    if is_method_name(name) || HOST_VERBS.contains(&name) {
        return Vec::new();
    }
    REMOVED_METHODS
        .iter()
        .filter(|row| row.name == name)
        .collect()
}

/// Mutating list methods and the copy-returning method that computes the
/// same result as a new value.
pub const COPY_COUNTERPARTS: &[(&str, &str)] = &[
    ("sort", "sorted"),
    ("sort_by", "sorted_by"),
    ("reverse", "reversed"),
];

/// The copy-returning counterpart of a mutating method (`sorted` for `sort`).
pub fn copy_counterpart(mutating: &str) -> Option<&'static str> {
    COPY_COUNTERPARTS
        .iter()
        .find(|(name, _)| *name == mutating)
        .map(|(_, copy)| *copy)
}

/// The mutating counterpart of a copy-returning method (`sort` for `sorted`).
pub fn in_place_counterpart(copy: &str) -> Option<&'static str> {
    COPY_COUNTERPARTS
        .iter()
        .find(|(_, name)| *name == copy)
        .map(|(mutating, _)| *mutating)
}

/// True when the method's result type mentions one of its generic
/// parameters, so the result depends on the receiver or the arguments.
pub fn returns_generic(row: &MethodDescriptor) -> bool {
    fn mentions(ty: &harn_builtin_meta::Ty, params: &[&str]) -> bool {
        use harn_builtin_meta::Ty;
        match ty {
            Ty::Generic(name) | Ty::Named(name) => params.contains(name),
            Ty::Optional(inner) => mentions(inner, params),
            Ty::Apply(_, args) | Ty::Union(args) => args.iter().any(|t| mentions(t, params)),
            Ty::Fn(args, ret) => args.iter().any(|t| mentions(t, params)) || mentions(ret, params),
            Ty::Shape(fields) => fields.iter().any(|f| mentions(&f.ty, params)),
            Ty::OpenShape(fields, rests) => {
                fields.iter().any(|f| mentions(&f.ty, params))
                    || rests.iter().any(|t| mentions(t, params))
            }
            Ty::SchemaOf(name) => params.contains(name),
            Ty::Any | Ty::Never | Ty::LitInt(_) | Ty::LitString(_) => false,
        }
    }
    mentions(&row.signature.returns, row.signature.type_params)
}

/// True when some builtin receiver has a mutating method called `name`.
pub fn is_mutating_method_name(name: &str) -> bool {
    BUILTIN_METHODS
        .iter()
        .any(|row| row.is_mutating() && row.name() == name)
}

/// True when some builtin receiver has a pure method called `name` and no
/// receiver has a mutating method of that name, so discarding the call's
/// result can never be intended.
pub fn is_pure_on_every_receiver(name: &str) -> bool {
    is_method_name(name) && !is_mutating_method_name(name)
}

/// A place a mutating method can rewrite: a binding, or a property or
/// subscript path rooted at one.
pub fn is_mutable_place(node: &SNode) -> bool {
    place_root(node).is_some()
}

/// The binding at the root of a place expression.
pub fn place_root(node: &SNode) -> Option<&str> {
    match &node.node {
        Node::Identifier(name) => Some(name),
        Node::PropertyAccess { object, .. } | Node::SubscriptAccess { object, .. } => {
            place_root(object)
        }
        _ => None,
    }
}

/// When `node` is `place.method(...)` naming a mutating builtin method, the
/// receiver place it may rewrite. This is syntactic: a user struct method
/// that happens to share the name also reports its receiver, which keeps
/// write analyses conservative.
pub fn mutating_call_place(node: &SNode) -> Option<&SNode> {
    match &node.node {
        Node::MethodCall { object, method, .. } if is_mutating_method_name(method) => {
            is_mutable_place(object).then_some(&**object)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn method_names_are_unique_per_receiver() {
        for receiver in MethodReceiver::ALL {
            let mut names = method_names(receiver);
            let total = names.len();
            names.sort_unstable();
            names.dedup();
            assert_eq!(names.len(), total, "{receiver:?} lists a method twice");
        }
    }

    #[test]
    fn removed_spellings_are_not_live_on_their_receiver() {
        for row in REMOVED_METHODS {
            assert!(
                method(row.receiver, row.name).is_none(),
                "`{}` is both removed and live on {:?}",
                row.name,
                row.receiver
            );
            if let MethodMigration::Rename(to) = row.migration {
                assert!(
                    method(row.receiver, to).is_some(),
                    "`{}` on {:?} renames to `{to}`, which does not exist",
                    row.name,
                    row.receiver
                );
            }
        }
    }

    #[test]
    fn receiver_generics_are_declared_by_the_signatures_that_use_them() {
        for row in BUILTIN_METHODS {
            for param in row.receiver.receiver_type_params() {
                let rendered = row.signature.to_string();
                let used = rendered
                    .split(|c: char| !c.is_alphanumeric() && c != '_')
                    .skip(1)
                    .any(|token| token == *param);
                if used {
                    assert!(
                        row.signature.is_type_param(param),
                        "{:?}.{} uses `{param}` without declaring it: {rendered}",
                        row.receiver,
                        row.name()
                    );
                }
            }
        }
    }

    #[test]
    fn copy_counterparts_pair_a_mutating_and_a_pure_list_method() {
        for (mutating, copy) in COPY_COUNTERPARTS {
            assert!(method(MethodReceiver::List, mutating).is_some_and(|row| row.is_mutating()));
            assert!(method(MethodReceiver::List, copy).is_some_and(|row| !row.is_mutating()));
        }
    }

    #[test]
    fn retired_spellings_only_flag_unknown_receivers_when_unambiguous() {
        assert_eq!(removed_method_on_any_receiver("to_lower").len(), 1);
        assert_eq!(removed_method_on_any_receiver("removing").len(), 2);
        // Iterators keep `count`, lists keep `reverse`: an unknown receiver
        // could be either, so neither is flagged without a receiver type.
        assert!(removed_method_on_any_receiver("count").is_empty());
        assert!(removed_method_on_any_receiver("reverse").is_empty());
        // `delete` is a host verb (`store.delete(key)`).
        assert!(removed_method_on_any_receiver("delete").is_empty());
    }
}
