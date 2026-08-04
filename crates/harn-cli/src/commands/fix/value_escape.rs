//! Value-escape facts for the capability migration.
//!
//! A callable whose value is read as a first-class reference is invoked at its
//! declared arity through a call site no static pass can see, so its signature
//! must not move (#6146). This module owns both halves of that decision: the
//! set of names that must stay frozen, and the record of what freezing
//! actually blocked.

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
        if info.can_add_harness_param {
            return;
        }
        if self.frozen.iter().any(|entry| entry.name == info.name) {
            return;
        }
        self.frozen.push(FrozenCallable::new(&info.name));
    }
}

impl FrozenCallable {
    pub(super) fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            reason: format!(
                "its value is read as a first-class reference, so it is invoked at its declared arity through a call site the fixer cannot see. It owns the ambient capability use, and a capability it cannot receive cannot be threaded into its body — pass the capability through an existing parameter, or wrap the reference as `{{ args -> {name}(harness, args) }}`"
            ),
        }
    }
}
