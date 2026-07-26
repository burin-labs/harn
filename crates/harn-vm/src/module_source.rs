//! Process-wide owner of module source bytes and everything derived from them.
//!
//! One spawn of a large pipeline visits every file in the transitive import
//! graph at least twice: once while folding the entry chunk's cache key, and
//! again while the VM actually loads each module. Every one of those sites
//! needs the same three facts — the text, its content digests, and its import
//! list — and each used to derive them independently, so a single module source
//! was read from disk twice, held in memory three times, and hashed three times
//! per process.
//!
//! [`ModuleSource`] owns those bytes and derives each fact at most once.
//! Instances are memoized by the file's stat identity `(len, mtime_ns)`, so an
//! on-disk edit yields a fresh entry and a stale one is never reused: a
//! long-lived worker still observes edited pipelines exactly as a cold process
//! would. The derived bytes are identical to what independent derivation
//! produced, so cache keys are unchanged.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use sha2::{Digest, Sha256};

/// One module's source text plus the facts callers derive from it.
///
/// Digests are computed lazily because no single caller needs all of them: the
/// import-graph walk folds the raw text, the artifact cache keys on SHA-256,
/// and the prepared-module cache keys on BLAKE3.
#[derive(Debug)]
pub struct ModuleSource {
    text: Arc<str>,
    sha256: OnceLock<[u8; 32]>,
    blake3: OnceLock<[u8; 32]>,
    imports: OnceLock<Vec<Arc<str>>>,
}

impl ModuleSource {
    /// Wrap already-in-memory source text. Used for sources that never came
    /// from a readable path — embedded stdlib modules, `-e` snippets, and
    /// package bytes whose authority is an execution guard rather than the
    /// filesystem.
    pub fn from_text(text: impl Into<Arc<str>>) -> Self {
        Self {
            text: text.into(),
            sha256: OnceLock::new(),
            blake3: OnceLock::new(),
            imports: OnceLock::new(),
        }
    }

    /// The shared source text. Cloning the returned handle shares the bytes
    /// rather than copying them.
    pub(crate) fn text(&self) -> &Arc<str> {
        &self.text
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.text
    }

    /// SHA-256 over the source bytes — the `source_hash` of both entry-chunk
    /// and module-artifact cache keys.
    pub(crate) fn sha256(&self) -> [u8; 32] {
        *self.sha256.get_or_init(|| {
            let mut hasher = Sha256::new();
            hasher.update(self.text.as_bytes());
            hasher.finalize().into()
        })
    }

    /// BLAKE3 over the source bytes — the prepared-module cache key digest.
    pub(crate) fn blake3(&self) -> [u8; 32] {
        *self
            .blake3
            .get_or_init(|| *blake3::hash(self.text.as_bytes()).as_bytes())
    }

    /// User (non-stdlib) import paths mentioned by this source, in source
    /// order. See [`collect_user_imports`].
    pub(crate) fn imports(&self) -> &[Arc<str>] {
        self.imports.get_or_init(|| {
            collect_user_imports(&self.text)
                .into_iter()
                .map(Arc::from)
                .collect()
        })
    }
}

type MemoKey = (PathBuf, u64, i128);
type Memo = Mutex<std::collections::HashMap<MemoKey, Arc<ModuleSource>>>;

fn memo() -> &'static Memo {
    static MEMO: OnceLock<Memo> = OnceLock::new();
    MEMO.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
}

/// Identity of the file version currently on disk. Any change to either
/// component invalidates the memo entry.
///
/// Also the unit the entry-chunk context manifest re-checks, so that the
/// in-process memo and the cross-process manifest agree on what "unchanged"
/// means by construction rather than by convention.
pub(crate) fn stat_identity(path: &Path) -> Option<(u64, i128)> {
    let meta = fs::metadata(path).ok()?;
    let len = meta.len();
    // Nanosecond mtime where available; fall back to coarse seconds.
    let mtime_ns = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as i128)
        .unwrap_or(0);
    Some((len, mtime_ns))
}

