//! Unified event tape for the testbench.
//!
//! A tape is the canonical artifact behind `harn test-bench --emit-tape`.
//! Every non-deterministic input the script consumed — clock advances,
//! LLM responses, FS reads/writes, subprocess spawns — lands as a typed
//! [`TapeRecord`] with a logical sequence number and a virtual-time
//! stamp. The tape is what the [`fidelity`] oracle compares; it is what
//! `harn test-bench replay` reads to drive a deterministic re-run.
//!
//! [`fidelity`]: super::fidelity
//!
//! ## File layout
//!
//! ```text
//! tape.tape       # NDJSON: one header line + one record line per event
//! tape.cas/       # content-addressed sidecar (BLAKE3 hex names)
//! ```
//!
//! Small payloads are serialized inline. Anything over [`MAX_INLINE_BYTES`]
//! lands in `tape.cas/<blake3>` and the record carries `{"cas": "<blake3>"}`.
//! That keeps the main stream diffable when the only thing that changes
//! is a multi-MB LLM response.
//!
//! ## Versioning
//!
//! Every tape carries a `version` integer in its header. The current
//! schema is [`TAPE_FORMAT_VERSION`]. Loaders accept anything `<=` the
//! current version and emit a structured error for newer tapes; this
//! gives us room to add record kinds (under `#[serde(other)]`) without
//! silently breaking older runners.
//!
//! ## Recording
//!
//! Recording is opt-in: the testbench installs a thread-local
//! [`TapeRecorder`] when `Testbench::tape = TapeConfig::Emit { path }`.
//! Every host-capability axis that already has a record path
//! ([`super::process_tape`], [`super::overlay_fs`], [`crate::llm::mock`],
//! [`crate::clock_mock`]) calls into this module to push a record. When
//! no recorder is installed, the helpers are no-ops — production code
//! pays nothing.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::clock_mock;

/// Format version of the on-disk tape representation. Bump on any
/// breaking change. Loaders refuse tapes with a higher version.
pub const TAPE_FORMAT_VERSION: u32 = 1;

/// Records whose serialized payload exceeds this size are spilled into
/// the content-addressed sidecar. Picked to be larger than typical
/// stdout/file-read sizes but smaller than full LLM responses, so the
/// main NDJSON stream stays diffable.
pub const MAX_INLINE_BYTES: usize = 4 * 1024;

/// Header line written first in every tape file. Captures the metadata a
/// fidelity-checker needs to interpret the records that follow.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TapeHeader {
    pub version: u32,
    /// Crate version of the producer (`harn-vm` `CARGO_PKG_VERSION`).
    /// Surfaced so a fidelity report can attribute divergences across
    /// runtime upgrades.
    pub harn_version: String,
    /// UNIX-epoch milliseconds the script was launched at — i.e. the
    /// initial value of the testbench's paused clock. `null` when the
    /// run used the real clock.
    #[serde(default)]
    pub started_at_unix_ms: Option<i64>,
    /// Path passed to `harn test-bench run`. Informational only; replays
    /// resolve scripts via the CLI argument, not this field.
    #[serde(default)]
    pub script_path: Option<String>,
    /// Positional arguments forwarded to the script (post `--`). Captured
    /// so two re-runs that differ only in argv are distinguishable.
    #[serde(default)]
    pub argv: Vec<String>,
}

impl TapeHeader {
    pub fn current(
        started_at_unix_ms: Option<i64>,
        script_path: Option<String>,
        argv: Vec<String>,
    ) -> Self {
        Self {
            version: TAPE_FORMAT_VERSION,
            harn_version: env!("CARGO_PKG_VERSION").to_string(),
            started_at_unix_ms,
            script_path,
            argv,
        }
    }
}

/// One on-disk line of the tape. Wrapping the header and record kinds
/// behind a single tagged enum lets us write the whole file as
/// homogeneous NDJSON.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum TapeLine {
    Header(TapeHeader),
    Record(TapeRecord),
}

