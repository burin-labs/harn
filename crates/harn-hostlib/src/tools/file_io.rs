//! `tools/{read_file, write_file, delete_file, list_directory}` —
//! deterministic filesystem primitives.
//!
//! Shapes are locked by `schemas/tools/{read_file,write_file,delete_file,list_directory}.{request,response}.json`.

use std::fs as stdfs;
use std::io::{Read, Seek};
use std::path::PathBuf;
use std::rc::Rc;

use base64::Engine;
use harn_vm::VmValue;

use crate::error::HostlibError;
use crate::tools::args::{
    build_dict, dict_arg, optional_bool, optional_int, optional_string, require_string, str_value,
};

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

    stdfs::write(&path, &bytes).map_err(|err| HostlibError::Backend {
        builtin: WRITE_FILE_BUILTIN,
        message: format!("write `{path_str}`: {err}"),
    })?;

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
            let metadata = match entry.metadata() {
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
        ("entries", VmValue::List(Rc::new(entries_list))),
        ("truncated", VmValue::Bool(truncated)),
    ]))
}

fn file_size(metadata: &stdfs::Metadata) -> u64 {
    metadata.len()
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
