//! Verification profile store and stale-diagnostic contract primitives.
//!
//! Provides the `verification_profiles_get`, `verification_profiles_set`,
//! `verification_profile_resolve`, `verification_profile_record_run`, and
//! `verification_diagnostic_classify` builtins.
//!
//! A profile record set is a versioned (`schemaVersion: 1`) list of check
//! rows keyed by scope selectors (repo -> dir glob -> language -> task
//! kind), resolved most-specific-wins. Rows are opaque JSON objects: the
//! store reads and updates only the fields it owns (`id`, `scope`,
//! `timings`, `lastRun`) and round-trips every other field untouched, so
//! newer writers can extend rows without breaking older readers. Languages
//! and toolchains are data in the rows — nothing in this module names a
//! language, toolchain, or build system.
//!
//! Persistence rides the hierarchical project metadata store
//! (`metadata.rs`) under the `verification-profiles` namespace, giving the
//! record set the same durability, sharding, and host-fallback behavior as
//! the other cross-run learning namespaces.
//!
//! The stale-diagnostic contract half is pure: a diagnostic envelope
//! `{rung, rowId, at, snapshot}` is classified against the current
//! file->hash map as `bound_fresh` (may feed no-progress detectors,
//! escalation streaks, verifier signatures, and completion gates),
//! `bound_stale` (an edit invalidated the snapshot; suppressed to advisory
//! and marked stale), or `unbound` (advisory-only; must never feed gates).
//! Loop consumers gate on the returned `feedsGates` bit; wiring the
//! detectors onto that bit lives with them, not here.

use std::collections::BTreeMap;

use crate::metadata::{chrono_now_iso, json_to_vm, with_state};
use crate::stdlib::macros::{harn_builtin, VmBuiltinDef};
use crate::value::vm_to_storage_json as vm_to_json;
use crate::value::{VmError, VmValue};

/// Highest record-set major version this runtime can safely mutate.
/// Reads of newer sets still round-trip; writes are rejected so an older
/// runtime cannot silently strip fields a newer schema made load-bearing.
const PROFILE_SCHEMA_VERSION: i64 = 1;

/// Metadata namespace the record set persists under (sibling to the other
/// cross-run learning namespaces).
const PROFILE_NAMESPACE: &str = "verification-profiles";

/// Recent-duration window per timing kind. Percentiles are recomputed over
/// this window on every observation, so a toolchain regime shift (cache
/// warmed, daemon started) is reflected within one window of runs instead
/// of being averaged away forever.
const TIMING_WINDOW_CAP: usize = 64;

pub(crate) const MODULE_BUILTINS: &[&VmBuiltinDef] = &[
    &VERIFICATION_PROFILES_GET_IMPL_DEF,
    &VERIFICATION_PROFILES_SET_IMPL_DEF,
    &VERIFICATION_PROFILE_RESOLVE_IMPL_DEF,
    &VERIFICATION_PROFILE_RECORD_RUN_IMPL_DEF,
    &VERIFICATION_DIAGNOSTIC_CLASSIFY_IMPL_DEF,
];

// -----------------------------------------------------------------------
// Record-set model (pure serde_json layer).
// -----------------------------------------------------------------------

/// Validate a caller-supplied record set and normalize the fields the
/// store owns. Unknown top-level fields pass through untouched.
fn validate_record_set(value: &VmValue) -> Result<BTreeMap<String, serde_json::Value>, VmError> {
    let VmValue::Dict(dict) = value else {
        return Err(VmError::Runtime(
            "verification_profiles_set: record_set must be a dict".to_string(),
        ));
    };
    let mut fields = BTreeMap::new();
    for (key, entry) in dict.iter() {
        fields.insert(key.to_string(), vm_to_json(entry));
    }
    match fields.get("schemaVersion") {
        None => {
            fields.insert(
                "schemaVersion".to_string(),
                serde_json::json!(PROFILE_SCHEMA_VERSION),
            );
        }
        Some(version) => {
            let Some(version) = version.as_i64() else {
                return Err(VmError::Runtime(
                    "verification_profiles_set: schemaVersion must be an int".to_string(),
                ));
            };
            if version < 1 || version > PROFILE_SCHEMA_VERSION {
                return Err(VmError::Runtime(format!(
                    "verification_profiles_set: unsupported schemaVersion {version} \
                     (this runtime writes schemaVersion <= {PROFILE_SCHEMA_VERSION})"
                )));
            }
        }
    }
    match fields.get("rows") {
        None => {
            fields.insert("rows".to_string(), serde_json::json!([]));
        }
        Some(rows) => {
            let Some(rows) = rows.as_array() else {
                return Err(VmError::Runtime(
                    "verification_profiles_set: rows must be a list".to_string(),
                ));
            };
            if rows.iter().any(|row| !row.is_object()) {
                return Err(VmError::Runtime(
                    "verification_profiles_set: every row must be a dict".to_string(),
                ));
            }
        }
    }
    fields.insert(
        "updatedAt".to_string(),
        serde_json::Value::String(chrono_now_iso()),
    );
    Ok(fields)
}