/// Stable identity for a module file, memoizing `Path::canonicalize`.
///
/// Relative imports resolve to unnormalized paths, so one file is reached under
/// many spellings in a single graph — `mode/../lib/runtime/./x.harn` and
/// `mode/../lib/host/../runtime/x.harn` name the same bytes. Keying anything by
/// the spelling instead of the file therefore misses constantly on a real
/// pipeline tree. The import-graph walk canonicalizes
/// the same resolved module paths hundreds of times across a cold `from_source`
/// fan-out, and each call is a `realpath(3)` syscall. A successful
/// canonicalization is stable for the process lifetime (the pipeline tree is not
/// moved mid-run), so it is memoized. A *failed* canonicalization (the path does
/// not exist yet) is NOT memoized: a file that later appears — or a symlink that
/// is created — must canonicalize freshly so the folded path key matches what a
/// cold process would produce. This keeps the memo a pure speed optimization with
/// byte-identical output.
pub(crate) fn canonical_identity(path: &Path) -> PathBuf {
    use std::sync::OnceLock;
    static MEMO: OnceLock<std::sync::Mutex<std::collections::HashMap<PathBuf, PathBuf>>> =
        OnceLock::new();
    let memo = MEMO.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()));
    if let Some(hit) = memo.lock().unwrap().get(path).cloned() {
        return hit;
    }
    match path.canonicalize() {
        Ok(canonical) => {
            memo.lock()
                .unwrap()
                .insert(path.to_path_buf(), canonical.clone());
            canonical
        }
        // Unresolved path: fall back to the input, but do not memoize, so a file
        // that appears later canonicalizes correctly on the next walk.
        Err(_) => path.to_path_buf(),
    }
}

/// Read `path`'s module source, reusing the process-wide memo when the file is
/// unchanged since it was last read.
///
/// Entries are keyed by [`canonical_identity`] so every spelling of one file
/// shares one read. Every call re-stats the file, so an edit is picked up
/// exactly as a direct read would pick it up — the memo only removes redundant
/// reads of a file version this process has already seen. I/O errors are never
/// memoized: a transient failure must not become sticky.
pub(crate) fn read(path: &Path) -> io::Result<Arc<ModuleSource>> {
    let path = canonical_identity(path);
    let Some((len, mtime_ns)) = stat_identity(&path) else {
        // No stat (the file vanished between resolve and read): read directly
        // so behavior matches the un-memoized path exactly.
        return Ok(Arc::new(ModuleSource::from_text(fs::read_to_string(
            &path,
        )?)));
    };
    let key = (path.clone(), len, mtime_ns);
    if let Some(hit) = memo().lock().unwrap().get(&key).cloned() {
        return Ok(hit);
    }
    let source = Arc::new(ModuleSource::from_text(fs::read_to_string(&path)?));
    memo().lock().unwrap().insert(key, Arc::clone(&source));
    Ok(source)
}

/// Lightweight regex-free scan that surfaces user imports without paying
/// a full lex+parse. False positives only increase cache churn, never
/// correctness; comments and string literals are skipped so neither a
/// commented-out import nor a `"import …"` value appearing inside an
/// unrelated string gates the hash.
///
/// Comments are skipped in place rather than scrubbed into a rewritten copy
/// first. The entry-chunk cache key runs this over every transitively
/// reachable file — several megabytes of source per spawn — and materializing
/// a comment-free copy cost more than the scan it fed, and more than the
/// SHA-256 the scan's output is folded into.
fn collect_user_imports(source: &str) -> Vec<String> {
    let bytes = source.as_bytes();
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if let Some(end) = comment_end(bytes, i) {
            i = end;
            continue;
        }
        if bytes[i] == b'"' {
            // Skip past any string literal so identifiers inside string
            // values cannot trigger the keyword match below.
            i = string_literal_end(bytes, i).unwrap_or(i + 1);
            continue;
        }
        if !matches_keyword(bytes, i, b"import") {
            i += 1;
            continue;
        }
        // Skip past `import` and any selective `{ ... } from` clause; we
        // only need the source-position of the path string literal.
        let mut j = i + b"import".len();
        let mut depth = 0i32;
        while j < bytes.len() {
            // A comment may sit between the keyword and the path, and a block
            // comment may carry the newline that would otherwise end the
            // clause. Stepping over it whole keeps both cases intact.
            if let Some(end) = comment_end(bytes, j) {
                j = end;
                continue;
            }
            match bytes[j] {
                b'"' => {
                    if let Some((path, end)) = read_string_literal(bytes, j) {
                        if !path.starts_with("std/") {
                            out.push(path);
                        }
                        i = end;
                        break;
                    }
                    j += 1;
                }
                b'{' => {
                    depth += 1;
                    j += 1;
                }
                b'}' => {
                    depth -= 1;
                    j += 1;
                }
                b'\n' if depth == 0 => {
                    // No string literal on this logical line; bail and
                    // continue scanning after the keyword to avoid an
                    // infinite loop.
                    i = j;
                    break;
                }
                _ => j += 1,
            }
        }
        if j >= bytes.len() {
            break;
        }
        if i < j {
            // Defensive: ensure forward progress when the inner loop
            // exited without setting `i`.
            i = j;
        }
    }
    out
}

