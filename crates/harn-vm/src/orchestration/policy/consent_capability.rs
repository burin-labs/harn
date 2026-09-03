//! Whether a `host_call` is the VM's own policy machinery asking for consent.
//!
//! A `command_policy` consent gate runs inside the execution scope of whichever
//! tool triggered it, so its `host_call("permission.request", ...)` is measured
//! against that tool's capability ceiling. A tool that runs commands declares
//! `process: ["exec"]` and has no other use for a `permission` capability, so
//! the gate could not ask. The throw surfaced inside the embedder's `try`,
//! where it read as "there is nobody to ask" while the host was reachable and,
//! on a non-interactive run with an authorized envelope, ready to approve.
//!
//! Asking for consent is not work the model requested. It is the VM deciding
//! whether to allow work the model requested, so it carries the policy
//! machinery's own capability instead of the tool's.
//!
//! The grant is deliberately one operation wide and lives in its own file so
//! that stays visible. Widening it is an edit to this module, not an
//! adjustment buried in a match arm.

use crate::orchestration::command_policy::command_policy_hook_depth;

/// The single capability the VM's own command hooks may exercise regardless of
/// the calling tool's ceiling.
const CONSENT_CAPABILITY: (&str, &str) = ("permission", "request");

/// Whether `capability.operation` is the consent gate asking on its own behalf.
///
/// `command_policy_hook_depth` is non-zero only while the VM is inside one of
/// its own command hooks, and `AmbientExecutionScope` swaps it per task, so
/// this stays correct across an `.await` and cannot leak to a sibling task
/// interleaving on the same thread.
///
/// False for the same call made by a tool directly, which keeps that refused by
/// the tool's own ceiling.
pub(super) fn is_policy_machinery_consent_call(capability: &str, operation: &str) -> bool {
    (capability, operation) == CONSENT_CAPABILITY && command_policy_hook_depth() > 0
}
