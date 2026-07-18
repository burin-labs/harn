//! Persistent per-file result cache for `harn check` (#4391).
//!
//! `harn run` skips parse/typecheck/compile on warm starts via the
//! content-addressed bytecode cache, but `check` re-did every phase on every
//! invocation — cold == warm — even though CI lint gates and editor hooks
//! re-check overwhelmingly unchanged trees. This module caches each file's
//! *complete* check outcome (structured diagnostics + rendered text), keyed
//! so that any input that could change the outcome flips the key or fails
//! validation:
//!
//! - the file's content and its transitive user-import closure, stdlib
//!   digest, codegen fingerprint, harn version, and compiler options — all
//!   via the bytecode cache's [`bytecode_cache::CacheKey`];
//! - a build-time fingerprint of the check driver itself (`harn-lint`,
//!   `commands/check`, package-config parsing) — `HARN_CHECK_FINGERPRINT` —
//!   so within-version edits to lint/preflight logic invalidate entries
//!   exactly like #2621 did for the compiler;
//! - the effective per-file `CheckConfig` (exhaustively destructured so new
//!   fields cannot be forgotten), the `--invariants` flag, the path string
//!   diagnostics render with, and the file's cross-file lint-exemption set;
//! - a recorded log of every filesystem probe the preflight scan made
//!   beyond the import closure (template/asset reads, directory checks,
//!   project-root resolution, the "did you mean" basename walk), replayed
//!   and compared on every load — the ninja/ccache discovered-dependency
//!   model. Any mismatch is a miss and the file re-checks fully.
//!
//! Artifacts are small JSON files under `<cache>/check/<key>.harncheck`,
//! written atomically. `HARN_BYTECODE_CACHE=0` disables this cache together
//! with the bytecode cache; `HARN_CHECK_RESULT_CACHE=0` disables just this
//! one.

use std::cell::RefCell;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use harn_vm::bytecode_cache;

use crate::package::CheckConfig;

use super::check_cmd::{CheckDiagnostic, CheckFileReport, CheckFileStatus, CheckSpan};
use super::driver::CheckedFile;

/// Bump when the artifact layout or replay semantics change.
const RESULT_CACHE_SCHEMA: u32 = 1;

/// Kill switch for just the check-result cache (the shared
/// `HARN_BYTECODE_CACHE=0` toggle also disables it).
pub(crate) const RESULT_CACHE_ENV: &str = "HARN_CHECK_RESULT_CACHE";

/// Build-time fingerprint of the check pipeline's own sources (harn-lint,
/// commands/check, package config parsing). See `build.rs`.
const CHECK_FINGERPRINT: &str = env!("HARN_CHECK_FINGERPRINT");

/// True when the check-result cache should be consulted and written.
pub(super) fn enabled() -> bool {
    if !bytecode_cache::cache_enabled() {
        return false;
    }
    match std::env::var(RESULT_CACHE_ENV).ok().as_deref() {
        Some(value) => !matches!(
            value.to_ascii_lowercase().as_str(),
            "0" | "false" | "no" | "off"
        ),
        None => true,
    }
}

// ── Probe recording ─────────────────────────────────────────────────────────
//
// The preflight scan consults files *outside* the import closure (prompt
// templates, asset files, execution directories, project-root discovery).
// While a file is being checked, those consults run through the helpers
// below, which record the observed outcome. On a cache hit the recorded
// probes are re-executed and compared; any drift fails closed to a full
// re-check.

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) enum Probe {
    /// `path.exists()` at scan time.
    Exists { path: PathBuf, existed: bool },
    /// `path.is_dir()` at scan time.
    IsDir { path: PathBuf, was_dir: bool },
    /// `std::fs::read_to_string(path)` at scan time; `digest` is the sha256
    /// hex of the content, `None` when the read failed.
    ReadFile {
        path: PathBuf,
        digest: Option<String>,
    },
    /// `resolve_preflight_target(anchor, target, config)` candidates. The
    /// anchor is recorded because preflight scans imported modules too, so
    /// the resolution anchor is often *not* the file being checked. The
    /// resolver probes project-root markers and `harn.toml` asset roots
    /// internally, so replaying it captures layout changes.
    ResolveTarget {
        anchor: PathBuf,
        target: String,
        candidates: Vec<PathBuf>,
    },
    /// `find_unique_basename(root, basename)` outcome (the "did you mean"
    /// suggestion walk).
    WalkUnique {
        root: PathBuf,
        basename: String,
        result: Option<PathBuf>,
    },
}

