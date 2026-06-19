//! JUnit XML parsing builtin.
//!
//! `parse_junit_xml(text_or_bytes)` returns a list of test-case dicts.
//! Accepts a `string` or `bytes` argument and is intentionally lenient:
//! malformed input yields fewer records, never an exception. JUnit XML is
//! the de facto interchange format emitted by GTest (`--gtest_output=xml`),
//! Maven Surefire / Gradle, xUnit, pytest, vitest, and cargo-nextest's
//! JUnit dialect, so a single parser covers most compiled-language runners.
//!
//! A second copy of this parser lives at
//! `crates/harn-hostlib/src/tools/test_parsers.rs`, where it serves the
//! `inspect_test_results` host capability. The two implementations are
//! deliberately independent — the format is small and stable, and consoli-
//! dating later is straightforward if drift becomes a real problem.

use crate::value::VmDictExt;
use std::time::Duration;

use crate::stdlib::macros::{harn_builtin, VmBuiltinDef};
use crate::value::{VmError, VmValue};
use crate::vm::Vm;

const MAX_DURATION_MS: u64 = i64::MAX as u64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Status {
    Passed,
    Failed,
    Skipped,
    Errored,
}

impl Status {
    fn as_str(self) -> &'static str {
        match self {
            Status::Passed => "passed",
            Status::Failed => "failed",
            Status::Skipped => "skipped",
            Status::Errored => "errored",
        }
    }
}

#[derive(Debug, Clone)]
struct TestRecord {
    name: String,
    status: Status,
    duration_ms: u64,
    message: Option<String>,
    stdout: Option<String>,
    stderr: Option<String>,
}

impl TestRecord {
    fn new(name: impl Into<String>, status: Status) -> Self {
        Self {
            name: name.into(),
            status,
            duration_ms: 0,
            message: None,
            stdout: None,
            stderr: None,
        }
    }
}

pub(crate) fn register_junit_builtins(vm: &mut Vm) {
    for def in MODULE_BUILTINS {
        vm.register_builtin_def(def);
    }
}

pub(crate) const MODULE_BUILTINS: &[&VmBuiltinDef] = &[&PARSE_JUNIT_XML_IMPL_DEF];

#[harn_builtin(
    sig = "parse_junit_xml(input: string | bytes | nil) -> list",
    category = "junit"
)]
fn parse_junit_xml_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let bytes: Vec<u8> = match args.first() {
        Some(VmValue::String(s)) => s.as_bytes().to_vec(),
        Some(VmValue::Bytes(b)) => (**b).clone(),
        Some(VmValue::Nil) | None => Vec::new(),
        Some(other) => {
            return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
                format!(
                    "parse_junit_xml: expected string or bytes, got {}",
                    other.type_name()
                ),
            ))));
        }
    };
    let records = parse_junit_xml(&bytes);
    let list: Vec<VmValue> = records.into_iter().map(record_to_value).collect();
    Ok(VmValue::List(std::sync::Arc::new(list)))
}

fn record_to_value(record: TestRecord) -> VmValue {
    let mut map: crate::value::DictMap = crate::value::DictMap::new();
    map.put_str("name", record.name.as_str());
    map.put_str("status", record.status.as_str());
    map.insert(
        "duration_ms".to_string(),
        VmValue::Int(record.duration_ms as i64),
    );
    map.insert(
        "message".to_string(),
        record
            .message
            .map(|s| VmValue::String(arcstr::ArcStr::from(s)))
            .unwrap_or(VmValue::Nil),
    );
    map.insert(
        "stdout".to_string(),
        record
            .stdout
            .map(|s| VmValue::String(arcstr::ArcStr::from(s)))
            .unwrap_or(VmValue::Nil),
    );
    map.insert(
        "stderr".to_string(),
        record
            .stderr
            .map(|s| VmValue::String(arcstr::ArcStr::from(s)))
            .unwrap_or(VmValue::Nil),
    );
    VmValue::dict(map)
}

