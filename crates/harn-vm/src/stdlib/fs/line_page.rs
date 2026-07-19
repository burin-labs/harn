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
        return Err(invalid(format!("{key} must be an integer")));
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
            .ok_or_else(|| invalid("offset must be a non-negative integer"))?,
        None => 0,
    };
    let line = match values.get("line") {
        Some(value) => value
            .as_int()
            .and_then(|value| u64::try_from(value).ok())
            .filter(|value| *value > 0)
            .ok_or_else(|| invalid("line must be a positive integer"))?,
        None => 1,
    };
    Ok(PageOptions {
        offset,
        line,
        max_lines: positive_option(values, "max_lines", DEFAULT_MAX_LINES, MAX_LINES)?,
        max_bytes: positive_option(values, "max_bytes", DEFAULT_MAX_BYTES, MAX_BYTES)?,
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

#[harn_builtin(
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
    }
}
