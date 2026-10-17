//! Canonical JUnit report decoding for scripting and host test inspection.
//!
//! A report is accepted as a whole. Truncated XML, unsupported case outcomes,
//! and inconsistent declared totals never produce a passing prefix.

use std::time::Duration;

use crate::stdlib::macros::harn_builtin;
use crate::value::{VmDictExt, VmError, VmValue};
use crate::vm::Vm;

const MAX_DURATION_MS: u64 = i64::MAX as u64;
const MAX_REPORT_BYTES: usize = 32 * 1024 * 1024;
const MAX_REPORT_NODES: u32 = 1_000_000;
const MAX_REPORT_DEPTH: usize = 128;

/// Status of one observed test case.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Passed,
    Failed,
    Skipped,
    Errored,
}

impl Status {
    /// Stable wire spelling used by both test-result projections.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Passed => "passed",
            Self::Failed => "failed",
            Self::Skipped => "skipped",
            Self::Errored => "errored",
        }
    }
}

/// Shared normalized case, including optional source locations from the producer.
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
    /// Create a case without optional diagnostics.
    #[must_use]
    pub fn new(name: impl Into<String>, status: Status) -> Self {
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

/// A report refusal distinguishes invalid evidence from an observed empty suite.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportError {
    pub kind: &'static str,
    pub message: String,
}

impl ReportError {
    fn invalid(message: impl Into<String>) -> Self {
        Self {
            kind: "invalid_report",
            message: message.into(),
        }
    }
}

pub(crate) fn register_junit_builtins(vm: &mut Vm) {
    crate::stdlib::macros::register_module_builtins(vm, module_path!());
}

#[harn_builtin(
    exposure = "pure", effects = [],
    sig = "parse_junit_xml(input: string | bytes | nil) -> list<{name: string, status: \"passed\" | \"failed\" | \"skipped\" | \"errored\", duration_ms: int, message: string?, stdout: string?, stderr: string?, path: string?, line: int?}>",
    category = "junit",
    doc = "Decode a complete JUnit report. Invalid reports return an empty list, never a partial prefix. Use parse_junit_xml_result to distinguish invalid reports from valid empty suites."
)]
fn parse_junit_xml_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let bytes = input_bytes(args, "parse_junit_xml")?;
    Ok(records_to_value(
        parse_junit_xml(&bytes).unwrap_or_default(),
    ))
}

#[harn_builtin(
    exposure = "pure", effects = [],
    sig = "parse_junit_xml_result(input: string | bytes) -> Result<list<{name: string, status: \"passed\" | \"failed\" | \"skipped\" | \"errored\", duration_ms: int, message: string?, stdout: string?, stderr: string?, path: string?, line: int?}>, {kind: \"invalid_xml\" | \"invalid_report\" | \"resource_limit\", message: string}>",
    category = "junit",
    doc = "Decode the complete JUnit document and verify declared test totals. Malformed or incomplete reports return a structured refusal; a valid empty suite returns Ok([]). DTDs are refused; reports are limited to 32 MiB, one million XML nodes, and 128 levels."
)]
fn parse_junit_xml_result_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let bytes = input_bytes(args, "parse_junit_xml_result")?;
    Ok(match parse_junit_xml(&bytes) {
        Ok(records) => VmValue::enum_variant("Result", "Ok", vec![records_to_value(records)]),
        Err(error) => {
            let mut value = crate::value::DictMap::new();
            value.put_str("kind", error.kind);
            value.put_str("message", error.message);
            VmValue::enum_variant("Result", "Err", vec![VmValue::dict(value)])
        }
    })
}

fn input_bytes(args: &[VmValue], name: &str) -> Result<Vec<u8>, VmError> {
    match args.first() {
        Some(VmValue::String(value)) => Ok(value.as_bytes().to_vec()),
        Some(VmValue::Bytes(value)) => Ok((**value).clone()),
        Some(VmValue::Nil) | None => Ok(Vec::new()),
        Some(other) => Err(VmError::Thrown(VmValue::string(format!(
            "{name}: expected string or bytes, got {}",
            other.type_name()
        )))),
    }
}

fn records_to_value(records: Vec<TestRecord>) -> VmValue {
    VmValue::list(records.into_iter().map(record_to_value).collect())
}

fn record_to_value(record: TestRecord) -> VmValue {
    let mut map = crate::value::DictMap::new();
    map.put_str("name", record.name);
    map.put_str("status", record.status.as_str());
    map.put_int("duration_ms", record.duration_ms as i64);
    for (name, value) in [
        ("message", record.message),
        ("stdout", record.stdout),
        ("stderr", record.stderr),
        ("path", record.path),
    ] {
        map.insert(
            crate::value::intern_key(name),
            value.map(VmValue::string).unwrap_or(VmValue::Nil),
        );
    }
    map.put_nullable("line", record.line.map(VmValue::Int));
    VmValue::dict(map)
}