/// End of the comment starting at `at`, or `None` if none starts there.
///
/// An unterminated block comment runs to end of input, matching how the rest
/// of this scan degrades: it can only hide imports, and a missed import only
/// costs a cache miss.
fn comment_end(bytes: &[u8], at: usize) -> Option<usize> {
    if bytes[at] != b'/' || at + 1 >= bytes.len() {
        return None;
    }
    match bytes[at + 1] {
        b'/' => {
            let mut i = at + 2;
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            // Stop *on* the newline: the import clause below treats it as the
            // end of a logical line.
            Some(i)
        }
        b'*' => {
            let mut i = at + 2;
            while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                i += 1;
            }
            Some((i + 2).min(bytes.len()))
        }
        _ => None,
    }
}

/// End of the string literal starting at `at`, without decoding it.
///
/// [`read_string_literal`] allocates the unescaped value, which the scan
/// throws away everywhere except the import path itself.
fn string_literal_end(bytes: &[u8], at: usize) -> Option<usize> {
    debug_assert_eq!(bytes[at], b'"');
    let mut i = at + 1;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => return Some(i + 1),
            b'\\' if i + 1 < bytes.len() => i += 2,
            b'\\' => return None,
            b'\n' => return None,
            _ => i += 1,
        }
    }
    None
}

fn matches_keyword(bytes: &[u8], at: usize, keyword: &[u8]) -> bool {
    let end = at + keyword.len();
    if end > bytes.len() {
        return false;
    }
    if &bytes[at..end] != keyword {
        return false;
    }
    if at > 0 && is_ident_char(bytes[at - 1]) {
        return false;
    }
    if end < bytes.len() && is_ident_char(bytes[end]) {
        return false;
    }
    true
}

