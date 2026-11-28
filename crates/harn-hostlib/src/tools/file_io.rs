//! `tools/{read_file, write_file, delete_file, list_directory}` —
//! deterministic filesystem primitives.
//!
//! Shapes are locked by `schemas/tools/{read_file,write_file,delete_file,list_directory}.{request,response}.json`.

use std::fs as stdfs;
use std::io::{Read, Seek, Write};
use std::path::{Path, PathBuf};

use base64::Engine;
use cap_fs_ext::{FollowSymlinks, OpenOptionsFollowExt};
use cap_std::ambient_authority;
use cap_std::fs::{Dir, OpenOptions};
use harn_vm::process_sandbox::{scoped_mutation_target, FsAccess};
use harn_vm::VmValue;

use crate::error::HostlibError;
use crate::tools::args::{
    build_dict, dict_arg, optional_bool, optional_int, optional_string, require_string,
    resolve_host_path, str_value,
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

    let path = resolve_host_path(READ_FILE_BUILTIN, &path_str)?;
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

    let path = resolve_host_path(WRITE_FILE_BUILTIN, &path_str)?;
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

    write_beneath_root(&path, &path_str, &bytes, create_parents)?;
    crate::fs_snapshot::record_completed_write(&path);

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

    let path = resolve_host_path(DELETE_FILE_BUILTIN, &path_str)?;
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

    // Capture the pre-image into any open snapshots before mutating disk
    // so a `session/restore_tool_call` can roll the delete back.
    crate::fs_snapshot::auto_capture_for_write(DELETE_FILE_BUILTIN, &path);

    let removal = match (metadata.is_dir(), recursive) {
        (true, true) => Removal::DirAll,
        (true, false) => Removal::Dir,
        (false, _) => Removal::File,
    };
    remove_beneath_root(&path, &path_str, removal)?;
    crate::fs_snapshot::record_completed_write(&path);

    Ok(build_dict([
        ("path", str_value(&path_str)),
        ("removed", VmValue::Bool(true)),
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

    let path = resolve_host_path(LIST_DIRECTORY_BUILTIN, &path_str)?;
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
        ("entries", VmValue::list(entries_list)),
        ("truncated", VmValue::Bool(truncated)),
    ]))
}

fn file_size(metadata: &stdfs::Metadata) -> u64 {
    metadata.len()
}

/// The workspace root that holds a scoped mutation, opened as a capability,
/// and the path beneath it. `None` when no path scope is active.
///
/// `enforce_path_scope` judges a path by resolving it once. The mutation then
/// resolves it again, so a directory on the path swapped for a symlink in
/// between would redirect it. Resolving from an opened root with cap-std
/// keeps every component beneath that root at the moment of the mutation: a
/// symlink that leads out is refused there, whenever it was planted.
fn scoped_root(
    builtin: &'static str,
    path: &Path,
    access: FsAccess,
) -> Result<Option<(Dir, PathBuf)>, HostlibError> {
    let target = scoped_mutation_target(builtin, path, access).map_err(|err| {
        HostlibError::SandboxViolation {
            builtin,
            path: path.display().to_string(),
            message: err.to_string(),
        }
    })?;
    let Some(target) = target else {
        return Ok(None);
    };
    let root = Dir::open_ambient_dir(&target.root, ambient_authority()).map_err(|err| {
        HostlibError::Backend {
            builtin,
            message: format!("open workspace root `{}`: {err}", target.root.display()),
        }
    })?;
    Ok(Some((root, target.relative)))
}

/// Classify a failed rooted mutation. A symlink anywhere on the path is the
/// reason cap-std refuses a component, so name it; anything else is the
/// operation's own error.
fn rooted_error(
    builtin: &'static str,
    root: &Dir,
    relative: &Path,
    path: &Path,
    action: &str,
    err: std::io::Error,
) -> HostlibError {
    let symlink = relative
        .ancestors()
        .filter(|prefix| !prefix.as_os_str().is_empty())
        .find(|prefix| {
            root.symlink_metadata(prefix)
                .is_ok_and(|meta| meta.file_type().is_symlink())
        });
    match symlink {
        Some(link) => HostlibError::SandboxViolation {
            builtin,
            path: path.display().to_string(),
            message: format!(
                "refusing to {action} `{}`: `{}` is a symlink, and a scoped {action} does not \
                 follow a symlink out of the workspace root or at the final component",
                path.display(),
                link.display()
            ),
        },
        None => HostlibError::Backend {
            builtin,
            message: format!("{action} `{}`: {err}", path.display()),
        },
    }
}

