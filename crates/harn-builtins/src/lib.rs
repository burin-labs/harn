#![recursion_limit = "256"]

//! Native builtin families of the Harn standard library that sit above the VM.
//!
//! Each family registers itself with the runtime through
//! `harn_vm_core::stdlib_installer!` and keeps its thread-local state behind
//! `harn_vm_core::stdlib_state_reset!`, so `harn-vm` names none of them and this
//! crate compiles beside the rest of the runtime.

pub mod agent_state;
pub mod approval_review_policy;
pub mod artifact_emit;
pub mod calendar;
pub mod channel_guardrails;
pub mod channels;
pub mod command_policy;
pub mod compaction;
#[cfg(feature = "compression")]
pub mod compression;
pub mod config;
pub mod cookies;
pub mod cron;
pub mod csv;
pub mod diff;
#[cfg(feature = "content")]
pub mod document;
pub mod durable_step;
pub mod event_log;
pub mod external_agent;
pub mod git;
pub mod grounding;
pub mod hitl_read;
pub mod http_response;
pub mod jsonrpc;
pub mod lifecycle_receipts;
pub mod makefile;
#[cfg(feature = "content")]
pub mod markup;
pub mod multipart;
pub mod net;
pub mod oauth_dynreg;
pub mod oauth_storage;
pub mod path;
pub mod path_scope_guard;
pub mod portable;
#[cfg(feature = "postgres")]
pub mod postgres;
pub mod project;
pub mod project_catalog;
pub mod project_enrich;
pub mod review;
pub mod secret_scan;
#[cfg(any(feature = "sqlite", feature = "postgres"))]
pub mod sql_common;
#[cfg(feature = "sqlite")]
pub mod sqlite;
pub mod testbench;
pub mod timing;
pub mod tool_hooks;
pub mod transcript_compact;
pub mod tui;
pub mod url_parse;
#[cfg(feature = "content")]
pub mod web;
pub mod workflow_messages;

/// Reference this crate so a binary links its builtins. Called by `harn_runtime::force_link`.
pub fn force_link() {}
