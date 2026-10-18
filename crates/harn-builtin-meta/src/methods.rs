//! Descriptor shapes for builtin value methods (`xs.len()`, `s.lower()`,
//! `xs.push(v)`).
//!
//! The method table itself lives in `harn-parser` (`builtin_methods`), which
//! builds each row's [`BuiltinSignature`] with the same sig grammar
//! `#[harn_builtin]` uses. Only the shapes live here so every consumer can
//! name them without depending on the parser.

use crate::BuiltinSignature;

/// The builtin value kind a method is dispatched on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MethodReceiver {
    String,
    List,
    Set,
    Dict,
    Range,
    Iter,
    Generator,
}

impl MethodReceiver {
    pub const ALL: [MethodReceiver; 7] = [
        MethodReceiver::String,
        MethodReceiver::List,
        MethodReceiver::Set,
        MethodReceiver::Dict,
        MethodReceiver::Range,
        MethodReceiver::Iter,
        MethodReceiver::Generator,
    ];

    /// The runtime type name (`type_of`) of the receiver.
    pub const fn type_name(self) -> &'static str {
        match self {
            MethodReceiver::String => "string",
            MethodReceiver::List => "list",
            MethodReceiver::Set => "set",
            MethodReceiver::Dict => "dict",
            MethodReceiver::Range => "range",
            MethodReceiver::Iter => "iter",
            MethodReceiver::Generator => "generator",
        }
    }

    /// Generic parameter names that a method signature binds from the
    /// receiver itself rather than from its arguments: the element type of a
    /// list, set, iter, or generator, and the key/value types of a dict.
    pub const fn receiver_type_params(self) -> &'static [&'static str] {
        match self {
            MethodReceiver::List
            | MethodReceiver::Set
            | MethodReceiver::Iter
            | MethodReceiver::Generator => &["T"],
            MethodReceiver::Dict => &["K", "V"],
            MethodReceiver::String | MethodReceiver::Range => &[],
        }
    }
}

/// Whether calling the method changes its receiver.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MethodEffect {
    /// Returns a value and leaves the receiver untouched. Discarding the
    /// result of a pure method is always a bug.
    Pure,
    /// Swift-style `mutating`: the receiver must be a mutable place (a `let`
    /// binding or a property/subscript path rooted at one), and the call
    /// rewrites that place with the updated value before returning.
    Mutating,
}

/// One builtin value method: the single owner of its name, signature, effect,
/// and reference documentation.
#[derive(Debug, Clone, Copy)]
pub struct MethodDescriptor {
    pub receiver: MethodReceiver,
    /// `signature.name` is the method name. Generic parameters listed in
    /// [`MethodReceiver::receiver_type_params`] are bound from the receiver.
    pub signature: BuiltinSignature,
    pub effect: MethodEffect,
    pub doc: &'static str,
}

impl MethodDescriptor {
    pub const fn pure(
        receiver: MethodReceiver,
        signature: BuiltinSignature,
        doc: &'static str,
    ) -> Self {
        Self {
            receiver,
            signature,
            effect: MethodEffect::Pure,
            doc,
        }
    }

    pub const fn mutating(
        receiver: MethodReceiver,
        signature: BuiltinSignature,
        doc: &'static str,
    ) -> Self {
        Self {
            receiver,
            signature,
            effect: MethodEffect::Mutating,
            doc,
        }
    }

    pub const fn name(&self) -> &'static str {
        self.signature.name
    }

    pub const fn is_mutating(&self) -> bool {
        matches!(self.effect, MethodEffect::Mutating)
    }
}

/// How source written against a removed method spelling is repaired.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MethodMigration {
    /// Same operation under its canonical name; a mechanical rename.
    Rename(&'static str),
    /// `xs = xs.appending(v)` becomes `xs.push(v)`; any other use becomes
    /// `xs + [v]`.
    AppendingToPush,
    /// `s = s.adding(v)` becomes the mutating call `s.add(v)`; any other use
    /// needs the guidance's copy-returning spelling.
    MutatingWhenSelfAssigned {
        mutating: &'static str,
        guidance: &'static str,
    },
    /// No mechanical equivalent; the guidance names the replacement.
    Manual(&'static str),
}

/// A method spelling that no longer exists on a receiver.
#[derive(Debug, Clone, Copy)]
pub struct RemovedMethod {
    pub receiver: MethodReceiver,
    pub name: &'static str,
    pub migration: MethodMigration,
}

impl RemovedMethod {
    pub const fn renamed(receiver: MethodReceiver, name: &'static str, to: &'static str) -> Self {
        Self {
            receiver,
            name,
            migration: MethodMigration::Rename(to),
        }
    }

    pub const fn mutating_when_self_assigned(
        receiver: MethodReceiver,
        name: &'static str,
        mutating: &'static str,
        guidance: &'static str,
    ) -> Self {
        Self {
            receiver,
            name,
            migration: MethodMigration::MutatingWhenSelfAssigned { mutating, guidance },
        }
    }

    pub const fn manual(
        receiver: MethodReceiver,
        name: &'static str,
        guidance: &'static str,
    ) -> Self {
        Self {
            receiver,
            name,
            migration: MethodMigration::Manual(guidance),
        }
    }

    /// One-line repair text for diagnostics and runtime errors.
    pub fn repair_hint(&self) -> String {
        match self.migration {
            MethodMigration::Rename(to) => format!("did you mean `{to}`?"),
            MethodMigration::AppendingToPush => {
                "use `xs.push(v)` to append in place, or `xs + [v]` for a new list".to_string()
            }
            MethodMigration::MutatingWhenSelfAssigned { guidance, .. }
            | MethodMigration::Manual(guidance) => guidance.to_string(),
        }
    }
}
