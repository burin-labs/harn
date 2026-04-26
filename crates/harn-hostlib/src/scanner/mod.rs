//! Repo scanner host capability.
//!
//! Ports `Sources/BurinCore/Scanner/CoreRepoScanner.swift` from
//! `burin-labs/burin-code` into Rust: deterministic project-wide file
//! enumeration honoring `.gitignore` and the [`extensions::EXCLUDED_DIRS`]
//! table, symbol extraction, import-derived dependency graph,
//! reference + churn + importance scoring, source/test pairing, folder
//! aggregates, project metadata (language stats + detected test
//! commands + code-pattern hints), sub-project detection, and a
//! token-budgeted text repo map.
//!
//! `scan_project` returns the full [`result::ScanResult`] alongside an
//! opaque `snapshot_token` derived from the canonicalized root path. The
//! result is persisted to `<root>/.harn/hostlib/scanner-snapshot.json` so
//! that `scan_incremental` can diff against it later — without forcing the
//! caller to pass the previous result back over the wire.

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use harn_vm::VmValue;

use crate::error::HostlibError;
use crate::registry::{BuiltinRegistry, HostlibCapability, RegisteredBuiltin, SyncHandler};
use crate::tools::args::{
    build_dict, dict_arg, optional_bool, optional_int, require_string, str_value,
};

mod commands;
mod discover;
mod extensions;
mod folders;
mod imports;
mod result;
mod scoring;
mod snapshot;
mod subproject;
mod symbols;
mod test_mapping;

pub use result::{
    DependencyEdge, FileRecord, FolderRecord, LanguageStat, ProjectMetadata, ScanDelta, ScanResult,
    SubProject, SymbolKind, SymbolRecord,
};

const SCAN_PROJECT_BUILTIN: &str = "hostlib_scanner_scan_project";
const SCAN_INCREMENTAL_BUILTIN: &str = "hostlib_scanner_scan_incremental";

/// Scanner capability handle.
#[derive(Default)]
pub struct ScannerCapability;

impl HostlibCapability for ScannerCapability {
    fn module_name(&self) -> &'static str {
        "scanner"
    }

    fn register_builtins(&self, registry: &mut BuiltinRegistry) {
        let scan_project: SyncHandler = Arc::new(scan_project_handler);
        registry.register(RegisteredBuiltin {
            name: SCAN_PROJECT_BUILTIN,
            module: "scanner",
            method: "scan_project",
            handler: scan_project,
        });
        let scan_incremental: SyncHandler = Arc::new(scan_incremental_handler);
        registry.register(RegisteredBuiltin {
            name: SCAN_INCREMENTAL_BUILTIN,
            module: "scanner",
            method: "scan_incremental",
            handler: scan_incremental,
        });
    }
}

// MARK: - Public Rust API (used by tests + by harn-cli embedders).

/// Tunable knobs accepted by [`scan_project`].
#[derive(Clone, Debug)]
pub struct ScanProjectOptions {
    /// Include hidden (`.`) entries during walking.
    pub include_hidden: bool,
    /// Honor `.gitignore`.
    pub respect_gitignore: bool,
    /// Hard cap on file count (0 = unlimited).
    pub max_files: usize,
    /// Run `git log` to compute churn scores.
    pub include_git_history: bool,
    /// Approximate token budget for the text repo map.
    pub repo_map_token_budget: usize,
}

impl Default for ScanProjectOptions {
    fn default() -> Self {
        Self {
            include_hidden: false,
            respect_gitignore: true,
            max_files: 0,
            include_git_history: true,
            repo_map_token_budget: 1200,
        }
    }
}

