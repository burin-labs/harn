//! `tools/list_handles` — enumerate the live long-running command handles for a
//! session, for the agent loop's command-ledger reconciliation and digest
//! rendering.
//!
//! Accepts an optional `{ session_id: string }` (defaults to the current agent
//! session). Returns a list of rows, empty when nothing is live:
//!
//! ```json
//! [{ "handle_id": "hto-…", "session_id": "…", "lease": "awaited"|"service",
//!    "command_or_op_descriptor": "cargo build --release", "started_at": "…" }]
//! ```
//!
//! An entry leaves this list the instant its process exits and its waiter drains
//! it, so a completed command is never reported as live.

use harn_vm::VmValue;

use crate::error::HostlibError;
use crate::tools::payload::{optional_string, require_dict_arg};

pub(crate) const NAME: &str = "hostlib_tools_list_handles";

pub(crate) fn handle(args: &[VmValue]) -> Result<VmValue, HostlibError> {
    // Tolerate a bare call, an empty dict, or `{ session_id }`.
    let requested = match args.first() {
        Some(VmValue::Dict(_)) => {
            let map = require_dict_arg(NAME, args)?;
            optional_string(NAME, &map, "session_id")?
        }
        _ => None,
    };
    let session_id = requested
        .filter(|s| !s.is_empty())
        .or_else(harn_vm::current_agent_session_id)
        .unwrap_or_default();
    Ok(super::long_running::list_session_handles(&session_id))
}
