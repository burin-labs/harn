use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use harn_parser::{Parser, SNode};

/// A successfully parsed resolved module: the raw source plus its AST.
pub(super) type ParsedModule = Arc<(String, Vec<SNode>)>;

/// Identity of an on-disk file for memo invalidation: `(len, mtime_ns)`.
/// Mirrors the bytecode cache's `file_stat_identity` — any content change
/// flips at least one component, so long-lived processes (`harn watch`,
/// `harn dev`) never replay a stale parse. Virtual `<std>/...` paths have no
/// stat; their content is embedded in the binary and immutable per process,
/// so they memoize under a fixed sentinel identity.
type FileIdentity = (u64, i128);

type ParseMemoKey = (PathBuf, FileIdentity);
type ParseMemo = Mutex<HashMap<ParseMemoKey, Option<ParsedModule>>>;

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
    let key = (path.to_path_buf(), identity);
    if let Some(hit) = parse_memo()
        .lock()
        .expect("check parse memo lock poisoned")
        .get(&key)
    {
        return hit.clone();
    }
    let parsed = parse_module_uncached(path);
    parse_memo()
        .lock()
        .expect("check parse memo lock poisoned")
        .insert(key, parsed.clone());
    parsed
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
