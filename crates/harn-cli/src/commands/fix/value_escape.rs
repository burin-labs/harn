//! Frozen-signature facts for the capability migration.
//!
//! Some callables must not have their capability parameter chosen by the
//! migration, for one underlying reason: something outside the fixer's view
//! decides how they are called. A callable whose value is read as a first-class
//! reference is invoked through a call site no static pass can see (#6146); one
//! declared `@host_entry` is entered by an embedding host whose registration
//! lives in that host's source (#6193); one named by a `harn.toml` hook or
//! trigger is entered by the runtime through a registration that is written
//! down but is not a call site (#6272). Value references can be unblocked by
//! wrapping the hand-over site (see `value_wrap`); host and manifest entries
//! cannot. This module owns the freeze record.

use std::collections::{BTreeMap, BTreeSet};

use super::value_wrap::format_escape_sites;
use super::CallableInfo;

/// A callable whose signature the capability migration wanted to change but
/// could not.
///
/// Freezing the CALLABLE is correct (#6146 / #6193 / #6272). Freezing SILENTLY
/// is not (#6153): the owner of the ambient capability use returns `None` from
/// `add_harness_param_edit`, and that `?` aborts that owner's repair synthesis.
/// Sibling callables in the same file are planned as separate repairs and still
/// migrate — partial file repair is sound because each repair's `needed` set
/// only contains the owner and its unfrozen callers, never a frozen neighbour.
/// What must not stay silent is *which* callable blocked *its* ambient uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct FrozenCallable {
    pub(super) name: String,
    pub(super) reason: String,
}

/// Why one callable's signature is frozen.
///
/// The causes are equally final but not equally explicable, and an operator's
/// next move differs: a value reference can be wrapped in a closure, a host
/// contract has to change on the host's side, and a manifest handler is a
/// registration to edit. Collapsing them to one message would send the reader
/// to the wrong fix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FrozenCause {
    /// The callable's value is read as a first-class reference (#6146).
    ValueReference,
    /// The callable is declared `@host_entry` (#6193).
    HostEntry,
    /// A `harn.toml` block registers the callable as a runtime entry point
    /// (#6272). Same contract as `@host_entry`, already written down.
    ManifestHandler,
}

/// The value-escape facts one planning pass carries.
///
/// `referenced_by_value` names what must not be re-signed without a wrap;
/// `escape_sites` maps each such name to the `file:line` hand-over locations
/// so a freeze reason is a residual work list rather than a search;
/// `manifest_handlers` names this file's runtime registrations from
/// `harn.toml`; `frozen` collects what that actually blocked.
pub(super) struct ValueEscape<'a> {
    pub(super) referenced_by_value: &'a BTreeSet<String>,
    pub(super) escape_sites: &'a BTreeMap<String, Vec<(String, usize)>>,
    /// Names this file's manifest registers as runtime handlers, already
    /// resolved to this file — see `manifest_host_entries`.
    pub(super) manifest_handlers: &'a BTreeSet<String>,
    pub(super) frozen: &'a mut Vec<FrozenCallable>,
}

impl ValueEscape<'_> {
    /// Note a frozen callable before the edit that is about to fail on it.
    ///
    /// `add_harness_param_edit` returns `None` for a frozen callable and the
    /// caller's `?` then discards that repair, so this must run BEFORE it or
    /// the reason is lost along with the candidate.
    pub(super) fn record(&mut self, info: &CallableInfo) {
        let Some(cause) = info.frozen_cause else {
            return;
        };
        let sites = self
            .escape_sites
            .get(&info.name)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        if self.frozen.iter().any(|entry| entry.name == info.name) {
            return;
        }
        self.frozen
            .push(FrozenCallable::new(&info.name, cause, sites));
    }
}

impl FrozenCallable {
    pub(super) fn new(name: &str, cause: FrozenCause, sites: &[(String, usize)]) -> Self {
        let site_note = format_escape_sites(sites);
        let reason = match cause {
            FrozenCause::ValueReference => format!(
                "its value is read as a first-class reference, so it is invoked at its declared arity through a call site the fixer cannot see{site_note}. It owns the ambient capability use, and a capability it cannot receive cannot be threaded into its body — pass the capability through an existing parameter, or wrap the reference as `{{ args -> {name}(harness, args) }}`"
            ),
            FrozenCause::HostEntry => format!(
                "it is declared `@host_entry`, so an embedding host supplies its arguments at the arity it declares{site_note}. It owns the ambient capability use, and a parameter the host was never asked to pass cannot be introduced — thread the capability through an existing parameter, or have the host pass it and drop `@host_entry` from `{name}`"
            ),
            FrozenCause::ManifestHandler => format!(
                "`harn.toml` registers it as a runtime handler, so the runtime supplies its capability argument — and it supplies the root `Harness`, which a narrowed or record carrier cannot receive{site_note}. Thread the capability through an existing parameter, declare the root handle yourself, or remove the `harn.toml` block that names `{name}`"
            ),
        };
        Self {
            name: name.to_string(),
            reason,
        }
    }
}
