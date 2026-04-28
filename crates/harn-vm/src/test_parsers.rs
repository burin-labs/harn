//! Per-runner test-output parsers, shared between the
//! `parse_junit_xml` / `parse_trx_xml` / `parse_tap` stdlib builtins and
//! the `inspect_test_results` host capability in `harn-hostlib`.
//!
//! - [`parse_junit_xml`]: handles the JUnit XML dialects emitted by GTest
//!   (`--gtest_output=xml`), Maven Surefire / Gradle, JUnit 4/5, xUnit,
//!   pytest (`--junitxml`), vitest (`--reporter=junit`), cargo-nextest,
//!   PHPUnit, Swift's `swift test --xunit-output`, jest-junit, ScalaTest.
//! - [`parse_trx_xml`]: handles the Microsoft VS Test (`.trx`) format
//!   produced by `dotnet test --logger trx`.
//! - [`parse_tap`]: handles the Test Anything Protocol used by bats
//!   (Bash), Perl `prove`, Lua `busted --output=tap`, deno test, and
//!   Node.js `--test-reporter=tap`.
//! - [`parse_cargo_libtest`]: parses the plain-text format `cargo test`
//!   produces by default (`test foo::bar ... ok` lines + a summary).
//! - [`parse_go_text`]: handles `go test` non-`-json` output (`PASS`/`FAIL`
//!   plus `--- FAIL: TestX (0.01s)` blocks).
//!
//! All parsers are deliberately lenient: malformed input yields fewer
//! records, never a parse error, and unknown lines / elements are
//! ignored rather than rejected. Callers fall back to raw stdout/stderr
//! the response already includes.

use std::time::Duration;

/// Status of one test run, matching the `status` enum in the hostlib's
/// `inspect_test_results` schema and the `parse_junit_xml` /
/// `parse_trx_xml` / `parse_tap` stdlib builtins.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Passed,
    Failed,
    Skipped,
    Errored,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Passed => "passed",
            Status::Failed => "failed",
            Status::Skipped => "skipped",
            Status::Errored => "errored",
        }
    }
}

/// One per-test record. Mirrors the `TestRecord` schema in
/// `inspect_test_results.response.json`. The `path` and `line` fields are
/// reserved for future runner integrations that can pinpoint a failing
/// source location; today no parser populates them, so callers see `None`.
#[derive(Debug, Clone)]
pub struct TestRecord {
    pub name: String,
    pub status: Status,
    pub duration_ms: u64,
    pub message: Option<String>,
    pub stdout: Option<String>,
    pub stderr: Option<String>,
    pub path: Option<String>,
    pub line: Option<i64>,
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

// =====================================================================
// JUnit XML
// =====================================================================

/// Parse a JUnit XML byte stream into [`TestRecord`]s. Returns `Err(())`
/// if the input is not valid UTF-8 — the caller falls back to other
/// parsers. Unparseable input that *is* valid UTF-8 yields an empty
/// list.
///
/// Real-world quirks handled:
/// - Multiple sibling `<failure>` / `<error>` elements per testcase
///   (GTest emits one per `EXPECT_*` failure) — messages are joined.
/// - `status` attribute on `<testcase>` carrying `"skipped"` /
///   `"ignored"` / `"skip"` (older xUnit.NET, pytest in some configs).
///   GTest's `status="run"` / `status="notrun"` is intentionally *not*
///   treated as a skip indicator since it means "executed" / "filtered".
/// - `<skip/>` (no "ped") child element used by older xUnit.NET emitters.
/// - `time` attribute with comma thousands separators (Surefire under
///   some locales) — commas are stripped before parsing.
/// - CDATA-wrapped failure / error / system-out / system-err bodies
///   (Surefire wraps; pytest doesn't) — the wrapper is stripped before
///   XML entity decoding.
#[allow(clippy::result_unit_err)]
pub fn parse_junit_xml(bytes: &[u8]) -> Result<Vec<TestRecord>, ()> {
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
            .map(|s| s.replace(',', ""))
            .and_then(|s| s.parse::<f64>().ok())
            .unwrap_or(0.0);
        let testcase_status_attr = attr(header, "status");

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
            apply_junit_body(&mut record, body);
            cursor = close_idx + "</testcase>".len();
        } else {
            cursor = header_end + 1;
        }