/// Run a full scan of `root`, persist a snapshot, and return the result.
pub fn scan_project(root: &Path, opts: ScanProjectOptions) -> ScanResult {
    let canonical = canonicalize(root);
    let discover_opts = discover::DiscoverOptions {
        include_hidden: opts.include_hidden,
        respect_gitignore: opts.respect_gitignore,
    };
    let mut discovered = discover::discover_files(&canonical, discover_opts);
    let truncated = if opts.max_files > 0 && discovered.len() > opts.max_files {
        discovered.truncate(opts.max_files);
        true
    } else {
        false
    };

    let (mut files, mut symbols, mut dependencies) = extract_per_file(&discovered);

    scoring::compute_reference_counts(&mut symbols, &files);

    if opts.include_git_history {
        let churn = scoring::compute_churn_scores(&canonical);
        scoring::apply_churn(&mut files, &churn);
    }
    scoring::compute_importance_scores(&mut symbols, &files);

    test_mapping::map_test_files(&mut files);

    let folder_records = folders::build_folder_records(&files, &symbols);
    let test_commands = commands::detect_test_commands(&canonical);
    let code_patterns = commands::detect_code_patterns(&files, &canonical);
    let project = folders::build_project_metadata(
        &canonical,
        &files,
        test_commands,
        code_patterns,
        now_iso8601(),
    );
    let repo_map = folders::build_repo_map(&symbols, &files, opts.repo_map_token_budget);
    let sub_projects = subproject::detect_subprojects(&canonical, 2);

    sort_for_output(&mut files, &mut symbols, &mut dependencies);

    let token = snapshot::root_to_token(&canonical);
    let result = ScanResult {
        snapshot_token: token,
        truncated,
        project,
        folders: folder_records,
        files,
        symbols,
        dependencies,
        sub_projects,
        repo_map,
    };
    snapshot::save(&canonical, &result);
    result
}

/// Result returned by [`scan_incremental`].
#[derive(Clone, Debug)]
pub struct IncrementalScan {
    /// Refreshed scan result.
    pub result: ScanResult,
    /// Path delta computed against the snapshot.
    pub delta: ScanDelta,
}

/// Refresh the snapshot named by `token`. If the snapshot is missing, the
/// diff is too large (>30%), or `changed_paths` is empty after `>30%` of
/// the workspace mtime-mismatched, falls back to a full rescan.
pub fn scan_incremental(
    token: &str,
    explicit_changed: Option<&[String]>,
    opts: ScanProjectOptions,
) -> IncrementalScan {
    let root = snapshot::token_to_root(token);
    let canonical = canonicalize(&root);

    let cached = snapshot::load(&canonical);
    let cached = match cached {
        Some(c) => c,
        None => {
            let result = scan_project(&canonical, opts);
            return IncrementalScan {
                result,
                delta: ScanDelta {
                    full_rescan: true,
                    ..ScanDelta::default()
                },
            };
        }
    };

    let discover_opts = discover::DiscoverOptions {
        include_hidden: opts.include_hidden,
        respect_gitignore: opts.respect_gitignore,
    };
    let mut current = discover::discover_files(&canonical, discover_opts);
    if opts.max_files > 0 && current.len() > opts.max_files {
        current.truncate(opts.max_files);
    }

    let delta = compute_delta(&current, &cached, explicit_changed);
    let total = current.len();
    let needs_full_rescan =
        total > 0 && (delta.added.len() + delta.modified.len()) * 10 > total * 3;

    if needs_full_rescan {
        let result = scan_project(&canonical, opts);
        return IncrementalScan {
            result,
            delta: ScanDelta {
                full_rescan: true,
                ..delta
            },
        };
    }

    if delta.added.is_empty() && delta.modified.is_empty() && delta.removed.is_empty() {
        return IncrementalScan {
            result: cached,
            delta,
        };
    }

    // Incremental path: rebuild only the touched files, then re-finalize.
    let mut files = cached.files;
    let mut symbols = cached.symbols;
    let mut dependencies = cached.dependencies;

    let removed_set: std::collections::HashSet<&str> =
        delta.removed.iter().map(|s| s.as_str()).collect();
    let touched_set: std::collections::HashSet<&str> = delta
        .added
        .iter()
        .chain(delta.modified.iter())
        .map(|s| s.as_str())
        .collect();

    files.retain(|f| !removed_set.contains(f.relative_path.as_str()));
    symbols.retain(|s| {
        !removed_set.contains(s.file_path.as_str()) && !touched_set.contains(s.file_path.as_str())
    });
    dependencies.retain(|d| {
        !removed_set.contains(d.from_file.as_str()) && !touched_set.contains(d.from_file.as_str())
    });

    let touched_entries: Vec<discover::DiscoveredFile> = current
        .iter()
        .filter(|e| touched_set.contains(e.relative_path.as_str()))
        .cloned()
        .collect();
    let (new_files, new_symbols, new_deps) = extract_per_file(&touched_entries);

    let mut by_path: std::collections::BTreeMap<String, FileRecord> = files
        .into_iter()
        .map(|f| (f.relative_path.clone(), f))
        .collect();
    for new_file in new_files {
        by_path.insert(new_file.relative_path.clone(), new_file);
    }
    let mut files: Vec<FileRecord> = by_path.into_values().collect();
    symbols.extend(new_symbols);
    dependencies.extend(new_deps);

    scoring::compute_reference_counts(&mut symbols, &files);
    if opts.include_git_history {
        let churn = scoring::compute_churn_scores(&canonical);
        scoring::apply_churn(&mut files, &churn);
    }
    scoring::compute_importance_scores(&mut symbols, &files);
    test_mapping::map_test_files(&mut files);

    let folder_records = folders::build_folder_records(&files, &symbols);
    let test_commands = commands::detect_test_commands(&canonical);
    let code_patterns = commands::detect_code_patterns(&files, &canonical);
    let project = folders::build_project_metadata(
        &canonical,
        &files,
        test_commands,
        code_patterns,
        now_iso8601(),
    );
    let repo_map = folders::build_repo_map(&symbols, &files, opts.repo_map_token_budget);
    let sub_projects = subproject::detect_subprojects(&canonical, 2);

    sort_for_output(&mut files, &mut symbols, &mut dependencies);

    let token = snapshot::root_to_token(&canonical);
    let result = ScanResult {
        snapshot_token: token,
        truncated: cached.truncated,
        project,
        folders: folder_records,
        files,
        symbols,
        dependencies,
        sub_projects,
        repo_map,
    };
    snapshot::save(&canonical, &result);
    IncrementalScan { result, delta }
}

