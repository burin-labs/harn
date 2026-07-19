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

use harn_vm::VmDictExt;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex};

use harn_vm::VmValue;

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
    /// Present only when the host selected the command from workspace
    /// discovery. Caller-supplied argv and caller-filtered partial plans remain
    /// executable but are never proof authority for a positive verdict.
    pub(crate) authorized_test_plan: Option<AuthorizedTestPlanIdentity>,
}

/// Host-owned identity of the discovered test plan that was actually
/// executed. Only `tools/run_test` constructs this, after binding discovery to
/// the active workspace and before spawning the selected argv.
#[derive(Debug, Clone)]
pub(crate) struct AuthorizedTestPlanIdentity {
    pub(crate) plan_id: String,
    pub(crate) workspace_hash: String,
    pub(crate) command_hash: String,
}

impl AuthorizedTestPlanIdentity {
    pub(crate) fn discovered(
        workspace: &std::path::Path,
        ecosystem: &str,
        argv: &[String],
    ) -> Self {
        fn tagged_hash(parts: &[&[u8]]) -> String {
            let mut hasher = Sha256::new();
            for part in parts {
                hasher.update((part.len() as u64).to_be_bytes());
                hasher.update(part);
            }
            format!("sha256:{}", hex::encode(hasher.finalize()))
        }

        let workspace_text = workspace.to_string_lossy();
        let workspace_hash = tagged_hash(&[workspace_text.as_bytes()]);
        let argv_bytes: Vec<&[u8]> = argv.iter().map(|arg| arg.as_bytes()).collect();
        let command_hash = tagged_hash(&argv_bytes);
        let plan_id = tagged_hash(&[
            b"host-discovered-test-plan-v1",
            ecosystem.as_bytes(),
            workspace_hash.as_bytes(),
            command_hash.as_bytes(),
        ]);
        Self {
            plan_id,
            workspace_hash,
            command_hash,
        }
    }
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

/// The host-owned record of a single `run_test` execution, keyed by an opaque
/// `result_handle`. Beyond the raw `artifacts` (which `inspect_test_results`
/// drills into on demand), it freezes — AT EXECUTION TIME, from bytes the host
/// itself captured — the pass/fail `summary` the host computed and a
/// `content_hash` of the captured output. Those two frozen fields are the
/// unforgeable execution provenance the verdict issuance authority
/// (`crate::verdict`) consumes: a positive verdict is minted only from a real
/// record here, never from caller-supplied filesystem bytes, and a post-record
/// artifact swap cannot change the frozen `summary`.
#[derive(Debug, Clone)]
pub(crate) struct StoredRun {
    pub(crate) artifacts: RawArtifacts,
    /// The host's own pass/fail counts, computed from the real execution's
    /// output at record time. `None` when nothing parsed — never fabricated.
    pub(crate) summary: Option<TestSummaryData>,
    /// `sha256:HEX` of the host-captured output (stdout, plus the JUnit XML the
    /// runner wrote) snapshotted at record time.
    pub(crate) content_hash: String,
    /// The execution-scope owner active when this run was RECORDED — the run
    /// that actually produced this evidence. Verdict issuance requires the
    /// active scope to still equal this at mint time, so a handle cannot bless a
    /// later, different run. `None` when no owning execution was active at
    /// record time (fail-closed: such a record can never mint a pass).
    pub(crate) execution_scope: Option<std::sync::Arc<str>>,
}

#[derive(Default)]
struct HandleStore {
    entries: BTreeMap<String, StoredRun>,
}

static STORE: LazyLock<Mutex<HandleStore>> = LazyLock::new(|| Mutex::new(HandleStore::default()));
static HANDLE_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Fingerprint the bytes the host captured from a real execution — stdout plus
/// the JUnit XML the runner wrote (read here, while the host still owns the
/// freshly-produced file) — so the receipt carries a host-owned content hash.
fn hash_captured(artifacts: &RawArtifacts) -> String {
    let mut hasher = Sha256::new();
    hasher.update(artifacts.stdout.as_bytes());
    if let Some(path) = artifacts.junit_path.as_ref() {
        if let Ok(bytes) = std::fs::read(path) {
            hasher.update(&bytes);
        }
    }
    format!("sha256:{}", hex::encode(hasher.finalize()))
}

/// Cache a real `run_test` execution's `artifacts` alongside the host-computed
/// `summary`, and return the opaque `result_handle` for them. The `summary` is
/// frozen here (passed in by the runner, which computed it from the same live
/// outcome) so verdict issuance never re-derives disposition from mutable bytes.
pub(crate) fn store_run(artifacts: RawArtifacts, summary: Option<TestSummaryData>) -> String {
    let id = HANDLE_COUNTER.fetch_add(1, Ordering::SeqCst);
    let handle = format!("htr-{:x}-{id}", std::process::id());
    let content_hash = hash_captured(&artifacts);
    // Capture the owning execution AT RECORD TIME — the run that actually
    // produced this evidence. This is the provenance the verdict authority
    // binds to; consumption-time identity (e.g. request_id) would let an old
    // handle bless a later run.
    let execution_scope = harn_vm::current_execution_scope();
    let mut store = STORE.lock().expect("hostlib test handle store poisoned");
    store.entries.insert(
        handle.clone(),
        StoredRun {
            artifacts,
            summary,
            content_hash,
            execution_scope,
        },
    );
    handle
}

/// Resolve a `result_handle` to the host-owned execution record, or `None` when
/// no such execution was recorded (a fabricated or stale handle). This is the
/// provenance gate the verdict issuance authority rides: the store is populated
/// ONLY by `store_run` (called only from a real `run_test` spawn), so a handle
/// that resolves here is proof the host itself executed and observed the run.
pub(crate) fn get_run(handle: &str) -> Option<StoredRun> {
    let store = STORE.lock().expect("hostlib test handle store poisoned");
    store.entries.get(handle).cloned()
}

pub(crate) fn handle(args: &[VmValue]) -> Result<VmValue, HostlibError> {
    let map = require_dict_arg(NAME, args)?;
    let handle = require_string(NAME, &map, "result_handle")?;
    let include_passing = optional_bool(NAME, &map, "include_passing")?.unwrap_or(false);

    let artifacts = get_run(&handle)
        .ok_or(HostlibError::InvalidParameter {
            builtin: NAME,
            param: "result_handle",
            message: format!("no test results stored under handle {handle}"),
        })?
        .artifacts;

    let mut records = artifacts.parse_records();
    if !include_passing {
        records.retain(|r| !matches!(r.status, test_parsers::Status::Passed));
    }
    let entries: Vec<VmValue> = records
        .into_iter()
        .map(|r| VmValue::dict(record_to_map(r)))
        .collect();
    Ok(ResponseBuilder::new()
        .str("result_handle", handle)
        .list("tests", entries)
        .build())
}

fn record_to_map(record: test_parsers::TestRecord) -> harn_vm::value::DictMap {
    let mut map = harn_vm::value::DictMap::new();
    map.put_str("name", record.name);
    map.put_str("status", record.status.as_str());
    map.insert(
        harn_vm::value::intern_key("duration_ms"),
        VmValue::Int(record.duration_ms as i64),
    );
    map.insert(
        harn_vm::value::intern_key("message"),
        record
            .message
            .map(|m| VmValue::String(arcstr::ArcStr::from(m)))
            .unwrap_or(VmValue::Nil),
    );
    map.insert(
        harn_vm::value::intern_key("stdout"),
        record
            .stdout
            .map(|s| VmValue::String(arcstr::ArcStr::from(s)))
            .unwrap_or(VmValue::Nil),
    );
    map.insert(
        harn_vm::value::intern_key("stderr"),
        record
            .stderr
            .map(|s| VmValue::String(arcstr::ArcStr::from(s)))
            .unwrap_or(VmValue::Nil),
    );
    map.insert(
        harn_vm::value::intern_key("path"),
        record
            .path
            .map(|p| VmValue::String(arcstr::ArcStr::from(p)))
            .unwrap_or(VmValue::Nil),
    );
    map.insert(
        harn_vm::value::intern_key("line"),
        record.line.map(VmValue::Int).unwrap_or(VmValue::Nil),
    );
    map
}