/// One captured non-deterministic event. The variant carries the record
/// payload; the wrapping [`TapeRecord`] adds the metadata every variant
/// shares.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TapeRecord {
    /// Monotonic logical sequence number assigned at record time.
    pub seq: u64,
    /// Wall-clock value (UNIX-epoch ms) observed at record time. Reads
    /// from the unified mock clock when one is installed.
    pub virtual_time_ms: i64,
    /// Monotonic ms since the testbench was activated. Independent of
    /// `virtual_time_ms` so a paused clock that never advances still
    /// produces an ordered stream.
    pub monotonic_ms: i64,
    /// The actual event.
    pub kind: TapeRecordKind,
}

/// Discriminated union of every record kind the v1 tape captures. New
/// kinds can be added without breaking older readers (`serde(other)`
/// support is intentional — unknown variants surface as
/// [`TapeRecordKind::Unknown`] so a fidelity check still runs).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TapeRecordKind {
    /// Script read the wall-clock or monotonic clock. The captured value
    /// is what the script actually saw, so a re-run that drifts (e.g.
    /// because the operator forgot `--clock paused`) produces a
    /// different content hash and the fidelity oracle flags it.
    ClockRead { source: ClockSource, value_ms: i64 },
    /// Script slept (or otherwise advanced the unified mock clock) by
    /// `duration_ms`. The recorded virtual time is post-advance.
    ClockSleep { duration_ms: u64 },
    /// LLM call. `request_digest` is a deterministic hash of the call's
    /// matchable surface (messages + system + tools + tool_choice +
    /// thinking). `response` is the recorded mock — inline JSON for
    /// small payloads, a CAS reference for large ones.
    LlmCall {
        request_digest: String,
        response: TapePayload,
    },
    /// Filesystem read against the testbench overlay. The content hash
    /// lets fidelity checks reason about read consistency without
    /// inlining every byte.
    FileRead {
        path: String,
        content_hash: String,
        len_bytes: u64,
    },
    /// Filesystem write into the testbench overlay.
    FileWrite {
        path: String,
        content_hash: String,
        len_bytes: u64,
    },
    /// Filesystem delete in the overlay layer.
    FileDelete { path: String },
    /// Subprocess spawn captured by [`super::process_tape`]. Stdout and
    /// stderr are stored under `stdout_payload`/`stderr_payload` so the
    /// large blobs land in CAS rather than the NDJSON line.
    ProcessSpawn {
        program: String,
        args: Vec<String>,
        cwd: Option<String>,
        exit_code: i32,
        duration_ms: u64,
        stdout_payload: TapePayload,
        stderr_payload: TapePayload,
    },
    /// Catch-all for record kinds emitted by a newer producer. Lets
    /// older fidelity checkers compare what they understand and flag
    /// the rest as `Unknown` divergence rather than refusing to load.
    #[serde(other)]
    Unknown,
}

impl TapeRecordKind {
    /// Stable, snake_case label for this kind. Mirrors the `kind` tag
    /// `serde` writes to disk so display-side code (CLI summaries,
    /// report headers, error messages) is consistent with the wire
    /// format without re-deriving the string each call site.
    pub fn label(&self) -> &'static str {
        match self {
            Self::ClockRead { .. } => "clock_read",
            Self::ClockSleep { .. } => "clock_sleep",
            Self::LlmCall { .. } => "llm_call",
            Self::FileRead { .. } => "file_read",
            Self::FileWrite { .. } => "file_write",
            Self::FileDelete { .. } => "file_delete",
            Self::ProcessSpawn { .. } => "process_spawn",
            Self::Unknown => "unknown",
        }
    }
}

/// Which face of the unified clock the script read. Captured so a
/// fidelity report can attribute drift back to the right axis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClockSource {
    Wall,
    Monotonic,
}

