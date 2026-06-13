//! `tools/cancel_handle` — cancel a specific in-flight long-running tool.
//!
//! Accepts `{ handle_id: string }`. Kills the spawned process (SIGKILL) and
//! lets the waiter drain artifacts. Returns:
//!
//! ```json
//! { "handle_id": "...", "cancelled": true|false }
//! ```
//!
//! `cancelled: false` means the handle was not found — either it already
//! completed or the id was invalid.

use harn_vm::VmDictExt;
use std::collections::BTreeMap;
use std::time::Duration;

use harn_vm::VmValue;

use crate::error::HostlibError;
use crate::tools::payload::{optional_bool, optional_u64, require_dict_arg, require_string};

pub(crate) const NAME: &str = "hostlib_tools_cancel_handle";

pub(crate) fn handle(args: &[VmValue]) -> Result<VmValue, HostlibError> {
    let map = require_dict_arg(NAME, args)?;
    let handle_id = require_string(NAME, &map, "handle_id")?;
    let wait_result = optional_u64(NAME, &map, "wait_result_ms")?.map(Duration::from_millis);
    let timed_out = optional_bool(NAME, &map, "timed_out")?.unwrap_or(false);

    let outcome = super::long_running::cancel_handle_with_options(
        &handle_id,
        super::long_running::CancelOptions {
            timed_out,
            wait_result,
        },
    );
    let cancelled = outcome.cancelled || harn_vm::cancel_long_running_handle(&handle_id);

    let mut out = BTreeMap::new();
    out.put_str("handle_id", handle_id);
    out.insert("cancelled".to_string(), VmValue::Bool(cancelled));
    if let Some(result) = outcome.result {
        out.insert("result".to_string(), result);
    }
    Ok(VmValue::dict(out))
}
