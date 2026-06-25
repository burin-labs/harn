//! `tools/{read_file, write_file, delete_file, list_directory}` —
//! deterministic filesystem primitives.
//!
//! Shapes are locked by `schemas/tools/{read_file,write_file,delete_file,list_directory}.{request,response}.json`.

use std::fs as stdfs;
use std::io::{Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use base64::Engine;
use harn_vm::process_sandbox::FsAccess;
use harn_vm::VmValue;

use crate::error::HostlibError;
use crate::tools::args::{
    build_dict, dict_arg, optional_bool, optional_int, optional_string, require_string, str_value,
};
use crate::tools::permissions::enforce_path_scope;

const READ_FILE_BUILTIN: &str = "hostlib_tools_read_file";
const WRITE_FILE_BUILTIN: &str = "hostlib_tools_write_file";
const DELETE_FILE_BUILTIN: &str = "hostlib_tools_delete_file";
const LIST_DIRECTORY_BUILTIN: &str = "hostlib_tools_list_directory";

/// Encoding flavors accepted by [`read_file`] / produced by [`write_file`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Encoding {
    Utf8,
    Binary,
}

impl Encoding {
    fn parse(builtin: &'static str, raw: Option<&str>) -> Result<Self, HostlibError> {
        match raw {
            None | Some("utf-8") => Ok(Encoding::Utf8),
            Some("binary") => Ok(Encoding::Binary),
            Some(other) => Err(HostlibError::InvalidParameter {
                builtin,
                param: "encoding",
                message: format!("expected one of [\"utf-8\", \"binary\"], got `{other}`"),
            }),
        }
    }
}

pub(super) fn read_file(args: &[VmValue]) -> Result<VmValue, HostlibError> {
    let raw = dict_arg(READ_FILE_BUILTIN, args)?;
    let dict = raw.as_ref();

    let path_str = require_string(READ_FILE_BUILTIN, dict, "path")?;
    let offset = optional_int(READ_FILE_BUILTIN, dict, "offset", 0)?;
    let limit_bytes = optional_int(READ_FILE_BUILTIN, dict, "limit_bytes", 0)?;
    let encoding_raw = optional_string(READ_FILE_BUILTIN, dict, "encoding")?;
    let encoding = Encoding::parse(READ_FILE_BUILTIN, encoding_raw.as_deref())?;
    let session_id = optional_string(READ_FILE_BUILTIN, dict, "session_id")?;

    if offset < 0 {
        return Err(HostlibError::InvalidParameter {
            builtin: READ_FILE_BUILTIN,
            param: "offset",
            message: "must be >= 0".to_string(),
        });
    }
    if limit_bytes < 0 {
        return Err(HostlibError::InvalidParameter {
            builtin: READ_FILE_BUILTIN,
            param: "limit_bytes",
            message: "must be >= 0".to_string(),
        });
    }

    let path = PathBuf::from(&path_str);
    enforce_path_scope(READ_FILE_BUILTIN, &path, FsAccess::Read)?;
    let offset_u64 = offset as u64;
    let (buf, total_size) = read_bytes(
        &path,
        &path_str,
        session_id.as_deref(),
        offset_u64,
        limit_bytes as u64,
    )?;

    let truncated = (offset_u64 + buf.len() as u64) < total_size;

    let (content, response_encoding) = match encoding {
        Encoding::Utf8 => match std::str::from_utf8(&buf) {
            Ok(s) => (s.to_string(), "utf-8"),
            Err(_) => {
                // Fall back to base64 when the bytes aren't valid UTF-8 so
                // callers always get a string they can transport over JSON.
                (
                    base64::engine::general_purpose::STANDARD.encode(&buf),
                    "base64",
                )
            }
        },
        Encoding::Binary => (
            base64::engine::general_purpose::STANDARD.encode(&buf),
            "base64",
        ),
    };

    Ok(build_dict([
        ("path", str_value(&path_str)),
        ("encoding", str_value(response_encoding)),
        ("content", str_value(&content)),
        ("size", VmValue::Int(buf.len() as i64)),
        ("truncated", VmValue::Bool(truncated)),
    ]))
}

