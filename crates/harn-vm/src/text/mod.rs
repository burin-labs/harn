//! Shared text utilities.
//!
//! These are plain string helpers with no VM dependency; they live here
//! because `harn-vm` is the lowest crate reachable from every consumer
//! (`harn-cli`, `harn-hostlib`, `harn-lint`), and a leaf crate of their own
//! would not earn its keep for three small modules.

pub mod ansi;
pub mod case;
pub mod truncate;

pub use ansi::strip_ansi;
pub use truncate::{truncate_end, truncate_end_bytes, truncate_start};