fn is_ident_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn read_string_literal(bytes: &[u8], at: usize) -> Option<(String, usize)> {
    debug_assert_eq!(bytes[at], b'"');
    let mut out = String::new();
    let mut i = at + 1;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => return Some((out, i + 1)),
            b'\\' => {
                if i + 1 >= bytes.len() {
                    return None;
                }
                match bytes[i + 1] {
                    b'"' => out.push('"'),
                    b'\\' => out.push('\\'),
                    b'n' => out.push('\n'),
                    b'r' => out.push('\r'),
                    b't' => out.push('\t'),
                    other => out.push(other as char),
                }
                i += 2;
            }
            b'\n' => return None,
            byte => {
                out.push(byte as char);
                i += 1;
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collect_user_imports_ignores_stdlib_and_comments() {
        let source = r#"
            // import "comment/should/be/ignored"
            import "std/agents"
            import { foo } from "pkg/bar"
            import "./relative/path"
        "#;
        let imports = collect_user_imports(source);
        assert_eq!(imports, vec!["pkg/bar", "./relative/path"]);
    }

    #[test]
    fn import_path_inside_string_literal_is_ignored() {
        let source = r#"
            const payload = "import { foo } from \"./other\""
            import "./real"
        "#;
        assert_eq!(collect_user_imports(source), vec!["./real".to_string()]);
    }

    #[test]
    fn comments_around_and_inside_an_import_clause_are_stepped_over() {
        // The scan skips comments in place rather than scrubbing them out of a
        // rewritten copy first, so every position a comment can occupy has to
        // be handled by the scan itself — including between the keyword and its
        // path, where a block comment can also carry the newline that would
        // otherwise end the clause.
        let source = concat!(
            "/* leading */ import \"./before\"\n",
            "import /* between */ \"./between\"\n",
            "import /* spans\na line */ \"./across\"\n",
            "import \"./trailing\" // after\n",
            "/* import \"./blocked/out\" */\n",
        );
        assert_eq!(
            collect_user_imports(source),
            vec!["./before", "./between", "./across", "./trailing"],
            "a block comment must not hide an import, or expose a commented-out one"
        );
    }

    #[test]
    fn an_escaped_quote_does_not_end_the_string_it_appears_in() {
        // Skipping a string literal no longer decodes it, so the skip has to
        // honour escapes on its own: reading `\"` as the closing quote would
        // leave the scan inside string data and surface the fake import.
        let source = "const s = \"a \\\" import \\\"./fake\\\"\"\nimport \"./real\"\n";
        assert_eq!(collect_user_imports(source), vec!["./real".to_string()]);
    }

    #[test]
    fn an_unterminated_block_comment_hides_the_rest_of_the_file() {
        // Defines the degradation rather than leaving it to chance. Hiding an
        // import can only cost a cache miss; the file does not parse anyway.
        let source = "import \"./seen\"\n/* unterminated\nimport \"./hidden\"\n";
        assert_eq!(collect_user_imports(source), vec!["./seen".to_string()]);
    }

    #[test]
    fn derived_facts_are_computed_once_and_match_direct_derivation() {
        let source = ModuleSource::from_text("import \"./dep\"\npub fn v() -> int { return 1 }\n");
        let text = source.as_str().to_string();

        assert_eq!(source.sha256(), source.sha256());
        assert_eq!(source.blake3(), source.blake3());
        assert_eq!(
            source.sha256(),
            <[u8; 32]>::from(Sha256::digest(text.as_bytes())),
            "the memoized digest must equal a direct SHA-256 of the same bytes"
        );
        assert_eq!(
            source.blake3(),
            *blake3::hash(text.as_bytes()).as_bytes(),
            "the memoized digest must equal a direct BLAKE3 of the same bytes"
        );
        assert_eq!(source.imports(), [Arc::<str>::from("./dep")]);
    }

    #[test]
    fn repeated_reads_of_an_unchanged_file_share_one_allocation() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("module.harn");
        std::fs::write(&path, "import \"./first\"\nimport \"./second\"\n").unwrap();

        let first = read(&path).unwrap();
        let second = read(&path).unwrap();

        assert!(
            Arc::ptr_eq(&first, &second),
            "a memo hit must reuse the source instead of reading and copying it again"
        );
        assert_eq!(first.imports().len(), 2);
    }

    #[test]
    fn every_spelling_of_one_file_shares_a_single_read() {
        // Relative imports resolve to unnormalized paths, so a real pipeline
        // tree reaches one file under many spellings. Keying by the spelling
        // makes the memo miss on nearly every edge of a large graph.
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("lib/runtime")).unwrap();
        std::fs::create_dir_all(tmp.path().join("mode")).unwrap();
        let direct = tmp.path().join("lib/runtime/util.harn");
        std::fs::write(&direct, "pub fn v() -> int { return 1 }\n").unwrap();

        std::fs::create_dir_all(tmp.path().join("lib/host")).unwrap();
        let spellings = [
            tmp.path().join("mode/../lib/runtime/util.harn"),
            tmp.path().join("lib/runtime/./util.harn"),
            tmp.path().join("lib/host/../runtime/util.harn"),
        ];

        let first = read(&direct).unwrap();
        for spelling in &spellings {
            assert!(
                Arc::ptr_eq(&first, &read(spelling).unwrap()),
                "{} names the same file and must share its single read",
                spelling.display()
            );
        }
    }

    #[test]
    fn a_same_length_edit_is_re_read_in_a_warm_process() {
        // The hardest case for a `(len, mtime_ns)` key is an edit that
        // preserves byte length: only the mtime distinguishes the versions.
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("leaf.harn");
        std::fs::write(&path, "pub fn x() -> int { return 111 }\n").unwrap();
        let before = read(&path).unwrap();

        std::fs::write(&path, "pub fn x() -> int { return 222 }\n").unwrap();
        // Push the rewritten file's mtime deterministically into the future
        // instead of sleeping out the coarsest plausible mtime granularity.
        let future = std::fs::metadata(&path).unwrap().modified().unwrap()
            + std::time::Duration::from_secs(10);
        std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(future))
            .unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().len(),
            33,
            "the two versions must be the same byte length for this test to \
             exercise the mtime path"
        );

        let after = read(&path).unwrap();
        assert_ne!(
            before.as_str(),
            after.as_str(),
            "a same-length edit must be re-read rather than served from the memo"
        );
        assert_ne!(before.sha256(), after.sha256());
    }

    #[test]
    fn a_missing_file_reports_the_io_error_without_memoizing_it() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("appears-later.harn");

        let missing = read(&path).unwrap_err();
        assert_eq!(missing.kind(), io::ErrorKind::NotFound);

        std::fs::write(&path, "pub fn v() -> int { return 1 }\n").unwrap();
        assert_eq!(
            read(&path).unwrap().as_str(),
            "pub fn v() -> int { return 1 }\n",
            "a file that appears after a failed read must be readable"
        );
    }
}
