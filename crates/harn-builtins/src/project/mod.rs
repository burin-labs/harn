//! Project scanning, fingerprinting, context-profile resolution, git-remote
//! detection, MCP-preset credential checks, and the VM builtin registration
//! glue that exposes them to Harn pipelines.

mod builtins;
mod context_profile;
mod git_remote;
mod mcp;
mod scan;
mod shared;

use context_profile::*;
use git_remote::*;
use mcp::*;
use scan::*;
use shared::*;

pub(crate) use builtins::project_scan_config_value;
pub(crate) use scan::project_language;

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
