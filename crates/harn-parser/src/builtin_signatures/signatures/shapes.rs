//! Shared structural-record [`Ty::Shape`] aliases reused across signature
//! groups.
//!
//! The shape vocabulary now lives in the dep-free `harn-builtin-meta` crate
//! (`harn_builtin_meta::shapes`) so that both this typechecking side and the
//! `#[harn_builtin]` proc-macro's `@NAME` signature injection can reference
//! one definition. This module re-exports them so existing
//! `super::shapes::NAME` call sites keep resolving.

pub(crate) use harn_builtin_meta::shapes::*;