pub(super) fn write_file(args: &[VmValue]) -> Result<VmValue, HostlibError> {
    let raw = dict_arg(WRITE_FILE_BUILTIN, args)?;
    let dict = raw.as_ref();

    let path_str = require_string(WRITE_FILE_BUILTIN, dict, "path")?;
    let content = require_string(WRITE_FILE_BUILTIN, dict, "content")?;
    let encoding_raw = optional_string(WRITE_FILE_BUILTIN, dict, "encoding")?;
    let create_parents = optional_bool(WRITE_FILE_BUILTIN, dict, "create_parents", true)?;
    let overwrite = optional_bool(WRITE_FILE_BUILTIN, dict, "overwrite", true)?;
    let session_id = optional_string(WRITE_FILE_BUILTIN, dict, "session_id")?;

    let path = PathBuf::from(&path_str);
    enforce_path_scope(WRITE_FILE_BUILTIN, &path, FsAccess::Write)?;

    let bytes: Vec<u8> = match encoding_raw.as_deref() {
        None | Some("utf-8") => content.into_bytes(),
        Some("base64") => base64::engine::general_purpose::STANDARD
            .decode(content.as_bytes())
            .map_err(|err| HostlibError::InvalidParameter {
                builtin: WRITE_FILE_BUILTIN,
                param: "content",
                message: format!("invalid base64: {err}"),
            })?,
        Some(other) => {
            return Err(HostlibError::InvalidParameter {
                builtin: WRITE_FILE_BUILTIN,
                param: "encoding",
                message: format!("expected one of [\"utf-8\", \"base64\"], got `{other}`"),
            });
        }
    };

    if let Some(outcome) = crate::fs::stage_write_or_none(
        WRITE_FILE_BUILTIN,
        &path,
        &bytes,
        create_parents,
        overwrite,
        session_id.as_deref(),
    )? {
        return Ok(build_dict([
            ("path", str_value(&path_str)),
            ("bytes_written", VmValue::Int(outcome.bytes_written as i64)),
            ("created", VmValue::Bool(outcome.created)),
        ]));
    }

    let preexisted = path.exists();
    if preexisted && !overwrite {
        return Err(HostlibError::Backend {
            builtin: WRITE_FILE_BUILTIN,
            message: format!("`{path_str}` exists and overwrite=false"),
        });
    }

    // Capture the pre-image into any open snapshots before mutating disk
    // so a `session/restore_tool_call` can roll the write back surgically.
    crate::fs_snapshot::auto_capture_for_write(WRITE_FILE_BUILTIN, &path);

    if create_parents {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                stdfs::create_dir_all(parent).map_err(|err| HostlibError::Backend {
                    builtin: WRITE_FILE_BUILTIN,
                    message: format!("mkdir `{}`: {err}", parent.display()),
                })?;
            }
        }
    }

    write_no_follow(WRITE_FILE_BUILTIN, &path, &path_str, &bytes)?;

    Ok(build_dict([
        ("path", str_value(&path_str)),
        ("bytes_written", VmValue::Int(bytes.len() as i64)),
        ("created", VmValue::Bool(!preexisted)),
    ]))
}