fn record_set_fields_to_vm(fields: &BTreeMap<String, serde_json::Value>) -> VmValue {
    let mut map = BTreeMap::new();
    for (key, value) in fields {
        map.insert(key.clone(), json_to_vm(value));
    }
    VmValue::dict(map)
}

// -----------------------------------------------------------------------
// Scope-selector resolution.
// -----------------------------------------------------------------------

#[derive(Default)]
struct ScopeQuery {
    repo: Option<String>,
    path: Option<String>,
    language: Option<String>,
    task: Option<String>,
}

fn optional_query_string(dict: &crate::value::DictMap, key: &str) -> Option<String> {
    match dict.get(key) {
        Some(VmValue::Nil) | None => None,
        Some(value) => {
            let text = value.display();
            if text.trim().is_empty() {
                None
            } else {
                Some(text)
            }
        }
    }
}

fn scope_query_from_vm(value: Option<&VmValue>) -> ScopeQuery {
    let Some(VmValue::Dict(dict)) = value else {
        return ScopeQuery::default();
    };
    ScopeQuery {
        repo: optional_query_string(dict, "repo"),
        path: optional_query_string(dict, "path"),
        language: optional_query_string(dict, "language"),
        task: optional_query_string(dict, "task"),
    }
}

fn scope_selector<'a>(row: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    row.get("scope")
        .and_then(|scope| scope.get(key))
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|text| !text.is_empty())
}

/// Match one row against a query. `None` means the row does not apply;
/// `Some(specificity)` orders applicable rows.
///
/// Specificity is lexicographic over (task, language, dir, repo): a row
/// constraining a narrower axis always beats a row constraining only
/// broader axes, mirroring the selector hierarchy repo -> dir glob ->
/// language -> task kind. A selector the row specifies must be satisfied
/// by the query — a row scoped to a language never matches a query that
/// does not name that language. Rows with no scope match everything at
/// specificity 0.
fn row_specificity(row: &serde_json::Value, query: &ScopeQuery) -> Option<u32> {
    let mut specificity = 0u32;
    if let Some(repo) = scope_selector(row, "repo") {
        match &query.repo {
            Some(candidate) if candidate == repo => specificity |= 1,
            _ => return None,
        }
    }
    if let Some(dir_glob) = scope_selector(row, "dir") {
        match &query.path {
            Some(path) if harn_glob::match_name(dir_glob, path) => specificity |= 2,
            _ => return None,
        }
    }
    if let Some(language) = scope_selector(row, "language") {
        match &query.language {
            Some(candidate) if candidate.eq_ignore_ascii_case(language) => specificity |= 4,
            _ => return None,
        }
    }
    if let Some(task) = scope_selector(row, "task") {
        match &query.task {
            Some(candidate) if candidate == task => specificity |= 8,
            _ => return None,
        }
    }
    Some(specificity)
}

/// Most-specific matching row; ties keep the earliest row so a record set
/// stays deterministic under append-only edits.
fn resolve_row<'a>(
    rows: &'a [serde_json::Value],
    query: &ScopeQuery,
) -> Option<&'a serde_json::Value> {
    let mut best: Option<(u32, &serde_json::Value)> = None;
    for row in rows {
        let Some(specificity) = row_specificity(row, query) else {
            continue;
        };
        if best.map(|(score, _)| specificity > score).unwrap_or(true) {
            best = Some((specificity, row));
        }
    }
    best.map(|(_, row)| row)
}

// -----------------------------------------------------------------------
// Timing + lastRun updates.
// -----------------------------------------------------------------------

/// Nearest-rank percentile over a sorted window.
fn percentile_ms(sorted: &[i64], percentile: usize) -> i64 {
    if sorted.is_empty() {
        return 0;
    }
    let rank = (percentile * sorted.len()).div_ceil(100).max(1);
    sorted[rank - 1]
}

