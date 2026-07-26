use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};

use globset::{Glob, GlobSet, GlobSetBuilder};
use ignore::WalkBuilder;
use serde::Serialize;

use crate::stdlib::macros::harn_builtin;
use crate::value::{VmError, VmValue};

use super::{
    bool_option, int_option, reject_retired_long_running_option, resolve_fs_path,
    string_list_option, string_option, u64_option,
};

const DEFAULT_MATCH_BUDGET: usize = 1_000;
const DEFAULT_THREAD_CAP: usize = 8;

#[derive(Clone)]
struct LabeledRoot {
    id: String,
    path: PathBuf,
    denied: bool,
}

#[derive(Clone)]
struct LabeledPattern {
    id: String,
    text: String,
}

#[derive(Clone)]
struct EvidenceOptions {
    max_depth: Option<usize>,
    max_filesize: Option<u64>,
    follow_symlinks: bool,
    include_hidden: bool,
    respect_gitignore: bool,
    case_insensitive: bool,
    include_globs: Vec<String>,
    exclude_globs: Vec<String>,
    max_matches: usize,
    max_matches_per_root: usize,
    threads: usize,
    background: bool,
}

#[derive(Clone, Serialize)]
struct EvidenceHit {
    path: String,
    line: u64,
    column: u64,
    text: String,
}

#[derive(Serialize)]
struct PatternEvidence {
    id: String,
    count: usize,
    truncated: bool,
    hits: Vec<EvidenceHit>,
}

#[derive(Serialize)]
struct RootEvidence {
    id: String,
    status: String,
    complete: bool,
    truncated: bool,
    files_scanned: usize,
    files_skipped: usize,
    match_count: usize,
    error_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    patterns: Vec<PatternEvidence>,
}

#[derive(Serialize)]
struct EvidenceReceipt {
    schema_version: &'static str,
    status: String,
    complete: bool,
    truncated: bool,
    cancelled: bool,
    root_count: usize,
    failed_root_count: usize,
    files_scanned: usize,
    match_count: usize,
    roots: Vec<RootEvidence>,
}

struct IndexedHit {
    pattern: usize,
    hit: EvidenceHit,
}

struct LiteralMatcher {
    buckets: BTreeMap<u8, Vec<usize>>,
    needles: Vec<Vec<u8>>,
    case_insensitive: bool,
}

impl LiteralMatcher {
    fn new(patterns: &[LabeledPattern], case_insensitive: bool) -> Self {
        let needles = patterns
            .iter()
            .map(|pattern| pattern.text.as_bytes().to_vec())
            .collect::<Vec<_>>();
        let mut buckets = BTreeMap::<u8, Vec<usize>>::new();
        for (index, needle) in needles.iter().enumerate() {
            let first = if case_insensitive {
                needle[0].to_ascii_lowercase()
            } else {
                needle[0]
            };
            buckets.entry(first).or_default().push(index);
        }
        Self {
            buckets,
            needles,
            case_insensitive,
        }
    }

    fn candidates(&self, byte: u8) -> &[usize] {
        let first = if self.case_insensitive {
            byte.to_ascii_lowercase()
        } else {
            byte
        };
        self.buckets.get(&first).map_or(&[], Vec::as_slice)
    }

    fn matches_at(&self, pattern: usize, bytes: &[u8], start: usize) -> bool {
        let needle = &self.needles[pattern];
        let Some(candidate) = bytes.get(start..start.saturating_add(needle.len())) else {
            return false;
        };
        if self.case_insensitive {
            candidate.eq_ignore_ascii_case(needle)
        } else {
            candidate == needle
        }
    }
}

fn evidence_error(message: impl Into<String>) -> VmError {
    VmError::Thrown(VmValue::String(arcstr::ArcStr::from(message.into())))
}

fn required_labeled_values(
    value: Option<&VmValue>,
    collection: &str,
    value_key: &str,
) -> Result<Vec<(String, String)>, VmError> {
    let Some(VmValue::List(items)) = value else {
        return Err(evidence_error(format!(
            "find_evidence: {collection} must be a non-empty list"
        )));
    };
    if items.is_empty() {
        return Err(evidence_error(format!(
            "find_evidence: {collection} must be a non-empty list"
        )));
    }
    let mut seen = BTreeSet::new();
    let mut parsed = Vec::with_capacity(items.len());
    for item in items.iter() {
        let Some(row) = item.as_dict() else {
            return Err(evidence_error(format!(
                "find_evidence: every {collection} entry must be a dict"
            )));
        };
        let id = row.get("id").map(VmValue::display).unwrap_or_default();
        let raw_value = row.get(value_key).map(VmValue::display).unwrap_or_default();
        if id.trim().is_empty() {
            return Err(evidence_error(format!(
                "find_evidence: {collection} id must not be empty"
            )));
        }
        if raw_value.is_empty() {
            return Err(evidence_error(format!(
                "find_evidence: {collection} {value_key} must not be empty"
            )));
        }
        if !seen.insert(id.clone()) {
            return Err(evidence_error(format!(
                "find_evidence: duplicate {collection} id `{id}`"
            )));
        }
        parsed.push((id, raw_value));
    }
    Ok(parsed)
}