        // Apply testcase-level status attribute as a fallback skip
        // indicator only when no child element has already escalated
        // the status to Failed/Errored/Skipped. GTest's "run" / "notrun"
        // are not treated as skip — they mean "executed" / "filtered out
        // by --gtest_filter".
        if record.status == Status::Passed {
            if let Some(s) = testcase_status_attr.as_deref() {
                let lc = s.to_ascii_lowercase();
                if matches!(lc.as_str(), "skipped" | "ignored" | "skip") {
                    record.status = Status::Skipped;
                }
            }
        }

        out.push(record);
    }
    Ok(out)
}

fn apply_junit_body(record: &mut TestRecord, body: &str) {
    let failure_messages = collect_child_messages(body, "failure");
    let error_messages = collect_child_messages(body, "error");

    if !failure_messages.is_empty() {
        record.status = Status::Failed;
        record.message = Some(failure_messages.join("\n---\n"));
    } else if !error_messages.is_empty() {
        record.status = Status::Errored;
        record.message = Some(error_messages.join("\n---\n"));
    } else if has_skip_child(body) {
        record.status = Status::Skipped;
    }

    if let Some(text) = first_child_text(body, "system-out") {
        record.stdout = Some(text);
    }
    if let Some(text) = first_child_text(body, "system-err") {
        record.stderr = Some(text);
    }
}

fn has_skip_child(body: &str) -> bool {
    // Match `<skipped>`, `<skipped/>`, `<skipped …>`, plus the older
    // xUnit.NET `<skip>` / `<skip/>` / `<skip …>` forms.
    for tag in ["skipped", "skip"] {
        let needle = format!("<{tag}");
        if let Some(idx) = body.find(needle.as_str()) {
            // Make sure the next byte is `>`, `/`, or whitespace so we
            // don't match `<skipped>` when looking for `<skip`.
            let after = body.as_bytes().get(idx + needle.len()).copied();
            if matches!(
                after,
                Some(b'>') | Some(b'/') | Some(b' ') | Some(b'\t') | Some(b'\n') | Some(b'\r')
            ) {
                return true;
            }
        }
    }
    false
}

fn collect_child_messages(body: &str, tag: &str) -> Vec<String> {
    let open = format!("<{tag}");
    let close_open = format!("</{tag}>");
    let mut out = Vec::new();
    let mut cursor = 0;
    while let Some(rel) = body[cursor..].find(open.as_str()) {
        let pos = cursor + rel;
        // Make sure we're matching the right tag — `<failure` should
        // not match `<failures` (rare but possible in custom dialects).
        let after = body.as_bytes().get(pos + open.len()).copied();
        if !matches!(
            after,
            Some(b'>') | Some(b'/') | Some(b' ') | Some(b'\t') | Some(b'\n') | Some(b'\r')
        ) {
            cursor = pos + open.len();
            continue;
        }
        let header_end = match body[pos..].find('>') {
            Some(i) => pos + i,
            None => break,
        };
        let header = &body[pos..header_end];
        let message = attr(header, "message");
        let self_closing = header.ends_with('/');
        let body_text = if self_closing {
            String::new()
        } else {
            let close_pos = match body[header_end..].find(&close_open) {
                Some(i) => header_end + i,
                None => break,
            };
            decode_text_content(&body[header_end + 1..close_pos])
        };
        out.push(combined_message(message, body_text));
        cursor = if self_closing {
            header_end + 1
        } else {
            // skip past closing tag
            body[header_end..]
                .find(&close_open)
                .map(|i| header_end + i + close_open.len())
                .unwrap_or(header_end + 1)
        };
    }
    out
}