/// On-disk representation of a record payload. Inline for small values,
/// CAS-by-hash for anything over [`MAX_INLINE_BYTES`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum TapePayload {
    /// Inline UTF-8 text payload. Carries a content hash so a fidelity
    /// check can compare without re-hashing.
    Inline { content_hash: String, text: String },
    /// CAS-stored payload. The bytes live at `<tape>.cas/<content_hash>`.
    Cas {
        content_hash: String,
        len_bytes: u64,
    },
}

impl TapePayload {
    pub fn content_hash(&self) -> &str {
        match self {
            Self::Inline { content_hash, .. } | Self::Cas { content_hash, .. } => content_hash,
        }
    }

    pub fn len_bytes(&self) -> u64 {
        match self {
            Self::Inline { text, .. } => text.len() as u64,
            Self::Cas { len_bytes, .. } => *len_bytes,
        }
    }
}

/// Compute a stable BLAKE3 hex digest for a byte slice. Centralized so
/// every record path keys CAS lookups identically.
pub fn content_hash(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

/// Build a [`TapePayload`] from raw bytes, spilling to the sidecar map
/// when the body is large enough to clutter the NDJSON.
fn build_payload(bytes: Vec<u8>, cas: &mut BTreeMap<String, Vec<u8>>) -> TapePayload {
    let hash = content_hash(&bytes);
    if bytes.len() > MAX_INLINE_BYTES {
        let len_bytes = bytes.len() as u64;
        cas.entry(hash.clone()).or_insert(bytes);
        TapePayload::Cas {
            content_hash: hash,
            len_bytes,
        }
    } else {
        let text = match String::from_utf8(bytes) {
            Ok(text) => text,
            Err(error) => {
                // Non-utf8 bytes still need to round-trip. Stash the raw
                // bytes in CAS and inline a sentinel so a fidelity diff
                // is still meaningful.
                let bytes = error.into_bytes();
                let len_bytes = bytes.len() as u64;
                cas.entry(hash.clone()).or_insert(bytes);
                return TapePayload::Cas {
                    content_hash: hash,
                    len_bytes,
                };
            }
        };
        TapePayload::Inline {
            content_hash: hash,
            text,
        }
    }
}

/// In-memory tape: header + ordered record list + sidecar bytes pending
/// flush to disk. Built by [`TapeRecorder`] during a record run; loaded
/// by [`EventTape::load`] for replay or fidelity comparison.
#[derive(Debug, Clone)]
pub struct EventTape {
    pub header: TapeHeader,
    pub records: Vec<TapeRecord>,
    /// Content-addressed bodies. Populated either by the recorder (in
    /// memory until [`EventTape::persist`]) or by [`EventTape::load`]
    /// (read back from `<tape>.cas/`).
    cas: BTreeMap<String, Vec<u8>>,
}

impl EventTape {
    pub fn new(header: TapeHeader) -> Self {
        Self {
            header,
            records: Vec::new(),
            cas: BTreeMap::new(),
        }
    }

    /// Resolve a payload to its full bytes. Inline payloads return their
    /// text; CAS payloads look up the sidecar.
    pub fn resolve_payload(&self, payload: &TapePayload) -> Result<Vec<u8>, String> {
        match payload {
            TapePayload::Inline { text, .. } => Ok(text.as_bytes().to_vec()),
            TapePayload::Cas { content_hash, .. } => self
                .cas
                .get(content_hash)
                .cloned()
                .ok_or_else(|| format!("tape CAS missing entry for {content_hash}")),
        }
    }

    /// Total CAS payload count. Useful for diagnostics and tests.
    pub fn cas_len(&self) -> usize {
        self.cas.len()
    }

    /// Persist the tape (NDJSON + sidecar) to `path`. The sidecar lives
    /// at `<path>.cas/`; the parent directory is created if needed.
    pub fn persist(&self, path: &Path) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .map_err(|err| format!("mkdir {}: {err}", parent.display()))?;
            }
        }

        let mut body = String::new();
        let header_line = serde_json::to_string(&TapeLine::Header(self.header.clone()))
            .map_err(|err| format!("serialize tape header: {err}"))?;
        body.push_str(&header_line);
        body.push('\n');
        for record in &self.records {
            let line = serde_json::to_string(&TapeLine::Record(record.clone()))
                .map_err(|err| format!("serialize tape record: {err}"))?;
            body.push_str(&line);
            body.push('\n');
        }
        std::fs::write(path, body).map_err(|err| format!("write {}: {err}", path.display()))?;

        if !self.cas.is_empty() {
            let cas_dir = cas_dir_for(path);
            std::fs::create_dir_all(&cas_dir)
                .map_err(|err| format!("mkdir {}: {err}", cas_dir.display()))?;
            for (hash, bytes) in &self.cas {
                let entry = cas_dir.join(hash);
                std::fs::write(&entry, bytes)
                    .map_err(|err| format!("write {}: {err}", entry.display()))?;
            }
        }
        Ok(())
    }

    /// Load a tape from `path`. Reads the NDJSON body and lazily fetches
    /// any referenced CAS entries from `<path>.cas/`.
    pub fn load(path: &Path) -> Result<Self, String> {
        let body = std::fs::read_to_string(path)
            .map_err(|err| format!("read {}: {err}", path.display()))?;
        let mut lines = body.lines();
        let first_line = lines
            .next()
            .ok_or_else(|| format!("empty tape file: {}", path.display()))?;
        let header_line: TapeLine = serde_json::from_str(first_line)
            .map_err(|err| format!("parse tape header in {}: {err}", path.display()))?;
        let header = match header_line {
            TapeLine::Header(header) => header,
            TapeLine::Record(_) => {
                return Err(format!(
                    "tape {} is missing its header (first line is a record)",
                    path.display()
                ))
            }
        };
        if header.version > TAPE_FORMAT_VERSION {
            return Err(format!(
                "tape {} declares version {} but this runtime supports up to {TAPE_FORMAT_VERSION}",
                path.display(),
                header.version
            ));
        }
        let mut records = Vec::new();
        for (idx, line) in lines.enumerate() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let parsed: TapeLine = serde_json::from_str(trimmed).map_err(|err| {
                format!(
                    "parse tape record at line {} in {}: {err}",
                    idx + 2,
                    path.display()
                )
            })?;
            match parsed {
                TapeLine::Record(record) => records.push(record),
                TapeLine::Header(_) => {
                    return Err(format!(
                        "tape {} contains a second header at line {}",
                        path.display(),
                        idx + 2
                    ))
                }
            }
        }

        let mut cas = BTreeMap::new();
        let cas_dir = cas_dir_for(path);
        if cas_dir.is_dir() {
            for record in &records {
                visit_payloads(&record.kind, |payload| {
                    if let TapePayload::Cas { content_hash, .. } = payload {
                        if cas.contains_key(content_hash) {
                            return;
                        }
                        let entry = cas_dir.join(content_hash);
                        if let Ok(bytes) = std::fs::read(&entry) {
                            cas.insert(content_hash.clone(), bytes);
                        }
                    }
                });
            }
        }
        Ok(Self {
            header,
            records,
            cas,
        })
    }
}

