//! `tools/{read_file, write_file, delete_file, list_directory}` —
//! deterministic filesystem primitives.
//!
//! Shapes are locked by `schemas/tools/{read_file,write_file,delete_file,list_directory}.{request,response}.json`.

use std::fs;
use std::io::Read;
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

    let metadata = fs::metadata(&path).map_err(|err| HostlibError::Backend {
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
    let offset_u64 = offset as u64;
    if offset_u64 > total_size {
        return Err(HostlibError::InvalidParameter {
            builtin: READ_FILE_BUILTIN,
            param: "offset",
            message: format!("offset {offset_u64} exceeds file length {total_size}"),
        });
    }

    let mut file = fs::File::open(&path).map_err(|err| HostlibError::Backend {
        builtin: READ_FILE_BUILTIN,
        message: format!("open `{path_str}`: {err}"),
    })?;
    if offset_u64 > 0 {
        use std::io::Seek;
        file.seek(std::io::SeekFrom::Start(offset_u64))
            .map_err(|err| HostlibError::Backend {
                builtin: READ_FILE_BUILTIN,
                message: format!("seek `{path_str}`: {err}"),
            })?;
    }

    let to_read_planned: u64 = if limit_bytes == 0 {
        total_size - offset_u64
    } else {
        std::cmp::min(limit_bytes as u64, total_size - offset_u64)
    };

    let mut buf = Vec::with_capacity(to_read_planned as usize);
    file.take(to_read_planned)
        .read_to_end(&mut buf)
        .map_err(|err| HostlibError::Backend {
            builtin: READ_FILE_BUILTIN,
            message: format!("read `{path_str}`: {err}"),
        })?;

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
                fs::create_dir_all(parent).map_err(|err| HostlibError::Backend {
                    builtin: WRITE_FILE_BUILTIN,
                    message: format!("mkdir `{}`: {err}", parent.display()),
                })?;
            }
        }
    }

    fs::write(&path, &bytes).map_err(|err| HostlibError::Backend {
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

    let path = PathBuf::from(&path_str);

    let metadata = match fs::symlink_metadata(&path) {
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
            fs::remove_dir_all(&path).map_err(|err| HostlibError::Backend {
                builtin: DELETE_FILE_BUILTIN,
                message: format!("remove_dir_all `{path_str}`: {err}"),
            })?;
            true
        } else {
            fs::remove_dir(&path).map_err(|err| HostlibError::Backend {
                builtin: DELETE_FILE_BUILTIN,
                message: format!(
                    "remove_dir `{path_str}` (pass recursive=true to delete non-empty dirs): {err}"
                ),
            })?;
            true
        }
    } else {
        fs::remove_file(&path).map_err(|err| HostlibError::Backend {
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
    let read = fs::read_dir(&path).map_err(|err| HostlibError::Backend {
        builtin: LIST_DIRECTORY_BUILTIN,
        message: format!("read_dir `{path_str}`: {err}"),
    })?;

    let mut entries: Vec<(String, VmValue)> = Vec::new();
    let mut truncated = false;
    let mut all_names: Vec<(String, fs::DirEntry, fs::Metadata)> = Vec::new();

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
        all_names.push((name, entry, metadata));
    }
    all_names.sort_by(|a, b| a.0.cmp(&b.0));

    for (name, entry, metadata) in all_names {
        if entries.len() >= cap {
            truncated = true;
            break;
        }
        let file_type = entry.file_type().ok();
        let is_dir = file_type.map(|t| t.is_dir()).unwrap_or(false);
        let is_symlink = file_type.map(|t| t.is_symlink()).unwrap_or(false);
        let size = file_size(&metadata);
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

fn file_size(metadata: &fs::Metadata) -> u64 {
    metadata.len()
}
