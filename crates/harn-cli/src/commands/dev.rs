//! `harn dev --watch` — incremental dev loop driven by per-module
//! **interface fingerprints**.
//!
//! Implements the contract from #1786: a file edit only invalidates
//! dependents when the changed module's public surface — types,
//! signatures, `pub import` re-exports — actually moved. Internal-only
//! edits leave the fingerprint stable and re-check just the edited
//! file.
//!
//! Output modes:
//! - human (default): coloured progress lines on stderr.
//! - `--json`: NDJSON event stream on stdout, one
//!   [`JsonEnvelope`]-wrapped event per line. Event shapes are
//!   registered in [`crate::json_envelope::catalog`] under the `dev`
//!   command.

use std::collections::{BTreeMap, BTreeSet, HashSet, VecDeque};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process;
use std::time::Duration;

use harn_modules::fingerprint::{fingerprint_file, fingerprint_hex, Fingerprint};
use harn_modules::ModuleGraph;
use harn_parser::{DiagnosticSeverity, Parser, TypeChecker, TypeDiagnostic};
use notify::{Event, EventKind, RecursiveMode, Watcher};
use serde::Serialize;
use tokio::sync::mpsc;

use crate::cli::DevArgs;
use crate::json_envelope::{JsonEnvelope, JsonOutput};
use crate::test_runner::run_test_file;

/// Schema version of the `dev --json` event stream. Bump when the
/// `DevEvent` shape changes in a way agents need to detect.
pub const DEV_SCHEMA_VERSION: u32 = 1;

/// Debounce window for filesystem notifications. `notify` typically
/// fires several events per save (atomic-rename editors emit
/// create+remove+modify); coalescing them avoids redundant re-checks.
const DEBOUNCE: Duration = Duration::from_millis(200);

/// Top-level entry point for `harn dev`.
pub(crate) async fn run(args: DevArgs) {
    if !args.watch {
        eprintln!(
            "error: `harn dev` currently requires `--watch`. Pass `--watch` to start the incremental loop."
        );
        process::exit(2);
    }

    let root = match args.root.as_deref() {
        Some(p) => PathBuf::from(p),
        None => std::env::current_dir().unwrap_or_else(|e| {
            eprintln!("error: could not read current directory: {e}");
            process::exit(1);
        }),
    };
    let root = match root.canonicalize() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: could not canonicalize {}: {e}", root.display());
            process::exit(1);
        }
    };

    let mut emitter = Emitter::new(args.json);
    let opts = RunOptions {
        with_tests: args.with_tests,
        test_timeout_ms: args.test_timeout_ms,
    };

    let mut state = DevState::initial_scan(&root);
    emitter.emit(DevEvent::Ready {
        root: root.to_string_lossy().into_owned(),
        modules: state.fingerprints.len(),
        fingerprints: state.fingerprint_hex_map(),
    });

    // Type-check the whole project once on startup so diagnostics surface
    // before the user has typed a key.
    let initial_paths: Vec<PathBuf> = state.fingerprints.keys().cloned().collect();
    process_invalidations(&mut state, &initial_paths, &mut emitter, &opts).await;

    let (tx, mut rx) = mpsc::channel::<()>(1);
    let _watcher = match start_watcher(&root, tx.clone()) {
        Ok(w) => w,
        Err(e) => {
            eprintln!(
                "error: could not start file watcher on {}: {e}",
                root.display()
            );
            process::exit(1);
        }
    };

    if !args.json {
        eprintln!(
            "\x1b[2m[dev] watching {} for .harn changes (ctrl-c to stop)\x1b[0m",
            root.display()
        );
    }

    loop {
        if rx.recv().await.is_none() {
            return;
        }
        tokio::time::sleep(DEBOUNCE).await;
        while rx.try_recv().is_ok() {}

        let changed = state.refresh_filesystem(&root);
        let mut invalidated = BTreeSet::<PathBuf>::new();
        for path in &changed {
            let (event, dependents) = state.apply_change(path);
            if let Some(ev) = event {
                emitter.emit(ev);
            }
            for d in dependents {
                invalidated.insert(d);
            }
        }
        if invalidated.is_empty() {
            continue;
        }
        let ordered: Vec<PathBuf> = invalidated.into_iter().collect();
        process_invalidations(&mut state, &ordered, &mut emitter, &opts).await;
    }
}