fn cas_dir_for(tape_path: &Path) -> PathBuf {
    let mut buf = tape_path.as_os_str().to_owned();
    buf.push(".cas");
    PathBuf::from(buf)
}

fn visit_payloads(kind: &TapeRecordKind, mut visit: impl FnMut(&TapePayload)) {
    match kind {
        TapeRecordKind::LlmCall { response, .. } => visit(response),
        TapeRecordKind::ProcessSpawn {
            stdout_payload,
            stderr_payload,
            ..
        } => {
            visit(stdout_payload);
            visit(stderr_payload);
        }
        TapeRecordKind::ClockRead { .. }
        | TapeRecordKind::ClockSleep { .. }
        | TapeRecordKind::FileRead { .. }
        | TapeRecordKind::FileWrite { .. }
        | TapeRecordKind::FileDelete { .. }
        | TapeRecordKind::Unknown => {}
    }
}

/// Recorder consulted by every host-capability axis. When installed as
/// the [`active_recorder`], each axis's record path also pushes a
/// [`TapeRecord`] here so the unified tape stays in sync without
/// re-routing every capability through this module.
#[derive(Debug)]
pub struct TapeRecorder {
    next_seq: AtomicU64,
    started_at: clock_mock::ClockInstant,
    inner: Mutex<RecorderInner>,
}