/// Parse a complete UTF-8 JUnit document, refusing partial or inconsistent evidence.
pub fn parse_junit_xml(bytes: &[u8]) -> Result<Vec<TestRecord>, ReportError> {
    if bytes.len() > MAX_REPORT_BYTES {
        return Err(ReportError {
            kind: "resource_limit",
            message: "JUnit report exceeds 32 MiB".into(),
        });
    }
    let text = std::str::from_utf8(bytes).map_err(|error| ReportError {
        kind: "invalid_xml",
        message: error.to_string(),
    })?;
    let document = roxmltree::Document::parse_with_options(
        text,
        roxmltree::ParsingOptions {
            allow_dtd: false,
            nodes_limit: MAX_REPORT_NODES,
            ..Default::default()
        },
    )
    .map_err(|error| ReportError {
        kind: "invalid_xml",
        message: error.to_string(),
    })?;
    let root = document.root_element();
    for node in document.descendants() {
        if node.ancestors().take(MAX_REPORT_DEPTH + 1).count() > MAX_REPORT_DEPTH {
            return Err(ReportError {
                kind: "resource_limit",
                message: "JUnit report exceeds 128 levels".into(),
            });
        }
        if node.is_element() {
            validate_structure(node)?;
        }
    }
    if !matches!(root.tag_name().name(), "testsuite" | "testsuites") {
        return Err(ReportError::invalid(
            "JUnit root must be testsuite or testsuites",
        ));
    }
    let records = document
        .descendants()
        .filter(|node| node.has_tag_name("testcase"))
        .map(parse_case)
        .collect::<Result<Vec<_>, _>>()?;
    for suite in document
        .descendants()
        .filter(|node| node.has_tag_name("testsuite") || node.has_tag_name("testsuites"))
    {
        validate_totals(suite)?;
    }
    Ok(records)
}

fn validate_structure(node: roxmltree::Node<'_, '_>) -> Result<(), ReportError> {
    let name = node.tag_name().name();
    if matches!(
        name,
        "testsuite" | "testsuites" | "testcase" | "failure" | "error" | "skipped"
    ) && node.tag_name().namespace().is_some()
    {
        return Err(ReportError::invalid(
            "namespaced JUnit outcomes are unsupported",
        ));
    }
    if matches!(name, "testsuite" | "testsuites") {
        for child in node.children().filter(roxmltree::Node::is_element) {
            let allowed = matches!(
                child.tag_name().name(),
                "testsuite"
                    | "testsuites"
                    | "properties"
                    | "system-out"
                    | "system-err"
                    | "testcase"
            );
            if !allowed {
                return Err(ReportError::invalid(format!(
                    "unsupported {name} child: {}",
                    child.tag_name().name()
                )));
            }
        }
    }
    Ok(())
}

fn parse_case(node: roxmltree::Node<'_, '_>) -> Result<TestRecord, ReportError> {
    if !node
        .parent()
        .is_some_and(|parent| parent.has_tag_name("testsuite") || parent.has_tag_name("testsuites"))
    {
        return Err(ReportError::invalid(
            "testcase must belong to testsuite or testsuites",
        ));
    }
    let name = node
        .attribute("name")
        .filter(|name| !name.is_empty())
        .ok_or_else(|| ReportError::invalid("testcase name is missing"))?;
    let qualified = node
        .attribute("classname")
        .filter(|name| !name.is_empty())
        .map_or_else(|| name.to_owned(), |class| format!("{class}::{name}"));
    let mut record = TestRecord::new(qualified, Status::Passed);
    record.duration_ms = node
        .attribute("time")
        .and_then(|value| value.parse().ok())
        .map(duration_seconds_to_ms)
        .unwrap_or_default();
    record.path = node.attribute("file").map(str::to_owned);
    record.line = node
        .attribute("line")
        .map(|value| {
            value
                .parse::<i64>()
                .map_err(|_| ReportError::invalid("testcase line is invalid"))
        })
        .transpose()?;
    for child in node.children().filter(roxmltree::Node::is_element) {
        match child.tag_name().name() {
            "failure" | "error" => {
                record.status = if child.has_tag_name("failure") {
                    Status::Failed
                } else {
                    Status::Errored
                };
                let text = element_text(child);
                let message = match (child.attribute("message"), text.is_empty()) {
                    (Some(message), true) => message.to_owned(),
                    (Some(message), false) => format!("{message}\n{text}"),
                    (None, _) => text,
                };
                record.message = Some(match record.message {
                    Some(previous) => format!("{previous}\n{message}"),
                    None => message,
                });
            }
            "skipped" if matches!(record.status, Status::Passed) => record.status = Status::Skipped,
            "skipped" => {}
            "system-out" => record.stdout = Some(element_text(child)),
            "system-err" => record.stderr = Some(element_text(child)),
            "properties" => {}
            other => {
                return Err(ReportError::invalid(format!(
                    "unsupported testcase child: {other}"
                )))
            }
        }
    }
    Ok(record)
}

