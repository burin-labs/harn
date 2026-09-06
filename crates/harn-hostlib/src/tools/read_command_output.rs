//! `tools/read_command_output` — range-read command runner artifacts.

use harn_vm::VmValue;

use crate::error::HostlibError;
use crate::tools::args::to_agent_path;
use crate::tools::payload::{optional_string, optional_u64, require_dict_arg};
use crate::tools::proc;
use crate::tools::response::ResponseBuilder;

pub(crate) const NAME: &str = "hostlib_tools_read_command_output";

pub(crate) fn handle(args: &[VmValue]) -> Result<VmValue, HostlibError> {
    let map = require_dict_arg(NAME, args)?;
    let command_id = optional_string(NAME, &map, "command_id")?;
    let handle_id = optional_string(NAME, &map, "handle_id")?;
    let path = optional_string(NAME, &map, "path")?;
    let offset = optional_u64(NAME, &map, "offset")?.unwrap_or(0);
    let length = optional_u64(NAME, &map, "length")?.unwrap_or(64 * 1024);

    let path = path.map(std::path::PathBuf::from);
    let Some(read) = proc::read_output(
        command_id.as_deref(),
        handle_id.as_deref(),
        path.as_deref(),
        offset,
        length,
    )?
    else {
        return Err(HostlibError::MissingParameter {
            builtin: NAME,
            param: "command_id|handle_id|path",
        });
    };
    let bytes_read = read.bytes.len();

    Ok(ResponseBuilder::new()
        .str("path", to_agent_path(&read.path))
        .int("offset", read.offset as i64)
        .int("bytes_read", bytes_read as i64)
        .int("total_bytes", read.total_bytes as i64)
        .bool(
            "eof",
            read.offset.saturating_add(bytes_read as u64) >= read.total_bytes,
        )
        .str("content", String::from_utf8_lossy(&read.bytes).into_owned())
        .build())
}
