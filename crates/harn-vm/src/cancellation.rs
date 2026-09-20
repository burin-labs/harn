//! The one place the host-cancellation error is constructed.
//!
//! A cancellation that is thrown without dispatching `on_interrupt` handlers
//! is a cleanup hook that silently does not run. That drifted across eleven
//! call sites because nothing said they had to agree: three modules each grew
//! their own constructor for the identical payload, two of them sharing a
//! function name, and three more wrote it inline.
//!
//! Every cancellation now states, in this constructor's signature, whether
//! handlers ran for it or the reason they could not. `scripts/check_cancellation_owner.harn`
//! keeps the payload to this file so a twelfth site cannot compile past CI.

use crate::value::{VmError, VmValue};

/// The thrown payload.
///
/// This text is a wire contract, not an implementation detail: consumers match
/// it, one by prefix and one by full equality. It may not change shape without
/// changing them, which is tracked separately as narrowing those consumers to
/// a typed match.
const CANCELLATION_PAYLOAD: &str = "kind:cancelled:VM cancelled by host";

/// Why a cancellation could not run interrupt handlers.
///
/// Each variant is a claim about the call site that a reviewer can check, not
/// a catch-all. Adding one means a new reason genuinely exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NotDispatchedReason {
    /// The observing code holds a cancellation token and no machine, so it has
    /// nothing to dispatch through. Its caller dispatches instead.
    NoMachineInScope,
    /// The cancellation is being replayed from a recorded run. Handlers ran
    /// when the run was live; running them again would repeat their effects.
    ReplayPath,
}

impl NotDispatchedReason {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::NoMachineInScope => "no machine in scope",
            Self::ReplayPath => "replay path",
        }
    }
}

/// Whether interrupt handlers ran for this cancellation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HandlerDispatch {
    /// Handlers were dispatched through the machine's single owner before this
    /// error was constructed.
    Dispatched,
    /// Handlers did not run, for the stated reason.
    NotDispatched(NotDispatchedReason),
}

/// Construct the host-cancellation error.
///
/// The payload is identical for every caller. `dispatch` records what happened
/// to the interrupt handlers so a receipt can report it; it deliberately does
/// not reach the thrown value, because consumers match that value by text.
pub(crate) fn cancelled_error(dispatch: HandlerDispatch) -> VmError {
    if let HandlerDispatch::NotDispatched(reason) = dispatch {
        record_undispatched_cancellation(reason);
    }
    VmError::Thrown(VmValue::String(arcstr::ArcStr::from(CANCELLATION_PAYLOAD)))
}

/// Note a cancellation that ran no handlers, so the count is visible rather
/// than inferred from silence.
fn record_undispatched_cancellation(reason: NotDispatchedReason) {
    tracing::debug!(
        target: "harn::cancellation",
        reason = reason.as_str(),
        "cancellation thrown without dispatching interrupt handlers",
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The payload is a wire contract. If this assertion has to change, the two
    /// consumers that match the text have to change in the same commit.
    #[test]
    fn payload_is_byte_identical_for_every_dispatch_outcome() {
        let expected = "kind:cancelled:VM cancelled by host";
        for dispatch in [
            HandlerDispatch::Dispatched,
            HandlerDispatch::NotDispatched(NotDispatchedReason::NoMachineInScope),
            HandlerDispatch::NotDispatched(NotDispatchedReason::ReplayPath),
        ] {
            match cancelled_error(dispatch) {
                VmError::Thrown(VmValue::String(message)) => {
                    assert_eq!(message.as_str(), expected);
                }
                other => panic!("cancellation must stay runtime control flow, got {other:?}"),
            }
        }
    }
}
