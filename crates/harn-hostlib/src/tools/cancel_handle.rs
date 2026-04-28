//! `tools/cancel_handle` — cancel a specific in-flight long-running tool.
//!
//! Accepts `{ handle_id: string }`. Kills the spawned process (SIGKILL) and
//! removes the entry from the handle store. Returns:
//!
//! ```json
//! { "handle_id": "...", "cancelled": true|false }
//! ```
//!
//! `cancelled: false` means the handle was not found — either it already
//! completed or the id was invalid.

use harn_vm::VmValue;

use crate::error::HostlibError;
use crate::tools::payload::{require_dict_arg, require_string};
use crate::tools::response::ResponseBuilder;

pub(crate) const NAME: &str = "hostlib_tools_cancel_handle";

pub(crate) fn handle(args: &[VmValue]) -> Result<VmValue, HostlibError> {
    let map = require_dict_arg(NAME, args)?;
    let handle_id = require_string(NAME, &map, "handle_id")?;

    let cancelled = super::long_running::cancel_handle(&handle_id);

    Ok(ResponseBuilder::new()
        .str("handle_id", handle_id)
        .bool("cancelled", cancelled)
        .build())
}
