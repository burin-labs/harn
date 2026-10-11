//! Precompiled Harn stdlib module artifacts for a host binary.
//!
//! Without them, a host's first run on a new machine compiles every stdlib
//! module it imports and writes each one into the shared bytecode cache
//! (harn#9652). Link this crate and call [`register`] once at startup, before
//! the first Harn import, and those imports load from the binary instead.
//!
//! The artifacts are compiled by this crate's build script, with the same
//! harn-vm source the host links, when the `embed` feature is on. Without it
//! the table is empty and [`register`] changes nothing. The loader checks
//! every header, so a table that does not match the running compiler is
//! ignored rather than trusted.

pub use harn_vm::bytecode_cache::EmbeddedStdlibRegistrationError;

include!(concat!(env!("OUT_DIR"), "/stdlib_artifacts.rs"));

/// Register this binary's stdlib artifacts with the Harn loader.
///
/// # Errors
///
/// Refuses when a table is already registered in this process.
pub fn register() -> Result<(), EmbeddedStdlibRegistrationError> {
    harn_vm::bytecode_cache::register_embedded_stdlib_artifacts(ARTIFACTS)
}