struct RunOptions {
    with_tests: bool,
    test_timeout_ms: u64,
}

/// Cached per-watch state. `graph` is rebuilt on every change so import
/// edits (a new `import "./x"` line) immediately participate in
/// invalidation; rebuilding is cheap relative to the user's edit
/// cadence and keeps the dependent-set computation honest.
struct DevState {
    /// Current interface fingerprints, keyed by canonical path.
    fingerprints: BTreeMap<PathBuf, Fingerprint>,
    /// BLAKE3 of the raw source content, keyed by canonical path. Used
    /// to detect any edit (body-only or otherwise) so we know which
    /// modules even need to be re-checked. The fingerprint then
    /// decides whether dependents are invalidated too.
    sources: BTreeMap<PathBuf, [u8; 32]>,
    /// Module graph reflecting the on-disk set of `.harn` files.
    graph: ModuleGraph,
    /// Project root used for human-friendly display paths.
    root: PathBuf,
}

impl DevState {
    fn initial_scan(root: &Path) -> Self {
        let files = scan_harn_files(root);
        let mut state = Self {
            fingerprints: BTreeMap::new(),
            sources: BTreeMap::new(),
            graph: ModuleGraph::default(),
            root: root.to_path_buf(),
        };
        state.rebuild_graph(&files);
        for file in &files {
            let canon = canonical(file);
            if let Some(fp) = fingerprint_file(file) {
                state.fingerprints.insert(canon.clone(), fp);
            }
            if let Some(hash) = read_source_hash(file) {
                state.sources.insert(canon, hash);
            }
        }
        state
    }

