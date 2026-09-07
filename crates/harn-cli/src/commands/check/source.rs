use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use harn_parser::{Parser, SNode};

/// A successfully parsed resolved module: the raw source plus its AST.
pub(super) type ParsedModule = Arc<(String, Vec<SNode>)>;

/// Identity of an on-disk file for memo invalidation: `(len, mtime_ns)`.
/// Mirrors the bytecode cache's `file_stat_identity`: a changed length or
/// modification time starts a new memo entry. Virtual `<std>/...` paths have no
/// stat; their content is embedded in the binary and immutable per process,
/// so they memoize under a fixed sentinel identity.
type FileIdentity = (u64, i128);

type ParseMemoKey = (PathBuf, FileIdentity);
type ParseMemo = Mutex<HashMap<ParseMemoKey, Arc<OnceLock<Option<ParsedModule>>>>>;

fn parse_memo() -> &'static ParseMemo {
    static MEMO: OnceLock<ParseMemo> = OnceLock::new();
    MEMO.get_or_init(|| Mutex::new(HashMap::new()))
}

const STDLIB_IDENTITY: FileIdentity = (0, -1);

fn file_stat_identity(path: &Path) -> Option<FileIdentity> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime_ns = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as i128)
        .unwrap_or(-1);
    Some((meta.len(), mtime_ns))
}

/// Parse a resolved module path (real file or `<std>/...` virtual path),
/// memoized process-wide.
///
/// Preflight, mock-host capability collection, import-collision scans, and
/// bundle manifests all recurse over each checked file's import closure. The
/// same core library modules sit on nearly every file's closure, so a
/// whole-tree `harn check` used to re-lex and re-parse the same files once
/// per *importing* file — the single largest cost in a large-tree check.
/// One shared memo turns that `O(files x closure)` into `O(distinct modules)`
/// for the whole process, across every scan and every driver worker thread.
///
/// Unparseable or unreadable modules memoize as `None` (every scan treats
/// them as "skip"; the file's own check still reports its lex/parse errors
/// through the analysis path).
pub(super) fn parse_resolved_module(path: &Path) -> Option<ParsedModule> {
    let identity = if is_stdlib_virtual_path(path) {
        Some(STDLIB_IDENTITY)
    } else {
        file_stat_identity(path)
    };
    let Some(identity) = identity else {
        // No stat (file vanished between resolve and read): parse directly so
        // behavior matches the un-memoized path exactly.
        return parse_module_uncached(path);
    };
    let key = (harn_modules::canonical_path(path), identity);
    let parsed = parse_memo()
        .lock()
        .expect("check parse memo lock poisoned")
        .entry(key)
        .or_default()
        .clone();
    parsed.get_or_init(|| parse_module_uncached(path)).clone()
}

fn parse_module_uncached(path: &Path) -> Option<ParsedModule> {
    let source = harn_modules::read_module_source(path)?;
    let mut lexer = harn_lexer::Lexer::new(&source);
    let tokens = lexer.tokenize().ok()?;
    let mut parser = Parser::new(tokens);
    let program = parser.parse().ok()?;
    Some(Arc::new((source, program)))
}

fn is_stdlib_virtual_path(path: &Path) -> bool {
    path.to_str()
        .is_some_and(|value| value.starts_with("<std>/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aliases_share_a_parse_across_threads_and_edits_invalidate_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("module.harn");
        std::fs::create_dir(dir.path().join("nested")).unwrap();
        std::fs::write(&path, "const answer = 1").unwrap();
        let alias = dir.path().join("nested/../module.harn");
        let barrier = std::sync::Barrier::new(8);
        let parsed = std::thread::scope(|scope| {
            let workers: Vec<_> = (0..8)
                .map(|i| {
                    let path = if i % 2 == 0 { &path } else { &alias };
                    let barrier = &barrier;
                    scope.spawn(move || {
                        barrier.wait();
                        parse_resolved_module(path).unwrap()
                    })
                })
                .collect();
            workers
                .into_iter()
                .map(|w| w.join().unwrap())
                .collect::<Vec<_>>()
        });
        let first = &parsed[0];
        assert!(parsed.iter().all(|shared| Arc::ptr_eq(first, shared)));
        std::fs::write(&path, "const =").unwrap();
        assert!(parse_resolved_module(&path).is_none());
        std::fs::write(&path, "const answer = 222").unwrap();
        let edited = parse_resolved_module(&path).unwrap();
        assert!(!Arc::ptr_eq(first, &edited));
        assert_eq!(edited.0, "const answer = 222");
    }
}
