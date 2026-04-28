//! `tools/read_command_output` — range-read command runner artifacts.

use std::io::{Read, Seek, SeekFrom};

use harn_vm::VmValue;

use crate::error::HostlibError;
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

    let Some(path) =
        proc::resolve_output_path(command_id.as_deref(), handle_id.as_deref(), path.as_deref())
    else {
        return Err(HostlibError::MissingParameter {
            builtin: NAME,
            param: "command_id|handle_id|path",
        });
    };
    if command_id.is_none() && handle_id.is_none() && !looks_like_command_artifact_path(&path) {
        return Err(HostlibError::InvalidParameter {
            builtin: NAME,
            param: "path",
            message: "path must point at a harn-command artifact directory".to_string(),
        });
    }

    let mut file = std::fs::File::open(&path).map_err(|e| HostlibError::Backend {
        builtin: NAME,
        message: format!("failed to open command output '{}': {e}", path.display()),
    })?;
    let total_bytes = file.metadata().map(|m| m.len()).unwrap_or(0);
    file.seek(SeekFrom::Start(offset))
        .map_err(|e| HostlibError::Backend {
            builtin: NAME,
            message: format!("failed to seek command output '{}': {e}", path.display()),
        })?;
    let mut buf = vec![
        0_u8;
        usize::try_from(length)
            .unwrap_or(usize::MAX)
            .min(1024 * 1024)
    ];
    let bytes_read = file.read(&mut buf).map_err(|e| HostlibError::Backend {
        builtin: NAME,
        message: format!("failed to read command output '{}': {e}", path.display()),
    })?;
    buf.truncate(bytes_read);

    Ok(ResponseBuilder::new()
        .str("path", path.display().to_string())
        .int("offset", offset as i64)
        .int("bytes_read", bytes_read as i64)
        .int("total_bytes", total_bytes as i64)
        .bool(
            "eof",
            offset.saturating_add(bytes_read as u64) >= total_bytes,
        )
        .str("content", String::from_utf8_lossy(&buf).into_owned())
        .build())
}

fn looks_like_command_artifact_path(path: &std::path::Path) -> bool {
    let Some(parent) = path.parent() else {
        return false;
    };
    let Some(dir_name) = parent.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    dir_name.starts_with("harn-command-cmd_")
        && matches!(file_name, "combined.txt" | "stdout.txt" | "stderr.txt")
}