    fn display_path(&self, path: &Path) -> String {
        path.strip_prefix(&self.root)
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| path.to_string_lossy().into_owned())
    }

    fn fingerprint_hex_map(&self) -> BTreeMap<String, String> {
        self.fingerprints
            .iter()
            .map(|(p, fp)| (self.display_path(p), fingerprint_hex(fp)))
            .collect()
    }

    fn rebuild_graph(&mut self, files: &[PathBuf]) {
        let owned: Vec<PathBuf> = files.to_vec();
        self.graph = harn_modules::build(&owned);
    }

    /// Walk the filesystem and report the files whose source content
    /// (or existence) has changed since the last scan. Also rebuilds
    /// `self.graph` so import additions/removals propagate.
    fn refresh_filesystem(&mut self, root: &Path) -> Vec<PathBuf> {
        let files = scan_harn_files(root);
        self.rebuild_graph(&files);
        let mut on_disk: HashSet<PathBuf> = HashSet::new();
        let mut changed: Vec<PathBuf> = Vec::new();
        for file in &files {
            let canon = canonical(file);
            on_disk.insert(canon.clone());
            let new_hash = read_source_hash(file);
            let prev_hash = self.sources.get(&canon).copied();
            if prev_hash != new_hash {
                changed.push(canon);
            }
        }
        // Surface deletions so dependents can re-error.
        let deleted: Vec<PathBuf> = self
            .sources
            .keys()
            .filter(|p| !on_disk.contains(*p))
            .cloned()
            .collect();
        for d in deleted {
            changed.push(d);
        }
        changed.sort();
        changed.dedup();
        changed
    }

    /// Apply a single file change to `self.fingerprints` and return the
    /// event to emit (if any) plus the set of paths whose
    /// type-checking is now stale.
    fn apply_change(&mut self, path: &Path) -> (Option<DevEvent>, Vec<PathBuf>) {
        let canon = canonical(path);
        let new_fp = fingerprint_file(&canon);
        let new_hash = read_source_hash(&canon);
        let prev_fp = self.fingerprints.get(&canon).copied();
        match new_hash {
            Some(hash) => {
                self.sources.insert(canon.clone(), hash);
            }
            None => {
                self.sources.remove(&canon);
            }
        }
        match (prev_fp, new_fp) {
            (Some(old), Some(new)) if old == new => {
                // Body-only edit (or non-public surface change): rerun
                // just this module so syntax / private-only type errors
                // surface, but leave dependents alone.
                (None, vec![canon])
            }
            (Some(old), Some(new)) => {
                self.fingerprints.insert(canon.clone(), new);
                let module = self.display_path(&canon);
                let event = DevEvent::FingerprintChanged {
                    module,
                    old: fingerprint_hex(&old),
                    new: fingerprint_hex(&new),
                };
                let mut affected = self.transitive_importers(&canon);
                affected.insert(canon);
                (Some(event), affected.into_iter().collect())
            }
            (None, Some(new)) => {
                self.fingerprints.insert(canon.clone(), new);
                let event = DevEvent::FingerprintChanged {
                    module: self.display_path(&canon),
                    old: String::new(),
                    new: fingerprint_hex(&new),
                };
                (Some(event), vec![canon])
            }
            (Some(old), None) => {
                self.fingerprints.remove(&canon);
                let module = self.display_path(&canon);
                let event = DevEvent::FingerprintChanged {
                    module,
                    old: fingerprint_hex(&old),
                    new: String::new(),
                };
                let affected = self.transitive_importers(&canon);
                (Some(event), affected.into_iter().collect())
            }
            (None, None) => (None, Vec::new()),
        }
    }

    fn transitive_importers(&self, start: &Path) -> BTreeSet<PathBuf> {
        let mut seen: BTreeSet<PathBuf> = BTreeSet::new();
        let mut queue: VecDeque<PathBuf> = VecDeque::new();
        queue.push_back(canonical(start));
        while let Some(path) = queue.pop_front() {
            for importer in self.graph.importers_of(&path) {
                let canon = canonical(&importer);
                if seen.insert(canon.clone()) {
                    queue.push_back(canon);
                }
            }
        }
        seen
    }
}