/// Write `bytes` to `path`, creating parents on request. Under a path scope,
/// every component resolves beneath the workspace root and the final
/// component is never followed as a symlink.
fn write_beneath_root(
    path: &Path,
    path_str: &str,
    bytes: &[u8],
    create_parents: bool,
) -> Result<(), HostlibError> {
    let builtin = WRITE_FILE_BUILTIN;
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty());
    let Some((root, relative)) = scoped_root(builtin, path, FsAccess::Write)? else {
        if let (true, Some(parent)) = (create_parents, parent) {
            stdfs::create_dir_all(parent).map_err(|err| HostlibError::Backend {
                builtin,
                message: format!("mkdir `{}`: {err}", parent.display()),
            })?;
        }
        return stdfs::write(path, bytes).map_err(|err| HostlibError::Backend {
            builtin,
            message: format!("write `{path_str}`: {err}"),
        });
    };
    rooted_write(&root, &relative, bytes, create_parents)
        .map_err(|(action, err)| rooted_error(builtin, &root, &relative, path, action, err))
}

/// Write beneath an opened root without following a symlink out of it or at
/// the final component. Errors carry the action that failed.
fn rooted_write(
    root: &Dir,
    relative: &Path,
    bytes: &[u8],
    create_parents: bool,
) -> Result<(), (&'static str, std::io::Error)> {
    if let (true, Some(parent)) = (
        create_parents,
        relative
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty()),
    ) {
        root.create_dir_all(parent)
            .map_err(|err| ("create the parents of", err))?;
    }
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    options.follow(FollowSymlinks::No);
    root.open_with(relative, &options)
        .and_then(|mut file| file.write_all(bytes))
        .map_err(|err| ("write", err))
}

#[derive(Clone, Copy)]
enum Removal {
    File,
    Dir,
    DirAll,
}