fn first_child_text(body: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    let pos = body.find(open.as_str())?;
    let header_end = body[pos..].find('>').map(|i| pos + i)?;
    let close_pos = body[header_end..].find(&close).map(|i| header_end + i)?;
    Some(decode_text_content(&body[header_end + 1..close_pos]))
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

/// Strip a CDATA wrapper if present, then XML-entity-decode the
/// remainder. Inside CDATA, entities are *not* recognized, so we skip
/// `unescape_xml` for that branch.
fn decode_text_content(raw: &str) -> String {
    let trimmed = raw.trim();
    if let Some(stripped) = trimmed
        .strip_prefix("<![CDATA[")
        .and_then(|s| s.strip_suffix("]]>"))
    {
        stripped.trim().to_string()
    } else {
        unescape_xml(trimmed)
    }
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

// =====================================================================
// TRX (Visual Studio Test Results)
// =====================================================================

/// Parse a TRX byte stream (Microsoft VS Test results) into
/// [`TestRecord`]s. Emitted by `dotnet test --logger trx`. Returns
/// `Err(())` for invalid UTF-8.
///
/// Walks every `<UnitTestResult>` element, reading `testName`,
/// `outcome`, and `duration` attributes plus the optional
/// `<Output>/<ErrorInfo>/<Message>`, `<Output>/<ErrorInfo>/<StackTrace>`,
/// `<Output>/<StdOut>`, and `<Output>/<StdErr>` children.
///
/// Outcome → status mapping (from vstest's TestOutcome enum, of which
/// only Passed/Failed/NotExecuted/Error are emitted by mainstream
/// `dotnet test`; the rest cover third-party loggers):
///
/// - `Passed` → Passed
/// - `Failed` / `Aborted` / `Timeout` → Failed
/// - `Error` → Errored
/// - `NotExecuted` / `NotRunnable` / `Disconnected` / `Pending` → Skipped
/// - Anything else (`Inconclusive`, `Warning`, `Completed`,
///   `PassedButRunAborted`, `In Progress`) → Skipped, with the original
///   outcome name preserved as a note in `message`.
///
/// Duration is `.NET TimeSpan.ToString()` — `[-][d.]HH:MM:SS[.fffffff]`.
#[allow(clippy::result_unit_err)]
pub fn parse_trx_xml(bytes: &[u8]) -> Result<Vec<TestRecord>, ()> {
    let text = std::str::from_utf8(bytes).map_err(|_| ())?;
    let mut out = Vec::new();
    let mut cursor = 0;
    while let Some(rel_open) = text[cursor..].find("<UnitTestResult") {
        let open_start = cursor + rel_open;
        let header_end = match text[open_start..].find('>') {
            Some(idx) => open_start + idx,
            None => break,
        };
        let header = &text[open_start..header_end];
        let self_closing = header.ends_with('/');
        let name = attr(header, "testName").unwrap_or_default();
        let outcome_raw = attr(header, "outcome").unwrap_or_default();
        let duration_raw = attr(header, "duration");

        let (status, status_note) = trx_outcome_to_status(&outcome_raw);
        let mut record = TestRecord::new(name, status);
        if let Some(d) = duration_raw.as_deref() {
            record.duration_ms = parse_trx_duration_ms(d);
        }
        if let Some(note) = status_note {
            record.message = Some(note);
        }

        if !self_closing {
            let close_idx = match text[header_end..].find("</UnitTestResult>") {
                Some(idx) => header_end + idx,
                None => break,
            };
            let body = &text[header_end + 1..close_idx];
            apply_trx_body(&mut record, body);
            cursor = close_idx + "</UnitTestResult>".len();
        } else {
            cursor = header_end + 1;
        }
        out.push(record);
    }
    Ok(out)
}

fn trx_outcome_to_status(outcome: &str) -> (Status, Option<String>) {
    match outcome {
        "Passed" => (Status::Passed, None),
        "Failed" | "Aborted" | "Timeout" => (Status::Failed, None),
        "Error" => (Status::Errored, None),
        "NotExecuted" | "NotRunnable" | "Disconnected" | "Pending" => (Status::Skipped, None),
        "" => (Status::Skipped, None),
        other => (Status::Skipped, Some(format!("trx outcome: {other}"))),
    }
}

/// Parse a .NET `TimeSpan.ToString()` value (`[-][d.]HH:MM:SS[.fffffff]`)
/// to milliseconds. Negative durations clamp to 0. Anything that doesn't
/// match the expected shape returns 0 rather than throwing.
fn parse_trx_duration_ms(raw: &str) -> u64 {
    let mut s = raw.trim();
    if let Some(stripped) = s.strip_prefix('-') {
        // Negative — vstest doesn't emit these but be defensive.
        let _ = stripped;
        return 0;
    }
    let mut days: u64 = 0;
    if let Some(dot) = s.find('.') {
        // Could be the `d.HH:MM:SS` separator OR the `.fffffff` fractional
        // separator — distinguish by what follows. The day prefix must
        // contain a `:` after the dot.
        if s[dot + 1..].contains(':') {
            if let Ok(d) = s[..dot].parse::<u64>() {
                days = d;
                s = &s[dot + 1..];
            }
        }
    }
    let (clock, frac) = match s.find('.') {
        Some(idx) => (&s[..idx], Some(&s[idx + 1..])),
        None => (s, None),
    };
    let mut parts = clock.split(':');
    let h = parts
        .next()
        .and_then(|p| p.parse::<u64>().ok())
        .unwrap_or(0);
    let m = parts
        .next()
        .and_then(|p| p.parse::<u64>().ok())
        .unwrap_or(0);
    let sec = parts
        .next()
        .and_then(|p| p.parse::<u64>().ok())
        .unwrap_or(0);
    let total_seconds = days * 86_400 + h * 3_600 + m * 60 + sec;
    let mut ms = total_seconds * 1_000;
    if let Some(f) = frac {
        // Fractional is up to 7 digits (100ns resolution). Take the
        // first 3 for ms.
        let trimmed: String = f.chars().take_while(|c| c.is_ascii_digit()).collect();
        let take = trimmed.chars().take(3).collect::<String>();
        let pad = format!("{take:0<3}");
        if let Ok(extra) = pad.parse::<u64>() {
            ms += extra;
        }
    }
    ms
}

fn apply_trx_body(record: &mut TestRecord, body: &str) {
    // <Output>/<ErrorInfo>/<Message> + <StackTrace>
    if let Some(output) = element_inner(body, "Output") {
        if let Some(error_info) = element_inner(output, "ErrorInfo") {
            let message = element_inner(error_info, "Message")
                .map(decode_text_content)
                .filter(|s| !s.is_empty());
            let stack = element_inner(error_info, "StackTrace")
                .map(decode_text_content)
                .filter(|s| !s.is_empty());
            let combined = match (message, stack) {
                (Some(m), Some(s)) => Some(format!("{m}\n{s}")),
                (Some(m), None) => Some(m),
                (None, Some(s)) => Some(s),
                (None, None) => None,
            };
            if let Some(c) = combined {
                // Preserve any tool-specific outcome note already set.
                record.message = Some(match record.message.take() {
                    Some(prev) => format!("{prev}\n{c}"),
                    None => c,
                });
            }
        }
        if let Some(stdout) = element_inner(output, "StdOut") {
            let s = decode_text_content(stdout);
            if !s.is_empty() {
                record.stdout = Some(s);
            }
        }
        if let Some(stderr) = element_inner(output, "StdErr") {
            let s = decode_text_content(stderr);
            if !s.is_empty() {
                record.stderr = Some(s);
            }
        }
    }
}

/// Return the inner text/markup of the first occurrence of `<tag …>…</tag>`.
/// Used for TRX where the schema is regular and namespaced — we don't
/// need the full lenient JUnit-style multi-element handling.
fn element_inner<'a>(haystack: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    let pos = haystack.find(open.as_str())?;
    let after_open_name = pos + open.len();
    let next = haystack.as_bytes().get(after_open_name).copied()?;
    if !matches!(next, b'>' | b' ' | b'\t' | b'\n' | b'\r' | b'/') {
        return None;
    }
    let header_end = haystack[pos..].find('>').map(|i| pos + i)?;
    if haystack.as_bytes().get(header_end - 1).copied() == Some(b'/') {
        // Self-closing — nothing inside.
        return Some("");
    }
    let close_pos = haystack[header_end..]
        .find(&close)
        .map(|i| header_end + i)?;
    Some(&haystack[header_end + 1..close_pos])
}

// =====================================================================
// TAP (Test Anything Protocol)
// =====================================================================

/// Parse a TAP (Test Anything Protocol) text stream into [`TestRecord`]s.
/// Recognizes TAP 12 / 13 / 14 — version-line presence is detected, not
/// required. Subtests are collapsed: only the parent test point is
/// reported.
///
/// Recognized lines:
/// - `TAP version N` (consumed, ignored)
/// - `1..N` plan (consumed; `1..0 # SKIP <reason>` marks the whole run
///   as skipped but otherwise has no effect on individual records)
/// - `(not )?ok [num] [- ]description [# (SKIP|TODO|skip|todo) [reason]]`
/// - `# ...` diagnostic comments (ignored)
/// - `Bail out![ <reason>]` (stops record collection)
/// - YAML blocks (delimited by `---` / `...` lines, indented under a
///   test point) — content is folded into the preceding record's
///   `message` field as a raw block.
///
/// Directive semantics (TAP 14):
/// - `ok ... # SKIP …` → Skipped
/// - `not ok ... # SKIP …` → Skipped
/// - `not ok ... # TODO …` → still Passed (TODO failures are expected)
/// - `ok ... # TODO …` → Passed (a "bonus" pass)
///
/// Anything that doesn't match a known shape is treated as a diagnostic
/// and ignored, as required by the TAP spec — emitters frequently
/// interleave compiler warnings, `println!` output, and other text.
pub fn parse_tap(text: &str) -> Vec<TestRecord> {
    // Strip BOM and normalize CRLF.
    let cleaned = text.trim_start_matches('\u{feff}');
    let mut out: Vec<TestRecord> = Vec::new();
    let mut next_id: usize = 1;
    let mut bailed = false;
    // YAML block tied to the most recent record. None when not inside.
    let mut yaml_buf: Option<(String, usize)> = None;

    for raw_line in cleaned.lines() {
        let line = raw_line.trim_end_matches('\r');
        if bailed {
            break;
        }

        // Inside an active YAML block — accumulate until the closing
        // `...` line at the same indent. We anchor to the indent of the
        // opening `---`.
        if let Some((mut buf, opener_indent)) = yaml_buf.take() {
            let indent = leading_spaces(line);
            let trimmed = &line[indent..];
            if trimmed == "..." && indent == opener_indent {
                if let Some(rec) = out.last_mut() {
                    let prev = rec.message.take();
                    rec.message = Some(match prev {
                        Some(p) if !p.is_empty() => format!("{p}\n{buf}"),
                        _ => buf,
                    });
                }
                continue;
            }
            buf.push_str(line);
            buf.push('\n');
            yaml_buf = Some((buf, opener_indent));
            continue;
        }

        let trimmed = line.trim_start();
        if trimmed.is_empty() {
            continue;
        }

        if let Some(rest) = trimmed.strip_prefix("Bail out!") {
            // Remaining text on the line is an optional reason; we don't
            // attach it anywhere right now.
            let _ = rest;
            bailed = true;
            continue;
        }

        if trimmed.starts_with("TAP version ") || trimmed.starts_with("pragma ") {
            continue;
        }

        // Plan line, e.g. `1..3` or `1..0 # SKIP all gone`.
        if let Some((lhs, _rhs)) = trimmed.split_once("..") {
            if lhs.chars().all(|c| c.is_ascii_digit()) && !lhs.is_empty() {
                continue;
            }
        }

        // YAML block opener indented under the previous record.
        if line.trim_start() == "---" {
            let indent = leading_spaces(line);
            yaml_buf = Some((String::new(), indent));
            continue;
        }

        // Diagnostic / unknown line — skip silently.
        if trimmed.starts_with('#') {
            continue;
        }

        // Result line.
        let (is_ok, after_ok) = if let Some(rest) = trimmed.strip_prefix("ok") {
            (true, rest)
        } else if let Some(rest) = trimmed.strip_prefix("not ok") {
            (false, rest)
        } else {
            // Unknown — treated as diagnostic per spec.
            continue;
        };

        // Make sure the `ok`/`not ok` prefix isn't `okay` etc.
        if !after_ok.is_empty() && !after_ok.starts_with(' ') && !after_ok.starts_with('\t') {
            continue;
        }

        let mut rest = after_ok.trim_start();

        // Optional test number.
        let id = take_leading_int(&mut rest).unwrap_or(next_id);
        next_id = id + 1;
        rest = rest.trim_start();
        if let Some(stripped) = rest.strip_prefix("- ") {
            rest = stripped;
        } else if rest.starts_with('-') {
            rest = rest[1..].trim_start();
        }

        // Split off the directive `# ...` if present.
        let (description, directive) = match split_tap_directive(rest) {
            Some((desc, dir)) => (desc, Some(dir)),
            None => (rest, None),
        };

        let mut status = if is_ok {
            Status::Passed
        } else {
            Status::Failed
        };
        let mut message: Option<String> = None;
        if let Some(dir) = directive {
            let dir_trim = dir.trim();
            let upper = dir_trim
                .chars()
                .take_while(|c| !c.is_whitespace())
                .collect::<String>()
                .to_ascii_uppercase();
            let reason = dir_trim
                .split_once(char::is_whitespace)
                .map(|(_, rest)| rest.trim())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string());
            match upper.as_str() {
                "SKIP" | "SKIPPED" => {
                    status = Status::Skipped;
                    message = reason;
                }
                "TODO" => {
                    // TAP: `not ok ... # TODO` is still a pass; `ok ...
                    // # TODO` is a "bonus" — also pass.
                    status = Status::Passed;
                    message = reason;
                }
                _ => {}
            }
        }

        let name = if description.is_empty() {
            format!("test {id}")
        } else {
            description.trim().to_string()
        };
        let mut record = TestRecord::new(name, status);
        record.message = message;
        out.push(record);
    }

    // Drop a dangling YAML block (no closing `...`) but keep its
    // accumulated text on the preceding record.
    if let Some((buf, _)) = yaml_buf {
        if !buf.is_empty() {
            if let Some(rec) = out.last_mut() {
                let prev = rec.message.take();
                rec.message = Some(match prev {
                    Some(p) if !p.is_empty() => format!("{p}\n{buf}"),
                    _ => buf,
                });
            }
        }
    }

    out
}

