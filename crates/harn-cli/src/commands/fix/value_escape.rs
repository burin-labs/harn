//! Frozen-signature facts for the capability migration.
//!
//! Two different callables must not have a `harness` parameter introduced, for
//! the same underlying reason: something outside the fixer's view calls them at
//! their declared arity. A callable whose value is read as a first-class
//! reference is invoked through a call site no static pass can see (#6146); a
//! callable declared `@host_entry` is entered by an embedding host whose only
//! registration lives in that host's source (#6193). This module owns both
//! halves of the decision: which names stay frozen, and the record of what
//! freezing actually blocked.

use std::collections::BTreeSet;

use super::CallableInfo;

/// A callable whose signature the capability migration wanted to change but
/// could not, because its value is read as a first-class reference.
///
/// Freezing is correct (#6146) — the callable is invoked at its declared arity
/// through a call site no static pass can see. Freezing SILENTLY is not: the
/// frozen callable is the owner of the ambient capability use, so
/// `add_harness_param_edit` returns `None` and the `?` discards the entire
/// file's repair. The run then reports `applied 0 repair(s), skipped 0` and
/// exits 0 with the capability diagnostics still standing, giving an operator
/// no way to learn which callable blocked the migration (#6153).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct FrozenCallable {
    pub(super) name: String,
    pub(super) reason: String,
}

/// Why one callable's signature is frozen.
///
/// The two causes are equally final but not equally explicable, and an
/// operator's next move differs: a value reference can be wrapped in a closure,
/// while a host contract has to change on the host's side. Collapsing them to
/// one message would send the reader to the wrong fix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FrozenCause {
    /// The callable's value is read as a first-class reference (#6146).
    ValueReference,
    /// The callable is declared `@host_entry` (#6193).
    HostEntry,
}

/// The value-escape facts one planning pass carries.
///
/// `referenced_by_value` names what must not be re-signed;
/// `frozen` collects what that actually blocked. They are decided together and
/// consumed together, so they travel as one value rather than as two
/// positional parameters threaded through every synthesizer.
pub(super) struct ValueEscape<'a> {
    pub(super) referenced_by_value: &'a BTreeSet<String>,
    pub(super) frozen: &'a mut Vec<FrozenCallable>,
}

impl ValueEscape<'_> {
    /// Note a frozen callable before the edit that is about to fail on it.
    ///
    /// `add_harness_param_edit` returns `None` for a frozen callable and the
    /// caller's `?` then discards the whole repair, so this must run BEFORE it
    /// or the reason is lost along with the candidate.
    pub(super) fn record(&mut self, info: &CallableInfo) {
        let Some(cause) = info.frozen_cause else {
            return;
        };
        if self.frozen.iter().any(|entry| entry.name == info.name) {
            return;
        }
        self.frozen.push(FrozenCallable::new(&info.name, cause));
    }
}

impl FrozenCallable {
    pub(super) fn new(name: &str, cause: FrozenCause) -> Self {
        let reason = match cause {
            FrozenCause::ValueReference => format!(
                "its value is read as a first-class reference, so it is invoked at its declared arity through a call site the fixer cannot see. It owns the ambient capability use, and a capability it cannot receive cannot be threaded into its body — pass the capability through an existing parameter, or wrap the reference as `{{ args -> {name}(harness, args) }}`"
            ),
            FrozenCause::HostEntry => format!(
                "it is declared `@host_entry`, so an embedding host supplies its arguments at the arity it declares. It owns the ambient capability use, and a parameter the host was never asked to pass cannot be introduced — thread the capability through an existing parameter, or have the host pass it and drop `@host_entry` from `{name}`"
            ),
        };
        Self {
            name: name.to_string(),
            reason,
        }
    }
}
