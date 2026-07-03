//! Untrusted-origin file provenance (taint-on-write, distrust-on-read).
//!
//! The lethal-trifecta gate ([`super::classify_result_trust`] +
//! [`crate::llm::agent_host_primitives`]) contains untrusted content that
//! arrives over a *live* channel — an MCP server result, an internet fetch, a
//! cross-agent hand-off. But content can also arrive on disk and be read back
//! *later*: a cloned dependency's `README`, a downloaded dataset, a file the
//! model itself wrote while a poisoned page was in context. A plain
//! `read_file` is first-party by design ([`super::classify_result_trust`]
//! returns `None` for it), so that deferred payload slips past the gate — the
//! measured first-party residual of the containment battery
//! ([`super::battery::run_containment_battery`]).
//!
//! Closing it does **not** mean distrusting every file read — a file you
//! authored is not an injection vector, and gating first-party source reads
//! would wreck usability. It means distrusting a file by its **origin**. This
//! module is the persistent-storage analog of the in-context taint ledger
//! ([`super::TaintRecord`]): a session-scoped map from a workspace path to the
//! untrusted origin its content came from.
//!
//! Two seams, both reusing signals the runtime already computes:
//!
//!   * **taint-on-write** — when a tool [`super::mutates_workspace`] *and*
//!     untrusted content is already in the session's context (or the writing
//!     tool's own result is untrusted, e.g. a fetch-to-disk / clone / MCP
//!     write), the written path inherits that untrusted origin. This is
//!     textbook taint propagation, extended from context to the file system.
//!     It needs no per-tool name allowlist: the "is this a download?" signal is
//!     the tool's existing [`crate::tool_annotations::ToolKind::Fetch`] /
//!     `Network` side-effect annotation, surfaced through
//!     [`super::classify_result_trust`].
//!   * **distrust-on-read** — a `Read`-kind tool whose target path is in the
//!     ledger classifies [`super::TrustLevel::Untrusted`], so its content flows
//!     into the same taint / trifecta gate as any other untrusted ingress.
//!
//! Gated behind the default-OFF `taint_file_provenance` policy flag, so
//! behaviour is byte-identical when disabled (net-new enforcement, like
//! `authenticate_directives`).
//!
//! Scope, stated honestly: the ledger keys on the **lexical** path a read/write
//! names, so a command-based read (`cat vendor/dep/README`) whose payload path
//! never appears as a structured `path` argument is out of scope — that is the
//! `tool_result` residual the battery still reports. Per-value dataflow taint is
//! not recoverable once content passes through the model, so (like
//! [`super::TaintRecord`]) provenance is tracked at path granularity, not byte
//! granularity.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::TrustLevel;

/// Origin-id prefix recorded on the [`super::TaintRecord`] when a tainted file
/// is read, surfaced in the trifecta gate's confirmation reason. The stored
/// origin chains the source, e.g. `file:fetch:web_fetch` (this file's content
/// came from a web fetch) or `file:tainted-context` (written while untrusted
/// content was in context).
pub const FILE_ORIGIN_PREFIX: &str = "file";

/// Origin recorded when a write propagates ambient context taint (as opposed to
/// inheriting a specific untrusted result's origin).
pub const TAINTED_CONTEXT_ORIGIN: &str = "tainted-context";

/// Structured-argument keys that name a single file a tool reads or writes.
/// Matched across the common editor/host tool vocabulary; unknown shapes simply
/// yield no path (fail-open to "no provenance", never a false quarantine).
const PATH_KEYS: &[&str] = &["path", "file_path", "file", "filename", "target_file"];

/// Lexical path normalization so a write and a later read that name the same
/// file by a slightly different spelling still match. Deliberately lexical (no
/// filesystem `canonicalize`): the security layer must not touch disk, and the
/// battery's modelled paths do not exist. Strips a leading `./` and trailing
/// slashes and trims surrounding whitespace.
fn normalize_path(path: &str) -> String {
    let trimmed = path.trim();
    let trimmed = trimmed.strip_prefix("./").unwrap_or(trimmed);
    trimmed.trim_end_matches('/').to_string()
}

/// Session-scoped map from a workspace path to the untrusted origin its content
/// was last known to carry. Owned by the agent host session so it drops with the
/// session (no cross-session leak), mirroring the [`super::TaintRecord`] ledger.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileProvenanceLedger {
    tainted: BTreeMap<String, String>,
}

