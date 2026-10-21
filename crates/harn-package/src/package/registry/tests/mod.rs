//! Tests for the package registry and cache, grouped by the concern each one
//! covers.

mod cache;
#[cfg(unix)]
mod git_lifecycle;
mod git_source;
mod index;
mod resolve;
mod source;
mod verify;