fn leading_spaces(line: &str) -> usize {
    line.bytes().take_while(|b| *b == b' ').count()
}

fn take_leading_int(s: &mut &str) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut idx = 0;
    while idx < bytes.len() && bytes[idx].is_ascii_digit() {
        idx += 1;
    }
    if idx == 0 {
        return None;
    }
    let n = s[..idx].parse::<usize>().ok()?;
    *s = &s[idx..];
    Some(n)
}

/// Split a TAP description from its directive, returning
/// `Some((description, directive_body))` when a `# DIRECTIVE` segment is
/// present. Treats the first unescaped `#` followed by whitespace or the
/// start of a recognized keyword as the directive boundary.
fn split_tap_directive(rest: &str) -> Option<(&str, &str)> {
    let bytes = rest.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'#' && (i == 0 || bytes[i - 1] != b'\\') {
            // Make sure what follows looks like a directive keyword
            // (SKIP / TODO, case-insensitive) so we don't eat `#` that
            // belongs to the description text.
            let after = rest[i + 1..].trim_start();
            let head: String = after
                .chars()
                .take_while(|c| !c.is_whitespace())
                .collect::<String>()
                .to_ascii_uppercase();
            if matches!(head.as_str(), "SKIP" | "SKIPPED" | "TODO") {
                let desc = rest[..i].trim_end();
                return Some((desc, after));
            }
        }
        i += 1;
    }
    None
}