/// Fold one observed duration into the row's `timings`: append to the
/// bounded recent window for the warm/cold kind, recompute p50/p95/p99
/// from the window, and bump the total sample count. Fields inside
/// `timings` that this store does not own are preserved.
fn update_timings(
    row: &mut serde_json::Map<String, serde_json::Value>,
    duration_ms: i64,
    warm: bool,
) {
    let timings = row
        .entry("timings")
        .or_insert_with(|| serde_json::json!({}));
    if !timings.is_object() {
        *timings = serde_json::json!({});
    }
    let timings = timings.as_object_mut().expect("timings coerced to object");

    let (stats_key, window_key) = if warm {
        ("warmMs", "recentWarmMs")
    } else {
        ("coldMs", "recentColdMs")
    };
    let mut window: Vec<i64> = timings
        .get(window_key)
        .and_then(|value| value.as_array())
        .map(|values| values.iter().filter_map(|value| value.as_i64()).collect())
        .unwrap_or_default();
    window.push(duration_ms);
    if window.len() > TIMING_WINDOW_CAP {
        window.drain(0..window.len() - TIMING_WINDOW_CAP);
    }
    let mut sorted = window.clone();
    sorted.sort_unstable();

    let stats = timings
        .entry(stats_key)
        .or_insert_with(|| serde_json::json!({}));
    if !stats.is_object() {
        *stats = serde_json::json!({});
    }
    let stats = stats.as_object_mut().expect("stats coerced to object");
    stats.insert(
        "p50".to_string(),
        serde_json::json!(percentile_ms(&sorted, 50)),
    );
    stats.insert(
        "p95".to_string(),
        serde_json::json!(percentile_ms(&sorted, 95)),
    );
    stats.insert(
        "p99".to_string(),
        serde_json::json!(percentile_ms(&sorted, 99)),
    );

    timings.insert(window_key.to_string(), serde_json::json!(window));
    let samples = timings
        .get("samples")
        .and_then(|value| value.as_i64())
        .unwrap_or(0);
    timings.insert("samples".to_string(), serde_json::json!(samples + 1));
}

/// Replace the row's `lastRun` with this run's outcome. `lastRun`
/// describes exactly one execution, so it is overwritten wholesale — a
/// merge would let a stale failure signature outlive the run that
/// produced it.
fn update_last_run(
    row: &mut serde_json::Map<String, serde_json::Value>,
    observation: &serde_json::Map<String, serde_json::Value>,
) {
    let mut last_run = serde_json::Map::new();
    let at = observation
        .get("at")
        .and_then(|value| value.as_str())
        .map(ToString::to_string)
        .unwrap_or_else(chrono_now_iso);
    last_run.insert("at".to_string(), serde_json::Value::String(at));
    for key in ["exit", "failureSignature", "snapshot"] {
        if let Some(value) = observation.get(key) {
            if !value.is_null() {
                last_run.insert(key.to_string(), value.clone());
            }
        }
    }
    row.insert("lastRun".to_string(), serde_json::Value::Object(last_run));
}

/// Apply one observation to one row. Timing-only observations leave
/// `lastRun` untouched; outcome-bearing observations (exit, failure
/// signature, or snapshot) replace it.
fn apply_observation(
    row: &mut serde_json::Map<String, serde_json::Value>,
    observation: &serde_json::Map<String, serde_json::Value>,
) {
    if let Some(duration_ms) = observation
        .get("durationMs")
        .and_then(|value| value.as_i64())
    {
        let warm = observation
            .get("warm")
            .and_then(|value| value.as_bool())
            .unwrap_or(true);
        update_timings(row, duration_ms, warm);
    }
    let has_outcome = ["exit", "failureSignature", "snapshot"].iter().any(|key| {
        observation
            .get(*key)
            .map(|value| !value.is_null())
            .unwrap_or(false)
    });
    if has_outcome {
        update_last_run(row, observation);
    }
}

// -----------------------------------------------------------------------
// Stale-diagnostic classification (pure).
// -----------------------------------------------------------------------

