use std::collections::BTreeMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use crate::stdlib::macros::harn_builtin;
use crate::value::{VmDictExt, VmError, VmValue};

const DEFAULT_MAX_LINES: usize = 256;
const DEFAULT_MAX_BYTES: usize = 1024 * 1024;
const MAX_LINES: usize = 10_000;
const MAX_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone, Copy)]
struct PageOptions {
    offset: u64,
    line: u64,
    max_lines: usize,
    max_bytes: usize,
}

#[derive(Clone, Debug)]
struct AppendCursor {
    offset: u64,
    line: u64,
    file_identity: String,
    observed_size: u64,
    generation: Option<String>,
}

#[derive(Clone, Debug)]
struct AppendPageOptions {
    cursor: Option<AppendCursor>,
    generation: Option<String>,
    max_lines: usize,
    max_bytes: usize,
}

#[derive(Debug)]
struct LineRecord {
    line: u64,
    offset: u64,
    text: String,
}

#[derive(Debug)]
struct LinePage {
    lines: Vec<LineRecord>,
    next_offset: u64,
    next_line: u64,
    done: bool,
}

#[derive(Debug)]
struct AppendLinePage {
    lines: Vec<LineRecord>,
    next_cursor: AppendCursor,
    reset_reason: Option<&'static str>,
    partial_tail_bytes: u64,
    caught_up: bool,
}

struct AppendReadWindow {
    bytes: Vec<u8>,
    length: u64,
    identity: String,
    options: PageOptions,
    reset_reason: Option<&'static str>,
}

fn invalid(message: impl AsRef<str>) -> VmValue {
    super::io_error_value_with_kind("invalid_input", message)
}

fn positive_option(
    values: &crate::value::DictMap,
    key: &str,
    default: usize,
    maximum: usize,
) -> Result<usize, VmValue> {
    let Some(value) = values.get(key) else {
        return Ok(default);
    };
    let Some(value) = value.as_int() else {
        return Err(invalid(format!("{key} must be an int")));
    };
    let value = usize::try_from(value)
        .ok()
        .filter(|value| *value > 0 && *value <= maximum)
        .ok_or_else(|| invalid(format!("{key} must be between 1 and {maximum}")))?;
    Ok(value)
}

fn parse_options(args: &[VmValue]) -> Result<PageOptions, VmValue> {
    let Some(value) = args.get(1) else {
        return Ok(PageOptions {
            offset: 0,
            line: 1,
            max_lines: DEFAULT_MAX_LINES,
            max_bytes: DEFAULT_MAX_BYTES,
        });
    };
    if matches!(value, VmValue::Nil) {
        return parse_options(&args[..1]);
    }
    let VmValue::Dict(values) = value else {
        return Err(invalid("line page options must be a dict"));
    };
    for key in values.keys() {
        if !matches!(key.as_str(), "offset" | "line" | "max_lines" | "max_bytes") {
            return Err(invalid(format!("unknown line page option '{key}'")));
        }
    }
    let offset = match values.get("offset") {
        Some(value) => value
            .as_int()
            .and_then(|value| u64::try_from(value).ok())
            .ok_or_else(|| invalid("offset must be a non-negative int"))?,
        None => 0,
    };
    let line = match values.get("line") {
        Some(value) => value
            .as_int()
            .and_then(|value| u64::try_from(value).ok())
            .filter(|value| *value > 0)
            .ok_or_else(|| invalid("line must be a positive int"))?,
        None => 1,
    };
    Ok(PageOptions {
        offset,
        line,
        max_lines: positive_option(values, "max_lines", DEFAULT_MAX_LINES, MAX_LINES)?,
        max_bytes: positive_option(values, "max_bytes", DEFAULT_MAX_BYTES, MAX_BYTES)?,
    })
}

fn non_negative_int(values: &crate::value::DictMap, key: &str) -> Result<u64, VmValue> {
    values
        .get(key)
        .and_then(VmValue::as_int)
        .and_then(|value| u64::try_from(value).ok())
        .ok_or_else(|| invalid(format!("{key} must be a non-negative int")))
}

fn positive_int(values: &crate::value::DictMap, key: &str) -> Result<u64, VmValue> {
    non_negative_int(values, key).and_then(|value| {
        (value > 0)
            .then_some(value)
            .ok_or_else(|| invalid(format!("{key} must be a positive int")))
    })
}