thread_local! {
    static PROBE_LOG: RefCell<Option<Vec<Probe>>> = const { RefCell::new(None) };
}

/// Run `f` with probe recording enabled on this thread (when `active`),
/// returning its result and the recorded log. Nested recording is not
/// supported (the driver records exactly one file at a time per worker
/// thread).
pub(super) fn with_probe_recording<T>(active: bool, f: impl FnOnce() -> T) -> (T, Vec<Probe>) {
    if !active {
        return (f(), Vec::new());
    }
    PROBE_LOG.with(|log| *log.borrow_mut() = Some(Vec::new()));
    let value = f();
    let probes = PROBE_LOG.with(|log| log.borrow_mut().take().unwrap_or_default());
    (value, probes)
}

fn record(probe: Probe) {
    PROBE_LOG.with(|log| {
        if let Some(probes) = log.borrow_mut().as_mut() {
            probes.push(probe);
        }
    });
}

/// Probe-recording `path.exists()`.
pub(super) fn probe_exists(path: &Path) -> bool {
    let existed = path.exists();
    record(Probe::Exists {
        path: path.to_path_buf(),
        existed,
    });
    existed
}

/// Probe-recording `path.is_dir()`.
pub(super) fn probe_is_dir(path: &Path) -> bool {
    let was_dir = path.is_dir();
    record(Probe::IsDir {
        path: path.to_path_buf(),
        was_dir,
    });
    was_dir
}

/// Probe-recording `std::fs::read_to_string`.
pub(super) fn probe_read_to_string(path: &Path) -> io::Result<String> {
    let result = std::fs::read_to_string(path);
    record(Probe::ReadFile {
        path: path.to_path_buf(),
        digest: result.as_deref().ok().map(sha256_hex),
    });
    result
}

/// Record a resolved preflight target's candidate list.
pub(super) fn record_resolve_target(anchor: &Path, target: &str, candidates: &[PathBuf]) {
    record(Probe::ResolveTarget {
        anchor: anchor.to_path_buf(),
        target: target.to_string(),
        candidates: candidates.to_vec(),
    });
}

/// Record a unique-basename walk outcome.
pub(super) fn record_walk_unique(root: &Path, basename: &str, result: Option<&Path>) {
    record(Probe::WalkUnique {
        root: root.to_path_buf(),
        basename: basename.to_string(),
        result: result.map(Path::to_path_buf),
    });
}

/// Re-execute one recorded probe and compare against the recorded outcome.
fn probe_still_valid(probe: &Probe, config: &CheckConfig) -> bool {
    match probe {
        Probe::Exists { path, existed } => path.exists() == *existed,
        Probe::IsDir { path, was_dir } => path.is_dir() == *was_dir,
        Probe::ReadFile { path, digest } => {
            std::fs::read_to_string(path).ok().map(|c| sha256_hex(&c)) == *digest
        }
        Probe::ResolveTarget {
            anchor,
            target,
            candidates,
        } => super::preflight::resolve_preflight_target(anchor, target, config) == *candidates,
        Probe::WalkUnique {
            root,
            basename,
            result,
        } => super::preflight::find_unique_basename(root, basename) == *result,
    }
}

// ── Key derivation ──────────────────────────────────────────────────────────

/// Everything that keys a file's cached check result. The `CacheKey` half is
/// the same content-addressed identity `harn run` trusts; the rest captures
/// check-only inputs.
pub(super) fn result_cache_key(
    file: &Path,
    path_str: &str,
    source: &str,
    config: &CheckConfig,
    host_capabilities_content: Option<&str>,
    check_invariants: bool,
    lint_exemptions: &[String],
) -> [u8; 32] {
    let base = bytecode_cache::CacheKey::from_source(file, source);
    let mut hasher = Sha256::new();
    let mut fold = |label: &str, value: &[u8]| {
        hasher.update(label.as_bytes());
        hasher.update(b"\0");
        hasher.update(value);
        hasher.update(b"\0");
    };
    fold("schema", &RESULT_CACHE_SCHEMA.to_le_bytes());
    fold(
        "check-schema",
        &super::check_cmd::CHECK_SCHEMA_VERSION.to_le_bytes(),
    );
    fold("source-hash", &base.source_hash);
    fold("compilation-context-hash", &base.context_hash);
    fold("harn-version", base.harn_version.as_bytes());
    fold("compiler-tag", &[base.compiler_tag]);
    fold("check-fingerprint", CHECK_FINGERPRINT.as_bytes());
    fold("path", path_str.as_bytes());
    fold("invariants", &[u8::from(check_invariants)]);
    for name in lint_exemptions {
        fold("lint-exemption", name.as_bytes());
    }
    fold_config(&mut fold, config, host_capabilities_content);
    hasher.finalize().into()
}