fn parse_junit_xml(bytes: &[u8]) -> Vec<TestRecord> {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut cursor = 0;
    while let Some(rel_open) = text[cursor..].find("<testcase") {
        let open_start = cursor + rel_open;
        let header_end = match text[open_start..].find('>') {
            Some(idx) => open_start + idx,
            None => break,
        };
        let header = &text[open_start..header_end];
        let self_closing = header.ends_with('/');
        let name = attr(header, "name").unwrap_or_default();
        let classname = attr(header, "classname");
        let time_seconds = attr(header, "time")
            .and_then(|s| s.parse::<f64>().ok())
            .unwrap_or(0.0);

        let qualified = match (&classname, name.is_empty()) {
            (Some(cls), false) if !cls.is_empty() => format!("{cls}::{name}"),
            (_, _) => name.clone(),
        };

        let mut record = TestRecord::new(qualified, Status::Passed);
        record.duration_ms = duration_seconds_to_ms(time_seconds);

        if !self_closing {
            let close_idx = match text[header_end..].find("</testcase>") {
                Some(idx) => header_end + idx,
                None => break,
            };
            let body = &text[header_end + 1..close_idx];
            apply_body(&mut record, body);
            cursor = close_idx + "</testcase>".len();
        } else {
            cursor = header_end + 1;
        }

        out.push(record);
    }
    out
}

fn apply_body(record: &mut TestRecord, body: &str) {
    if let Some((message, body_text)) = first_child_with_message(body, "failure") {
        record.status = Status::Failed;
        record.message = Some(combined_message(message, body_text));
    } else if let Some((message, body_text)) = first_child_with_message(body, "error") {
        record.status = Status::Errored;
        record.message = Some(combined_message(message, body_text));
    } else if body.contains("<skipped") {
        record.status = Status::Skipped;
    }

    if let Some(text) = first_child_text(body, "system-out") {
        record.stdout = Some(text);
    }
    if let Some(text) = first_child_text(body, "system-err") {
        record.stderr = Some(text);
    }
}

fn first_child_with_message(body: &str, tag: &str) -> Option<(Option<String>, String)> {
    let open = format!("<{tag}");
    let close_open = format!("</{tag}>");
    let pos = body.find(open.as_str())?;
    let header_end = body[pos..].find('>').map(|i| pos + i)?;
    let header = &body[pos..header_end];
    let message = attr(header, "message");
    let self_closing = header.ends_with('/');
    let body_text = if self_closing {
        String::new()
    } else {
        let close_pos = body[header_end..]
            .find(&close_open)
            .map(|i| header_end + i)?;
        unescape_xml(body[header_end + 1..close_pos].trim())
    };
    Some((message, body_text))
}

fn first_child_text(body: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    let pos = body.find(open.as_str())?;
    let header_end = body[pos..].find('>').map(|i| pos + i)?;
    let close_pos = body[header_end..].find(&close).map(|i| header_end + i)?;
    Some(unescape_xml(body[header_end + 1..close_pos].trim()))
}

fn combined_message(message: Option<String>, body_text: String) -> String {
    match (message, body_text.is_empty()) {
        (Some(m), true) => m,
        (Some(m), false) => format!("{m}\n{body_text}"),
        (None, _) => body_text,
    }
}

fn attr(header: &str, key: &str) -> Option<String> {
    let bytes = header.as_bytes();
    let mut idx = 0;
    while idx < bytes.len() {
        while idx < bytes.len() && bytes[idx].is_ascii_whitespace() {
            idx += 1;
        }
        if idx >= bytes.len() {
            break;
        }
        if bytes[idx] == b'<' || bytes[idx] == b'/' {
            idx += 1;
            continue;
        }
        let name_start = idx;
        while idx < bytes.len()
            && (bytes[idx].is_ascii_alphanumeric()
                || matches!(bytes[idx], b'_' | b'-' | b':' | b'.'))
        {
            idx += 1;
        }
        let name = &header[name_start..idx];
        while idx < bytes.len() && bytes[idx].is_ascii_whitespace() {
            idx += 1;
        }
        if idx >= bytes.len() || bytes[idx] != b'=' {
            if idx == name_start || idx >= bytes.len() || matches!(bytes[idx], b'>' | b'/') {
                idx += 1;
            }
            continue;
        }
        idx += 1;
        while idx < bytes.len() && bytes[idx].is_ascii_whitespace() {
            idx += 1;
        }
        if idx >= bytes.len() || !matches!(bytes[idx], b'"' | b'\'') {
            continue;
        }
        let quote = bytes[idx];
        idx += 1;
        let value_start = idx;
        while idx < bytes.len() && bytes[idx] != quote {
            idx += 1;
        }
        if idx >= bytes.len() {
            break;
        }
        if name == key {
            return Some(unescape_xml(&header[value_start..idx]));
        }
        idx += 1;
    }
    None
}