fn element_text(node: roxmltree::Node<'_, '_>) -> String {
    node.descendants()
        .filter(roxmltree::Node::is_text)
        .filter_map(|node| node.text())
        .collect::<String>()
        .trim()
        .to_owned()
}

fn validate_totals(suite: roxmltree::Node<'_, '_>) -> Result<(), ReportError> {
    let mut tests = 0_usize;
    let mut failures = 0_usize;
    let mut errors = 0_usize;
    let mut skipped = 0_usize;
    for case in suite
        .descendants()
        .filter(|node| node.has_tag_name("testcase"))
    {
        tests += 1;
        failures += usize::from(case.children().any(|node| node.has_tag_name("failure")));
        errors += usize::from(case.children().any(|node| node.has_tag_name("error")));
        skipped += usize::from(case.children().any(|node| node.has_tag_name("skipped")));
    }
    for (name, actual) in [
        ("tests", tests),
        ("failures", failures),
        ("errors", errors),
        ("skipped", skipped),
    ] {
        if let Some(value) = suite.attribute(name) {
            let declared = value
                .parse::<usize>()
                .map_err(|_| ReportError::invalid(format!("invalid {name} total")))?;
            if declared != actual {
                return Err(ReportError::invalid(format!(
                    "{name} total declares {declared}, observed {actual}"
                )));
            }
        }
    }
    Ok(())
}

/// Convert producer timing without overflow or non-finite values leaking into receipts.
#[must_use]
pub fn duration_seconds_to_ms(seconds: f64) -> u64 {
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
    fn complete_document_preserves_diagnostics_and_locations() {
        let xml = r"<testsuite tests='2' failures='1'><testcase classname='pkg' name='pass'/><testcase name='fail' file='src/a.rs' line='9' time='0.003'><failure message='a &amp; b'><![CDATA[left < right]]></failure><system-out>hello &amp; goodbye</system-out></testcase></testsuite>";
        let records = parse_junit_xml(xml.as_bytes()).unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].name, "pkg::pass");
        assert_eq!(records[1].status, Status::Failed);
        assert_eq!(records[1].message.as_deref(), Some("a & b\nleft < right"));
        assert_eq!(records[1].stdout.as_deref(), Some("hello & goodbye"));
        assert_eq!(records[1].duration_ms, 3);
        assert_eq!(records[1].path.as_deref(), Some("src/a.rs"));
        assert_eq!(records[1].line, Some(9));
        // Node's stock JUnit reporter emits cases directly under testsuites.
        let node_report = b"<testsuites tests='2' failures='1'><testcase name='pass'/><testcase name='fail'><failure/></testcase></testsuites>";
        let node_records = parse_junit_xml(node_report).unwrap();
        assert_eq!(node_records.len(), 2);
        assert_eq!(node_records[1].status, Status::Failed);
    }

    #[test]
    fn truncated_failure_never_returns_the_passing_prefix() {
        assert!(parse_junit_xml(
            b"<testsuite><testcase name='pass'/><testcase name='fail'><failure>"
        )
        .is_err());
        assert!(parse_junit_xml(b"<testsuite><testcase name='pass'/>").is_err());
    }

    #[test]
    fn declared_inventory_must_match_observed_cases() {
        assert!(
            parse_junit_xml(b"<testsuite tests='2'><testcase name='only'/></testsuite>").is_err()
        );
        assert!(parse_junit_xml(
            b"<testsuite failures='0'><testcase name='fail'><failure/></testcase></testsuite>"
        )
        .is_err());
        assert!(parse_junit_xml(
            b"<testsuite><testcase name='flake'><flakyFailure/></testcase></testsuite>"
        )
        .is_err());
        assert!(parse_junit_xml(b"<testsuite><testcase/></testsuite>").is_err());
        assert!(parse_junit_xml(
            b"<testsuite><testcase name='pass'/><error>suite failed</error></testsuite>"
        )
        .is_err());
        assert!(parse_junit_xml(b"<testsuite><testcase name='pass'/><x:testcase xmlns:x='urn:other' name='fail'><failure/></x:testcase></testsuite>").is_err());
    }

    #[test]
    fn empty_suite_is_measured_but_non_junit_and_dtds_are_refused() {
        assert!(parse_junit_xml(b"<testsuite tests='0'/>")
            .unwrap()
            .is_empty());
        assert!(parse_junit_xml(b"<root><testcase name='fake'/></root>").is_err());
        assert!(parse_junit_xml(b"<!DOCTYPE testsuite [<!ENTITY a 'x'>]><testsuite/>").is_err());
        assert_eq!(duration_seconds_to_ms(1e308), MAX_DURATION_MS);
    }

    #[test]
    fn nesting_is_bounded_before_suite_accounting() {
        let xml = format!(
            "{}{}",
            "<testsuite>".repeat(129),
            "</testsuite>".repeat(129)
        );
        assert_eq!(
            parse_junit_xml(xml.as_bytes()).unwrap_err().kind,
            "resource_limit"
        );
    }
}