fn source_excludes() -> Vec<String> {
    [
        ".git/**",
        ".harn-runs/**",
        "dist/**",
        "node_modules/**",
        "target/**",
        "vendor/**",
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}

fn parse_options(args: &[VmValue], root_count: usize) -> Result<EvidenceOptions, VmError> {
    let mut options = EvidenceOptions {
        max_depth: None,
        max_filesize: None,
        follow_symlinks: false,
        include_hidden: false,
        respect_gitignore: true,
        case_insensitive: false,
        include_globs: Vec::new(),
        exclude_globs: Vec::new(),
        max_matches: DEFAULT_MATCH_BUDGET,
        max_matches_per_root: DEFAULT_MATCH_BUDGET,
        threads: root_count.clamp(1, DEFAULT_THREAD_CAP),
        background: false,
    };
    let Some(VmValue::Dict(raw)) = args.get(2) else {
        return Ok(options);
    };
    reject_retired_long_running_option(raw, "find_evidence")?;
    match string_option(raw, "preset").as_deref() {
        Some("source") | Some("sources") => {
            options.exclude_globs = source_excludes();
            options.max_filesize = Some(1_048_576);
        }
        Some("all") => {
            options.include_hidden = true;
            options.respect_gitignore = false;
        }
        Some("default") | None => {}
        Some(other) => {
            return Err(evidence_error(format!(
                "find_evidence: unknown preset `{other}`"
            )));
        }
    }
    options.max_depth = int_option(raw, "max_depth")
        .filter(|value| *value >= 0)
        .and_then(|value| usize::try_from(value).ok());
    options.max_filesize = u64_option(raw, "max_filesize")
        .or_else(|| u64_option(raw, "max_file_size"))
        .or(options.max_filesize);
    options.follow_symlinks = bool_option(raw, "follow_symlinks").unwrap_or(false);
    options.include_hidden = bool_option(raw, "include_hidden")
        .or_else(|| bool_option(raw, "hidden"))
        .unwrap_or(options.include_hidden);
    options.respect_gitignore =
        bool_option(raw, "respect_gitignore").unwrap_or(options.respect_gitignore);
    options.case_insensitive = bool_option(raw, "case_insensitive")
        .unwrap_or_else(|| !bool_option(raw, "case_sensitive").unwrap_or(true));
    options.include_globs = string_list_option(raw, "include")
        .into_iter()
        .chain(string_list_option(raw, "include_globs"))
        .collect();
    options
        .exclude_globs
        .extend(string_list_option(raw, "exclude"));
    options
        .exclude_globs
        .extend(string_list_option(raw, "exclude_globs"));
    options
        .exclude_globs
        .extend(string_list_option(raw, "ignore"));
    options
        .exclude_globs
        .extend(string_list_option(raw, "ignore_globs"));
    if let Some(value) = int_option(raw, "max_matches") {
        options.max_matches = usize::try_from(value.max(1)).unwrap_or(usize::MAX);
    }
    options.max_matches_per_root = int_option(raw, "max_matches_per_root")
        .or_else(|| int_option(raw, "per_root_max_matches"))
        .map_or(options.max_matches, |value| {
            usize::try_from(value.max(1)).unwrap_or(usize::MAX)
        });
    if let Some(value) = int_option(raw, "threads") {
        options.threads = usize::try_from(value.max(1))
            .unwrap_or(usize::MAX)
            .min(root_count.max(1));
    }
    options.background = bool_option(raw, "background").unwrap_or(false);
    Ok(options)
}

fn normalize_glob(pattern: &str) -> String {
    if pattern.contains('/') && !pattern.starts_with("**/") {
        format!("**/{pattern}")
    } else {
        pattern.to_string()
    }
}

fn glob_set(patterns: &[String], label: &str) -> Result<Option<GlobSet>, VmError> {
    if patterns.is_empty() {
        return Ok(None);
    }
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        builder.add(Glob::new(&normalize_glob(pattern)).map_err(|error| {
            evidence_error(format!(
                "find_evidence: invalid {label} glob `{pattern}`: {error}"
            ))
        })?);
    }
    builder
        .build()
        .map(Some)
        .map_err(|error| evidence_error(format!("find_evidence: invalid {label} globs: {error}")))
}