fn unescape_xml(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

fn duration_seconds_to_ms(seconds: f64) -> u64 {
    if !seconds.is_finite() || seconds < 0.0 {
        return 0;
    }
    Duration::try_from_secs_f64(seconds)
        .map(|duration| duration.as_millis().min(u128::from(MAX_DURATION_MS)) as u64)
        .unwrap_or(MAX_DURATION_MS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_pass_fail_skip() {
        let xml = r#"<?xml version="1.0"?>
<testsuites>
  <testsuite name="suite">
    <testcase classname="C" name="passes" time="0.001"/>
    <testcase classname="C" name="fails" time="0.002">
      <failure message="boom">stack trace here</failure>
    </testcase>
    <testcase classname="C" name="skipped"><skipped/></testcase>
  </testsuite>
</testsuites>"#;
        let records = parse_junit_xml(xml.as_bytes());
        assert_eq!(records.len(), 3);
        assert_eq!(records[0].status, Status::Passed);
        assert_eq!(records[0].name, "C::passes");
        assert_eq!(records[0].duration_ms, 1);
        assert_eq!(records[1].status, Status::Failed);
        assert!(records[1].message.as_deref().unwrap().contains("boom"));
        assert!(records[1]
            .message
            .as_deref()
            .unwrap()
            .contains("stack trace"));
        assert_eq!(records[2].status, Status::Skipped);
    }

    #[test]
    fn parses_error_and_streams() {
        let xml = r#"<testsuite>
  <testcase name="errors">
    <error message="segfault">core dumped</error>
    <system-out>hello</system-out>
    <system-err>warn: x</system-err>
  </testcase>
</testsuite>"#;
        let records = parse_junit_xml(xml.as_bytes());
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].status, Status::Errored);
        assert_eq!(records[0].name, "errors");
        assert_eq!(records[0].stdout.as_deref(), Some("hello"));
        assert_eq!(records[0].stderr.as_deref(), Some("warn: x"));
    }

    #[test]
    fn unescapes_entities_in_messages() {
        let xml = r#"<testsuite>
  <testcase name="t">
    <failure message="a &amp; b">left &lt; right</failure>
  </testcase>
</testsuite>"#;
        let records = parse_junit_xml(xml.as_bytes());
        let msg = records[0].message.as_deref().unwrap();
        assert!(msg.contains("a & b"));
        assert!(msg.contains("left < right"));
    }

    #[test]
    fn malformed_xml_yields_empty() {
        let records = parse_junit_xml(b"not xml at all");
        assert!(records.is_empty());
    }

    #[test]
    fn classname_does_not_shadow_name_attribute() {
        let xml = r#"<testsuite>
  <testcase classname="pkg.Suite" name="actual" time="0"/>
</testsuite>"#;
        let records = parse_junit_xml(xml.as_bytes());
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].name, "pkg.Suite::actual");
    }

    #[test]
    fn huge_duration_saturates_instead_of_panicking() {
        let xml = r#"<testsuite>
  <testcase name="slow" time="1e308"/>
</testsuite>"#;
        let records = parse_junit_xml(xml.as_bytes());
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].duration_ms, MAX_DURATION_MS);
    }

    #[test]
    fn parses_single_quoted_and_spaced_attributes() {
        let xml = r"<testsuite>
  <testcase classname = 'pkg.Suite' name = 'actual' time = '0.003'>
    <failure message = 'a &amp; b'>left &lt; right</failure>
  </testcase>
</testsuite>";
        let records = parse_junit_xml(xml.as_bytes());
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].name, "pkg.Suite::actual");
        assert_eq!(records[0].duration_ms, 3);
        assert_eq!(records[0].status, Status::Failed);
        let message = records[0].message.as_deref().unwrap();
        assert!(message.contains("a & b"));
        assert!(message.contains("left < right"));
    }
}
