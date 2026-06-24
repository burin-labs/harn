//! Line coverage for executed Harn programs.
//!
//! Harn already stores a source line for every emitted instruction
//! (`Chunk::lines`), so line coverage needs no separate debug-info pass: the
//! denominator is the set of distinct non-zero lines a chunk (and its nested
//! function bodies) emit, and the numerator is the subset whose instructions
//! actually ran.
//!
//! ## How it is wired
//!
//! Coverage is opt-in and process-global so it captures every VM isolate a run
//! spins up (imports, parallel branches, spawned agents) without threading a
//! flag through every constructor:
//!
//! * [`begin_session`] flips [`is_enabled`] on and clears the merged report.
//! * Each [`crate::vm::Vm`] checks [`is_enabled`] at construction; when on it
//!   carries its own [`Coverage`] accumulator and records a hit per executed
//!   instruction in the dispatch loop.
//! * On drop a VM folds its accumulator into the global report.
//! * [`end_session`] flips coverage off and returns the merged [`Coverage`].
//!
//! ## File attribution
//!
//! A chunk compiled from an imported module carries its own `source_file`; the
//! entry file's top-level chunk and its same-file function bodies carry `None`.
//! We attribute a `None` chunk to the VM's primary file (the script under
//! execution), and otherwise to the chunk's `source_file`. Nested function
//! chunks inherit their parent's effective file when they carry no
//! `source_file` of their own, so a module's uncalled helpers are still counted
//! against the module — not misattributed to the entry script.
//!
//! Render filters to files that exist on disk, which drops the synthetic paths
//! the embedded stdlib and in-memory `eval` chunks report.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use crate::chunk::Chunk;

static COVERAGE_ON: AtomicBool = AtomicBool::new(false);
static GLOBAL_REPORT: OnceLock<Mutex<Coverage>> = OnceLock::new();

fn global() -> &'static Mutex<Coverage> {
    GLOBAL_REPORT.get_or_init(|| Mutex::new(Coverage::new()))
}

/// True while a coverage session is active. Read once per VM construction and
/// once per executed instruction, so it is a relaxed atomic load — effectively
/// free and branch-predicted "off" when no session is running.
#[inline]
pub fn is_enabled() -> bool {
    COVERAGE_ON.load(Ordering::Relaxed)
}

/// Start a coverage session: clear the merged report and enable recording on
/// every VM constructed until [`end_session`].
pub fn begin_session() {
    {
        let mut report = global().lock().unwrap();
        *report = Coverage::new();
    }
    COVERAGE_ON.store(true, Ordering::SeqCst);
}

/// End the coverage session and return the merged report.
pub fn end_session() -> Coverage {
    COVERAGE_ON.store(false, Ordering::SeqCst);
    let mut report = global().lock().unwrap();
    std::mem::take(&mut *report)
}

/// Build a per-VM accumulator when a session is active, seeding the primary
/// file used to attribute same-file (`source_file: None`) chunks. Returns
/// `None` when coverage is off, so the dispatch-loop hook is a single
/// `Option::is_some` branch on the hot path.
pub(crate) fn for_primary(primary_file: Option<&str>) -> Option<Coverage> {
    if !is_enabled() {
        return None;
    }
    let mut cov = Coverage::new();
    if let Some(file) = primary_file {
        cov.set_primary_file(file);
    }
    Some(cov)
}

/// Fold one VM's accumulator into the global report. Called from `Vm::drop`.
pub(crate) fn merge_into_global(data: Coverage) {
    if data.files.is_empty() {
        return;
    }
    let mut report = global().lock().unwrap();
    report.merge(data);
}

/// Hit/total line sets for a single source file.
#[derive(Debug, Clone, Default)]
struct FileLines {
    /// Every instrumentable (non-zero) line emitted for this file.
    total: BTreeSet<u32>,
    /// The subset that executed.
    hit: BTreeSet<u32>,
}

/// Accumulated line coverage. Used both as a per-VM accumulator and, after
/// merging, as the whole-run report.
#[derive(Debug, Clone, Default)]
pub struct Coverage {
    /// The script under execution; receives lines from chunks that carry no
    /// `source_file` of their own.
    primary_file: Option<Arc<str>>,
    files: BTreeMap<Arc<str>, FileLines>,
    /// Chunk ids whose denominator tree has already been walked (per VM).
    seen: HashSet<u64>,
    /// Resolved effective file per chunk id, so a hit needs no re-walk.
    file_of: HashMap<u64, Arc<str>>,
}

impl Coverage {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Record the VM's primary file (the script passed to `execute`). Only the
    /// first call wins so a nested sub-execution can't clobber it.
    pub(crate) fn set_primary_file(&mut self, file: &str) {
        if self.primary_file.is_none() {
            self.primary_file = Some(Arc::from(file));
        }
    }