fn parse_append_cursor(value: &VmValue) -> Result<AppendCursor, VmValue> {
    let VmValue::Dict(values) = value else {
        return Err(invalid("append cursor must be a dict"));
    };
    for key in values.keys() {
        if !matches!(
            key.as_str(),
            "offset" | "line" | "file_identity" | "observed_size" | "generation"
        ) {
            return Err(invalid(format!("unknown append cursor field '{key}'")));
        }
    }
    let file_identity = match values.get("file_identity") {
        Some(VmValue::String(value)) if !value.is_empty() => value.to_string(),
        _ => return Err(invalid("file_identity must be a non-empty string")),
    };
    let generation = match values.get("generation") {
        Some(VmValue::Nil) | None => None,
        Some(VmValue::String(value)) if !value.is_empty() => Some(value.to_string()),
        Some(_) => return Err(invalid("generation must be a non-empty string")),
    };
    let offset = non_negative_int(values, "offset")?;
    let observed_size = non_negative_int(values, "observed_size")?;
    if observed_size < offset {
        return Err(invalid("observed_size must be at least offset"));
    }
    Ok(AppendCursor {
        offset,
        line: positive_int(values, "line")?,
        file_identity,
        observed_size,
        generation,
    })
}

fn parse_append_options(args: &[VmValue]) -> Result<AppendPageOptions, VmValue> {
    let Some(value) = args.get(1) else {
        return Ok(AppendPageOptions {
            cursor: None,
            generation: None,
            max_lines: DEFAULT_MAX_LINES,
            max_bytes: DEFAULT_MAX_BYTES,
        });
    };
    if matches!(value, VmValue::Nil) {
        return parse_append_options(&args[..1]);
    }
    let VmValue::Dict(values) = value else {
        return Err(invalid("append page options must be a dict"));
    };
    for key in values.keys() {
        if !matches!(
            key.as_str(),
            "cursor" | "generation" | "max_lines" | "max_bytes"
        ) {
            return Err(invalid(format!("unknown append page option '{key}'")));
        }
    }
    let cursor = match values.get("cursor") {
        Some(VmValue::Nil) | None => None,
        Some(value) => Some(parse_append_cursor(value)?),
    };
    let generation = match values.get("generation") {
        Some(VmValue::Nil) | None => None,
        Some(VmValue::String(value)) if !value.is_empty() => Some(value.to_string()),
        Some(_) => return Err(invalid("generation must be a non-empty string")),
    }
    .or_else(|| cursor.as_ref().and_then(|cursor| cursor.generation.clone()));
    Ok(AppendPageOptions {
        cursor,
        generation,
        max_lines: positive_option(values, "max_lines", DEFAULT_MAX_LINES, MAX_LINES)?,
        max_bytes: positive_option(values, "max_bytes", DEFAULT_MAX_BYTES, MAX_BYTES)?,
    })
}

#[cfg(unix)]
fn file_identity(_file: &std::fs::File, metadata: &std::fs::Metadata) -> std::io::Result<String> {
    use std::os::unix::fs::MetadataExt;
    Ok(format!("unix:{}:{}", metadata.dev(), metadata.ino()))
}

#[cfg(windows)]
fn file_identity(file: &std::fs::File, _metadata: &std::fs::Metadata) -> std::io::Result<String> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
    };

    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    let ok = unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) };
    if ok == 0 {
        return Err(std::io::Error::last_os_error());
    }
    let volume = info.dwVolumeSerialNumber;
    let index = (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow);
    Ok(format!("windows:{volume}:{index}"))
}

#[cfg(not(any(unix, windows)))]
fn file_identity(_file: &std::fs::File, _metadata: &std::fs::Metadata) -> std::io::Result<String> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "exact append cursor file identity is unavailable on this platform",
    ))
}