fn walk_builder(root: &Path, options: &EvidenceOptions) -> WalkBuilder {
    let mut walker = WalkBuilder::new(root);
    walker
        .hidden(!options.include_hidden)
        .ignore(options.respect_gitignore)
        .git_ignore(options.respect_gitignore)
        .git_global(options.respect_gitignore)
        .git_exclude(options.respect_gitignore)
        .require_git(false)
        .parents(true)
        .follow_links(options.follow_symlinks)
        .sort_by_file_name(|left, right| left.cmp(right));
    if let Some(depth) = options.max_depth {
        walker.max_depth(Some(depth));
    }
    if let Some(size) = options.max_filesize {
        walker.max_filesize(Some(size));
    }
    walker
}

fn included(
    root: &Path,
    path: &Path,
    include: Option<&GlobSet>,
    exclude: Option<&GlobSet>,
) -> bool {
    let relative = path.strip_prefix(root).unwrap_or(path);
    include.is_none_or(|set| set.is_match(relative))
        && !exclude.is_some_and(|set| set.is_match(relative))
}

fn line_hit(bytes: &[u8], newlines: &[usize], start: usize) -> (u64, u64, String) {
    let line_index = newlines.partition_point(|newline| *newline < start);
    let line_start = line_index
        .checked_sub(1)
        .map_or(0, |index| newlines[index].saturating_add(1));
    let line_end = newlines.get(line_index).copied().unwrap_or(bytes.len());
    let text = String::from_utf8_lossy(&bytes[line_start..line_end])
        .trim_end_matches('\r')
        .to_string();
    (
        u64::try_from(line_index).unwrap_or(u64::MAX - 1) + 1,
        u64::try_from(start.saturating_sub(line_start)).unwrap_or(u64::MAX - 1) + 1,
        text,
    )
}

fn empty_patterns(patterns: &[LabeledPattern]) -> Vec<PatternEvidence> {
    patterns
        .iter()
        .map(|pattern| PatternEvidence {
            id: pattern.id.clone(),
            count: 0,
            truncated: false,
            hits: Vec::new(),
        })
        .collect()
}

fn failed_root(id: String, patterns: &[LabeledPattern], error: &str) -> RootEvidence {
    RootEvidence {
        id,
        status: "failed".to_string(),
        complete: false,
        truncated: false,
        files_scanned: 0,
        files_skipped: 0,
        match_count: 0,
        error_count: 1,
        error: Some(error.to_string()),
        patterns: empty_patterns(patterns),
    }
}