/// Classify a diagnostic envelope against the current file->hash map.
///
/// The contract: no diagnostic is authoritative unless it names the
/// snapshot it applies to. A binding needs a rung, a timestamp, and a
/// non-empty file->hash snapshot (`rowId` stays nullable until a profile
/// row exists). Anything less is `unbound` and advisory-only. A bound
/// diagnostic whose snapshot no longer matches the current hashes — a
/// hash changed, or a snapshot file has no current hash at all — is
/// `bound_stale`: suppressed to advisory, never re-presented as a current
/// fact. Only `bound_fresh` diagnostics may feed no-progress detectors,
/// escalation streaks, verifier signatures, or completion gates.
fn classify_diagnostic(
    envelope: &serde_json::Value,
    current_hashes: &serde_json::Value,
) -> serde_json::Value {
    let mut result = serde_json::Map::new();
    let envelope_obj = envelope.as_object();

    let rung = envelope_obj
        .and_then(|obj| obj.get("rung"))
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|text| !text.is_empty());
    let has_timestamp = envelope_obj
        .and_then(|obj| obj.get("at"))
        .map(|value| value.is_string() || value.is_number())
        .unwrap_or(false);
    let snapshot = envelope_obj
        .and_then(|obj| obj.get("snapshot"))
        .and_then(|value| value.as_object())
        .filter(|snapshot| !snapshot.is_empty());

    if let Some(rung) = rung {
        result.insert("rung".to_string(), serde_json::json!(rung));
    }
    if let Some(row_id) = envelope_obj
        .and_then(|obj| obj.get("rowId"))
        .filter(|value| !value.is_null())
    {
        result.insert("rowId".to_string(), row_id.clone());
    }

    let (status, stale_files, reason) = match (rung, has_timestamp, snapshot) {
        (Some(_), true, Some(snapshot)) => {
            let current = current_hashes.as_object();
            let mut stale_files: Vec<String> = snapshot
                .iter()
                .filter(|(file, recorded_hash)| {
                    current.and_then(|map| map.get(*file)) != Some(recorded_hash)
                })
                .map(|(file, _)| file.clone())
                .collect();
            stale_files.sort();
            if stale_files.is_empty() {
                (
                    "bound_fresh",
                    stale_files,
                    "snapshot matches current file hashes".to_string(),
                )
            } else {
                let reason = format!(
                    "snapshot superseded for {} file(s); re-check before trusting",
                    stale_files.len()
                );
                ("bound_stale", stale_files, reason)
            }
        }
        _ => (
            "unbound",
            Vec::new(),
            "diagnostic carries no rung+timestamp+snapshot binding; advisory only".to_string(),
        ),
    };

    result.insert("status".to_string(), serde_json::json!(status));
    result.insert("staleFiles".to_string(), serde_json::json!(stale_files));
    let feeds_gates = status == "bound_fresh";
    result.insert("feedsGates".to_string(), serde_json::json!(feeds_gates));
    result.insert("advisory".to_string(), serde_json::json!(!feeds_gates));
    result.insert("reason".to_string(), serde_json::json!(reason));
    serde_json::Value::Object(result)
}

// -----------------------------------------------------------------------
// Builtins.
// -----------------------------------------------------------------------

fn optional_dir_arg(args: &[VmValue], index: usize) -> String {
    match args.get(index) {
        Some(VmValue::Nil) | None => ".".to_string(),
        Some(value) => {
            let text = value.display();
            if text.trim().is_empty() {
                ".".to_string()
            } else {
                text
            }
        }
    }
}

/// Reads the record set with hierarchical directory resolution, so a
/// record set stored at the repo root is visible from any subdirectory.
#[harn_builtin(
    sig = "verification_profiles_get(dir?: string|nil) -> dict|nil",
    category = "verification"
)]
fn verification_profiles_get_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let dir = optional_dir_arg(args, 0);
    with_state("verification_profiles_get", |st| {
        match st.get_namespace(&dir, PROFILE_NAMESPACE) {
            Some(fields) => Ok(record_set_fields_to_vm(&fields)),
            None => Ok(VmValue::Nil),
        }
    })
}

/// Replaces the stored record set at `dir` (default repo root) and saves.
/// Unknown top-level and row fields are persisted untouched; only
/// `schemaVersion` (defaulted/validated), `rows` (must be a list of
/// dicts), and `updatedAt` (stamped by the store) are normalized.
#[harn_builtin(
    sig = "verification_profiles_set(record_set: dict, dir?: string|nil) -> nil",
    category = "verification"
)]
fn verification_profiles_set_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let record_set = args.first().cloned().unwrap_or(VmValue::Nil);
    let dir = optional_dir_arg(args, 1);
    let fields = validate_record_set(&record_set)?;
    with_state("verification_profiles_set", |st| {
        st.replace_namespace(&dir, PROFILE_NAMESPACE, fields);
        st.save().map_err(VmError::Runtime)?;
        Ok(VmValue::Nil)
    })
}