impl FileProvenanceLedger {
    /// Record that `path` now holds content of untrusted `origin`. The first
    /// origin sticks (a path does not become "more trusted" by a later write);
    /// empty paths are ignored.
    pub fn record(&mut self, path: &str, origin: &str) {
        let key = normalize_path(path);
        if key.is_empty() {
            return;
        }
        self.tainted
            .entry(key)
            .or_insert_with(|| origin.to_string());
    }

    /// Trust classification for a read of `path`. `Some((Untrusted, origin))`
    /// when the path is a known untrusted-origin file; `None` otherwise. Shaped
    /// like [`super::classify_result_trust`] so the read path can `.or_else(...)`
    /// the two.
    pub fn classify(&self, path: &str) -> Option<(TrustLevel, String)> {
        let key = normalize_path(path);
        self.tainted.get(&key).map(|origin| {
            (
                TrustLevel::Untrusted,
                format!("{FILE_ORIGIN_PREFIX}:{origin}"),
            )
        })
    }

    pub fn is_empty(&self) -> bool {
        self.tainted.is_empty()
    }

    pub fn len(&self) -> usize {
        self.tainted.len()
    }
}

/// Extract the file path(s) a tool call names in its structured arguments. Reads
/// the canonical single-path keys plus a plural `paths` array (multi-file
/// write/patch). Used on the write path to record targets and on the read path
/// to look them up, so both sides agree on what "the file" is.
pub fn path_arguments(arguments: &serde_json::Value) -> Vec<String> {
    let mut paths = Vec::new();
    let Some(obj) = arguments.as_object() else {
        return paths;
    };
    for key in PATH_KEYS {
        if let Some(serde_json::Value::String(value)) = obj.get(*key) {
            if !value.trim().is_empty() {
                paths.push(value.clone());
            }
        }
    }
    if let Some(serde_json::Value::Array(items)) = obj.get("paths") {
        for item in items {
            if let serde_json::Value::String(value) = item {
                if !value.trim().is_empty() {
                    paths.push(value.clone());
                }
            }
        }
    }
    paths
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_read_of_an_untainted_path_is_trusted() {
        let ledger = FileProvenanceLedger::default();
        assert!(ledger.is_empty());
        assert!(ledger.classify("src/main.rs").is_none());
    }

    #[test]
    fn a_read_of_a_recorded_path_is_untrusted_with_a_chained_origin() {
        let mut ledger = FileProvenanceLedger::default();
        ledger.record("vendor/dep/README.md", "fetch:web_fetch");
        assert_eq!(ledger.len(), 1);
        assert_eq!(
            ledger.classify("vendor/dep/README.md"),
            Some((TrustLevel::Untrusted, "file:fetch:web_fetch".to_string()))
        );
    }

    #[test]
    fn normalization_matches_write_and_read_spellings() {
        let mut ledger = FileProvenanceLedger::default();
        ledger.record("./notes/summary.md", TAINTED_CONTEXT_ORIGIN);
        // Read names the same file without the leading `./`.
        assert_eq!(
            ledger.classify("notes/summary.md"),
            Some((
                TrustLevel::Untrusted,
                format!("{FILE_ORIGIN_PREFIX}:{TAINTED_CONTEXT_ORIGIN}")
            ))
        );
    }

    #[test]
    fn the_first_origin_sticks() {
        let mut ledger = FileProvenanceLedger::default();
        ledger.record("a.txt", "fetch:web_fetch");
        ledger.record("a.txt", TAINTED_CONTEXT_ORIGIN);
        assert_eq!(
            ledger.classify("a.txt"),
            Some((TrustLevel::Untrusted, "file:fetch:web_fetch".to_string()))
        );
    }

    #[test]
    fn empty_paths_are_ignored() {
        let mut ledger = FileProvenanceLedger::default();
        ledger.record("   ", "fetch:web_fetch");
        ledger.record("", "fetch:web_fetch");
        assert!(ledger.is_empty());
    }

    #[test]
    fn path_arguments_reads_the_common_vocabulary() {
        assert_eq!(
            path_arguments(&json!({"path": "src/a.rs"})),
            vec!["src/a.rs".to_string()]
        );
        assert_eq!(
            path_arguments(&json!({"file_path": "src/b.rs", "content": "..."})),
            vec!["src/b.rs".to_string()]
        );
        assert_eq!(
            path_arguments(&json!({"paths": ["x.md", "y.md"], "note": "z"})),
            vec!["x.md".to_string(), "y.md".to_string()]
        );
    }

    #[test]
    fn path_arguments_ignores_non_path_and_blank_values() {
        assert!(path_arguments(&json!({"query": "ripgrep this"})).is_empty());
        assert!(path_arguments(&json!({"path": "   "})).is_empty());
        assert!(path_arguments(&json!("not an object")).is_empty());
    }
}
