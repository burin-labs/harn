//! Harn Flow — agent-native shipping substrate.
//!
//! See parent epic #571 for the four-primitive model (atoms, intents, slices,
//! streams). This module currently implements the foundational primitives,
//! [`Atom`](atom::Atom) and [`Intent`](intent::Intent).

pub mod atom;
pub mod intent;

pub use atom::{Atom, AtomError, AtomId, AtomSignature, Provenance, TextOp};
pub use intent::{
    Intent, IntentBoundaryClassifier, IntentBoundaryDecision, IntentBoundaryDispute,
    IntentClusterOptions, IntentClusterer, IntentError, IntentId, ObservedAtom, SealedIntent,
    TranscriptSpan,
};