fn reset_for_snapshot(
    cursor: Option<&AppendCursor>,
    identity: &str,
    length: u64,
    generation: Option<&str>,
) -> (u64, u64, Option<&'static str>) {
    let Some(cursor) = cursor else {
        return (0, 1, None);
    };
    if cursor.generation.as_deref() != generation {
        return (0, 1, Some("generation_changed"));
    }
    if cursor.file_identity != identity {
        return (0, 1, Some("file_replaced"));
    }
    if length < cursor.observed_size || cursor.offset > length {
        return (0, 1, Some("file_truncated"));
    }
    (cursor.offset, cursor.line, None)
}

fn read_append_window(
    path: &Path,
    options: &AppendPageOptions,
) -> std::io::Result<AppendReadWindow> {
    let limit = options.max_bytes.saturating_add(1);
    if let Some(result) = crate::testbench::overlay_fs::active_overlay().and_then(|overlay| {
        overlay.with_append_snapshot(path, |contents, length, identity| {
            let (offset, line, reset_reason) = reset_for_snapshot(
                options.cursor.as_ref(),
                &identity,
                length,
                options.generation.as_deref(),
            );
            let start = usize::try_from(offset)
                .unwrap_or(usize::MAX)
                .min(contents.len());
            let end = start.saturating_add(limit).min(contents.len());
            Ok(AppendReadWindow {
                bytes: contents[start..end].to_vec(),
                length,
                identity,
                options: PageOptions {
                    offset,
                    line,
                    max_lines: options.max_lines,
                    max_bytes: options.max_bytes,
                },
                reset_reason,
            })
        })
    }) {
        return result;
    }
    let mut file = std::fs::File::open(path)?;
    let metadata = file.metadata()?;
    let length = metadata.len();
    let identity = file_identity(&file, &metadata)?;
    let (offset, line, reset_reason) = reset_for_snapshot(
        options.cursor.as_ref(),
        &identity,
        length,
        options.generation.as_deref(),
    );
    file.seek(SeekFrom::Start(offset))?;
    let available = length.saturating_sub(offset).min(limit as u64) as usize;
    let mut bytes = Vec::with_capacity(available);
    file.take(available as u64).read_to_end(&mut bytes)?;
    Ok(AppendReadWindow {
        bytes,
        length,
        identity,
        options: PageOptions {
            offset,
            line,
            max_lines: options.max_lines,
            max_bytes: options.max_bytes,
        },
        reset_reason,
    })
}

fn read_window(path: &Path, options: PageOptions) -> std::io::Result<(Vec<u8>, u64)> {
    let limit = options.max_bytes.saturating_add(1);
    if let Some(result) = crate::testbench::overlay_fs::active_overlay()
        .and_then(|overlay| overlay.bounded_override(path, options.offset, limit))
    {
        return result;
    }
    let mut file = std::fs::File::open(path)?;
    let length = file.metadata()?.len();
    if options.offset > length {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "byte offset {} exceeds file length {length}",
                options.offset
            ),
        ));
    }
    file.seek(SeekFrom::Start(options.offset))?;
    let available = length.saturating_sub(options.offset).min(limit as u64) as usize;
    let mut bytes = Vec::with_capacity(available);
    file.take(limit as u64).read_to_end(&mut bytes)?;
    Ok((bytes, length))
}

fn page_window(bytes: &[u8], file_length: u64, options: PageOptions) -> std::io::Result<LinePage> {
    let mut lines = Vec::new();
    let mut cursor = 0usize;
    while cursor < bytes.len() && lines.len() < options.max_lines {
        let record_start = cursor;
        let newline = bytes[cursor..].iter().position(|byte| *byte == b'\n');
        let (record_end, next) = match newline {
            Some(relative) => {
                let end = cursor + relative;
                (end, end + 1)
            }
            None if options.offset + bytes.len() as u64 >= file_length => {
                (bytes.len(), bytes.len())
            }
            None => {
                if lines.is_empty() {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::FileTooLarge,
                        format!(
                            "line {} exceeds max_bytes {}",
                            options.line, options.max_bytes
                        ),
                    ));
                }
                break;
            }
        };
        if next > options.max_bytes {
            if lines.is_empty() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::FileTooLarge,
                    format!(
                        "line {} exceeds max_bytes {}",
                        options.line, options.max_bytes
                    ),
                ));
            }
            break;
        }
        let mut content_end = record_end;
        if content_end > record_start && bytes[content_end - 1] == b'\r' {
            content_end -= 1;
        }
        let text = std::str::from_utf8(&bytes[record_start..content_end]).map_err(|error| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "line {} is not valid UTF-8: {error}",
                    options.line + lines.len() as u64
                ),
            )
        })?;
        lines.push(LineRecord {
            line: options.line + lines.len() as u64,
            offset: options.offset + record_start as u64,
            text: text.to_string(),
        });
        cursor = next;
        if record_end == bytes.len() {
            break;
        }
    }
    let next_offset = options.offset + cursor as u64;
    Ok(LinePage {
        next_line: options.line + lines.len() as u64,
        lines,
        next_offset,
        done: next_offset >= file_length,
    })
}