#[derive(Debug, Default)]
struct RecorderInner {
    records: Vec<TapeRecord>,
    cas: BTreeMap<String, Vec<u8>>,
}

impl Default for TapeRecorder {
    fn default() -> Self {
        Self::new()
    }
}

impl TapeRecorder {
    pub fn new() -> Self {
        Self {
            next_seq: AtomicU64::new(0),
            started_at: clock_mock::instant_now(),
            inner: Mutex::new(RecorderInner::default()),
        }
    }

    /// Append a record built from `kind`. The recorder stamps the seq
    /// number and timing metadata; callers only worry about the payload.
    pub fn record(&self, kind: TapeRecordKind) {
        let seq = self.next_seq.fetch_add(1, Ordering::SeqCst);
        let virtual_time_ms = clock_mock::now_ms();
        let monotonic_ms = clock_mock::instant_now()
            .duration_since(self.started_at)
            .as_millis()
            .min(i64::MAX as u128) as i64;
        let record = TapeRecord {
            seq,
            virtual_time_ms,
            monotonic_ms,
            kind,
        };
        self.inner
            .lock()
            .expect("tape recorder mutex poisoned")
            .records
            .push(record);
    }

    /// Convenience wrapper: build a [`TapePayload`] from `bytes` (spilling
    /// to CAS as needed) and register the bytes for persistence. Used by
    /// axes that have raw bodies on hand (subprocess stdout, LLM
    /// response JSON, file content).
    pub fn payload_from_bytes(&self, bytes: Vec<u8>) -> TapePayload {
        let mut inner = self.inner.lock().expect("tape recorder mutex poisoned");
        build_payload(bytes, &mut inner.cas)
    }

    /// Snapshot the tape into a self-contained [`EventTape`]. Consumes
    /// the recorder's CAS by `clone()` so a recorder can be sampled
    /// mid-run for diagnostics — production callers usually move into
    /// `into_tape` instead.
    pub fn snapshot(&self, header: TapeHeader) -> EventTape {
        let inner = self.inner.lock().expect("tape recorder mutex poisoned");
        EventTape {
            header,
            records: inner.records.clone(),
            cas: inner.cas.clone(),
        }
    }
}

thread_local! {
    static ACTIVE_RECORDER: RefCell<Option<Arc<TapeRecorder>>> = const { RefCell::new(None) };
}

/// RAII guard returned by [`install_recorder`]. Restores the previous
/// recorder (if any) on drop so nested testbench sessions stay sane.
pub struct TapeRecorderGuard {
    previous: Option<Arc<TapeRecorder>>,
}

impl Drop for TapeRecorderGuard {
    fn drop(&mut self) {
        let prev = self.previous.take();
        ACTIVE_RECORDER.with(|slot| {
            *slot.borrow_mut() = prev;
        });
    }
}

pub fn install_recorder(recorder: Arc<TapeRecorder>) -> TapeRecorderGuard {
    let previous = ACTIVE_RECORDER.with(|slot| slot.replace(Some(recorder)));
    TapeRecorderGuard { previous }
}

/// Currently installed recorder, if any. Production callers stay
/// untouched because nothing installs a recorder outside testbench mode.
pub fn active_recorder() -> Option<Arc<TapeRecorder>> {
    ACTIVE_RECORDER.with(|slot| slot.borrow().clone())
}