    /// Record execution of the instruction at `ip` in `chunk`.
    pub(crate) fn record(&mut self, chunk: &Chunk, ip: usize) {
        let id = chunk.cache_id();
        let file = match self.file_of.get(&id) {
            Some(file) => file.clone(),
            None => {
                let effective = self.effective_file(chunk.source_file.as_deref());
                self.register_tree(chunk, &effective);
                self.file_of.get(&id).cloned().unwrap_or(effective)
            }
        };
        if let Some(&line) = chunk.lines.get(ip) {
            if line != 0 {
                self.files.entry(file).or_default().hit.insert(line);
            }
        }
    }

    /// Resolve the file a `None`-`source_file` chunk belongs to.
    fn effective_file(&self, source_file: Option<&str>) -> Arc<str> {
        match source_file {
            Some(path) => Arc::from(path),
            None => self
                .primary_file
                .clone()
                .unwrap_or_else(|| Arc::from("<unknown>")),
        }
    }

    /// Walk `chunk` and its nested function bodies once, adding every
    /// instrumentable line to the denominator. Idempotent per chunk id.
    fn register_tree(&mut self, chunk: &Chunk, effective: &Arc<str>) {
        let id = chunk.cache_id();
        if !self.seen.insert(id) {
            return;
        }
        self.file_of.insert(id, effective.clone());
        {
            let entry = self.files.entry(effective.clone()).or_default();
            for &line in &chunk.lines {
                if line != 0 {
                    entry.total.insert(line);
                }
            }
        }
        for func in &chunk.functions {
            let child = match func.chunk.source_file.as_deref() {
                Some(path) => Arc::from(path),
                None => effective.clone(),
            };
            self.register_tree(func.chunk.as_ref(), &child);
        }
    }

    fn merge(&mut self, other: Coverage) {
        for (file, lines) in other.files {
            let entry = self.files.entry(file).or_default();
            entry.total.extend(lines.total);
            entry.hit.extend(lines.hit);
        }
    }

    /// Files that exist on disk, in deterministic order. Drops the synthetic
    /// paths embedded-stdlib and in-memory `eval` chunks report.
    fn real_files(&self) -> Vec<(&str, &FileLines)> {
        self.files
            .iter()
            .filter(|(file, _)| Path::new(file.as_ref()).exists())
            .map(|(file, lines)| (file.as_ref(), lines))
            .collect()
    }

    /// `(covered, total)` line counts across all on-disk files.
    pub fn totals(&self) -> (usize, usize) {
        self.real_files()
            .into_iter()
            .fold((0, 0), |(cov, total), (_, lines)| {
                (cov + lines.hit.len(), total + lines.total.len())
            })
    }

    /// Whole-run line coverage percentage (0.0 when there is nothing to cover).
    pub fn percent(&self) -> f64 {
        let (covered, total) = self.totals();
        if total == 0 {
            0.0
        } else {
            covered as f64 / total as f64 * 100.0
        }
    }

    /// True when no on-disk file has any instrumentable line.
    pub fn is_empty(&self) -> bool {
        self.real_files().is_empty()
    }

    /// A human-readable per-file table plus a total line.
    pub fn render_text(&self) -> String {
        let files = self.real_files();
        if files.is_empty() {
            return "No coverage data (no executed source files found on disk).".to_string();
        }
        let name_width = files
            .iter()
            .map(|(file, _)| display_path(file).chars().count())
            .max()
            .unwrap_or(4)
            .clamp(4, 60);
        let mut out = String::new();
        out.push_str(&format!(
            "{:<name_width$}  {:>6}  {:>7}  {:>6}\n",
            "File", "Lines", "Covered", "%"
        ));
        for (file, lines) in &files {
            let total = lines.total.len();
            let covered = lines.hit.len();
            out.push_str(&format!(
                "{:<name_width$}  {:>6}  {:>7}  {:>5.1}\n",
                truncate(&display_path(file), name_width),
                total,
                covered,
                pct(covered, total),
            ));
        }
        let (covered, total) = self.totals();
        out.push_str(&format!(
            "{:<name_width$}  {:>6}  {:>7}  {:>5.1}\n",
            "TOTAL",
            total,
            covered,
            pct(covered, total),
        ));
        out
    }

    /// LCOV `tracefile` output for Codecov / VS Code Coverage Gutters / genhtml.
    pub fn render_lcov(&self) -> String {
        let mut out = String::new();
        for (file, lines) in self.real_files() {
            out.push_str("TN:\n");
            out.push_str(&format!("SF:{file}\n"));
            for &line in &lines.total {
                let count = u8::from(lines.hit.contains(&line));
                out.push_str(&format!("DA:{line},{count}\n"));
            }
            out.push_str(&format!("LF:{}\n", lines.total.len()));
            out.push_str(&format!("LH:{}\n", lines.hit.len()));
            out.push_str("end_of_record\n");
        }
        out
    }
}