// MARK: - Internals

fn canonicalize(root: &Path) -> PathBuf {
    std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf())
}

fn extract_per_file(
    discovered: &[discover::DiscoveredFile],
) -> (Vec<FileRecord>, Vec<SymbolRecord>, Vec<DependencyEdge>) {
    let mut files: Vec<FileRecord> = Vec::with_capacity(discovered.len());
    let mut symbols: Vec<SymbolRecord> = Vec::new();
    let mut dependencies: Vec<DependencyEdge> = Vec::new();

    for entry in discovered {
        let metadata = std::fs::metadata(&entry.absolute_path);
        let size = metadata.as_ref().map(|m| m.len()).unwrap_or(0);
        let modified = metadata
            .as_ref()
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);

        let content = std::fs::read_to_string(&entry.absolute_path).unwrap_or_default();
        if content.is_empty() && size != 0 {
            // Likely a non-utf8 binary; skip symbol/import extraction but still record the file.
        }
        let language = extensions::file_extension(&entry.relative_path);
        let imports = imports::extract_imports(&content, &language);
        let file_symbols = symbols::extract_symbols(&content, &language, &entry.relative_path);
        let line_count = count_lines(&content);

        for imp in &imports {
            dependencies.push(DependencyEdge {
                from_file: entry.relative_path.clone(),
                to_module: imp.clone(),
            });
        }
        symbols.extend(file_symbols);

        files.push(FileRecord {
            id: entry.relative_path.clone(),
            relative_path: entry.relative_path.clone(),
            file_name: extensions::file_name(&entry.relative_path).to_string(),
            language,
            line_count,
            size_bytes: size,
            last_modified_unix_ms: modified,
            imports,
            churn_score: 0.0,
            corresponding_test_file: None,
        });
    }

    (files, symbols, dependencies)
}

fn count_lines(content: &str) -> usize {
    if content.is_empty() {
        return 0;
    }
    let nl = content.bytes().filter(|b| *b == b'\n').count();
    let trailing = content.as_bytes().last() != Some(&b'\n');
    nl + if trailing { 1 } else { 0 }
}

