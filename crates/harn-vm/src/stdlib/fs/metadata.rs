use std::collections::BTreeMap;

use crate::stdlib::args::Args;
use crate::stdlib::macros::harn_builtin;
use crate::value::{VmDictExt, VmError, VmValue};

use super::{io_error_thrown, resolve_fs_path};

#[cfg(unix)]
pub(super) fn unix_file_identity(metadata: &std::fs::Metadata) -> String {
    use std::os::unix::fs::MetadataExt;
    format!("unix:{}:{}", metadata.dev(), metadata.ino())
}

#[harn_builtin(
    exposure = "runtime_internal",
    sig = "stat(path: string, options?: {follow_symlinks?: bool}) -> {size: int, is_file: bool, is_dir: bool, is_symlink: bool, readonly: bool, modified?: float, mode?: int, identity?: string}",
    category = "fs",
    doc = "Read file metadata. Set follow_symlinks=false to inspect the link itself. Unix hosts also report permission mode bits and file identity."
)]
fn stat_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let reader = Args::thrown("stat", args);
    reader.arity(1, 2)?;
    let path = reader.string(0, "path")?;
    let mut options = reader.options(1, "options")?;
    let follow_symlinks = options.bool_or("follow_symlinks", true)?;
    options.finish(&[])?;
    let resolved = resolve_fs_path(path)?;
    let authorize = if follow_symlinks {
        crate::stdlib::sandbox::enforce_fs_path
    } else {
        crate::stdlib::sandbox::enforce_fs_entry_path
    };
    authorize("stat", &resolved, crate::stdlib::sandbox::FsAccess::Read)?;
    let metadata = if follow_symlinks {
        std::fs::metadata(&resolved)
    } else {
        std::fs::symlink_metadata(&resolved)
    }
    .map_err(|e| io_error_thrown(format!("Failed to stat {}: {e}", resolved.display()), &e))?;
    let mut info = BTreeMap::new();
    info.put_int("size", metadata.len() as i64);
    info.put_bool("is_file", metadata.is_file());
    info.put_bool("is_dir", metadata.is_dir());
    info.put_bool("is_symlink", metadata.is_symlink());
    info.put_bool("readonly", metadata.permissions().readonly());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        info.put_int("mode", i64::from(metadata.permissions().mode()));
        info.put_str("identity", unix_file_identity(&metadata));
    }
    if let Ok(modified) = metadata.modified() {
        if let Ok(dur) = modified.duration_since(std::time::UNIX_EPOCH) {
            info.put("modified", VmValue::Float(dur.as_secs_f64()));
        }
    }
    Ok(VmValue::dict(info))
}
