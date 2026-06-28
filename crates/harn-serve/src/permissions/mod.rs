//! The single authoritative permission primitive for harn.
//!
//! Subsumes what used to live as four parallel implementations: the TUI
//! permission policy, a host IDE approval modal, a cloud
//! gateway middleware, and a cloud sandbox backend. Each of those
//! surfaces now delegates to the same data model, store, and audit
//! channel exposed here, so a user's "remember this answer" rule flows
//! between local sessions and supervised cloud agents identically.
//!
//! The pieces (each in its own file):
//!
//! - [`policy`] — declarative [`PermissionPolicy`]: read/write/exec/net
//!   globs, llm provider list with optional cost ceiling, redaction
//!   patterns, version content-hashed.
//! - [`request`] — runtime types: [`PermissionRequest`], the
//!   [`PermissionDecision`] verdict it produces, and the [`Risk`] /
//!   [`DecisionScope`] dimensions agents and humans key off of.
//! - [`rules`] — persistent "remember" rules. Each rule pins one
//!   action+target shape to a verdict at a chosen scope (session,
//!   workspace, user, always), with optional `expires_at` for
//!   time-bound grants.
//! - [`store`] — [`PermissionStore`] trait + in-memory implementation
//!   that owns the rule set + audit history. Lives behind a trait so
//!   the eventual A.5 session-store can swap in a durable backend
//!   without touching any caller.
//! - [`audit`] — [`AuditEntry`] events emitted on every grant / deny /
//!   escalation, queryable through `store.history(filter)`.
//! - [`enforcement`] — the runtime arm: lowers a [`PermissionPolicy`]
//!   into the [`CapabilityPolicy`](harn_vm::orchestration::CapabilityPolicy)
//!   the VM enforces and (with the `hostlib` feature) the
//!   [`SandboxSpec`](harn_hostlib::sandbox::SandboxSpec) a sandbox
//!   backend provisions from.

pub mod audit;
pub mod enforcement;
pub mod policy;
pub mod request;
pub mod rules;
pub mod store;

pub use audit::{AuditEntry, AuditFilter, AuditOutcome};
#[cfg(feature = "hostlib")]
pub use enforcement::sandbox;
pub use policy::{LlmPolicy, PermissionPolicy, PolicyVersion, RedactionPolicy};
pub use request::{ActionClass, DecisionScope, PermissionDecision, PermissionRequest, Risk};
pub use rules::{RememberRule, RuleId};
pub use store::{InMemoryConfig, InMemoryPermissionStore, PermissionStore, RememberSpec};