fn scan_root(
    root: &LabeledRoot,
    patterns: &[LabeledPattern],
    matcher: &LiteralMatcher,
    options: &EvidenceOptions,
    include: Option<&GlobSet>,
    exclude: Option<&GlobSet>,
    cancel: &AtomicBool,
) -> RootEvidence {
    if root.denied {
        return failed_root(
            root.id.clone(),
            patterns,
            "root is outside the allowed filesystem scope",
        );
    }
    if !root.path.is_dir() {
        return failed_root(
            root.id.clone(),
            patterns,
            "root is not a readable directory",
        );
    }
    let mut hits = Vec::new();
    let mut files_scanned = 0usize;
    let mut files_skipped = 0usize;
    let mut error_count = 0usize;
    let mut truncated = false;
    let mut cancelled = false;
    'walk: for entry in walk_builder(&root.path, options).build() {
        if cancel.load(Ordering::Acquire) {
            cancelled = true;
            break;
        }
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => {
                error_count += 1;
                continue;
            }
        };
        if !entry.file_type().is_some_and(|kind| kind.is_file()) {
            continue;
        }
        let path = entry.path();
        if !included(&root.path, path, include, exclude) {
            continue;
        }
        if options.follow_symlinks
            && crate::stdlib::sandbox::enforce_fs_path(
                "find_evidence",
                path,
                crate::stdlib::sandbox::FsAccess::Read,
            )
            .is_err()
        {
            error_count += 1;
            continue;
        }
        let Ok(bytes) = std::fs::read(path) else {
            files_skipped += 1;
            error_count += 1;
            continue;
        };
        files_scanned += 1;
        let newlines = bytes
            .iter()
            .enumerate()
            .filter_map(|(index, byte)| (*byte == b'\n').then_some(index))
            .collect::<Vec<_>>();
        let relative = path
            .strip_prefix(&root.path)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");
        for (start, byte) in bytes.iter().copied().enumerate() {
            for &pattern in matcher.candidates(byte) {
                if !matcher.matches_at(pattern, &bytes, start) {
                    continue;
                }
                if hits.len() >= options.max_matches_per_root {
                    truncated = true;
                    break 'walk;
                }
                let (line, column, text) = line_hit(&bytes, &newlines, start);
                hits.push(IndexedHit {
                    pattern,
                    hit: EvidenceHit {
                        path: relative.clone(),
                        line,
                        column,
                        text,
                    },
                });
            }
        }
    }
    hits.sort_by(|left, right| {
        patterns[left.pattern]
            .id
            .cmp(&patterns[right.pattern].id)
            .then_with(|| left.hit.path.cmp(&right.hit.path))
            .then_with(|| left.hit.line.cmp(&right.hit.line))
            .then_with(|| left.hit.column.cmp(&right.hit.column))
    });
    let mut grouped = empty_patterns(patterns);
    grouped.sort_by(|left, right| left.id.cmp(&right.id));
    let pattern_positions = grouped
        .iter()
        .enumerate()
        .map(|(index, pattern)| (pattern.id.clone(), index))
        .collect::<BTreeMap<_, _>>();
    for indexed in hits {
        let pattern_id = &patterns[indexed.pattern].id;
        let position = pattern_positions[pattern_id];
        grouped[position].hits.push(indexed.hit);
    }
    for group in &mut grouped {
        group.count = group.hits.len();
        group.truncated = truncated;
    }
    let match_count = grouped.iter().map(|group| group.count).sum();
    let status = if cancelled {
        "cancelled"
    } else if error_count > 0 {
        "partial"
    } else if truncated {
        "truncated"
    } else {
        "complete"
    };
    RootEvidence {
        id: root.id.clone(),
        status: status.to_string(),
        complete: status == "complete",
        truncated,
        files_scanned,
        files_skipped,
        match_count,
        error_count,
        error: (error_count > 0).then(|| "one or more paths could not be read".to_string()),
        patterns: grouped,
    }
}

fn apply_global_budget(roots: &mut [RootEvidence], max_matches: usize) -> bool {
    let mut remaining = max_matches;
    let mut truncated = false;
    for root in roots {
        for pattern in &mut root.patterns {
            if pattern.hits.len() > remaining {
                pattern.hits.truncate(remaining);
                pattern.truncated = true;
                root.truncated = true;
                root.complete = false;
                if root.status == "complete" {
                    root.status = "truncated".to_string();
                }
                truncated = true;
            }
            remaining = remaining.saturating_sub(pattern.hits.len());
            pattern.count = pattern.hits.len();
        }
        root.match_count = root.patterns.iter().map(|pattern| pattern.count).sum();
    }
    truncated
}

fn search_receipt(
    roots: Vec<LabeledRoot>,
    patterns: Vec<LabeledPattern>,
    options: EvidenceOptions,
    cancel: &AtomicBool,
) -> Result<EvidenceReceipt, VmError> {
    let matcher = LiteralMatcher::new(&patterns, options.case_insensitive);
    let include = glob_set(&options.include_globs, "include")?;
    let exclude = glob_set(&options.exclude_globs, "exclude")?;
    let roots = Arc::new(roots);
    let patterns = Arc::new(patterns);
    let matcher = Arc::new(matcher);
    let next = AtomicUsize::new(0);
    let (sender, receiver) = mpsc::channel();
    std::thread::scope(|scope| {
        for _ in 0..options.threads.min(roots.len()).max(1) {
            let sender = sender.clone();
            let roots = Arc::clone(&roots);
            let patterns = Arc::clone(&patterns);
            let matcher = Arc::clone(&matcher);
            let include = include.clone();
            let exclude = exclude.clone();
            let options = options.clone();
            let next = &next;
            scope.spawn(move || loop {
                let index = next.fetch_add(1, Ordering::AcqRel);
                let Some(root) = roots.get(index) else { break };
                let result = scan_root(
                    root,
                    &patterns,
                    &matcher,
                    &options,
                    include.as_ref(),
                    exclude.as_ref(),
                    cancel,
                );
                if sender.send(result).is_err() {
                    break;
                }
            });
        }
        drop(sender);
    });
    let mut results = receiver.into_iter().collect::<Vec<_>>();
    results.sort_by(|left, right| left.id.cmp(&right.id));
    let globally_truncated = apply_global_budget(&mut results, options.max_matches);
    let cancelled =
        cancel.load(Ordering::Acquire) || results.iter().any(|root| root.status == "cancelled");
    let failed_root_count = results
        .iter()
        .filter(|root| root.status == "failed")
        .count();
    let partial = results
        .iter()
        .any(|root| matches!(root.status.as_str(), "failed" | "partial"));
    let truncated = globally_truncated || results.iter().any(|root| root.truncated);
    let status = if cancelled {
        "cancelled"
    } else if failed_root_count == results.len() {
        "failed"
    } else if partial {
        "partial"
    } else if truncated {
        "truncated"
    } else {
        "complete"
    };
    Ok(EvidenceReceipt {
        schema_version: "harn.fs.evidence.v1",
        status: status.to_string(),
        complete: status == "complete",
        truncated,
        cancelled,
        root_count: results.len(),
        failed_root_count,
        files_scanned: results.iter().map(|root| root.files_scanned).sum(),
        match_count: results.iter().map(|root| root.match_count).sum(),
        roots: results,
    })
}