pub(super) fn delete_file(args: &[VmValue]) -> Result<VmValue, HostlibError> {
    let raw = dict_arg(DELETE_FILE_BUILTIN, args)?;
    let dict = raw.as_ref();

    let path_str = require_string(DELETE_FILE_BUILTIN, dict, "path")?;
    let recursive = optional_bool(DELETE_FILE_BUILTIN, dict, "recursive", false)?;
    let session_id = optional_string(DELETE_FILE_BUILTIN, dict, "session_id")?;

    let path = PathBuf::from(&path_str);
    enforce_path_scope(DELETE_FILE_BUILTIN, &path, FsAccess::Delete)?;
    if let Some(removed) = crate::fs::stage_delete_or_none(
        DELETE_FILE_BUILTIN,
        &path,
        recursive,
        session_id.as_deref(),
    )? {
        return Ok(build_dict([
            ("path", str_value(&path_str)),
            ("removed", VmValue::Bool(removed)),
        ]));
    }

    let metadata = match stdfs::symlink_metadata(&path) {
        Ok(m) => m,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(build_dict([
                ("path", str_value(&path_str)),
                ("removed", VmValue::Bool(false)),
            ]));
        }
        Err(err) => {
            return Err(HostlibError::Backend {
                builtin: DELETE_FILE_BUILTIN,
                message: format!("stat `{path_str}`: {err}"),
            });
        }
    };

    // Defend against a symlink-swap TOCTOU: `enforce_path_scope` above
    // canonicalized the path through any symlinks and validated where it
    // *resolved*, but the actual `remove_*` runs on the raw path. If the
    // final component is a symlink whose target now escapes the workspace
    // roots, removing through it (notably `remove_dir_all`, which descends
    // into a symlinked directory's real target) could delete out-of-root
    // files. Re-validate the link target under the policy and reject an
    // escaping symlink rather than following it. Removing an in-scope
    // symlink, or a real file/dir, stays allowed.
    if metadata.file_type().is_symlink() {
        reject_escaping_symlink(DELETE_FILE_BUILTIN, &path, &path_str, FsAccess::Delete)?;
    }

    // Capture the pre-image into any open snapshots before mutating disk
    // so a `session/restore_tool_call` can roll the delete back.
    crate::fs_snapshot::auto_capture_for_write(DELETE_FILE_BUILTIN, &path);

    let removed = if metadata.is_dir() {
        if recursive {
            stdfs::remove_dir_all(&path).map_err(|err| HostlibError::Backend {
                builtin: DELETE_FILE_BUILTIN,
                message: format!("remove_dir_all `{path_str}`: {err}"),
            })?;
            true
        } else {
            stdfs::remove_dir(&path).map_err(|err| HostlibError::Backend {
                builtin: DELETE_FILE_BUILTIN,
                message: format!(
                    "remove_dir `{path_str}` (pass recursive=true to delete non-empty dirs): {err}"
                ),
            })?;
            true
        }
    } else {
        stdfs::remove_file(&path).map_err(|err| HostlibError::Backend {
            builtin: DELETE_FILE_BUILTIN,
            message: format!("remove_file `{path_str}`: {err}"),
        })?;
        true
    };

    Ok(build_dict([
        ("path", str_value(&path_str)),
        ("removed", VmValue::Bool(removed)),
    ]))
}

pub(super) fn list_directory(args: &[VmValue]) -> Result<VmValue, HostlibError> {
    let raw = dict_arg(LIST_DIRECTORY_BUILTIN, args)?;
    let dict = raw.as_ref();

    let path_str = require_string(LIST_DIRECTORY_BUILTIN, dict, "path")?;
    let include_hidden = optional_bool(LIST_DIRECTORY_BUILTIN, dict, "include_hidden", false)?;
    let max_entries = optional_int(LIST_DIRECTORY_BUILTIN, dict, "max_entries", 0)?;
    let session_id = optional_string(LIST_DIRECTORY_BUILTIN, dict, "session_id")?;

    if max_entries < 0 {
        return Err(HostlibError::InvalidParameter {
            builtin: LIST_DIRECTORY_BUILTIN,
            param: "max_entries",
            message: "must be >= 0".to_string(),
        });
    }
    let cap = if max_entries == 0 {
        usize::MAX
    } else {
        max_entries as usize
    };

    let path = PathBuf::from(&path_str);
    enforce_path_scope(LIST_DIRECTORY_BUILTIN, &path, FsAccess::Read)?;
    let mut entries: Vec<(String, VmValue)> = Vec::new();
    let mut truncated = false;
    let mut all_names: Vec<(String, bool, bool, u64)> = Vec::new();

    if let Some(read) = crate::fs::read_dir(&path, session_id.as_deref()) {
        for entry in read.map_err(|err| HostlibError::Backend {
            builtin: LIST_DIRECTORY_BUILTIN,
            message: format!("read_dir `{path_str}`: {err}"),
        })? {
            if !include_hidden && entry.name.starts_with('.') {
                continue;
            }
            all_names.push((entry.name, entry.is_dir, entry.is_symlink, entry.size));
        }
    } else {
        let read = stdfs::read_dir(&path).map_err(|err| HostlibError::Backend {
            builtin: LIST_DIRECTORY_BUILTIN,
            message: format!("read_dir `{path_str}`: {err}"),
        })?;
        for entry in read {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };
            let name = entry.file_name().to_string_lossy().into_owned();
            if !include_hidden && name.starts_with('.') {
                continue;
            }
            let metadata = match stdfs::symlink_metadata(entry.path()) {
                Ok(m) => m,
                Err(_) => continue,
            };
            let file_type = entry.file_type().ok();
            all_names.push((
                name,
                file_type.map(|t| t.is_dir()).unwrap_or(false),
                file_type.map(|t| t.is_symlink()).unwrap_or(false),
                file_size(&metadata),
            ));
        }
    }
    all_names.sort_by(|a, b| a.0.cmp(&b.0));

    for (name, is_dir, is_symlink, size) in all_names {
        if entries.len() >= cap {
            truncated = true;
            break;
        }
        let entry_value = build_dict([
            ("name", str_value(&name)),
            ("is_dir", VmValue::Bool(is_dir)),
            ("is_symlink", VmValue::Bool(is_symlink)),
            ("size", VmValue::Int(size as i64)),
        ]);
        entries.push((name, entry_value));
    }

    let entries_list: Vec<VmValue> = entries.into_iter().map(|(_, v)| v).collect();
    Ok(build_dict([
        ("path", str_value(&path_str)),
        ("entries", VmValue::List(Arc::new(entries_list))),
        ("truncated", VmValue::Bool(truncated)),
    ]))
}