fn canonical(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn read_source_hash(path: &Path) -> Option<[u8; 32]> {
    let bytes = std::fs::read(path).ok()?;
    Some(blake3::hash(&bytes).into())
}

/// Walk `root` recursively for `.harn` files, skipping the same
/// generated directories (`target/`, `node_modules/`, `.harn/`,
/// `.git/`) that the rest of the toolchain ignores.
fn scan_harn_files(root: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    walk(root, &mut out);
    out.sort();
    out
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if path.is_dir() {
            if matches!(
                name,
                "target" | "node_modules" | ".git" | ".harn" | ".harn-runs" | ".harn-cache"
            ) {
                continue;
            }
            walk(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "harn") {
            out.push(path);
        }
    }
}

fn start_watcher(
    root: &Path,
    tx: mpsc::Sender<()>,
) -> Result<notify::RecommendedWatcher, notify::Error> {
    let mut watcher = notify::recommended_watcher(move |res: Result<Event, _>| {
        if let Ok(event) = res {
            if matches!(
                event.kind,
                EventKind::Modify(_) | EventKind::Create(_) | EventKind::Remove(_)
            ) && event
                .paths
                .iter()
                .any(|p| p.extension().is_some_and(|ext| ext == "harn"))
            {
                let _ = tx.blocking_send(());
            }
        }
    })?;
    watcher.watch(root, RecursiveMode::Recursive)?;
    Ok(watcher)
}

async fn process_invalidations(
    state: &mut DevState,
    paths: &[PathBuf],
    emitter: &mut Emitter,
    opts: &RunOptions,
) {
    if paths.is_empty() {
        return;
    }
    let module_names: Vec<String> = paths.iter().map(|p| state.display_path(p)).collect();
    emitter.emit(DevEvent::Rerun {
        modules: module_names.clone(),
    });

    for path in paths {
        if !path.exists() {
            // Deletion: nothing to type-check for the file itself.
            // Dependents are re-checked separately and will produce
            // unresolved-import diagnostics naturally.
            continue;
        }
        let diags = typecheck_file(&state.graph, path);
        let count = diags.len();
        let serialized = diags.iter().map(serialize_diag).collect();
        emitter.emit(DevEvent::Diagnostics {
            module: state.display_path(path),
            count,
            diagnostics: serialized,
        });

        if opts.with_tests {
            run_module_tests(path, opts.test_timeout_ms, state, emitter).await;
        }
    }
}

async fn run_module_tests(path: &Path, timeout_ms: u64, state: &DevState, emitter: &mut Emitter) {
    let cli_skill_dirs: Vec<PathBuf> = Vec::new();
    let results = match run_test_file(path, None, timeout_ms, None, &cli_skill_dirs).await {
        Ok(r) => r,
        Err(error) => {
            emitter.emit(DevEvent::TestError {
                module: state.display_path(path),
                error,
            });
            return;
        }
    };
    let passed = results.iter().filter(|r| r.passed).count();
    let failed = results.len() - passed;
    let failures = results
        .iter()
        .filter(|r| !r.passed)
        .map(|r| TestFailure {
            name: r.name.clone(),
            error: r.error.clone().unwrap_or_default(),
        })
        .collect();
    emitter.emit(DevEvent::Tests {
        module: state.display_path(path),
        passed,
        failed,
        failures,
    });
}

fn typecheck_file(graph: &ModuleGraph, path: &Path) -> Vec<TypeDiagnostic> {
    let Ok(source) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let mut lexer = harn_lexer::Lexer::new(&source);
    let tokens = match lexer.tokenize() {
        Ok(t) => t,
        Err(e) => return vec![lex_diagnostic(format!("{e}"))],
    };
    let program = match Parser::new(tokens).parse() {
        Ok(p) => p,
        Err(e) => return vec![lex_diagnostic(format!("{e}"))],
    };
    let mut checker = TypeChecker::new();
    if let Some(imported) = graph.imported_names_for_file(path) {
        checker = checker.with_imported_names(imported);
    }
    if let Some(imported) = graph.imported_type_declarations_for_file(path) {
        checker = checker.with_imported_type_decls(imported);
    }
    if let Some(imported) = graph.imported_callable_declarations_for_file(path) {
        checker = checker.with_imported_callable_decls(imported);
    }
    checker.check_with_source(&program, &source)
}

fn lex_diagnostic(message: String) -> TypeDiagnostic {
    TypeDiagnostic {
        code: harn_parser::DiagnosticCode::ParserUnexpectedToken,
        message,
        severity: DiagnosticSeverity::Error,
        span: None,
        help: None,
        related: Vec::new(),
        fix: None,
        details: None,
        repair: None,
    }
}

fn serialize_diag(d: &TypeDiagnostic) -> SerializedDiagnostic {
    SerializedDiagnostic {
        severity: match d.severity {
            DiagnosticSeverity::Error => "error",
            DiagnosticSeverity::Warning => "warning",
        },
        code: d.code.as_str().to_string(),
        message: d.message.clone(),
        line: d.span.map(|s| s.line),
        column: d.span.map(|s| s.column),
    }
}

/// JSON or human-friendly emitter. Owns stdout buffering: NDJSON lines
/// are flushed after every event so a downstream agent reading the
/// pipe sees each event as it happens, not at process exit.
struct Emitter {
    json: bool,
}

impl Emitter {
    fn new(json: bool) -> Self {
        Self { json }
    }

    fn emit(&mut self, event: DevEvent) {
        if self.json {
            let envelope = event.into_envelope();
            let line = serde_json::to_string(&envelope).expect("DevEvent serializes");
            let stdout = std::io::stdout();
            let mut lock = stdout.lock();
            let _ = lock.write_all(line.as_bytes());
            let _ = lock.write_all(b"\n");
            let _ = lock.flush();
        } else {
            self.print_human(&event);
        }
    }

    fn print_human(&self, event: &DevEvent) {
        match event {
            DevEvent::Ready { root, modules, .. } => {
                eprintln!("\x1b[2m[dev] ready: {modules} module(s) under {root}\x1b[0m");
            }
            DevEvent::FingerprintChanged { module, old, new } => {
                let label = match (old.is_empty(), new.is_empty()) {
                    (true, false) => "added",
                    (false, true) => "removed",
                    _ => "interface changed",
                };
                eprintln!("\x1b[33m[dev] {label}: {module}\x1b[0m");
            }
            DevEvent::Rerun { modules } => {
                eprintln!("\x1b[2m[dev] rerunning {} module(s)\x1b[0m", modules.len());
            }
            DevEvent::Diagnostics {
                module,
                count,
                diagnostics,
            } => {
                if *count == 0 {
                    eprintln!("\x1b[32m[dev] {module}: ok\x1b[0m");
                } else {
                    eprintln!("\x1b[31m[dev] {module}: {count} diagnostic(s)\x1b[0m");
                    for diag in diagnostics {
                        let loc = match (diag.line, diag.column) {
                            (Some(l), Some(c)) => format!(":{l}:{c}"),
                            _ => String::new(),
                        };
                        eprintln!(
                            "  {} {}{loc} {}: {}",
                            diag.severity, module, diag.code, diag.message
                        );
                    }
                }
            }
            DevEvent::Tests {
                module,
                passed,
                failed,
                failures,
            } => {
                if *failed == 0 {
                    eprintln!("\x1b[32m[dev] {module}: tests passed ({passed})\x1b[0m");
                } else {
                    eprintln!(
                        "\x1b[31m[dev] {module}: tests {passed} passed, {failed} failed\x1b[0m"
                    );
                    for f in failures {
                        eprintln!("    - {}: {}", f.name, f.error);
                    }
                }
            }
            DevEvent::TestError { module, error } => {
                eprintln!("\x1b[31m[dev] {module}: test runner error: {error}\x1b[0m");
            }
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
enum DevEvent {
    Ready {
        root: String,
        modules: usize,
        fingerprints: BTreeMap<String, String>,
    },
    FingerprintChanged {
        module: String,
        old: String,
        new: String,
    },
    Rerun {
        modules: Vec<String>,
    },
    Diagnostics {
        module: String,
        count: usize,
        diagnostics: Vec<SerializedDiagnostic>,
    },
    Tests {
        module: String,
        passed: usize,
        failed: usize,
        failures: Vec<TestFailure>,
    },
    TestError {
        module: String,
        error: String,
    },
}

#[derive(Debug, Clone, Serialize)]
struct SerializedDiagnostic {
    severity: &'static str,
    code: String,
    message: String,
    line: Option<usize>,
    column: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
struct TestFailure {
    name: String,
    error: String,
}

impl JsonOutput for DevEvent {
    const SCHEMA_VERSION: u32 = DEV_SCHEMA_VERSION;
    type Data = DevEvent;
    fn into_envelope(self) -> JsonEnvelope<Self::Data> {
        JsonEnvelope::ok(DEV_SCHEMA_VERSION, self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn write(dir: &Path, name: &str, contents: &str) -> PathBuf {
        let path = dir.join(name);
        fs::write(&path, contents).unwrap();
        path
    }

    #[test]
    fn refresh_detects_any_source_edit() {
        let tmp = tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let lib = write(
            &root,
            "lib.harn",
            "pub fn add(a: int, b: int) -> int { a + b }\n",
        );
        let mut state = DevState::initial_scan(&root);
        assert_eq!(state.fingerprints.len(), 1);

        // A body-only edit changes the source hash even though the
        // interface fingerprint stays put.
        write(
            &root,
            "lib.harn",
            "pub fn add(a: int, b: int) -> int { const s = a + b; s }\n",
        );
        let changed = state.refresh_filesystem(&root);
        assert_eq!(changed, vec![canonical(&lib)]);
    }

    #[test]
    fn signature_change_invalidates_importers() {
        let tmp = tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let lib = write(
            &root,
            "lib.harn",
            "pub fn add(a: int, b: int) -> int { a + b }\n",
        );
        write(
            &root,
            "user.harn",
            "import { add } from \"./lib\"\npub fn main() { add(1, 2) }\n",
        );

        let mut state = DevState::initial_scan(&root);
        write(
            &root,
            "lib.harn",
            "pub fn add(a: int, b: int, c: int) -> int { a + b + c }\n",
        );

        let _ = state.refresh_filesystem(&root);
        let (event, dependents) = state.apply_change(&lib);
        assert!(matches!(event, Some(DevEvent::FingerprintChanged { .. })));
        let names: Vec<String> = dependents
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert!(names.contains(&"lib.harn".to_string()));
        assert!(names.contains(&"user.harn".to_string()));
    }

    #[test]
    fn body_only_edit_skips_dependents() {
        let tmp = tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let lib = write(
            &root,
            "lib.harn",
            "pub fn add(a: int, b: int) -> int { a + b }\n",
        );
        write(
            &root,
            "user.harn",
            "import { add } from \"./lib\"\npub fn main() { add(1, 2) }\n",
        );

        let mut state = DevState::initial_scan(&root);
        write(
            &root,
            "lib.harn",
            "pub fn add(a: int, b: int) -> int { const s = a + b; s }\n",
        );
        let _ = state.refresh_filesystem(&root);
        let (event, dependents) = state.apply_change(&lib);
        assert!(
            event.is_none(),
            "body edit must not emit fingerprint_changed"
        );
        let names: Vec<String> = dependents
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["lib.harn".to_string()]);
    }

    #[test]
    fn transitive_invalidation_walks_chain() {
        let tmp = tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let leaf = write(&root, "leaf.harn", "pub fn leaf() -> int { 1 }\n");
        write(
            &root,
            "mid.harn",
            "import { leaf } from \"./leaf\"\npub fn mid() -> int { leaf() }\n",
        );
        write(
            &root,
            "top.harn",
            "import { mid } from \"./mid\"\npub fn top() -> int { mid() }\n",
        );

        let mut state = DevState::initial_scan(&root);
        write(&root, "leaf.harn", "pub fn leaf() -> string { \"x\" }\n");
        let _ = state.refresh_filesystem(&root);
        let (_, dependents) = state.apply_change(&leaf);
        let names: Vec<String> = dependents
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert!(names.contains(&"mid.harn".to_string()));
        assert!(names.contains(&"top.harn".to_string()));
    }

    #[test]
    fn scan_skips_generated_dirs() {
        let tmp = tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        write(&root, "real.harn", "pub fn r() {}\n");
        std::fs::create_dir_all(root.join("target/build")).unwrap();
        write(
            &root.join("target/build"),
            "ignored.harn",
            "pub fn i() {}\n",
        );
        let files = scan_harn_files(&root);
        let names: Vec<String> = files
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert!(names.contains(&"real.harn".to_string()));
        assert!(!names.contains(&"ignored.harn".to_string()));
    }

    #[test]
    fn diagnostics_serialize_to_envelope_shape() {
        let event = DevEvent::Diagnostics {
            module: "x.harn".into(),
            count: 0,
            diagnostics: Vec::new(),
        };
        let envelope = event.into_envelope();
        let value = serde_json::to_value(&envelope).unwrap();
        assert_eq!(value["schemaVersion"], DEV_SCHEMA_VERSION);
        assert_eq!(value["ok"], true);
        assert_eq!(value["data"]["event"], "diagnostics");
        assert_eq!(value["data"]["module"], "x.harn");
    }
}
