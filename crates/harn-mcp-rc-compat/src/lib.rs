//! MCP DRAFT-2026-v1 release-candidate compatibility harness.
//!
//! This crate provides three building blocks that together exercise the
//! wire surface every Harn MCP component has to agree on:
//!
//! - [`fake_server`] spins up an in-process RC MCP server (HTTP or stdio)
//!   that intentionally covers modern success, unsupported-version retry,
//!   `server/discover`, cache hints, MRTR/input-required, header
//!   mismatch, and no-session HTTP. Drives Harn's MCP **client**.
//! - [`fake_client`] drives any Harn MCP server (the generic
//!   `harn-serve::McpServer` and the orchestrator both implement the
//!   same wire) with RC request sequences. Used to validate **server**
//!   behavior end-to-end.
//! - [`fixtures`] is the checked-in wire vocabulary every test loads
//!   from. The same JSON is exported under
//!   `spec/protocol-artifacts/fixtures/mcp-rc/` so downstream hosts and
//!   a cloud platform can replay the same flows in their own test suites.
//!
//! Failures are surfaced per-surface (client, generic server,
//! orchestrator server, artifacts, downstream-consumers) so CI breakage
//! attribution is unambiguous.

pub mod fake_client;
pub mod fake_server;
pub mod fixtures;
pub mod generic_server_harness;
pub mod recursive_schema;

/// Workspace-relative path to the canonical wire fixture root. Tests
/// from any crate in the workspace can load fixtures by joining a
/// fixture name onto this root.
pub const FIXTURE_ROOT: &str = "spec/protocol-artifacts/fixtures/mcp-rc";
