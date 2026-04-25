//! Per-runner test-output parsers.
//!
//! - `parse_junit_xml`: handles pytest, vitest, gradle/maven surefire, and
//!   the JUnit dialect cargo-nextest emits when configured to.
//! - `parse_cargo_libtest`: parses the plain-text format `cargo test`
//!   produces by default (`test foo::bar ... ok` lines + a summary).
//! - `parse_go_text`: handles `go test` non-`-json` output (PASS/FAIL +
//!   `--- FAIL: TestX (0.01s)` blocks).
//!
//! The parsers are deliberately lenient: a malformed run yields fewer
//! records, never a parse error. Callers fall back to the raw stdout/stderr
//! the response already includes.

use std::time::Duration;

/// Status of one test run, matching the `status` enum in the schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Status {
    Passed,
    Failed,
    Skipped,
    Errored,
}

impl Status {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Status::Passed => "passed",
            Status::Failed => "failed",
            Status::Skipped => "skipped",
            Status::Errored => "errored",
        }
    }
}

/// One per-test record. Mirrors the `TestRecord` schema in
/// `inspect_test_results.response.json`.
#[derive(Debug, Clone)]
pub(crate) struct TestRecord {
    pub(crate) name: String,
    pub(crate) status: Status,
    pub(crate) duration_ms: u64,
    pub(crate) message: Option<String>,
    pub(crate) stdout: Option<String>,
    pub(crate) stderr: Option<String>,
    pub(crate) path: Option<String>,
    pub(crate) line: Option<i64>,
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
            path: None,
            line: None,
        }
    }
}

/// Parse a JUnit XML byte stream into [`TestRecord`]s. Returns `Err(())` if
/// the XML is malformed enough that we can't extract anything — the
/// caller falls back to other parsers.
pub(crate) fn parse_junit_xml(bytes: &[u8]) -> Result<Vec<TestRecord>, ()> {
    let text = std::str::from_utf8(bytes).map_err(|_| ())?;
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
    Ok(out)
}

fn apply_body(record: &mut TestRecord, body: &str) {
    if let Some((tag, message, body_text)) = first_child_with_message(body, "failure") {
        record.status = Status::Failed;
        record.message = Some(combined_message(message, body_text));
        let _ = tag;
    } else if let Some((tag, message, body_text)) = first_child_with_message(body, "error") {
        record.status = Status::Errored;
        record.message = Some(combined_message(message, body_text));
        let _ = tag;
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

fn first_child_with_message<'a>(
    body: &'a str,
    tag: &'a str,
) -> Option<(&'a str, Option<String>, String)> {
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
        body[header_end + 1..close_pos].trim().to_string()
    };
    Some((tag, message, body_text))
}