// =====================================================================
// Cargo libtest plain text
// =====================================================================

/// Parse cargo's libtest plain-text output. Recognizes lines like
/// `test foo::bar ... ok` / `... FAILED` / `... ignored`, and matches the
/// trailing `failures:` block to attach failure messages.
pub fn parse_cargo_libtest(stdout: &str) -> Vec<TestRecord> {
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

// =====================================================================
// `go test` plain text
// =====================================================================

/// Parse plain-text `go test` output. Recognizes `--- PASS: TestX (0.01s)`
/// / `--- FAIL: TestX` / `--- SKIP: TestX` lines.
pub fn parse_go_text(stdout: &str, stderr: &str) -> Vec<TestRecord> {
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

    // --- JUnit XML --------------------------------------------------

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
    fn parses_junit_error_and_streams() {
        let xml = r#"<testsuite>
  <testcase name="errors">
    <error message="segfault">core dumped</error>
    <system-out>hello</system-out>
    <system-err>warn: x</system-err>
  </testcase>
</testsuite>"#;
        let records = parse_junit_xml(xml.as_bytes()).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].status, Status::Errored);
        assert_eq!(records[0].name, "errors");
        assert_eq!(records[0].stdout.as_deref(), Some("hello"));
        assert_eq!(records[0].stderr.as_deref(), Some("warn: x"));
    }

    #[test]
    fn junit_unescapes_entities_in_messages() {
        let xml = r#"<testsuite>
  <testcase name="t">
    <failure message="a &amp; b">left &lt; right</failure>
  </testcase>
</testsuite>"#;
        let records = parse_junit_xml(xml.as_bytes()).unwrap();
        let msg = records[0].message.as_deref().unwrap();
        assert!(msg.contains("a & b"));
        assert!(msg.contains("left < right"));
    }

    #[test]
    fn junit_classname_qualifies_name_attribute() {
        let xml = r#"<testsuite>
  <testcase classname="pkg.Suite" name="actual" time="0"/>
</testsuite>"#;
        let records = parse_junit_xml(xml.as_bytes()).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].name, "pkg.Suite::actual");
    }

    #[test]
    fn junit_malformed_input_yields_empty_or_err() {
        let records = parse_junit_xml(b"not xml at all").unwrap();
        assert!(records.is_empty());
        assert!(parse_junit_xml(&[0xff, 0xfe, 0xfd]).is_err());
    }

    #[test]
    fn junit_collects_multiple_failure_siblings_like_gtest() {
        let xml = r#"<testsuites>
  <testsuite name="GTest">
    <testcase classname="MathTest" name="Adds" time="0">
      <failure message="EXPECT_EQ failed">a.cpp:3 expected 2 got 3</failure>
      <failure message="EXPECT_EQ failed">a.cpp:4 expected 4 got 5</failure>
    </testcase>
  </testsuite>
</testsuites>"#;
        let records = parse_junit_xml(xml.as_bytes()).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].status, Status::Failed);
        let msg = records[0].message.as_deref().unwrap();
        assert!(msg.contains("expected 2 got 3"));
        assert!(msg.contains("expected 4 got 5"));
    }

    #[test]
    fn junit_recognizes_status_attribute_skip_but_not_gtest_run() {
        let xml = r#"<testsuite>
  <testcase name="xunit_skipped" status="skipped"/>
  <testcase name="gtest_executed" status="run"/>
  <testcase name="gtest_filtered_out" status="notrun"/>
</testsuite>"#;
        let records = parse_junit_xml(xml.as_bytes()).unwrap();
        assert_eq!(records.len(), 3);
        assert_eq!(records[0].status, Status::Skipped);
        assert_eq!(records[1].status, Status::Passed);
        assert_eq!(records[2].status, Status::Passed);
    }

    #[test]
    fn junit_recognizes_old_xunit_skip_element() {
        let xml = r#"<testsuite>
  <testcase name="t1"><skip message="legacy"/></testcase>
  <testcase name="t2"><skipped/></testcase>
</testsuite>"#;
        let records = parse_junit_xml(xml.as_bytes()).unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].status, Status::Skipped);
        assert_eq!(records[1].status, Status::Skipped);
    }

    #[test]
    fn junit_strips_thousands_separators_from_time() {
        let xml = r#"<testsuite>
  <testcase name="big" time="1,234.567"/>
</testsuite>"#;
        let records = parse_junit_xml(xml.as_bytes()).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].duration_ms, 1_234_567);
    }

    #[test]
    fn junit_strips_cdata_in_failure_body() {
        let xml = "<testsuite>\n  <testcase name=\"t\">\n    <failure message=\"boom\"><![CDATA[raw <not> escaped]]></failure>\n  </testcase>\n</testsuite>";
        let records = parse_junit_xml(xml.as_bytes()).unwrap();
        let msg = records[0].message.as_deref().unwrap();
        assert!(msg.contains("raw <not> escaped"));
        assert!(!msg.contains("CDATA"));
    }

    // --- TRX --------------------------------------------------------

    #[test]
    fn parses_trx_outcomes() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<TestRun id="x" xmlns="http://microsoft.com/schemas/VisualStudio/TeamTest/2010">
  <Results>
    <UnitTestResult testName="MyTest.Pass" outcome="Passed" duration="00:00:00.0050000"/>
    <UnitTestResult testName="MyTest.Fail" outcome="Failed" duration="00:00:00.1000000">
      <Output>
        <ErrorInfo>
          <Message>Assert.Equal Failure</Message>
          <StackTrace>at MyTest.Fail() in C:\f.cs:line 42</StackTrace>
        </ErrorInfo>
        <StdOut>captured stdout</StdOut>
        <StdErr>captured stderr</StdErr>
      </Output>
    </UnitTestResult>
    <UnitTestResult testName="MyTest.Skipped" outcome="NotExecuted" duration="00:00:00"/>
    <UnitTestResult testName="MyTest.Aborted" outcome="Aborted" duration="00:00:00"/>
    <UnitTestResult testName="MyTest.RunLevelError" outcome="Error" duration="00:00:00"/>
    <UnitTestResult testName="MyTest.Inconclusive" outcome="Inconclusive" duration="00:00:00"/>
  </Results>
