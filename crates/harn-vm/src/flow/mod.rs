//! Harn Flow — agent-native shipping substrate.
//!
//! See parent epic #571 for the four-primitive model (atoms, intents, slices,
//! streams). This module currently implements the foundational primitive,
//! [`Atom`](atom::Atom).

pub mod atom;

pub use atom::{Atom, AtomError, AtomId, AtomSignature, Provenance, TextOp};
