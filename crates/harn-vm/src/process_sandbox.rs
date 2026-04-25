//! Public re-exports of the platform-specific process sandbox primitives.
//!
//! Embedders that spawn subprocesses on behalf of Harn scripts (today: the
//! `harn-hostlib` deterministic-tool builtins) must funnel every spawn
//! through these helpers so the active orchestration capability policy is
//! enforced — Linux seccomp/landlock filters via `pre_exec`, macOS
//! `sandbox-exec` wrapping, plus workspace-root cwd enforcement.
//!
//! The helpers themselves live next to the rest of the sandbox state in
//! [`crate::stdlib::sandbox`]. This module exists so external crates have a
//! stable, documented surface to depend on without reaching into
//! `stdlib::*` plumbing.

pub use crate::stdlib::sandbox::{
    enforce_process_cwd, process_spawn_error, process_violation_error, std_command_for,
    tokio_command_for,
};
