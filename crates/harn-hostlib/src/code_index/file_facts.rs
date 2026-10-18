//! Per-file facts that need nothing from the index, and the bounded worker
//! pool that extracts them in parallel behind one in-order publisher.
//!
//! A cold build spends most of its time on work that reads one file and
//! nothing else: the read and hash, trigram and word extraction, and the
//! tree-sitter parse with its symbol and call-site sweeps. [`extract`] owns
//! all of it. Everything that reads other files' facts (file ids, posting
//! installation, graph nodes, REFS, import and call resolution) stays with
//! the single caller that publishes the facts, in walk order. Ids, posting
//! order, and graph contents therefore match a serial build exactly,
//! whatever the worker count.

use std::collections::BTreeMap;
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::sync::{Condvar, Mutex};

use crate::ast::{api as ast_api, Language as AstLanguage, Symbol};

use super::file_table::fnv1a64;
use super::imports;
use super::trigram::{extract_trigrams, Trigram};
use super::walker::MAX_FILE_BYTES;

/// Past six workers the single publisher, not extraction, bounds the build
/// (see `docs/src/dev/code-index-parallel-build-measurements.md`).
const DEFAULT_FACT_WORKERS: usize = 6;

/// Threads a build runs besides its fact workers: the publisher and the
/// trigram installer.
const DEDICATED_BUILD_THREADS: usize = 2;

/// [`DEFAULT_FACT_WORKERS`], leaving the publisher and trigram installer a
/// core each on smaller hosts. At least one worker always runs.
pub(super) fn default_workers() -> usize {
    std::thread::available_parallelism()
        .map_or(1, usize::from)
        .saturating_sub(DEDICATED_BUILD_THREADS)
        .clamp(1, DEFAULT_FACT_WORKERS)
}

/// Extracted facts each worker may hold ahead of the publisher. Bounds the
/// queued source-derived data to a fixed number of files instead of the
/// whole workspace.
const QUEUED_FACTS_PER_WORKER: usize = 8;

/// One file the walk offered, with the stat it already performed.
pub(super) struct FileJob {
    pub abs: PathBuf,
    pub rel: String,
    /// The registry language the walk's indexability check found.
    pub language: harn_languages::Language,
    pub size_bytes: u64,
    pub mtime_ms: i64,
    /// Content hash of the row this path already has, so unchanged bytes
    /// under a newer timestamp skip extraction entirely.
    pub known_hash: Option<u64>,
}

pub(super) enum FileFacts {
    /// Unreadable, not UTF-8, or over the size limit.
    Skipped,
    /// The bytes hash the same as the indexed row.
    Unchanged {
        hash: u64,
    },
    Changed(Box<ParsedFile>),
}

/// Everything the publisher installs for one changed file.
pub(super) struct ParsedFile {
    pub hash: u64,
    pub language: String,
    pub imports: Vec<String>,
    pub line_count: u32,
    pub trigrams: Vec<Trigram>,
    /// Distinct identifier tokens with the 1-based line of each occurrence.
    pub words: Vec<(String, Vec<u32>)>,
    /// `None` for files without a tree-sitter grammar.
    pub graph: Option<GraphFacts>,
}

/// One parse's contribution to the typed symbol graph.
pub(crate) struct GraphFacts {
    pub language: AstLanguage,
    /// Empty when the grammar rejected the source; the file then keeps a
    /// Module-only slice rather than failing the build.
    pub symbols: Vec<Symbol>,
    pub call_sites: Vec<(String, u32)>,
}

impl GraphFacts {
    /// Parse once and sweep the same tree for symbols and call sites.
    pub(crate) fn extract(path: &str, language: AstLanguage, source: &str) -> Self {
        let parse_phase = super::timing::Phase::start("graph.parse");
        let parsed = ast_api::parse_with_symbols(source, language);
        drop(parse_phase);
        match parsed {
            Ok((tree, symbols)) => {
                let _phase = super::timing::Phase::start("facts.call_sites");
                let call_sites = super::symbol_graph::extract_call_sites_from_tree(&tree, source);
                Self {
                    language,
                    symbols,
                    call_sites,
                }
            }
            Err(err) => {
                tracing::debug!(
                    "code_index: tree-sitter parse failed for `{path}`: {err}; \
                     symbol graph slice will be Module-only"
                );
                Self {
                    language,
                    symbols: Vec::new(),
                    call_sites: Vec::new(),
                }
            }
        }
    }
}

/// Read `job` and extract its facts. Reads nothing but the file.
pub(super) fn extract(job: &FileJob) -> FileFacts {
    let read_phase = super::timing::Phase::start("facts.read");
    if job.size_bytes > MAX_FILE_BYTES {
        return FileFacts::Skipped;
    }
    let Ok(source) = std::fs::read_to_string(&job.abs) else {
        return FileFacts::Skipped;
    };
    if source.len() > MAX_FILE_BYTES as usize {
        return FileFacts::Skipped;
    }
    let hash = fnv1a64(source.as_bytes());
    drop(read_phase);
    if job.known_hash == Some(hash) {
        return FileFacts::Unchanged { hash };
    }
    FileFacts::Changed(Box::new(parse_source(job, &source, hash)))
}

fn parse_source(job: &FileJob, source: &str, hash: u64) -> ParsedFile {
    let language = job.language.name().to_string();
    let imports = imports::extract_imports(source, &language);
    let line_count = crate::text::count_lines(source.as_bytes()) as u32;
    let trigrams = {
        let _phase = super::timing::Phase::start("facts.trigrams");
        extract_trigrams(source).into_iter().collect()
    };
    let words = {
        let _phase = super::timing::Phase::start("facts.words");
        super::words::occurrence_lines(source)
    };
    let graph = AstLanguage::detect(Path::new(&job.rel), None)
        .map(|language| GraphFacts::extract(&job.rel, language, source));
    ParsedFile {
        hash,
        language,
        imports,
        line_count,
        trigrams,
        words,
        graph,
    }
}