fn sort_for_output(
    files: &mut [FileRecord],
    symbols: &mut [SymbolRecord],
    dependencies: &mut [DependencyEdge],
) {
    files.sort_by(|a, b| a.relative_path.cmp(&b.relative_path));
    symbols.sort_by(|a, b| a.id.cmp(&b.id));
    dependencies.sort_by(|a, b| {
        a.from_file
            .cmp(&b.from_file)
            .then_with(|| a.to_module.cmp(&b.to_module))
    });
}

fn compute_delta(
    current: &[discover::DiscoveredFile],
    cached: &ScanResult,
    explicit_changed: Option<&[String]>,
) -> ScanDelta {
    let cached_files: std::collections::BTreeMap<&str, &FileRecord> = cached
        .files
        .iter()
        .map(|f| (f.relative_path.as_str(), f))
        .collect();
    let current_paths: std::collections::HashSet<&str> =
        current.iter().map(|e| e.relative_path.as_str()).collect();

    let added: Vec<String> = current
        .iter()
        .filter(|e| !cached_files.contains_key(e.relative_path.as_str()))
        .map(|e| e.relative_path.clone())
        .collect();
    let removed: Vec<String> = cached
        .files
        .iter()
        .filter(|f| !current_paths.contains(f.relative_path.as_str()))
        .map(|f| f.relative_path.clone())
        .collect();

    let modified: Vec<String> = if let Some(explicit) = explicit_changed {
        explicit
            .iter()
            .filter(|p| cached_files.contains_key(p.as_str()) && current_paths.contains(p.as_str()))
            .cloned()
            .collect()
    } else {
        let mut out = Vec::new();
        for entry in current {
            if let Some(prev) = cached_files.get(entry.relative_path.as_str()) {
                let mtime = std::fs::metadata(&entry.absolute_path)
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map(|d| d.as_millis() as i64)
                    .unwrap_or(0);
                if mtime > prev.last_modified_unix_ms {
                    out.push(entry.relative_path.clone());
                }
            }
        }
        out
    };

    ScanDelta {
        added,
        modified,
        removed,
        full_rescan: false,
    }
}

fn now_iso8601() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs() as i64;
    let nanos = now.subsec_nanos();
    let (year, month, day, hour, minute, second) = unix_to_civil(secs);
    format!(
        "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{millis:03}Z",
        millis = nanos / 1_000_000
    )
}

/// Convert a unix timestamp (seconds, UTC) to civil date components. Uses
/// Howard Hinnant's algorithm so we don't pull in `chrono` for one
/// formatter.
fn unix_to_civil(secs: i64) -> (i64, u32, u32, u32, u32, u32) {
    let days = secs.div_euclid(86_400);
    let day_secs = secs.rem_euclid(86_400);
    let hour = (day_secs / 3600) as u32;
    let minute = ((day_secs % 3600) / 60) as u32;
    let second = (day_secs % 60) as u32;

    // Days from 1970-01-01.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = if month <= 2 { y + 1 } else { y };
    (year, month, day, hour, minute, second)
}

// MARK: - Builtin handlers (Harn dict ↔ Rust struct).

fn scan_project_handler(args: &[VmValue]) -> Result<VmValue, HostlibError> {
    let raw = dict_arg(SCAN_PROJECT_BUILTIN, args)?;
    let dict = raw.as_ref();
    let root = require_string(SCAN_PROJECT_BUILTIN, dict, "root")?;
    let opts = parse_options(SCAN_PROJECT_BUILTIN, dict)?;
    let result = scan_project(Path::new(&root), opts);
    Ok(scan_result_to_value(&result, None))
}

fn scan_incremental_handler(args: &[VmValue]) -> Result<VmValue, HostlibError> {
    let raw = dict_arg(SCAN_INCREMENTAL_BUILTIN, args)?;
    let dict = raw.as_ref();
    let token = require_string(SCAN_INCREMENTAL_BUILTIN, dict, "snapshot_token")?;
    let opts = parse_options(SCAN_INCREMENTAL_BUILTIN, dict)?;
    let changed = parse_changed_paths(SCAN_INCREMENTAL_BUILTIN, dict)?;
    let scan = scan_incremental(&token, changed.as_deref(), opts);
    Ok(scan_result_to_value(&scan.result, Some(&scan.delta)))
}