fn file_size(metadata: &stdfs::Metadata) -> u64 {
    metadata.len()
}

/// Write `bytes` to `path` without ever following a symlink at the final
/// path component.
///
/// `enforce_path_scope` validated where `path` *resolves* by canonicalizing
/// a copy of it, but the subsequent disk write runs on the raw path and, by
/// default, follows a symlink at write time. An attacker with in-workspace
/// write access could swap the check-passed final component for a symlink
/// pointing outside the allowed roots between the scope check and the write
/// (a TOCTOU race), escaping the workspace.
///
/// On Unix we open with `O_NOFOLLOW`, which makes the kernel refuse to open
/// the final component if it is a symlink — closing the race atomically, so
/// the write targets the same inode the check observed (or fails). On other
/// platforms we fall back to an `lstat` (no-follow) check on the final
/// component before writing; this still rejects a symlink-final path,
/// shrinking the race window to the gap between the `lstat` and the open
/// (the OS sandbox layer remains the backstop there). Both paths keep
/// behavior identical for the normal case: creating a new file and
/// overwriting an existing real file inside the roots still succeed.
fn write_no_follow(
    builtin: &'static str,
    path: &Path,
    path_str: &str,
    bytes: &[u8],
) -> Result<(), HostlibError> {
    let mut options = stdfs::OpenOptions::new();
    options.write(true).create(true).truncate(true);

    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // libc::O_NOFOLLOW. Use the raw constant value so harn-hostlib does
        // not need to take a direct `libc` dependency; the value is part of
        // the stable Unix ABI across Linux and the BSDs/macOS.
        #[cfg(target_os = "linux")]
        const O_NOFOLLOW: i32 = 0x20000;
        #[cfg(not(target_os = "linux"))]
        const O_NOFOLLOW: i32 = 0x0100;
        options.custom_flags(O_NOFOLLOW);
    }

    #[cfg(not(unix))]
    {
        // Cross-platform fallback: reject a symlink at the final component
        // up front. lstat never follows the link, so this catches the swap
        // without resolving it.
        if let Ok(meta) = stdfs::symlink_metadata(path) {
            if meta.file_type().is_symlink() {
                return Err(HostlibError::SandboxViolation {
                    builtin,
                    path: path.display().to_string(),
                    message: format!(
                        "refusing to write `{path_str}`: final path component is a symlink \
                         (potential workspace-escape via symlink swap)"
                    ),
                });
            }
        }
    }

    let mut file = options.open(path).map_err(|err| {
        // On Unix an `O_NOFOLLOW` rejection of a symlink surfaces as ELOOP
        // ("Too many levels of symbolic links"); report it as a sandbox
        // violation rather than a generic backend error.
        #[cfg(unix)]
        if err.raw_os_error() == Some(40) {
            return HostlibError::SandboxViolation {
                builtin,
                path: path.display().to_string(),
                message: format!(
                    "refusing to write `{path_str}`: final path component is a symlink \
                     (potential workspace-escape via symlink swap)"
                ),
            };
        }
        HostlibError::Backend {
            builtin,
            message: format!("write `{path_str}`: {err}"),
        }
    })?;

    file.write_all(bytes).map_err(|err| HostlibError::Backend {
        builtin,
        message: format!("write `{path_str}`: {err}"),
    })
}