/// Fold every `CheckConfig` field into the key. Exhaustive destructuring
/// makes adding a config field without updating the cache key a compile
/// error rather than a stale-cache bug.
fn fold_config(
    fold: &mut impl FnMut(&str, &[u8]),
    config: &CheckConfig,
    host_capabilities_content: Option<&str>,
) {
    let CheckConfig {
        strict,
        strict_types,
        disable_rules,
        host_capabilities,
        host_capabilities_path,
        bundle_root,
        preflight_severity,
        preflight_allow,
    } = config;
    fold("strict", &[u8::from(*strict)]);
    fold("strict-types", &[u8::from(*strict_types)]);
    let mut disable_rules: Vec<&String> = disable_rules.iter().collect();
    disable_rules.sort();
    for rule in disable_rules {
        fold("disable-rule", rule.as_bytes());
    }
    let mut caps: Vec<(&String, &Vec<String>)> = host_capabilities.iter().collect();
    caps.sort_by_key(|(name, _)| *name);
    for (name, ops) in caps {
        fold("host-capability", name.as_bytes());
        let mut ops: Vec<&String> = ops.iter().collect();
        ops.sort();
        for op in ops {
            fold("host-capability-op", op.as_bytes());
        }
    }
    if let Some(path) = host_capabilities_path {
        fold("host-capabilities-path", path.as_bytes());
        // Hash the exact snapshot preflight parsed. The check driver resolves
        // it once per directory before worker fan-out, avoiding a second read
        // per file and preventing parse/key TOCTTOU drift.
        fold(
            "host-capabilities-content",
            host_capabilities_content.unwrap_or_default().as_bytes(),
        );
    }
    if let Some(root) = bundle_root {
        fold("bundle-root", root.as_bytes());
    }
    if let Some(severity) = preflight_severity {
        fold("preflight-severity", severity.as_bytes());
    }
    let mut allow: Vec<&String> = preflight_allow.iter().collect();
    allow.sort();
    for tag in allow {
        fold("preflight-allow", tag.as_bytes());
    }
}

// ── Artifact layout ─────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
struct CachedCheckResult {
    schema: u32,
    status: CachedStatus,
    diagnostics: Vec<CachedDiagnostic>,
    stdout_text: String,
    stderr_text: String,
    probes: Vec<Probe>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum CachedStatus {
    Ok,
    Warning,
    Error,
}

#[derive(Debug, Serialize, Deserialize)]
struct CachedDiagnostic {
    source: String,
    severity: String,
    code: Option<String>,
    message: String,
    span: Option<(usize, usize)>,
    help: Option<String>,
}

/// Map a cached severity/source string back to the `&'static str` the live
/// pipeline uses. Unknown values mean the artifact came from a different
/// (buggy or future) writer: fail closed by rejecting the artifact.
fn intern_diag_str(value: &str) -> Option<&'static str> {
    Some(match value {
        "type" => "type",
        "compile" => "compile",
        "lint" => "lint",
        "preflight" => "preflight",
        "invariant" => "invariant",
        "io" => "io",
        "analysis" => "analysis",
        "lexer" => "lexer",
        "parser" => "parser",
        "error" => "error",
        "warning" => "warning",
        "info" => "info",
        _ => return None,
    })
}

fn cache_path(key: &[u8; 32]) -> PathBuf {
    bytecode_cache::cache_dir()
        .join("check")
        .join(format!("{}.harncheck", hex(key)))
}

// ── Load / store ────────────────────────────────────────────────────────────