fn parse_options(
    builtin: &'static str,
    dict: &std::collections::BTreeMap<String, VmValue>,
) -> Result<ScanProjectOptions, HostlibError> {
    let include_hidden = optional_bool(builtin, dict, "include_hidden", false)?;
    let respect_gitignore = optional_bool(builtin, dict, "respect_gitignore", true)?;
    let max_files = optional_int(builtin, dict, "max_files", 0)?;
    let include_git_history = optional_bool(builtin, dict, "include_git_history", true)?;
    let repo_map_token_budget = optional_int(builtin, dict, "repo_map_token_budget", 1200)?;
    if max_files < 0 {
        return Err(HostlibError::InvalidParameter {
            builtin,
            param: "max_files",
            message: "must be >= 0".to_string(),
        });
    }
    if repo_map_token_budget < 0 {
        return Err(HostlibError::InvalidParameter {
            builtin,
            param: "repo_map_token_budget",
            message: "must be >= 0".to_string(),
        });
    }
    Ok(ScanProjectOptions {
        include_hidden,
        respect_gitignore,
        max_files: max_files as usize,
        include_git_history,
        repo_map_token_budget: repo_map_token_budget as usize,
    })
}

fn parse_changed_paths(
    builtin: &'static str,
    dict: &std::collections::BTreeMap<String, VmValue>,
) -> Result<Option<Vec<String>>, HostlibError> {
    let value = match dict.get("changed_paths") {
        None | Some(VmValue::Nil) => return Ok(None),
        Some(v) => v,
    };
    let list = match value {
        VmValue::List(items) => items,
        other => {
            return Err(HostlibError::InvalidParameter {
                builtin,
                param: "changed_paths",
                message: format!("expected list of strings, got {}", other.type_name()),
            });
        }
    };
    let mut out = Vec::with_capacity(list.len());
    for item in list.iter() {
        match item {
            VmValue::String(s) => out.push(s.to_string()),
            other => {
                return Err(HostlibError::InvalidParameter {
                    builtin,
                    param: "changed_paths",
                    message: format!("non-string entry: {}", other.type_name()),
                });
            }
        }
    }
    Ok(Some(out))
}

fn scan_result_to_value(result: &ScanResult, delta: Option<&ScanDelta>) -> VmValue {
    let mut entries: Vec<(&'static str, VmValue)> = vec![
        ("snapshot_token", str_value(&result.snapshot_token)),
        ("truncated", VmValue::Bool(result.truncated)),
        ("project", project_to_value(&result.project)),
        ("folders", list_of(&result.folders, folder_to_value)),
        ("files", list_of(&result.files, file_to_value)),
        ("symbols", list_of(&result.symbols, symbol_to_value)),
        (
            "dependencies",
            list_of(&result.dependencies, dependency_to_value),
        ),
        (
            "sub_projects",
            list_of(&result.sub_projects, subproject_to_value),
        ),
        ("repo_map", str_value(&result.repo_map)),
    ];
    if let Some(d) = delta {
        entries.push(("delta", delta_to_value(d)));
    }
    build_dict(entries)
}

fn list_of<T>(items: &[T], to_value: fn(&T) -> VmValue) -> VmValue {
    let list: Vec<VmValue> = items.iter().map(to_value).collect();
    VmValue::List(Rc::new(list))
}

fn project_to_value(project: &ProjectMetadata) -> VmValue {
    let test_commands_entries: Vec<(String, VmValue)> = project
        .test_commands
        .iter()
        .map(|(k, v)| (k.clone(), str_value(v)))
        .collect();
    let test_commands_dict = build_dict(test_commands_entries);

    let detected: VmValue = project
        .detected_test_command
        .as_deref()
        .map(str_value)
        .unwrap_or(VmValue::Nil);

    let code_patterns: Vec<VmValue> = project.code_patterns.iter().map(str_value).collect();

    build_dict([
        ("name", str_value(&project.name)),
        ("root_path", str_value(&project.root_path)),
        ("languages", list_of(&project.languages, language_to_value)),
        ("test_commands", test_commands_dict),
        ("detected_test_command", detected),
        ("code_patterns", VmValue::List(Rc::new(code_patterns))),
        ("total_files", VmValue::Int(project.total_files as i64)),
        ("total_lines", VmValue::Int(project.total_lines as i64)),
        ("last_scanned_at", str_value(&project.last_scanned_at)),
    ])
}