/// Reject deleting through a final-component symlink whose target escapes the
/// active workspace roots.
///
/// `enforce_path_scope` canonicalized the path through the symlink and may
/// have accepted it because the *target* was in-root at check time. Re-run
/// the scope check on the link's resolved target so a swap to an
/// out-of-root target is caught; if it now resolves outside the roots, the
/// delete is refused. An in-scope symlink (or a real file/dir) is left for
/// the caller to remove normally — `remove_file` unlinks a symlink itself
/// without following it, so only the escaping case needs blocking.
fn reject_escaping_symlink(
    builtin: &'static str,
    path: &Path,
    path_str: &str,
    access: FsAccess,
) -> Result<(), HostlibError> {
    // `enforce_path_scope` is a no-op when no restricted policy is active, so
    // re-running it here re-derives the same verdict against the symlink's
    // resolved target. If the target escapes the roots it now rejects.
    enforce_path_scope(builtin, path, access).map_err(|_| HostlibError::SandboxViolation {
        builtin,
        path: path.display().to_string(),
        message: format!(
            "refusing to delete `{path_str}`: final path component is a symlink whose \
             target escapes the workspace roots (potential symlink swap)"
        ),
    })
}

fn read_bytes(
    path: &PathBuf,
    path_str: &str,
    session_id: Option<&str>,
    offset: u64,
    limit_bytes: u64,
) -> Result<(Vec<u8>, u64), HostlibError> {
    if let Some(result) = crate::fs::read(path, session_id) {
        let bytes = result.map_err(|err| HostlibError::Backend {
            builtin: READ_FILE_BUILTIN,
            message: format!("read `{path_str}`: {err}"),
        })?;
        return slice_bytes(bytes, offset, limit_bytes);
    }

    let metadata = stdfs::metadata(path).map_err(|err| HostlibError::Backend {
        builtin: READ_FILE_BUILTIN,
        message: format!("stat `{path_str}`: {err}"),
    })?;
    if !metadata.is_file() {
        return Err(HostlibError::Backend {
            builtin: READ_FILE_BUILTIN,
            message: format!("`{path_str}` is not a regular file"),
        });
    }
    let total_size = metadata.len();
    validate_read_offset(offset, total_size)?;
    let to_read = planned_read_len(offset, limit_bytes, total_size);

    let mut file = stdfs::File::open(path).map_err(|err| HostlibError::Backend {
        builtin: READ_FILE_BUILTIN,
        message: format!("open `{path_str}`: {err}"),
    })?;
    if offset > 0 {
        file.seek(std::io::SeekFrom::Start(offset))
            .map_err(|err| HostlibError::Backend {
                builtin: READ_FILE_BUILTIN,
                message: format!("seek `{path_str}`: {err}"),
            })?;
    }
    let mut buf = Vec::with_capacity(to_read as usize);
    file.take(to_read)
        .read_to_end(&mut buf)
        .map_err(|err| HostlibError::Backend {
            builtin: READ_FILE_BUILTIN,
            message: format!("read `{path_str}`: {err}"),
        })?;
    Ok((buf, total_size))
}

fn slice_bytes(
    bytes: Vec<u8>,
    offset: u64,
    limit_bytes: u64,
) -> Result<(Vec<u8>, u64), HostlibError> {
    let total_size = bytes.len() as u64;
    validate_read_offset(offset, total_size)?;
    let to_read = planned_read_len(offset, limit_bytes, total_size);
    let start = offset as usize;
    let end = start + to_read as usize;
    Ok((bytes[start..end].to_vec(), total_size))
}

fn validate_read_offset(offset: u64, total_size: u64) -> Result<(), HostlibError> {
    if offset > total_size {
        return Err(HostlibError::InvalidParameter {
            builtin: READ_FILE_BUILTIN,
            param: "offset",
            message: format!("offset {offset} exceeds file length {total_size}"),
        });
    }
    Ok(())
}

fn planned_read_len(offset: u64, limit_bytes: u64, total_size: u64) -> u64 {
    if limit_bytes == 0 {
        total_size - offset
    } else {
        std::cmp::min(limit_bytes, total_size - offset)
    }
}
