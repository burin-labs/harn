//! `tools/inspect_test_results` — drill into the structured per-test record
//! from a previous `run_test`.
//!
//! Schema: `schemas/tools/inspect_test_results.{request,response}.json`.
//!
//! `run_test` stores raw stdout/stderr + the JUnit XML path it asked the
//! runner to write into a process-local cache, keyed by an opaque
//! `result_handle`. This builtin pulls that record out and parses it into
//! `TestRecord`s. Parsers in [`crate::tools::test_parsers`] handle the
//! per-runner formats (JUnit XML, cargo libtest plain text, go test text).
//!
//! The cache is intentionally scoped to the hostlib process: handles are
//! ephemeral, not persisted to disk, and are not shared across embedders.
//! That keeps the contract simple and avoids inventing a cross-session
//! handle namespace before there's a real consumer for one.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use harn_vm::VmValue;
use once_cell::sync::Lazy;

use crate::error::HostlibError;
use crate::tools::payload::{optional_bool, require_dict_arg, require_string};
use crate::tools::response::ResponseBuilder;
use crate::tools::test_parsers;

pub(crate) const NAME: &str = "hostlib_tools_inspect_test_results";

/// Captured outcome of a `run_test` invocation, plus the metadata
/// `inspect_test_results` needs to build per-test records on demand.
///
/// `exit_code` and `argv` are kept on the struct (not just for debugging) so
/// future parsers can correlate per-test status with the runner's exit code
/// — e.g. cargo libtest exits non-zero even when the failure summary is
/// empty (build errors). Today no parser uses them yet.
#[derive(Debug, Clone)]
pub(crate) struct RawArtifacts {
    pub(crate) stdout: String,
    pub(crate) stderr: String,
    #[allow(dead_code)]
    pub(crate) exit_code: i32,
    pub(crate) junit_path: Option<PathBuf>,
    pub(crate) ecosystem: Option<String>,
    #[allow(dead_code)]
    pub(crate) argv: Vec<String>,
}

/// Three-way summary surfaced inline by `run_test`. Same shape as the
/// `summary` dict in the schema.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct TestSummaryData {
    pub(crate) passed: u32,
    pub(crate) failed: u32,
    pub(crate) skipped: u32,
}

impl RawArtifacts {
    /// Produce an inline `(passed, failed, skipped)` summary by parsing
    /// whatever output the runner emitted. Returns `None` when no parser
    /// matched — `run_test` then omits the `summary` field entirely
    /// rather than returning fabricated zeros.
    pub(crate) fn compute_summary(&self) -> Option<TestSummaryData> {
        let records = self.parse_records();
        if records.is_empty() {
            return None;
        }
        let mut data = TestSummaryData::default();
        for r in &records {
            match r.status {
                test_parsers::Status::Passed => data.passed += 1,
                test_parsers::Status::Failed | test_parsers::Status::Errored => data.failed += 1,
                test_parsers::Status::Skipped => data.skipped += 1,
            }
        }
        Some(data)
    }

    fn parse_records(&self) -> Vec<test_parsers::TestRecord> {
        if let Some(path) = self.junit_path.as_ref() {
            if let Ok(bytes) = std::fs::read(path) {
                if let Ok(records) = test_parsers::parse_junit_xml(&bytes) {
                    return records;
                }
            }
        }
        if let Some(eco) = self.ecosystem.as_deref() {
            if eco == "cargo" {
                return test_parsers::parse_cargo_libtest(&self.stdout);
            }
            if eco == "go" {
                return test_parsers::parse_go_text(&self.stdout, &self.stderr);
            }
        }
        // Last-chance heuristic: if it *looks* like libtest output, parse
        // it as such — covers the explicit-argv cargo case.
        if self.stdout.contains("test result:") {
            return test_parsers::parse_cargo_libtest(&self.stdout);
        }
        Vec::new()
    }
}

#[derive(Default)]
struct HandleStore {
    entries: BTreeMap<String, RawArtifacts>,
}

static STORE: Lazy<Mutex<HandleStore>> = Lazy::new(|| Mutex::new(HandleStore::default()));
static HANDLE_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Cache `artifacts` and return the opaque `result_handle` for them.
pub(crate) fn store_run(artifacts: RawArtifacts) -> String {
    let id = HANDLE_COUNTER.fetch_add(1, Ordering::SeqCst);
    let handle = format!("htr-{:x}-{id}", std::process::id());
    let mut store = STORE.lock().expect("hostlib test handle store poisoned");
    store.entries.insert(handle.clone(), artifacts);
    handle
}

pub(crate) fn handle(args: &[VmValue]) -> Result<VmValue, HostlibError> {
    let map = require_dict_arg(NAME, args)?;
    let handle = require_string(NAME, &map, "result_handle")?;
    let include_passing = optional_bool(NAME, &map, "include_passing")?.unwrap_or(false);

    let artifacts = {
        let store = STORE.lock().expect("hostlib test handle store poisoned");
        store
            .entries
            .get(&handle)
            .cloned()
            .ok_or(HostlibError::InvalidParameter {
                builtin: NAME,
                param: "result_handle",
                message: format!("no test results stored under handle {handle}"),
            })?
    };

    let mut records = artifacts.parse_records();
    if !include_passing {
        records.retain(|r| !matches!(r.status, test_parsers::Status::Passed));
    }
    let entries: Vec<VmValue> = records
        .into_iter()
        .map(|r| VmValue::Dict(Rc::new(record_to_map(r))))
        .collect();
    Ok(ResponseBuilder::new()
        .str("result_handle", handle)
        .list("tests", entries)
        .build())
}

fn record_to_map(record: test_parsers::TestRecord) -> BTreeMap<String, VmValue> {
    let mut map = BTreeMap::new();
    map.insert("name".to_string(), VmValue::String(Rc::from(record.name)));
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
            .map(|m| VmValue::String(Rc::from(m)))
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
    map.insert(
        "path".to_string(),
        record
            .path
            .map(|p| VmValue::String(Rc::from(p)))
            .unwrap_or(VmValue::Nil),
    );
    map.insert(
        "line".to_string(),
        record.line.map(VmValue::Int).unwrap_or(VmValue::Nil),
    );
    map
}