/// Extract every job's facts on up to `workers` threads and hand them to
/// `publish` strictly in job order, on the calling thread.
///
/// Workers stay at most a fixed window ahead of the publisher, so queued
/// facts are bounded however slow publication is. When `publish` breaks,
/// panics, or a worker panics, no further job is claimed or published and
/// every worker exits before this returns or unwinds.
pub(super) fn publish_in_order(
    jobs: &[FileJob],
    workers: usize,
    publish: impl FnMut(&FileJob, FileFacts) -> ControlFlow<()>,
) {
    publish_extracted_in_order(jobs, workers, &extract, publish);
}

fn publish_extracted_in_order<E>(
    jobs: &[FileJob],
    workers: usize,
    extract: &E,
    mut publish: impl FnMut(&FileJob, FileFacts) -> ControlFlow<()>,
) where
    E: Fn(&FileJob) -> FileFacts + Sync,
{
    let workers = workers.min(jobs.len());
    if workers <= 1 {
        for job in jobs {
            if publish(job, extract(job)).is_break() {
                return;
            }
        }
        return;
    }
    let window = Window::new(workers * QUEUED_FACTS_PER_WORKER);
    super::parallel::scope(|scope| {
        // Stops the workers however publication ends, including a panic or
        // a failed spawn, so the scope can join them.
        let _stop = StopOnDrop(&window);
        for index in 0..workers {
            let window = &window;
            scope
                .spawn_named(format!("harn-code-index-facts-{index}"), move || {
                    window.work(jobs, extract);
                })
                .expect("spawn a code-index fact worker");
        }
        for (index, job) in jobs.iter().enumerate() {
            let facts = window.take(index);
            if publish(job, facts).is_break() {
                return;
            }
        }
    });
}

struct Window {
    state: Mutex<WindowState>,
    changed: Condvar,
    capacity: usize,
}

#[derive(Default)]
struct WindowState {
    next_claim: usize,
    /// Jobs the publisher has taken. Claims stay below
    /// `taken + capacity`.
    taken: usize,
    ready: BTreeMap<usize, FileFacts>,
    stopped: bool,
    /// A job whose worker unwound; its facts will never arrive.
    lost: Option<usize>,
}

impl Window {
    fn new(capacity: usize) -> Self {
        Self {
            state: Mutex::default(),
            changed: Condvar::new(),
            capacity,
        }
    }

    fn work(&self, jobs: &[FileJob], extract: &impl Fn(&FileJob) -> FileFacts) {
        let mut panic_flag = PanicFlag {
            window: self,
            claimed: None,
        };
        loop {
            let index = {
                let mut state = self.lock();
                loop {
                    if state.stopped || state.next_claim >= jobs.len() {
                        return;
                    }
                    if state.next_claim < state.taken + self.capacity {
                        break;
                    }
                    state = self.wait(state);
                }
                state.next_claim += 1;
                state.next_claim - 1
            };
            panic_flag.claimed = Some(index);
            let facts = extract(&jobs[index]);
            let mut state = self.lock();
            state.ready.insert(index, facts);
            // The job stays claimed until its facts are in `ready`, so an
            // unwind anywhere before that still marks it lost.
            panic_flag.claimed = None;
            self.changed.notify_all();
        }
    }

    fn take(&self, index: usize) -> FileFacts {
        let _phase = super::timing::Phase::start("build.facts_wait");
        let mut state = self.lock();
        loop {
            if let Some(facts) = state.ready.remove(&index) {
                state.taken = index + 1;
                self.changed.notify_all();
                return facts;
            }
            // Earlier jobs were claimed first and still arrive, so the build
            // publishes exactly the prefix before the failed file.
            assert!(
                state.lost != Some(index) && !(state.stopped && index >= state.next_claim),
                "a code-index fact worker panicked"
            );
            state = self.wait(state);
        }
    }

    fn stop(&self) {
        self.lock().stopped = true;
        self.changed.notify_all();
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, WindowState> {
        // A panicking worker holds no lock across extraction, so the state
        // stays consistent; the panic itself is reported through the flag.
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    fn wait<'a>(
        &self,
        state: std::sync::MutexGuard<'a, WindowState>,
    ) -> std::sync::MutexGuard<'a, WindowState> {
        self.changed
            .wait(state)
            .unwrap_or_else(|poison| poison.into_inner())
    }
}

struct StopOnDrop<'a>(&'a Window);

impl Drop for StopOnDrop<'_> {
    fn drop(&mut self) {
        self.0.stop();
    }
}

/// Wakes the publisher when a worker unwinds, so it fails instead of
/// waiting forever for the job that worker claimed.
struct PanicFlag<'a> {
    window: &'a Window,
    claimed: Option<usize>,
}

impl Drop for PanicFlag<'_> {
    fn drop(&mut self) {
        if std::thread::panicking() {
            let mut state = self.window.lock();
            // Keep the earliest lost job: the publisher reaches it first,
            // and a later panic must not hide it.
            state.lost = match (state.lost, self.claimed) {
                (Some(lost), Some(claimed)) => Some(lost.min(claimed)),
                (lost, claimed) => lost.or(claimed),
            };
            state.stopped = true;
            self.window.changed.notify_all();
        }
    }
}

#[cfg(test)]
#[path = "file_facts_tests.rs"]
mod tests;