fn language_to_value(stat: &LanguageStat) -> VmValue {
    build_dict([
        ("name", str_value(&stat.name)),
        ("file_count", VmValue::Int(stat.file_count as i64)),
        ("line_count", VmValue::Int(stat.line_count as i64)),
        ("percentage", VmValue::Float(stat.percentage)),
    ])
}

fn folder_to_value(folder: &FolderRecord) -> VmValue {
    let names: Vec<VmValue> = folder.key_symbol_names.iter().map(str_value).collect();
    build_dict([
        ("id", str_value(&folder.id)),
        ("relative_path", str_value(&folder.relative_path)),
        ("file_count", VmValue::Int(folder.file_count as i64)),
        ("line_count", VmValue::Int(folder.line_count as i64)),
        ("dominant_language", str_value(&folder.dominant_language)),
        ("key_symbol_names", VmValue::List(Rc::new(names))),
    ])
}

fn file_to_value(file: &FileRecord) -> VmValue {
    let imports: Vec<VmValue> = file.imports.iter().map(str_value).collect();
    let test_pair = file
        .corresponding_test_file
        .as_deref()
        .map(str_value)
        .unwrap_or(VmValue::Nil);
    build_dict([
        ("id", str_value(&file.id)),
        ("relative_path", str_value(&file.relative_path)),
        ("file_name", str_value(&file.file_name)),
        ("language", str_value(&file.language)),
        ("line_count", VmValue::Int(file.line_count as i64)),
        ("size_bytes", VmValue::Int(file.size_bytes as i64)),
        (
            "last_modified_unix_ms",
            VmValue::Int(file.last_modified_unix_ms),
        ),
        ("imports", VmValue::List(Rc::new(imports))),
        ("churn_score", VmValue::Float(file.churn_score)),
        ("corresponding_test_file", test_pair),
    ])
}

fn symbol_to_value(symbol: &SymbolRecord) -> VmValue {
    let container = symbol
        .container
        .as_deref()
        .map(str_value)
        .unwrap_or(VmValue::Nil);
    build_dict([
        ("id", str_value(&symbol.id)),
        ("name", str_value(&symbol.name)),
        ("kind", str_value(symbol.kind.keyword())),
        ("file_path", str_value(&symbol.file_path)),
        ("line", VmValue::Int(symbol.line as i64)),
        ("signature", str_value(&symbol.signature)),
        ("container", container),
        (
            "reference_count",
            VmValue::Int(symbol.reference_count as i64),
        ),
        ("importance_score", VmValue::Float(symbol.importance_score)),
    ])
}

fn dependency_to_value(dep: &DependencyEdge) -> VmValue {
    build_dict([
        ("from_file", str_value(&dep.from_file)),
        ("to_module", str_value(&dep.to_module)),
    ])
}

fn subproject_to_value(sp: &SubProject) -> VmValue {
    build_dict([
        ("path", str_value(&sp.path)),
        ("name", str_value(&sp.name)),
        ("language", str_value(&sp.language)),
        ("project_marker", str_value(&sp.project_marker)),
    ])
}

fn delta_to_value(delta: &ScanDelta) -> VmValue {
    let added: Vec<VmValue> = delta.added.iter().map(str_value).collect();
    let modified: Vec<VmValue> = delta.modified.iter().map(str_value).collect();
    let removed: Vec<VmValue> = delta.removed.iter().map(str_value).collect();
    build_dict([
        ("added", VmValue::List(Rc::new(added))),
        ("modified", VmValue::List(Rc::new(modified))),
        ("removed", VmValue::List(Rc::new(removed))),
        ("full_rescan", VmValue::Bool(delta.full_rescan)),
    ])
}