fn parse_request(
    args: &[VmValue],
) -> Result<(Vec<LabeledRoot>, Vec<LabeledPattern>, EvidenceOptions), VmError> {
    let roots = required_labeled_values(args.first(), "roots", "path")?
        .into_iter()
        .map(|(id, path)| LabeledRoot {
            id,
            path: resolve_fs_path(&path),
            denied: false,
        })
        .collect::<Vec<_>>();
    let patterns = required_labeled_values(args.get(1), "patterns", "text")?
        .into_iter()
        .map(|(id, text)| LabeledPattern { id, text })
        .collect::<Vec<_>>();
    let options = parse_options(args, roots.len())?;
    Ok((roots, patterns, options))
}

#[harn_builtin(
    sig = "find_evidence(roots: list, patterns: list, options?: dict) -> any",
    category = "fs",
    doc = "Search labeled roots once for many labeled literal patterns and return deterministic evidence."
)]
fn find_evidence_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let (mut roots, patterns, options) = parse_request(args)?;
    for root in &mut roots {
        if crate::stdlib::sandbox::enforce_fs_path(
            "find_evidence",
            &root.path,
            crate::stdlib::sandbox::FsAccess::Read,
        )
        .is_err()
        {
            // Keep path-specific authorization failures settled with their root
            // instead of discarding evidence from every other root.
            root.denied = true;
        }
    }
    if options.background {
        let session_id = crate::llm::current_agent_session_id().unwrap_or_default();
        let descriptor = format!(
            "find_evidence across {} roots and {} patterns",
            roots.len(),
            patterns.len()
        );
        let handle = crate::stdlib::long_running::spawn_json_operation(
            "find_evidence",
            descriptor,
            session_id,
            move |cancel| {
                search_receipt(roots, patterns, options, &cancel)
                    .and_then(|receipt| {
                        serde_json::to_value(receipt)
                            .map_err(|error| VmError::Runtime(error.to_string()))
                    })
                    .map_err(|error| error.to_string())
            },
        )
        .map_err(VmError::Runtime)?;
        return Ok(handle.into_vm_value());
    }
    let cancel = AtomicBool::new(false);
    let receipt = search_receipt(roots, patterns, options, &cancel)?;
    let json =
        serde_json::to_value(receipt).map_err(|error| VmError::Runtime(error.to_string()))?;
    Ok(crate::stdlib::json_to_vm_value(&json))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancellation_settles_without_leaking_root_paths() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("file.txt"), "needle\n").unwrap();
        let options = EvidenceOptions {
            max_depth: None,
            max_filesize: None,
            follow_symlinks: false,
            include_hidden: false,
            respect_gitignore: true,
            case_insensitive: false,
            include_globs: Vec::new(),
            exclude_globs: Vec::new(),
            max_matches: 10,
            max_matches_per_root: 10,
            threads: 2,
            background: false,
        };
        let cancel = AtomicBool::new(true);
        let receipt = search_receipt(
            vec![LabeledRoot {
                id: "repo".to_string(),
                path: root.path().to_path_buf(),
                denied: false,
            }],
            vec![LabeledPattern {
                id: "needle".to_string(),
                text: "needle".to_string(),
            }],
            options,
            &cancel,
        )
        .unwrap();
        let json = serde_json::to_string(&receipt).unwrap();
        assert_eq!(receipt.status, "cancelled");
        assert_eq!(receipt.match_count, 0);
        assert!(!json.contains(&root.path().to_string_lossy().to_string()));
    }
}
