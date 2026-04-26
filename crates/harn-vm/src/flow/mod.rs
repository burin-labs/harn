//! Harn Flow — agent-native shipping substrate.
//!
//! See parent epic #571 for the four-primitive model (atoms, intents, slices,
//! streams). This module currently implements the foundational primitives,
//! [`Atom`](atom::Atom), [`Intent`](intent::Intent), and [`Slice`](slice::Slice).

pub mod atom;
pub mod backend;
pub mod intent;
pub mod slice;

pub use atom::{Atom, AtomError, AtomId, AtomSignature, Provenance, TextOp};
pub use backend::{
    AtomRef, FlowNativeBackend, FlowSlice, GitExportReceipt, ShadowGitBackend, ShipReceipt,
    SliceId, VcsBackend, VcsBackendError,
};
pub use intent::{
    Intent, IntentBoundaryClassifier, IntentBoundaryDecision, IntentBoundaryDispute,
    IntentClusterOptions, IntentClusterer, IntentError, IntentId, ObservedAtom, SealedIntent,
    TranscriptSpan,
};
pub use slice::{
    derive_slice, Approval, CoverageMap, InvariantResult, PredicateHash, Slice,
    SliceDerivationError, SliceDerivationInput, SliceId, SliceStatus, TestId, UnresolvedParent,
};