/// Push a record if a recorder is active. The closure is only evaluated
/// when recording is on, so the per-axis hooks pay nothing in production.
pub fn with_active_recorder<F>(build: F)
where
    F: FnOnce(&Arc<TapeRecorder>) -> Option<TapeRecordKind>,
{
    let Some(recorder) = active_recorder() else {
        return;
    };
    if let Some(kind) = build(&recorder) {
        recorder.record(kind);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn small_record(seq: u64, dur: u64) -> TapeRecord {
        TapeRecord {
            seq,
            virtual_time_ms: seq as i64 * 1000,
            monotonic_ms: seq as i64 * 1000,
            kind: TapeRecordKind::ClockSleep { duration_ms: dur },
        }
    }

    #[test]
    fn round_trip_inline_records() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("run.tape");
        let mut tape = EventTape::new(TapeHeader::current(
            Some(1_700_000_000_000),
            Some("script.harn".to_string()),
            vec!["a".into()],
        ));
        tape.records.push(small_record(0, 250));
        tape.records.push(small_record(1, 750));
        tape.persist(&path).unwrap();

        let loaded = EventTape::load(&path).unwrap();
        assert_eq!(loaded.header.version, TAPE_FORMAT_VERSION);
        assert_eq!(loaded.header.argv, vec!["a".to_string()]);
        assert_eq!(loaded.records.len(), 2);
        match &loaded.records[0].kind {
            TapeRecordKind::ClockSleep { duration_ms } => assert_eq!(*duration_ms, 250),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn large_payloads_spill_to_cas_and_round_trip() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("run.tape");
        let mut tape = EventTape::new(TapeHeader::current(None, None, Vec::new()));
        let big = vec![b'x'; MAX_INLINE_BYTES + 32];
        let payload = build_payload(big.clone(), &mut tape.cas);
        let hash = payload.content_hash().to_string();
        let kind = TapeRecordKind::ProcessSpawn {
            program: "/bin/echo".to_string(),
            args: vec!["x".to_string()],
            cwd: None,
            exit_code: 0,
            duration_ms: 1,
            stdout_payload: payload,
            stderr_payload: build_payload(Vec::new(), &mut tape.cas),
        };
        tape.records.push(TapeRecord {
            seq: 0,
            virtual_time_ms: 0,
            monotonic_ms: 0,
            kind,
        });
        tape.persist(&path).unwrap();

        // CAS sidecar exists.
        assert!(path.with_extension("tape.cas").exists() || cas_dir_for(&path).exists());
        let cas_dir = cas_dir_for(&path);
        assert!(cas_dir.join(&hash).exists());

        let loaded = EventTape::load(&path).unwrap();
        let resolved = match &loaded.records[0].kind {
            TapeRecordKind::ProcessSpawn { stdout_payload, .. } => {
                loaded.resolve_payload(stdout_payload).unwrap()
            }
            other => panic!("unexpected: {other:?}"),
        };
        assert_eq!(resolved.len(), big.len());
    }

    #[test]
    fn rejects_newer_version() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("future.tape");
        std::fs::write(
            &path,
            r#"{"type":"header","version":99,"harn_version":"x","started_at_unix_ms":null,"script_path":null,"argv":[]}
"#,
        )
        .unwrap();
        let err = EventTape::load(&path).unwrap_err();
        assert!(err.contains("version 99"), "{err}");
    }

    #[test]
    fn recorder_assigns_monotonic_seq() {
        let recorder = Arc::new(TapeRecorder::new());
        recorder.record(TapeRecordKind::ClockSleep { duration_ms: 1 });
        recorder.record(TapeRecordKind::ClockSleep { duration_ms: 2 });
        let snapshot = recorder.snapshot(TapeHeader::current(None, None, Vec::new()));
        assert_eq!(snapshot.records[0].seq, 0);
        assert_eq!(snapshot.records[1].seq, 1);
    }
}
