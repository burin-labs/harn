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

use std::collections::BTreeMap;
use std::rc::Rc;
use std::time::Duration;

use crate::value::{VmError, VmValue};
use crate::vm::Vm;

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
    vm.register_builtin("parse_junit_xml", |args, _out| {
        let bytes: Vec<u8> = match args.first() {
            Some(VmValue::String(s)) => s.as_bytes().to_vec(),
            Some(VmValue::Bytes(b)) => (**b).clone(),
            Some(VmValue::Nil) | None => Vec::new(),
            Some(other) => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
                    "parse_junit_xml: expected string or bytes, got {}",
                    other.type_name()
                )))));
            }
        };
        let records = parse_junit_xml(&bytes);
        let list: Vec<VmValue> = records.into_iter().map(record_to_value).collect();
        Ok(VmValue::List(Rc::new(list)))
    });
}

fn record_to_value(record: TestRecord) -> VmValue {
    let mut map: BTreeMap<String, VmValue> = BTreeMap::new();
    map.insert(
        "name".to_string(),
        VmValue::String(Rc::from(record.name.as_str())),
    );
    map.insert(
        "status".to_string(),
        VmValue::String(Rc::from(record.status.as_str())),
    );
    map.insert(
        "duration_ms".to_string(),
        VmValue::Int(record.duration_ms as i64),
    );
    map.insert(
        "message".to_string(),
        record
            .message
            .map(|s| VmValue::String(Rc::from(s)))
            .unwrap_or(VmValue::Nil),
    );
    map.insert(
        "stdout".to_string(),
        record
            .stdout
            .map(|s| VmValue::String(Rc::from(s)))
            .unwrap_or(VmValue::Nil),
    );
    map.insert(
        "stderr".to_string(),
        record
            .stderr
            .map(|s| VmValue::String(Rc::from(s)))
            .unwrap_or(VmValue::Nil),
    );
    VmValue::Dict(Rc::new(map))
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
    // Require a leading space so we don't match `name="..."` inside
    // `classname="..."` and friends.
    let needle = format!(" {key}=\"");
    let start = header.find(&needle)?;
    let after = &header[start + needle.len()..];
    let end = after.find('"')?;
    Some(unescape_xml(&after[..end]))
}

fn unescape_xml(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

fn duration_seconds_to_ms(seconds: f64) -> u64 {
    if seconds.is_finite() && seconds >= 0.0 {
        Duration::from_secs_f64(seconds).as_millis() as u64
    } else {
        0
    }
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
}