/// Resolve the most-specific row for a scope query
/// `{repo?, path?, language?, task?}`. Returns the full row (unknown
/// fields included) or nil when no row applies.
#[harn_builtin(
    sig = "verification_profile_resolve(query: dict, dir?: string|nil) -> dict|nil",
    category = "verification"
)]
fn verification_profile_resolve_impl(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let query = scope_query_from_vm(args.first());
    let dir = optional_dir_arg(args, 1);
    with_state("verification_profile_resolve", |st| {
        let Some(fields) = st.get_namespace(&dir, PROFILE_NAMESPACE) else {
            return Ok(VmValue::Nil);
        };
        let Some(rows) = fields.get("rows").and_then(|value| value.as_array()) else {
            return Ok(VmValue::Nil);
        };
        match resolve_row(rows, &query) {
            Some(row) => Ok(json_to_vm(row)),
            None => Ok(VmValue::Nil),
        }
    })
}

/// Fold one run observation
/// `{durationMs?, warm?, at?, exit?, failureSignature?, snapshot?}` into
/// the row with the given id, persist, and return the updated row (nil
/// when no stored row carries that id). The write lands on the directory
/// the record set actually lives at, so updating from a subdirectory
/// never forks a shadowing copy.
#[harn_builtin(
    sig = "verification_profile_record_run(row_id: string, observation: dict, dir?: string|nil) -> dict|nil",
    category = "verification"
)]
fn verification_profile_record_run_impl(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let row_id = args
        .first()
        .map(|value| value.display())
        .unwrap_or_default();
    if row_id.trim().is_empty() {
        return Err(VmError::Runtime(
            "verification_profile_record_run: row_id must not be empty".to_string(),
        ));
    }
    let observation = match args.get(1) {
        Some(VmValue::Dict(_)) => vm_to_json(args.get(1).expect("checked")),
        _ => {
            return Err(VmError::Runtime(
                "verification_profile_record_run: observation must be a dict".to_string(),
            ))
        }
    };
    let observation = observation.as_object().cloned().unwrap_or_default();
    let dir = optional_dir_arg(args, 2);
    with_state("verification_profile_record_run", |st| {
        let origin = st
            .origin_directory(&dir, PROFILE_NAMESPACE, Some("rows"))
            .unwrap_or_else(|| dir.clone());
        let Some(mut fields) = st
            .local_directory(&origin)
            .namespaces
            .get(PROFILE_NAMESPACE)
            .cloned()
        else {
            return Ok(VmValue::Nil);
        };
        let Some(rows) = fields
            .get_mut("rows")
            .and_then(|value| value.as_array_mut())
        else {
            return Ok(VmValue::Nil);
        };
        let mut updated_row = None;
        for row in rows.iter_mut() {
            let Some(row_obj) = row.as_object_mut() else {
                continue;
            };
            let matches = row_obj
                .get("id")
                .and_then(|value| value.as_str())
                .map(|id| id == row_id)
                .unwrap_or(false);
            if matches {
                apply_observation(row_obj, &observation);
                updated_row = Some(row.clone());
                break;
            }
        }
        let Some(updated_row) = updated_row else {
            return Ok(VmValue::Nil);
        };
        fields.insert(
            "updatedAt".to_string(),
            serde_json::Value::String(chrono_now_iso()),
        );
        st.replace_namespace(&origin, PROFILE_NAMESPACE, fields);
        st.save().map_err(VmError::Runtime)?;
        Ok(json_to_vm(&updated_row))
    })
}