fn append_page_window(
    bytes: &[u8],
    file_length: u64,
    file_identity: String,
    options: PageOptions,
    generation: Option<String>,
    reset_reason: Option<&'static str>,
) -> std::io::Result<AppendLinePage> {
    let mut lines = Vec::new();
    let mut cursor = 0usize;
    while cursor < bytes.len() && lines.len() < options.max_lines {
        let record_start = cursor;
        let Some(relative) = bytes[cursor..].iter().position(|byte| *byte == b'\n') else {
            if bytes.len() > options.max_bytes && lines.is_empty() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::FileTooLarge,
                    format!(
                        "line {} exceeds max_bytes {}",
                        options.line, options.max_bytes
                    ),
                ));
            }
            break;
        };
        let record_end = cursor + relative;
        let next = record_end + 1;
        if next > options.max_bytes {
            if lines.is_empty() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::FileTooLarge,
                    format!(
                        "line {} exceeds max_bytes {}",
                        options.line, options.max_bytes
                    ),
                ));
            }
            break;
        }
        let mut content_end = record_end;
        if content_end > record_start && bytes[content_end - 1] == b'\r' {
            content_end -= 1;
        }
        let text = std::str::from_utf8(&bytes[record_start..content_end]).map_err(|error| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "line {} is not valid UTF-8: {error}",
                    options.line + lines.len() as u64
                ),
            )
        })?;
        lines.push(LineRecord {
            line: options.line + lines.len() as u64,
            offset: options.offset + record_start as u64,
            text: text.to_string(),
        });
        cursor = next;
    }
    let next_offset = options.offset + cursor as u64;
    let read_to_eof = options.offset + bytes.len() as u64 >= file_length;
    let remaining = &bytes[cursor..];
    let partial_tail = read_to_eof && !remaining.is_empty() && !remaining.contains(&b'\n');
    let caught_up = next_offset >= file_length || partial_tail;
    Ok(AppendLinePage {
        next_cursor: AppendCursor {
            offset: next_offset,
            line: options.line + lines.len() as u64,
            file_identity,
            observed_size: file_length,
            generation,
        },
        lines,
        reset_reason,
        partial_tail_bytes: if partial_tail {
            file_length.saturating_sub(next_offset)
        } else {
            0
        },
        caught_up,
    })
}

fn page_value(page: LinePage) -> VmValue {
    let lines = page
        .lines
        .into_iter()
        .map(|record| {
            let mut value = BTreeMap::new();
            value.put_int("line", i64::try_from(record.line).unwrap_or(i64::MAX));
            value.put_int("offset", i64::try_from(record.offset).unwrap_or(i64::MAX));
            value.put_str("text", record.text);
            VmValue::dict(value)
        })
        .collect::<Vec<_>>();
    let mut value = BTreeMap::new();
    value.insert(
        "lines".to_string(),
        VmValue::List(std::sync::Arc::new(lines)),
    );
    value.put_int(
        "next_offset",
        i64::try_from(page.next_offset).unwrap_or(i64::MAX),
    );
    value.put_int(
        "next_line",
        i64::try_from(page.next_line).unwrap_or(i64::MAX),
    );
    value.put_bool("done", page.done);
    VmValue::dict(value)
}

fn append_cursor_value(cursor: AppendCursor) -> VmValue {
    let mut value = BTreeMap::new();
    value.put_int("offset", i64::try_from(cursor.offset).unwrap_or(i64::MAX));
    value.put_int("line", i64::try_from(cursor.line).unwrap_or(i64::MAX));
    value.put_str("file_identity", cursor.file_identity);
    value.put_int(
        "observed_size",
        i64::try_from(cursor.observed_size).unwrap_or(i64::MAX),
    );
    if let Some(generation) = cursor.generation {
        value.put_str("generation", generation);
    }
    VmValue::dict(value)
}