fn pct(covered: usize, total: usize) -> f64 {
    if total == 0 {
        0.0
    } else {
        covered as f64 / total as f64 * 100.0
    }
}

/// Show a path relative to the current dir when possible, for compact tables.
fn display_path(file: &str) -> String {
    if let Ok(cwd) = std::env::current_dir() {
        if let Ok(rel) = Path::new(file).strip_prefix(&cwd) {
            return rel.to_string_lossy().into_owned();
        }
    }
    file.to_string()
}

fn truncate(text: &str, width: usize) -> String {
    let count = text.chars().count();
    if count <= width {
        return text.to_string();
    }
    // Keep the tail (the file name) since the leading dirs are the common
    // prefix that carries the least signal.
    let keep = width.saturating_sub(1);
    let tail: String = text.chars().skip(count - keep).collect();
    format!("…{tail}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk::{Chunk, Op};

    fn chunk_with_lines(lines: &[u32]) -> Chunk {
        let mut chunk = Chunk::new();
        for &line in lines {
            chunk.emit(Op::Nil, line);
        }
        chunk
    }

    #[test]
    fn denominator_counts_distinct_nonzero_lines() {
        let chunk = chunk_with_lines(&[1, 1, 2, 0, 3]);
        let mut cov = Coverage::new();
        cov.set_primary_file("/does/not/matter.harn");
        // Register the denominator without executing anything.
        cov.register_tree(&chunk, &Arc::from("/does/not/matter.harn"));
        let lines = cov.files.values().next().unwrap();
        // Lines 1, 2, 3 are instrumentable; the duplicate 1 and the 0 collapse.
        assert_eq!(
            lines.total.iter().copied().collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
        assert!(lines.hit.is_empty());
    }

    #[test]
    fn hits_are_a_subset_of_the_denominator() {
        let chunk = chunk_with_lines(&[10, 11, 12]);
        let mut cov = Coverage::new();
        cov.set_primary_file("/x.harn");
        // Execute the instructions at index 0 and 2 (lines 10 and 12).
        cov.record(&chunk, 0);
        cov.record(&chunk, 2);
        let lines = cov.files.values().next().unwrap();
        assert_eq!(lines.total.len(), 3);
        assert_eq!(lines.hit.iter().copied().collect::<Vec<_>>(), vec![10, 12]);
    }

    #[test]
    fn line_zero_is_not_instrumentable() {
        let chunk = chunk_with_lines(&[0, 5]);
        let mut cov = Coverage::new();
        cov.set_primary_file("/x.harn");
        cov.record(&chunk, 0); // line 0 — synthetic, ignored
        cov.record(&chunk, 1); // line 5 — counted
        let lines = cov.files.values().next().unwrap();
        assert_eq!(lines.total.iter().copied().collect::<Vec<_>>(), vec![5]);
        assert_eq!(lines.hit.iter().copied().collect::<Vec<_>>(), vec![5]);
    }

    #[test]
    fn merge_unions_totals_and_hits() {
        let mut a = Coverage::new();
        a.files.entry(Arc::from("/f.harn")).or_default().total = BTreeSet::from([1, 2, 3]);
        a.files.entry(Arc::from("/f.harn")).or_default().hit = BTreeSet::from([1]);
        let mut b = Coverage::new();
        b.files.entry(Arc::from("/f.harn")).or_default().total = BTreeSet::from([3, 4]);
        b.files.entry(Arc::from("/f.harn")).or_default().hit = BTreeSet::from([4]);
        a.merge(b);
        let lines = &a.files[&Arc::<str>::from("/f.harn")];
        assert_eq!(
            lines.total.iter().copied().collect::<Vec<_>>(),
            vec![1, 2, 3, 4]
        );
        assert_eq!(lines.hit.iter().copied().collect::<Vec<_>>(), vec![1, 4]);
    }

    #[test]
    fn lcov_shapes_da_lines() {
        // Use a real on-disk path so the render filter keeps it.
        let path = std::env::current_exe().unwrap();
        let path_str = path.to_string_lossy().into_owned();
        let mut cov = Coverage::new();
        let arc: Arc<str> = Arc::from(path_str.as_str());
        cov.files.entry(arc.clone()).or_default().total = BTreeSet::from([1, 2]);
        cov.files.entry(arc).or_default().hit = BTreeSet::from([1]);
        let lcov = cov.render_lcov();
        assert!(lcov.contains(&format!("SF:{path_str}")));
        assert!(lcov.contains("DA:1,1"));
        assert!(lcov.contains("DA:2,0"));
        assert!(lcov.contains("LF:2"));
        assert!(lcov.contains("LH:1"));
        assert!(lcov.contains("end_of_record"));
    }
}