/// Remove `path`. Under a path scope every component resolves beneath the
/// workspace root, so a swapped parent cannot point the removal elsewhere,
/// and a final symlink is unlinked, never followed.
fn remove_beneath_root(path: &Path, path_str: &str, removal: Removal) -> Result<(), HostlibError> {
    let builtin = DELETE_FILE_BUILTIN;
    let action = match removal {
        Removal::File => "remove_file",
        Removal::Dir => "remove_dir (pass recursive=true to delete non-empty dirs)",
        Removal::DirAll => "remove_dir_all",
    };
    let Some((root, relative)) = scoped_root(builtin, path, FsAccess::Delete)? else {
        let result = match removal {
            Removal::File => stdfs::remove_file(path),
            Removal::Dir => stdfs::remove_dir(path),
            Removal::DirAll => stdfs::remove_dir_all(path),
        };
        return result.map_err(|err| HostlibError::Backend {
            builtin,
            message: format!("{action} `{path_str}`: {err}"),
        });
    };
    match removal {
        Removal::File => root.remove_file(&relative),
        Removal::Dir => root.remove_dir(&relative),
        Removal::DirAll => root.remove_dir_all(&relative),
    }
    .map_err(|err| rooted_error(builtin, &root, &relative, path, action, err))
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

/// The escape corpus: layouts a workspace writer can plant before a scoped
/// mutation runs. The rooted operations must leave the outside directory
/// untouched in every case; the same operations through plain `std::fs` must
/// escape in every case, which proves the corpus can see an escape.
#[cfg(all(test, unix))]
mod escape_corpus {
    use std::os::unix::fs::symlink;

    use super::*;

    enum Op {
        Write {
            relative: &'static str,
            create_parents: bool,
        },
        Remove {
            relative: &'static str,
            removal: Removal,
        },
    }

    struct Case {
        name: &'static str,
        plant: fn(&Path, &Path),
        op: Op,
        escaped: fn(&Path) -> bool,
    }

    fn outside_name(outside: &Path) -> String {
        outside.file_name().unwrap().to_string_lossy().into_owned()
    }

    const CASES: &[Case] = &[
        Case {
            name: "parent symlink to outside (relative target)",
            plant: |root, outside| {
                symlink(format!("../{}", outside_name(outside)), root.join("sub")).unwrap();
            },
            op: Op::Write {
                relative: "sub/f.txt",
                create_parents: false,
            },
            escaped: |outside| outside.join("f.txt").exists(),
        },
        Case {
            name: "parent symlink to outside (absolute target)",
            plant: |root, outside| symlink(outside, root.join("abs")).unwrap(),
            op: Op::Write {
                relative: "abs/f.txt",
                create_parents: false,
            },
            escaped: |outside| outside.join("f.txt").exists(),
        },
        Case {
            name: "nested parent symlink with create_parents",
            plant: |root, outside| {
                fs_mkdir(&root.join("a"));
                symlink(outside, root.join("a/b")).unwrap();
            },
            op: Op::Write {
                relative: "a/b/new/f.txt",
                create_parents: true,
            },
            escaped: |outside| outside.join("new").exists(),
        },
        Case {
            name: "final symlink to an outside file",
            plant: |root, outside| {
                stdfs::write(outside.join("secret.txt"), "original").unwrap();
                symlink(outside.join("secret.txt"), root.join("link")).unwrap();
            },
            op: Op::Write {
                relative: "link",
                create_parents: false,
            },
            escaped: |outside| {
                stdfs::read_to_string(outside.join("secret.txt")).unwrap() != "original"
            },
        },
        Case {
            name: "dot-dot out of the root",
            // Nothing to plant: `..` alone leads out of the root.
            plant: |_, _| {},
            op: Op::Write {
                relative: "../OUTSIDE/f.txt",
                create_parents: false,
            },
            escaped: |outside| outside.join("f.txt").exists(),
        },
        Case {
            name: "remove a file through a parent symlink",
            plant: |root, outside| {
                stdfs::write(outside.join("keep.txt"), "keep").unwrap();
                symlink(outside, root.join("rm")).unwrap();
            },
            op: Op::Remove {
                relative: "rm/keep.txt",
                removal: Removal::File,
            },
            escaped: |outside| !outside.join("keep.txt").exists(),
        },
        Case {
            name: "remove a tree through a parent symlink",
            plant: |root, outside| {
                fs_mkdir(&outside.join("tree"));
                stdfs::write(outside.join("tree/keep.txt"), "keep").unwrap();
                symlink(outside, root.join("rmtree")).unwrap();
            },
            op: Op::Remove {
                relative: "rmtree/tree",
                removal: Removal::DirAll,
            },
            escaped: |outside| !outside.join("tree").exists(),
        },
    ];

    fn fs_mkdir(path: &Path) {
        stdfs::create_dir_all(path).unwrap();
    }

    fn relative_for(op_relative: &str, outside: &Path) -> PathBuf {
        PathBuf::from(op_relative.replace("OUTSIDE", &outside_name(outside)))
    }

    /// Run every case through `run`, returning the names of the cases that
    /// changed the outside directory.
    fn escapes(run: impl Fn(&Path, &Op, &Path) -> std::io::Result<()>) -> Vec<&'static str> {
        CASES
            .iter()
            .filter(|case| {
                let parent = tempfile::tempdir().unwrap();
                let root = parent.path().join("root");
                let outside = parent.path().join("outside");
                fs_mkdir(&root);
                fs_mkdir(&outside);
                (case.plant)(&root, &outside);
                let _ = run(&root, &case.op, &outside);
                (case.escaped)(&outside)
            })
            .map(|case| case.name)
            .collect()
    }

    #[test]
    fn rooted_mutations_never_leave_the_root() {
        let escaped = escapes(|root, op, outside| {
            let dir = Dir::open_ambient_dir(root, ambient_authority())?;
            match op {
                Op::Write {
                    relative,
                    create_parents,
                } => rooted_write(
                    &dir,
                    &relative_for(relative, outside),
                    b"x",
                    *create_parents,
                )
                .map_err(|(_, err)| err),
                Op::Remove { relative, removal } => {
                    let relative = relative_for(relative, outside);
                    match removal {
                        Removal::File => dir.remove_file(&relative),
                        Removal::Dir => dir.remove_dir(&relative),
                        Removal::DirAll => dir.remove_dir_all(&relative),
                    }
                }
            }
        });
        assert!(
            escaped.is_empty(),
            "rooted mutations escaped in: {escaped:?}"
        );
    }

    /// The final component is never followed, even to a file inside the root.
    #[test]
    fn rooted_write_refuses_a_final_symlink_inside_the_root() {
        let root = tempfile::tempdir().unwrap();
        stdfs::write(root.path().join("victim.txt"), "original").unwrap();
        symlink("victim.txt", root.path().join("alias")).unwrap();
        let dir = Dir::open_ambient_dir(root.path(), ambient_authority()).unwrap();
        assert!(rooted_write(&dir, Path::new("alias"), b"x", false).is_err());
        assert_eq!(
            stdfs::read_to_string(root.path().join("victim.txt")).unwrap(),
            "original"
        );
    }

    /// Negative control: the same corpus through plain `std::fs` escapes in
    /// every case, so an empty result above is a refusal, not a blind corpus.
    #[test]
    fn plain_std_fs_escapes_in_every_case() {
        let escaped = escapes(|root, op, outside| match op {
            Op::Write {
                relative,
                create_parents,
            } => {
                let path = root.join(relative_for(relative, outside));
                if *create_parents {
                    stdfs::create_dir_all(path.parent().unwrap())?;
                }
                stdfs::write(path, b"x")
            }
            Op::Remove { relative, removal } => {
                let path = root.join(relative_for(relative, outside));
                match removal {
                    Removal::File => stdfs::remove_file(path),
                    Removal::Dir => stdfs::remove_dir(path),
                    Removal::DirAll => stdfs::remove_dir_all(path),
                }
            }
        });
        let all: Vec<_> = CASES.iter().map(|case| case.name).collect();
        assert_eq!(
            escaped, all,
            "the corpus must detect an escape in every case"
        );
    }
}