fn first_child_text(body: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    let pos = body.find(open.as_str())?;
    let header_end = body[pos..].find('>').map(|i| pos + i)?;
    let close_pos = body[header_end..].find(&close).map(|i| header_end + i)?;
    Some(body[header_end + 1..close_pos].trim().to_string())
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
    // `classname="..."` and friends. XML attribute names always follow a
    // space (or the tag name itself), so this is safe for well-formed
    // input.
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

/// Parse cargo's libtest plain-text output. Recognizes lines like
/// `test foo::bar ... ok` / `... FAILED` / `... ignored`, and matches the
/// trailing `failures:` block to attach failure messages.
pub(crate) fn parse_cargo_libtest(stdout: &str) -> Vec<TestRecord> {
    let mut out: Vec<TestRecord> = Vec::new();
    for line in stdout.lines() {
        let trimmed = line.trim_start();
        if !trimmed.starts_with("test ") {
            continue;
        }
        // skip aggregate "test result: ..." line
        if trimmed.starts_with("test result:") {
            continue;
        }
        let body = &trimmed[5..];
        let Some((name, tail)) = body.split_once(" ... ") else {
            continue;
        };
        let status = match tail.trim() {
            "ok" => Status::Passed,
            "FAILED" => Status::Failed,
            "ignored" | "skipped" => Status::Skipped,
            _ if tail.starts_with("ok") => Status::Passed,
            _ if tail.starts_with("FAILED") => Status::Failed,
            _ => continue,
        };
        out.push(TestRecord::new(name.trim(), status));
    }
    attach_libtest_failure_bodies(&mut out, stdout);
    out
}

fn attach_libtest_failure_bodies(records: &mut [TestRecord], stdout: &str) {
    let Some(idx) = stdout.find("\nfailures:\n\n") else {
        return;
    };
    let body = &stdout[idx + "\nfailures:\n\n".len()..];
    let blocks = body.split("\n\n");
    for block in blocks {
        let Some(first_line) = block.lines().next() else {
            continue;
        };
        let header = first_line.trim();
        if !header.starts_with("---- ") || !header.ends_with(" stdout ----") {
            continue;
        }
        let name = header
            .trim_start_matches("---- ")
            .trim_end_matches(" stdout ----")
            .trim();
        let message = block.lines().skip(1).collect::<Vec<_>>().join("\n");
        if let Some(record) = records.iter_mut().find(|r| r.name == name) {
            if !message.is_empty() {
                record.message = Some(message);
            }
        }
    }
}

/// Parse plain-text `go test` output. Recognizes `--- PASS: TestX (0.01s)`
/// / `--- FAIL: TestX` / `--- SKIP: TestX` lines.
pub(crate) fn parse_go_text(stdout: &str, stderr: &str) -> Vec<TestRecord> {
    let mut out = Vec::new();
    let combined = format!("{stdout}\n{stderr}");
    for line in combined.lines() {
        let trimmed = line.trim_start();
        let parsed = if let Some(rest) = trimmed.strip_prefix("--- PASS: ") {
            parse_go_event(rest, Status::Passed)
        } else if let Some(rest) = trimmed.strip_prefix("--- FAIL: ") {
            parse_go_event(rest, Status::Failed)
        } else if let Some(rest) = trimmed.strip_prefix("--- SKIP: ") {
            parse_go_event(rest, Status::Skipped)
        } else {
            None
        };
        if let Some(record) = parsed {
            out.push(record);
        }
    }
    out
}

fn parse_go_event(rest: &str, status: Status) -> Option<TestRecord> {
    // "TestFoo (0.00s)"
    let (name, duration_part) = rest.split_once(" (").unwrap_or((rest, ""));
    let mut record = TestRecord::new(name.trim(), status);
    if let Some(secs) = duration_part.trim_end_matches(')').strip_suffix('s') {
        if let Ok(value) = secs.parse::<f64>() {
            record.duration_ms = duration_seconds_to_ms(value);
        }
    }
    Some(record)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_junit_xml_with_failure_and_skip() {
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
        let records = parse_junit_xml(xml.as_bytes()).unwrap();
        assert_eq!(records.len(), 3);
        assert_eq!(records[0].status, Status::Passed);
        assert_eq!(records[0].name, "C::passes");
        assert_eq!(records[1].status, Status::Failed);
        assert!(records[1].message.as_deref().unwrap().contains("boom"));
        assert_eq!(records[2].status, Status::Skipped);
    }

    #[test]
    fn parses_cargo_libtest_pass_and_fail() {
        let out = "running 3 tests
test mod_a::passes ... ok
test mod_a::fails ... FAILED
test mod_b::skipped ... ignored

failures:

---- mod_a::fails stdout ----
assertion `left == right` failed
  left: 1
  right: 2

failures:
    mod_a::fails

test result: FAILED. 1 passed; 1 failed; 1 ignored
";
        let records = parse_cargo_libtest(out);
        assert_eq!(records.len(), 3);
        let fail = records.iter().find(|r| r.name == "mod_a::fails").unwrap();
        assert_eq!(fail.status, Status::Failed);
        assert!(fail.message.as_deref().unwrap().contains("assertion"));
    }

    #[test]
    fn parses_go_text_blocks() {
        let stdout = "=== RUN TestA
--- PASS: TestA (0.01s)
=== RUN TestB
--- FAIL: TestB (0.02s)
PASS
";
        let records = parse_go_text(stdout, "");
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].status, Status::Passed);
        assert_eq!(records[0].duration_ms, 10);
        assert_eq!(records[1].status, Status::Failed);
        assert_eq!(records[1].duration_ms, 20);
    }
}