</TestRun>"#;
        let records = parse_trx_xml(xml.as_bytes()).unwrap();
        assert_eq!(records.len(), 6);
        assert_eq!(records[0].name, "MyTest.Pass");
        assert_eq!(records[0].status, Status::Passed);
        assert_eq!(records[0].duration_ms, 5);
        assert_eq!(records[1].status, Status::Failed);
        assert_eq!(records[1].duration_ms, 100);
        let msg = records[1].message.as_deref().unwrap();
        assert!(msg.contains("Assert.Equal Failure"));
        assert!(msg.contains("line 42"));
        assert_eq!(records[1].stdout.as_deref(), Some("captured stdout"));
        assert_eq!(records[1].stderr.as_deref(), Some("captured stderr"));
        assert_eq!(records[2].status, Status::Skipped); // NotExecuted
        assert_eq!(records[3].status, Status::Failed); // Aborted → Failed
        assert_eq!(records[4].status, Status::Errored); // Error
        assert_eq!(records[5].status, Status::Skipped); // Inconclusive
        assert!(records[5]
            .message
            .as_deref()
            .unwrap()
            .contains("Inconclusive"));
    }

    #[test]
    fn trx_parses_long_duration_with_days() {
        // 1 day, 2 hours, 3 minutes, 4 seconds, .5 fractional.
        assert_eq!(parse_trx_duration_ms("1.02:03:04.5000000"), 93_784_500);
        assert_eq!(parse_trx_duration_ms("00:00:01.2340000"), 1_234);
        assert_eq!(parse_trx_duration_ms("00:00:00"), 0);
        assert_eq!(parse_trx_duration_ms("garbage"), 0);
    }

    #[test]
    fn trx_invalid_utf8_returns_err() {
        assert!(parse_trx_xml(&[0xff, 0xfe]).is_err());
    }

    // --- TAP --------------------------------------------------------

    #[test]
    fn parses_basic_tap() {
        let tap = "TAP version 14\n1..3\nok 1 - alpha\nnot ok 2 - beta\nok 3 - gamma # SKIP slow\n";
        let records = parse_tap(tap);
        assert_eq!(records.len(), 3);
        assert_eq!(records[0].name, "alpha");
        assert_eq!(records[0].status, Status::Passed);
        assert_eq!(records[1].name, "beta");
        assert_eq!(records[1].status, Status::Failed);
        assert_eq!(records[2].name, "gamma");
        assert_eq!(records[2].status, Status::Skipped);
        assert_eq!(records[2].message.as_deref(), Some("slow"));
    }

    #[test]
    fn tap_todo_keeps_pass_status() {
        // TAP: a `not ok ... # TODO` is still passing — TODO failures
        // are expected.
        let tap = "1..2\nnot ok 1 - flaky # TODO not implemented\nok 2 - bonus # TODO\n";
        let records = parse_tap(tap);
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].status, Status::Passed);
        assert_eq!(records[0].message.as_deref(), Some("not implemented"));
        assert_eq!(records[1].status, Status::Passed);
    }

    #[test]
    fn tap_handles_missing_version_and_dash() {
        // bats-style: no version line, no `- ` separator.
        let tap = "ok 1 first test\nnot ok 2 second test\n";
        let records = parse_tap(tap);
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].name, "first test");
        assert_eq!(records[1].name, "second test");
        assert_eq!(records[1].status, Status::Failed);
    }

    #[test]
    fn tap_yaml_block_attaches_to_previous_record() {
        let tap = "1..1\nnot ok 1 - bad\n  ---\n  message: assert failed\n  at: line 42\n  ...\n";
        let records = parse_tap(tap);
        assert_eq!(records.len(), 1);
        let msg = records[0].message.as_deref().unwrap();
        assert!(msg.contains("message: assert failed"));
        assert!(msg.contains("at: line 42"));
    }

    #[test]
    fn tap_bail_out_stops_collection() {
        let tap = "ok 1 - first\nBail out! db down\nok 2 - never seen\n";
        let records = parse_tap(tap);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].name, "first");
    }

    #[test]
    fn tap_skip_directive_synonyms() {
        let tap =
            "ok 1 - a # skip too slow\nok 2 - b # SKIPPED maintenance\nok 3 - c # Skip lowercase\n";
        let records = parse_tap(tap);
        assert_eq!(records.len(), 3);
        assert!(records.iter().all(|r| r.status == Status::Skipped));
    }

    #[test]
    fn tap_ignores_diagnostic_and_unknown_lines() {
        let tap = "TAP version 13\n1..2\n# comment\nrandom emitter chatter\nok 1 - ok\nnot ok 2 - bad\n# trailer\n";
        let records = parse_tap(tap);
        assert_eq!(records.len(), 2);
    }

    #[test]
    fn tap_strips_bom_and_crlf() {
        let tap = "\u{feff}TAP version 14\r\n1..1\r\nok 1 - x\r\n";
        let records = parse_tap(tap);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].name, "x");
    }

    // --- Cargo libtest ---------------------------------------------

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

    // --- go test text ----------------------------------------------

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