/// Classify a diagnostic envelope `{rung, rowId?, at, snapshot}` against
/// the current file->hash map. Returns
/// `{status, staleFiles, feedsGates, advisory, reason, rung?, rowId?}`
/// with status one of `bound_fresh` / `bound_stale` / `unbound`.
#[harn_builtin(
    sig = "verification_diagnostic_classify(envelope: dict|nil, current_hashes: dict) -> dict",
    category = "verification"
)]
fn verification_diagnostic_classify_impl(
    args: &[VmValue],
    _out: &mut String,
) -> Result<VmValue, VmError> {
    let envelope = args
        .first()
        .map(vm_to_json)
        .unwrap_or(serde_json::Value::Null);
    let current_hashes = args
        .get(1)
        .map(vm_to_json)
        .unwrap_or_else(|| serde_json::json!({}));
    Ok(json_to_vm(&classify_diagnostic(&envelope, &current_hashes)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metadata::MetadataState;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEMP_PATH_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn temp_path(name: &str) -> PathBuf {
        let unique = TEMP_PATH_COUNTER.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        std::env::temp_dir().join(format!("harn-verification-{name}-{pid}-{unique}"))
    }

    fn query(path: &str, language: &str, task: &str) -> ScopeQuery {
        let optional = |text: &str| {
            if text.is_empty() {
                None
            } else {
                Some(text.to_string())
            }
        };
        ScopeQuery {
            repo: None,
            path: optional(path),
            language: optional(language),
            task: optional(task),
        }
    }

    // Selector names are synthetic on purpose: the store must resolve a
    // never-before-seen toolchain purely from data rows.
    fn sample_rows() -> Vec<serde_json::Value> {
        vec![
            serde_json::json!({"id": "any", "scope": {}}),
            serde_json::json!({"id": "by-dir", "scope": {"dir": "src/**"}}),
            serde_json::json!({"id": "by-lang", "scope": {"language": "toolchainx"}}),
            serde_json::json!({
                "id": "by-dir-lang",
                "scope": {"dir": "src/**", "language": "toolchainx"}
            }),
            serde_json::json!({
                "id": "by-task",
                "scope": {"language": "toolchainx", "task": "post_edit"}
            }),
        ]
    }

    fn resolved_id(rows: &[serde_json::Value], q: &ScopeQuery) -> Option<String> {
        resolve_row(rows, q)
            .and_then(|row| row.get("id"))
            .and_then(|id| id.as_str())
            .map(ToString::to_string)
    }

    #[test]
    fn resolve_prefers_most_specific_row() {
        let rows = sample_rows();
        // Task kind outranks dir glob + language.
        assert_eq!(
            resolved_id(&rows, &query("src/a.tx", "toolchainx", "post_edit")),
            Some("by-task".to_string())
        );
        // Without a task, dir+language outranks either alone.
        assert_eq!(
            resolved_id(&rows, &query("src/a.tx", "toolchainx", "")),
            Some("by-dir-lang".to_string())
        );
        // Language alone when the path misses the glob.
        assert_eq!(
            resolved_id(&rows, &query("docs/a.tx", "toolchainx", "")),
            Some("by-lang".to_string())
        );
        // Unscoped fallback when nothing narrower applies.
        assert_eq!(
            resolved_id(&rows, &query("docs/readme.md", "otherlang", "")),
            Some("any".to_string())
        );
        // A row's selector must be satisfiable: no language in the query
        // means language-scoped rows are out.
        assert_eq!(
            resolved_id(&rows, &query("src/a.tx", "", "")),
            Some("by-dir".to_string())
        );
        // Case-insensitive language match; exact task match.
        assert_eq!(
            resolved_id(&rows, &query("", "TOOLCHAINX", "")),
            Some("by-lang".to_string())
        );
        assert_eq!(
            resolved_id(&rows, &query("", "toolchainx", "other_task")),
            Some("by-lang".to_string())
        );
    }

    #[test]
    fn resolve_ties_keep_the_earliest_row() {
        let rows = vec![
            serde_json::json!({"id": "first", "scope": {"language": "toolchainx"}}),
            serde_json::json!({"id": "second", "scope": {"language": "toolchainx"}}),
        ];
        assert_eq!(
            resolved_id(&rows, &query("", "toolchainx", "")),
            Some("first".to_string())
        );
    }

    #[test]
    fn record_set_round_trips_unknown_fields_through_disk() {
        let base = temp_path("roundtrip");
        let record_set = VmValue::dict(BTreeMap::from([
            ("schemaVersion".to_string(), VmValue::Int(1)),
            (
                "vendorExt".to_string(),
                VmValue::dict(BTreeMap::from([("keep".to_string(), VmValue::Bool(true))])),
            ),
            (
                "rows".to_string(),
                VmValue::List(std::sync::Arc::new(vec![VmValue::dict(BTreeMap::from([
                    (
                        "id".to_string(),
                        VmValue::String(arcstr::ArcStr::from("r1")),
                    ),
                    (
                        "futureField".to_string(),
                        VmValue::String(arcstr::ArcStr::from("kept")),
                    ),
                ]))])),
            ),
        ]));
        let fields = validate_record_set(&record_set).expect("valid");
        assert_eq!(fields.get("schemaVersion"), Some(&serde_json::json!(1)));
        assert!(fields.contains_key("updatedAt"));

        let mut state = MetadataState::new(&base);
        state.replace_namespace(".", PROFILE_NAMESPACE, fields);
        state.save().expect("save");

        let mut reloaded = MetadataState::new(&base);
        let fields = reloaded
            .get_namespace(".", PROFILE_NAMESPACE)
            .expect("stored");
        assert_eq!(
            fields.get("vendorExt"),
            Some(&serde_json::json!({"keep": true}))
        );
        let rows = fields
            .get("rows")
            .and_then(|value| value.as_array())
            .unwrap();
        assert_eq!(rows[0].get("futureField"), Some(&serde_json::json!("kept")));
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn replace_namespace_drops_keys_absent_from_the_new_set() {
        let base = temp_path("replace");
        let mut state = MetadataState::new(&base);
        state.replace_namespace(
            ".",
            PROFILE_NAMESPACE,
            BTreeMap::from([("stray".into(), serde_json::json!(true))]),
        );
        state.replace_namespace(
            ".",
            PROFILE_NAMESPACE,
            BTreeMap::from([("rows".into(), serde_json::json!([]))]),
        );
        let fields = state.get_namespace(".", PROFILE_NAMESPACE).expect("stored");
        assert!(!fields.contains_key("stray"));
        assert!(fields.contains_key("rows"));
    }

    #[test]
    fn profiles_set_rejects_newer_schema_versions() {
        let record_set = VmValue::dict(BTreeMap::from([(
            "schemaVersion".to_string(),
            VmValue::Int(PROFILE_SCHEMA_VERSION + 1),
        )]));
        let error = validate_record_set(&record_set).expect_err("must reject");
        let VmError::Runtime(message) = error else {
            panic!("unexpected error kind");
        };
        assert!(message.contains("unsupported schemaVersion"));
    }

    #[test]
    fn timing_percentiles_track_the_recent_window() {
        let mut row = serde_json::json!({
            "id": "r1",
            "timings": {"customNote": "kept"}
        });
        let row_obj = row.as_object_mut().unwrap();
        for duration in 1..=100 {
            update_timings(row_obj, duration, true);
        }
        let timings = row_obj
            .get("timings")
            .and_then(|value| value.as_object())
            .unwrap();
        // Window keeps the newest 64 samples: 37..=100.
        let window = timings
            .get("recentWarmMs")
            .and_then(|value| value.as_array())
            .unwrap();
        assert_eq!(window.len(), TIMING_WINDOW_CAP);
        assert_eq!(window.first().and_then(|value| value.as_i64()), Some(37));
        assert_eq!(window.last().and_then(|value| value.as_i64()), Some(100));
        let warm = timings
            .get("warmMs")
            .and_then(|value| value.as_object())
            .unwrap();
        assert_eq!(warm.get("p50"), Some(&serde_json::json!(68)));
        assert_eq!(warm.get("p95"), Some(&serde_json::json!(97)));
        assert_eq!(warm.get("p99"), Some(&serde_json::json!(100)));
        assert_eq!(timings.get("samples"), Some(&serde_json::json!(100)));
        // Cold observations land in their own window + stats.
        update_timings(row_obj, 5000, false);
        let timings = row_obj
            .get("timings")
            .and_then(|value| value.as_object())
            .unwrap();
        let cold = timings
            .get("coldMs")
            .and_then(|value| value.as_object())
            .unwrap();
        assert_eq!(cold.get("p50"), Some(&serde_json::json!(5000)));
        assert_eq!(timings.get("samples"), Some(&serde_json::json!(101)));
        // Fields the store does not own survive every update.
        assert_eq!(timings.get("customNote"), Some(&serde_json::json!("kept")));
    }

    #[test]
    fn observations_update_last_run_only_when_outcome_bearing() {
        let mut row = serde_json::json!({"id": "r1"});
        let row_obj = row.as_object_mut().unwrap();

        let timing_only = serde_json::json!({"durationMs": 120})
            .as_object()
            .cloned()
            .unwrap();
        apply_observation(row_obj, &timing_only);
        assert!(row_obj.get("lastRun").is_none());

        let outcome = serde_json::json!({
            "durationMs": 90,
            "exit": 1,
            "failureSignature": "expected ';' after statement",
            "snapshot": {"src/a.tx": "h1"}
        })
        .as_object()
        .cloned()
        .unwrap();
        apply_observation(row_obj, &outcome);
        let last_run = row_obj
            .get("lastRun")
            .and_then(|value| value.as_object())
            .unwrap();
        assert_eq!(last_run.get("exit"), Some(&serde_json::json!(1)));
        assert_eq!(
            last_run.get("failureSignature"),
            Some(&serde_json::json!("expected ';' after statement"))
        );
        assert_eq!(
            last_run.get("snapshot"),
            Some(&serde_json::json!({"src/a.tx": "h1"}))
        );
        assert!(last_run
            .get("at")
            .and_then(|value| value.as_str())
            .is_some());

        // A later green replaces the failure wholesale — no stale
        // signature may survive the run that fixed it.
        let green = serde_json::json!({"exit": 0, "snapshot": {"src/a.tx": "h2"}})
            .as_object()
            .cloned()
            .unwrap();
        apply_observation(row_obj, &green);
        let last_run = row_obj
            .get("lastRun")
            .and_then(|value| value.as_object())
            .unwrap();
        assert_eq!(last_run.get("exit"), Some(&serde_json::json!(0)));
        assert!(last_run.get("failureSignature").is_none());
    }

    #[test]
    fn classify_bound_fresh_feeds_gates() {
        let envelope = serde_json::json!({
            "rung": "R2",
            "rowId": "r1",
            "at": "2026-07-02T00:00:00Z",
            "snapshot": {"src/a.tx": "h1", "src/b.tx": "h2"}
        });
        let current = serde_json::json!({"src/a.tx": "h1", "src/b.tx": "h2"});
        let result = classify_diagnostic(&envelope, &current);
        assert_eq!(
            result.get("status"),
            Some(&serde_json::json!("bound_fresh"))
        );
        assert_eq!(result.get("feedsGates"), Some(&serde_json::json!(true)));
        assert_eq!(result.get("advisory"), Some(&serde_json::json!(false)));
        assert_eq!(result.get("staleFiles"), Some(&serde_json::json!([])));
        assert_eq!(result.get("rowId"), Some(&serde_json::json!("r1")));
    }

    #[test]
    fn classify_bound_stale_on_hash_change_or_missing_hash() {
        let envelope = serde_json::json!({
            "rung": "R2",
            "at": "2026-07-02T00:00:00Z",
            "snapshot": {"src/a.tx": "h1", "src/b.tx": "h2"}
        });
        // Edited file: recorded hash superseded.
        let result = classify_diagnostic(
            &envelope,
            &serde_json::json!({"src/a.tx": "h9", "src/b.tx": "h2"}),
        );
        assert_eq!(
            result.get("status"),
            Some(&serde_json::json!("bound_stale"))
        );
        assert_eq!(result.get("feedsGates"), Some(&serde_json::json!(false)));
        assert_eq!(result.get("advisory"), Some(&serde_json::json!(true)));
        assert_eq!(
            result.get("staleFiles"),
            Some(&serde_json::json!(["src/a.tx"]))
        );
        // A snapshot file with no current hash cannot be confirmed fresh.
        let result = classify_diagnostic(&envelope, &serde_json::json!({"src/a.tx": "h1"}));
        assert_eq!(
            result.get("status"),
            Some(&serde_json::json!("bound_stale"))
        );
        assert_eq!(
            result.get("staleFiles"),
            Some(&serde_json::json!(["src/b.tx"]))
        );
    }

    #[test]
    fn classify_unbound_diagnostics_never_feed_gates() {
        let current = serde_json::json!({"src/a.tx": "h1"});
        let unbound_cases = [
            serde_json::Value::Null,
            serde_json::json!({}),
            // Missing snapshot.
            serde_json::json!({"rung": "R2", "at": "2026-07-02T00:00:00Z"}),
            // Empty snapshot binds nothing.
            serde_json::json!({"rung": "R2", "at": "2026-07-02T00:00:00Z", "snapshot": {}}),
            // Missing rung.
            serde_json::json!({"at": "2026-07-02T00:00:00Z", "snapshot": {"src/a.tx": "h1"}}),
            // Missing timestamp.
            serde_json::json!({"rung": "R2", "snapshot": {"src/a.tx": "h1"}}),
        ];
        for envelope in unbound_cases {
            let result = classify_diagnostic(&envelope, &current);
            assert_eq!(
                result.get("status"),
                Some(&serde_json::json!("unbound")),
                "envelope: {envelope}"
            );
            assert_eq!(result.get("feedsGates"), Some(&serde_json::json!(false)));
            assert_eq!(result.get("advisory"), Some(&serde_json::json!(true)));
        }
    }
}