fn append_page_value(page: AppendLinePage) -> VmValue {
    let lines = page
        .lines
        .into_iter()
        .map(|record| {
            let mut value = BTreeMap::new();
            value.put_int("line", i64::try_from(record.line).unwrap_or(i64::MAX));
            value.put_int("offset", i64::try_from(record.offset).unwrap_or(i64::MAX));
            value.put_str("text", record.text);
            VmValue::dict(value)
        })
        .collect::<Vec<_>>();
    let mut value = BTreeMap::new();
    value.insert(
        "lines".to_string(),
        VmValue::List(std::sync::Arc::new(lines)),
    );
    value.insert(
        "next_cursor".to_string(),
        append_cursor_value(page.next_cursor),
    );
    if let Some(reason) = page.reset_reason {
        value.put_str("reset_reason", reason);
    }
    value.put_int(
        "partial_tail_bytes",
        i64::try_from(page.partial_tail_bytes).unwrap_or(i64::MAX),
    );
    value.put_bool("caught_up", page.caught_up);
    VmValue::dict(value)
}

#[harn_builtin(
    exposure = "runtime_internal",
    effects = [],
    sig = "read_lines_page_result(path: string, options?: dict) -> Result<{lines: list<{line: int, offset: int, text: string}>, next_offset: int, next_line: int, done: bool}, dict>",
    category = "fs",
    doc = "Read one byte- and record-bounded page of UTF-8 lines with a resumable cursor."
)]
fn read_lines_page_result_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let path = args.first().map(VmValue::display).unwrap_or_default();
    let options = match parse_options(args) {
        Ok(options) => options,
        Err(error) => return Ok(super::result_err(error)),
    };
    let resolved = super::resolve_fs_path(&path);
    if let Err(violation) = crate::stdlib::sandbox::check_fs_path_scope(
        &resolved,
        crate::stdlib::sandbox::FsAccess::Read,
    ) {
        return Ok(super::result_err(super::sandbox_read_error_value(
            "read_lines_page_result",
            &violation,
        )));
    }
    match read_window(&resolved, options)
        .and_then(|(bytes, length)| page_window(&bytes, length, options))
    {
        Ok(page) => Ok(super::result_ok(page_value(page))),
        Err(error) => Ok(super::result_err(super::io_error_value(
            format!("Failed to read line page {}: {error}", resolved.display()),
            &error,
        ))),
    }
}

