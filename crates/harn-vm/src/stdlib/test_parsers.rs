//! Stdlib builtins for parsing test runner output.
//!
//! Thin wrappers around [`crate::test_parsers`]. All five parsers
//! return a list of dicts with the same shape — `{name, status,
//! duration_ms, message, stdout, stderr}` — so a Harn script wrapping
//! `process.run` of a test command can pick the parser that matches
//! the runner's output format and extract structured pass/fail data
//! without writing per-runner ad-hoc text scraping.
//!
//! Coverage:
//! - `parse_junit_xml(input)` — JUnit XML, the de facto interchange
//!   format (Maven Surefire, Gradle, JUnit 4/5, xUnit, GTest, pytest
//!   `--junitxml`, vitest `--reporter=junit`, cargo-nextest, PHPUnit,
//!   Swift `--xunit-output`, ScalaTest, jest-junit).
//! - `parse_trx_xml(input)` — Microsoft VS Test results from
//!   `dotnet test --logger trx`.
//! - `parse_tap(text)` — Test Anything Protocol (bats, Perl `prove`,
//!   `busted --output=tap`, deno test, Node `--test-reporter=tap`).
//! - `parse_cargo_test_text(text)` — `cargo test`'s default plain-text
//!   format.
//! - `parse_go_test_text(stdout, stderr?)` — `go test`'s default
//!   plain-text format.

use std::collections::BTreeMap;
use std::rc::Rc;

use crate::test_parsers as core;
use crate::value::{VmError, VmValue};
use crate::vm::Vm;

pub(crate) fn register_test_parser_builtins(vm: &mut Vm) {
    vm.register_builtin("parse_junit_xml", |args, _out| {
        let bytes = bytes_arg("parse_junit_xml", args.first())?;
        let records = core::parse_junit_xml(&bytes).unwrap_or_default();
        Ok(records_to_value(records))
    });

    vm.register_builtin("parse_trx_xml", |args, _out| {
        let bytes = bytes_arg("parse_trx_xml", args.first())?;
        let records = core::parse_trx_xml(&bytes).unwrap_or_default();
        Ok(records_to_value(records))
    });

    vm.register_builtin("parse_tap", |args, _out| {
        let text = string_arg("parse_tap", args.first())?;
        let records = core::parse_tap(&text);
        Ok(records_to_value(records))
    });

    vm.register_builtin("parse_cargo_test_text", |args, _out| {
        let text = string_arg("parse_cargo_test_text", args.first())?;
        let records = core::parse_cargo_libtest(&text);
        Ok(records_to_value(records))
    });

    vm.register_builtin("parse_go_test_text", |args, _out| {
        let stdout = string_arg("parse_go_test_text", args.first())?;
        let stderr = match args.get(1) {
            Some(VmValue::String(s)) => s.to_string(),
            Some(VmValue::Nil) | None => String::new(),
            Some(other) => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
                    "parse_go_test_text: stderr must be string or nil, got {}",
                    other.type_name()
                )))));
            }
        };
        let records = core::parse_go_text(&stdout, &stderr);
        Ok(records_to_value(records))
    });
}

fn bytes_arg(name: &'static str, value: Option<&VmValue>) -> Result<Vec<u8>, VmError> {
    match value {
        Some(VmValue::String(s)) => Ok(s.as_bytes().to_vec()),
        Some(VmValue::Bytes(b)) => Ok((**b).clone()),
        Some(VmValue::Nil) | None => Ok(Vec::new()),
        Some(other) => Err(VmError::Thrown(VmValue::String(Rc::from(format!(
            "{name}: expected string or bytes, got {}",
            other.type_name()
        ))))),
    }
}

fn string_arg(name: &'static str, value: Option<&VmValue>) -> Result<String, VmError> {
    match value {
        Some(VmValue::String(s)) => Ok(s.to_string()),
        Some(VmValue::Bytes(b)) => Ok(String::from_utf8_lossy(b).into_owned()),
        Some(VmValue::Nil) | None => Ok(String::new()),
        Some(other) => Err(VmError::Thrown(VmValue::String(Rc::from(format!(
            "{name}: expected string, got {}",
            other.type_name()
        ))))),
    }
}

fn records_to_value(records: Vec<core::TestRecord>) -> VmValue {
    let list: Vec<VmValue> = records.into_iter().map(record_to_value).collect();
    VmValue::List(Rc::new(list))
}

fn record_to_value(record: core::TestRecord) -> VmValue {
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