/// Try to replay a cached check result. Returns `None` on miss, disabled
/// cache, schema drift, unknown interned strings, or any probe mismatch.
pub(super) fn load(
    key: &[u8; 32],
    path_str: &str,
    config: &CheckConfig,
    want_text: bool,
) -> Option<CheckedFile> {
    if !enabled() {
        return None;
    }
    let bytes = std::fs::read(cache_path(key)).ok()?;
    let cached: CachedCheckResult = serde_json::from_slice(&bytes).ok()?;
    if cached.schema != RESULT_CACHE_SCHEMA {
        return None;
    }
    for probe in &cached.probes {
        if !probe_still_valid(probe, config) {
            return None;
        }
    }
    let mut diagnostics = Vec::with_capacity(cached.diagnostics.len());
    for diag in cached.diagnostics {
        diagnostics.push(CheckDiagnostic {
            source: intern_diag_str(&diag.source)?,
            severity: intern_diag_str(&diag.severity)?,
            code: diag.code,
            message: diag.message,
            span: diag.span.map(|(start, end)| CheckSpan { start, end }),
            help: diag.help,
        });
    }
    let status = match cached.status {
        CachedStatus::Ok => CheckFileStatus::Ok,
        CachedStatus::Warning => CheckFileStatus::Warning,
        CachedStatus::Error => CheckFileStatus::Error,
    };
    let mut text = super::check_cmd::CheckTextOutput::default();
    if want_text {
        text.stdout = cached.stdout_text;
        text.stderr = cached.stderr_text;
    }
    Some(CheckedFile {
        report: CheckFileReport {
            path: path_str.to_string(),
            status,
            diagnostics,
        },
        strict: config.strict,
        text,
    })
}

/// Persist a finished check result. Errors are ignored (cache is advisory);
/// writes are atomic (tmp + rename) so racing invocations stay consistent.
pub(super) fn store(key: &[u8; 32], checked: &CheckedFile, probes: Vec<Probe>) {
    if !enabled() {
        return;
    }
    let cached = CachedCheckResult {
        schema: RESULT_CACHE_SCHEMA,
        status: match checked.report.status {
            CheckFileStatus::Ok => CachedStatus::Ok,
            CheckFileStatus::Warning => CachedStatus::Warning,
            CheckFileStatus::Error => CachedStatus::Error,
        },
        diagnostics: checked
            .report
            .diagnostics
            .iter()
            .map(|diag| CachedDiagnostic {
                source: diag.source.to_string(),
                severity: diag.severity.to_string(),
                code: diag.code.clone(),
                message: diag.message.clone(),
                span: diag.span.map(|span| (span.start, span.end)),
                help: diag.help.clone(),
            })
            .collect(),
        stdout_text: checked.text.stdout.clone(),
        stderr_text: checked.text.stderr.clone(),
        probes,
    };
    let path = cache_path(key);
    let Some(parent) = path.parent() else {
        return;
    };
    if std::fs::create_dir_all(parent).is_err() {
        return;
    }
    let Ok(bytes) = serde_json::to_vec(&cached) else {
        return;
    };
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    if std::fs::write(&tmp, bytes).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
}

fn sha256_hex(content: &str) -> String {
    hex(&Sha256::digest(content.as_bytes()).into())
}

fn hex(bytes: &[u8; 32]) -> String {
    let mut out = String::with_capacity(64);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn result_key_hashes_resolved_host_capability_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("main.harn");
        let source = "pipeline main() {}\n";
        std::fs::write(&file, source).unwrap();
        let manifest = dir.path().join("host-capabilities.json");
        let first_snapshot = r#"{"capabilities":{"first":{"operations":["read"]}}}"#;
        let second_snapshot = r#"{"capabilities":{"second":{"operations":["read"]}}}"#;
        std::fs::write(&manifest, first_snapshot).unwrap();
        let config = CheckConfig {
            host_capabilities_path: Some(manifest.display().to_string()),
            ..CheckConfig::default()
        };

        let first = result_cache_key(
            &file,
            &file.to_string_lossy(),
            source,
            &config,
            Some(first_snapshot),
            false,
            &[],
        );
        std::fs::write(&manifest, second_snapshot).unwrap();
        let same_snapshot = result_cache_key(
            &file,
            &file.to_string_lossy(),
            source,
            &config,
            Some(first_snapshot),
            false,
            &[],
        );
        let second = result_cache_key(
            &file,
            &file.to_string_lossy(),
            source,
            &config,
            Some(second_snapshot),
            false,
            &[],
        );

        assert_eq!(first, same_snapshot);
        assert_ne!(first, second);
    }
}