#[harn_builtin(
    exposure = "runtime_internal",
    effects = [],
    sig = "read_lines_append_page_result(path: string, options?: dict) -> Result<{lines: list<{line: int, offset: int, text: string}>, next_cursor: {offset: int, line: int, file_identity: string, observed_size: int, generation?: string}, reset_reason?: string, partial_tail_bytes: int, caught_up: bool}, dict>",
    category = "fs",
    doc = "Read one bounded page of newline-committed UTF-8 lines from a growing file."
)]
fn read_lines_append_page_result_builtin(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let path = args.first().map(VmValue::display).unwrap_or_default();
    let options = match parse_append_options(args) {
        Ok(options) => options,
        Err(error) => return Ok(super::result_err(error)),
    };
    let resolved = super::resolve_fs_path(&path);
    if let Err(violation) = crate::stdlib::sandbox::check_fs_path_scope(
        &resolved,
        crate::stdlib::sandbox::FsAccess::Read,
    ) {
        return Ok(super::result_err(super::sandbox_read_error_value(
            "read_lines_append_page_result",
            &violation,
        )));
    }
    match read_append_window(&resolved, &options).and_then(
        |AppendReadWindow {
             bytes,
             length,
             identity,
             options: page_options,
             reset_reason,
         }| {
            append_page_window(
                &bytes,
                length,
                identity,
                page_options,
                options.generation.clone(),
                reset_reason,
            )
        },
    ) {
        Ok(page) => Ok(super::result_ok(append_page_value(page))),
        Err(error) => Ok(super::result_err(super::io_error_value(
            format!(
                "Failed to read append line page {}: {error}",
                resolved.display()
            ),
            &error,
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn options(max_lines: usize, max_bytes: usize) -> PageOptions {
        PageOptions {
            offset: 0,
            line: 1,
            max_lines,
            max_bytes,
        }
    }

    fn result_payload(value: VmValue, variant: &str) -> VmValue {
        let VmValue::EnumVariant(result) = value else {
            panic!("expected Result, got {value:?}");
        };
        assert!(result.is_variant("Result", variant));
        result.fields.first().cloned().expect("Result payload")
    }

    #[test]
    fn pages_crlf_utf8_and_final_unterminated_lines() {
        let bytes = "one\r\nπ\nlast".as_bytes();
        let first = page_window(bytes, bytes.len() as u64, options(2, 16)).unwrap();
        assert_eq!(first.lines.len(), 2);
        assert_eq!(first.lines[0].text, "one");
        assert_eq!(first.lines[1].text, "π");
        assert!(!first.done);
        let remaining = &bytes[first.next_offset as usize..];
        let second = page_window(
            remaining,
            bytes.len() as u64,
            PageOptions {
                offset: first.next_offset,
                line: first.next_line,
                max_lines: 2,
                max_bytes: 16,
            },
        )
        .unwrap();
        assert_eq!(second.lines[0].line, 3);
        assert_eq!(second.lines[0].text, "last");
        assert!(second.done);
    }

    #[test]
    fn append_pages_leave_an_unterminated_tail_uncommitted() {
        let partial = "{\"id\":\"π\"}".as_bytes();
        let first = append_page_window(
            partial,
            partial.len() as u64,
            "file:1".to_string(),
            options(10, 64),
            Some("launch-1".to_string()),
            None,
        )
        .unwrap();
        assert!(first.lines.is_empty());
        assert_eq!(first.next_cursor.offset, 0);
        assert_eq!(first.next_cursor.line, 1);
        assert_eq!(first.partial_tail_bytes, partial.len() as u64);
        assert!(first.caught_up);

        let mut completed = partial.to_vec();
        completed.push(b'\n');
        let second = append_page_window(
            &completed,
            completed.len() as u64,
            "file:1".to_string(),
            options(10, 64),
            Some("launch-1".to_string()),
            None,
        )
        .unwrap();
        assert_eq!(second.lines.len(), 1);
        assert_eq!(second.lines[0].text, "{\"id\":\"π\"}");
        assert_eq!(second.next_cursor.offset, completed.len() as u64);
        assert_eq!(second.partial_tail_bytes, 0);
        assert!(second.caught_up);
    }

    #[test]
    fn append_pages_enforce_record_and_byte_bounds() {
        let bounded = append_page_window(
            b"one\ntwo\n",
            8,
            "file:1".to_string(),
            options(1, 64),
            None,
            None,
        )
        .unwrap();
        assert_eq!(bounded.lines.len(), 1);
        assert_eq!(bounded.next_cursor.offset, 4);
        assert!(!bounded.caught_up);

        let oversized = append_page_window(
            b"12345\n",
            6,
            "file:1".to_string(),
            options(10, 4),
            None,
            None,
        )
        .unwrap_err();
        assert_eq!(oversized.kind(), std::io::ErrorKind::FileTooLarge);
    }

    #[test]
    fn append_cursor_resets_are_typed_and_precedence_is_deterministic() {
        let cursor = AppendCursor {
            offset: 8,
            line: 3,
            file_identity: "file:old".to_string(),
            observed_size: 12,
            generation: Some("launch-1".to_string()),
        };
        assert_eq!(
            reset_for_snapshot(Some(&cursor), "file:new", 12, Some("launch-1")),
            (0, 1, Some("file_replaced"))
        );
        assert_eq!(
            reset_for_snapshot(Some(&cursor), "file:old", 7, Some("launch-1")),
            (0, 1, Some("file_truncated"))
        );
        assert_eq!(
            reset_for_snapshot(Some(&cursor), "file:new", 7, Some("launch-2")),
            (0, 1, Some("generation_changed"))
        );
        assert_eq!(
            reset_for_snapshot(Some(&cursor), "file:old", 16, Some("launch-1")),
            (8, 3, None)
        );
    }

    #[test]
    fn overlay_append_preserves_identity_while_replacement_changes_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let overlay = crate::testbench::overlay_fs::OverlayFs::rooted_at(dir.path());
        overlay.write(&path, b"one\n").unwrap();
        let (_, first_size, first_identity) = overlay
            .with_append_snapshot(&path, |contents, length, identity| {
                Ok((contents.to_vec(), length, identity))
            })
            .unwrap()
            .unwrap();
        overlay.append(&path, b"two\n").unwrap();
        let (_, appended_size, appended_identity) = overlay
            .with_append_snapshot(&path, |contents, length, identity| {
                Ok((contents.to_vec(), length, identity))
            })
            .unwrap()
            .unwrap();
        assert_eq!(first_identity, appended_identity);
        assert!(appended_size > first_size);
        overlay.write(&path, b"same\nxxx").unwrap();
        let (_, replaced_size, replaced_identity) = overlay
            .with_append_snapshot(&path, |contents, length, identity| {
                Ok((contents.to_vec(), length, identity))
            })
            .unwrap()
            .unwrap();
        assert_eq!(replaced_size, appended_size);
        assert_ne!(first_identity, replaced_identity);
    }

    #[test]
    fn growing_file_truncation_resets_then_resumes_from_the_repaired_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        std::fs::write(&path, b"one\ntwo\n").unwrap();
        let initial = AppendPageOptions {
            cursor: None,
            generation: Some("launch-1".to_string()),
            max_lines: 10,
            max_bytes: 64,
        };
        let AppendReadWindow {
            bytes,
            length,
            identity,
            options: page_options,
            reset_reason: reset,
        } = read_append_window(&path, &initial).unwrap();
        let first = append_page_window(
            &bytes,
            length,
            identity,
            page_options,
            initial.generation.clone(),
            reset,
        )
        .unwrap();
        assert_eq!(first.lines.len(), 2);

        std::fs::write(&path, b"new\n").unwrap();
        let after_truncate = AppendPageOptions {
            cursor: Some(first.next_cursor),
            ..initial.clone()
        };
        let AppendReadWindow {
            bytes,
            length,
            identity,
            options: page_options,
            reset_reason: reset,
        } = read_append_window(&path, &after_truncate).unwrap();
        let reset_page = append_page_window(
            &bytes,
            length,
            identity,
            page_options,
            after_truncate.generation,
            reset,
        )
        .unwrap();
        assert_eq!(reset_page.reset_reason, Some("file_truncated"));
        assert_eq!(reset_page.lines[0].text, "new");

        use std::io::Write;
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"repaired\n")
            .unwrap();
        let repaired = AppendPageOptions {
            cursor: Some(reset_page.next_cursor),
            ..initial
        };
        let AppendReadWindow {
            bytes,
            length,
            identity,
            options: page_options,
            reset_reason: reset,
        } = read_append_window(&path, &repaired).unwrap();
        let repaired_page = append_page_window(
            &bytes,
            length,
            identity,
            page_options,
            repaired.generation,
            reset,
        )
        .unwrap();
        assert_eq!(repaired_page.reset_reason, None);
        assert_eq!(repaired_page.lines[0].text, "repaired");
    }

    #[test]
    fn oversized_first_line_is_rejected_without_partial_text() {
        let error = page_window(b"12345\n", 6, options(10, 4)).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::FileTooLarge);
        let exact = page_window(b"123\n", 4, options(10, 4)).unwrap();
        assert_eq!(exact.lines[0].text, "123");
        assert!(exact.done);
    }

    #[test]
    fn invalid_utf8_is_a_typed_data_failure() {
        let error = page_window(&[0xff, b'\n'], 2, options(10, 8)).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    }

    #[test]
    fn byte_ceiling_stops_before_the_next_complete_line() {
        let page = page_window(b"a\nbbbb\n", 7, options(10, 4)).unwrap();
        assert_eq!(page.lines.len(), 1);
        assert_eq!(page.next_offset, 2);
        assert!(!page.done);
    }

    #[test]
    fn option_limits_accept_boundaries_and_reject_values_outside_them() {
        let mut maximums = crate::value::DictMap::new();
        maximums.put_int("max_lines", MAX_LINES as i64);
        maximums.put_int("max_bytes", MAX_BYTES as i64);
        let parsed = parse_options(&[VmValue::Nil, VmValue::dict(maximums)]).unwrap();
        assert_eq!(parsed.max_lines, MAX_LINES);
        assert_eq!(parsed.max_bytes, MAX_BYTES);

        for (key, value) in [
            ("max_lines", 0),
            ("max_lines", MAX_LINES as i64 + 1),
            ("max_bytes", 0),
            ("max_bytes", MAX_BYTES as i64 + 1),
        ] {
            let mut invalid = crate::value::DictMap::new();
            invalid.put_int(key, value);
            assert!(parse_options(&[VmValue::Nil, VmValue::dict(invalid)]).is_err());
        }
    }

    #[test]
    fn append_cursor_rejects_an_offset_past_its_observed_snapshot() {
        let mut cursor = crate::value::DictMap::new();
        cursor.put_int("offset", 9);
        cursor.put_int("line", 2);
        cursor.put_str("file_identity", "file:1");
        cursor.put_int("observed_size", 8);
        let error = parse_append_cursor(&VmValue::dict(cursor)).unwrap_err();
        assert!(error
            .display()
            .contains("observed_size must be at least offset"));
    }

    #[test]
    fn builtin_reads_bounded_pages_from_the_testbench_overlay() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let overlay = Arc::new(crate::testbench::overlay_fs::OverlayFs::rooted_at(
            dir.path(),
        ));
        overlay.write(&path, b"one\ntwo\nthree\n").unwrap();
        let _overlay = crate::testbench::overlay_fs::install_overlay(overlay);
        let mut options = crate::value::DictMap::new();
        options.put_int("max_lines", 2);
        options.put_int("max_bytes", 8);
        let mut out = String::new();
        let page = result_payload(
            read_lines_page_result_builtin(
                &[
                    VmValue::String(arcstr::ArcStr::from(path.to_string_lossy().as_ref())),
                    VmValue::dict(options),
                ],
                &mut out,
            )
            .unwrap(),
            "Ok",
        );
        let VmValue::Dict(page) = page else {
            panic!("expected page dict");
        };
        let VmValue::List(lines) = page.get("lines").expect("lines") else {
            panic!("expected line list");
        };
        assert_eq!(lines.len(), 2);
        assert_eq!(page.get("next_offset").unwrap().display(), "8");
        assert!(!path.exists(), "overlay content must remain hermetic");
    }

    #[test]
    fn builtin_returns_typed_sandbox_denial() {
        use crate::orchestration::{
            pop_execution_policy, push_execution_policy, CapabilityPolicy, SandboxProfile,
        };

        let workspace = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let path = outside.path().join("events.jsonl");
        std::fs::write(&path, b"secret\n").unwrap();
        push_execution_policy(CapabilityPolicy {
            sandbox_profile: SandboxProfile::Worktree,
            workspace_roots: vec![workspace.path().to_string_lossy().into_owned()],
            ..CapabilityPolicy::default()
        });
        let mut out = String::new();
        let failure = result_payload(
            read_lines_page_result_builtin(
                &[VmValue::String(arcstr::ArcStr::from(
                    path.to_string_lossy().as_ref(),
                ))],
                &mut out,
            )
            .unwrap(),
            "Err",
        );
        pop_execution_policy();
        let VmValue::Dict(failure) = failure else {
            panic!("expected failure dict");
        };
        assert_eq!(failure.get("kind").unwrap().display(), "sandbox_denied");

        push_execution_policy(CapabilityPolicy {
            sandbox_profile: SandboxProfile::Worktree,
            workspace_roots: vec![workspace.path().to_string_lossy().into_owned()],
            ..CapabilityPolicy::default()
        });
        let append_failure = result_payload(
            read_lines_append_page_result_builtin(
                &[VmValue::String(arcstr::ArcStr::from(
                    path.to_string_lossy().as_ref(),
                ))],
                &mut out,
            )
            .unwrap(),
            "Err",
        );
        pop_execution_policy();
        let VmValue::Dict(append_failure) = append_failure else {
            panic!("expected append failure dict");
        };
        assert_eq!(
            append_failure.get("kind").unwrap().display(),
            "sandbox_denied"
        );
    }
}
