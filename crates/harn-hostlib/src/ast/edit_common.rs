//! Helpers shared by the AST-precise edit builtins (`apply_node`,
//! `insert_at_anchor`, …). Each is small on its own, but every edit
//! primitive needs them and copy-pasting them per file makes the wire
//! contract (post-edit validation, staged-fs routing, response shape)
//! easy to drift out of sync.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use harn_vm::VmValue;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Node, Query, QueryCursor, QueryError, QueryErrorKind, Tree};

use crate::error::HostlibError;
use crate::tools::args::{build_dict, str_value};

use super::health::{ParserHealth, ParserOperation};
use super::language::{Language, TEXT_PATCH_FALLBACK};
use super::parse::parse_bytes;

/// A matched, replaceable byte span plus its 0-based row/col coordinates
/// and the original text it covers. Shared by every query-driven edit
/// primitive (`apply_node`, `batch_apply`, …) so the wire coordinate
/// convention and the splice arithmetic live in exactly one place.
#[derive(Debug, Clone)]
pub(super) struct Span {
    pub(super) start_byte: usize,
    pub(super) end_byte: usize,
    pub(super) start_row: usize,
    pub(super) start_col: usize,
    pub(super) end_row: usize,
    pub(super) end_col: usize,
    pub(super) original: String,
}

/// Multi-match selector shared by the edit primitives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Selector {
    /// Require exactly one match; otherwise [`SelectFailure::Ambiguous`].
    Unique,
    /// Apply to the first match in document order.
    First,
    /// Apply to every match.
    All,
    /// Apply to the nth (1-based) match in document order.
    Nth(usize),
}

impl Selector {
    /// Parse the `select` / `nth` request fields of `dict`.
    pub(super) fn from_request(
        builtin: &'static str,
        dict: &harn_vm::value::DictMap,
    ) -> Result<Self, HostlibError> {
        let raw = crate::tools::args::optional_string(builtin, dict, "select")?;
        let nth = match dict.get("nth") {
            None | Some(VmValue::Nil) => None,
            Some(VmValue::Int(n)) => Some(*n),
            Some(other) => {
                return Err(HostlibError::InvalidParameter {
                    builtin,
                    param: "nth",
                    message: format!("expected integer, got {}", other.type_name()),
                });
            }
        };
        Self::parse(builtin, raw.as_deref(), nth)
    }

    /// Parse a `select` / `nth` request, attributing shape errors to
    /// `builtin`. Bad selectors are caller bugs, so they surface as
    /// [`HostlibError::InvalidParameter`] rather than an edit outcome.
    pub(super) fn parse(
        builtin: &'static str,
        raw: Option<&str>,
        nth: Option<i64>,
    ) -> Result<Self, HostlibError> {
        match raw.unwrap_or("unique") {
            "unique" => Ok(Self::Unique),
            "first" => Ok(Self::First),
            "all" => Ok(Self::All),
            "nth" => {
                let n = nth.ok_or(HostlibError::InvalidParameter {
                    builtin,
                    param: "nth",
                    message: "`select: \"nth\"` requires a positive `nth` (1-based)".into(),
                })?;
                if n < 1 {
                    return Err(HostlibError::InvalidParameter {
                        builtin,
                        param: "nth",
                        message: format!("`nth` must be >= 1, got {n}"),
                    });
                }
                Ok(Self::Nth(n as usize))
            }
            other => Err(HostlibError::InvalidParameter {
                builtin,
                param: "select",
                message: format!(
                    "expected one of [\"unique\", \"first\", \"all\", \"nth\"], got `{other}`"
                ),
            }),
        }
    }
}

/// Why [`select_spans`] could not pick a span set. Each caller maps this
/// to its own wire response (the response shape stays per-builtin; the
/// selection policy does not).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SelectFailure {
    /// `select: "unique"` but more than one span matched.
    Ambiguous,
    /// `select: "nth"` requested an index past the matched span count.
    NthOutOfRange { requested: usize },
}

/// Apply `selector` to the document-ordered `spans`. Returns the chosen
/// subset, or a [`SelectFailure`] the caller renders into a response.
pub(super) fn select_spans(spans: &[Span], selector: Selector) -> Result<Vec<Span>, SelectFailure> {
    match selector {
        Selector::Unique => {
            if spans.len() > 1 {
                Err(SelectFailure::Ambiguous)
            } else {
                Ok(spans.to_vec())
            }
        }
        Selector::First => Ok(spans.first().cloned().into_iter().collect()),
        Selector::All => Ok(spans.to_vec()),
        Selector::Nth(n) => match spans.get(n - 1) {
            Some(span) => Ok(vec![span.clone()]),
            None => Err(SelectFailure::NthOutOfRange { requested: n }),
        },
    }
}

/// Collect every node captured by `target_index`, deduplicated by byte
/// range and sorted in document order. Distinct query matches that land
/// on the same node collapse to a single span.
pub(super) fn collect_target_spans(
    query: &Query,
    tree: &Tree,
    source_bytes: &[u8],
    target_index: u32,
) -> Vec<Span> {
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(query, tree.root_node(), source_bytes);
    let mut seen: BTreeMap<(usize, usize), Span> = BTreeMap::new();

    while let Some(m) = matches.next() {
        for capture in m.captures() {
            if capture.index != target_index {
                continue;
            }
            insert_span(&mut seen, capture.node, source_bytes);
        }
    }

    let mut spans: Vec<Span> = seen.into_values().collect();
    spans.sort_by_key(|s| s.start_byte);
    spans
}

fn insert_span(into: &mut BTreeMap<(usize, usize), Span>, node: Node<'_>, source_bytes: &[u8]) {
    let key = (node.start_byte(), node.end_byte());
    into.entry(key).or_insert_with(|| {
        let start = node.start_position();
        let end = node.end_position();
        let original = std::str::from_utf8(&source_bytes[node.start_byte()..node.end_byte()])
            .unwrap_or_default()
            .to_string();
        Span {
            start_byte: node.start_byte(),
            end_byte: node.end_byte(),
            start_row: start.row,
            start_col: start.column,
            end_row: end.row,
            end_col: end.column,
            original,
        }
    });
}

/// Replace each chosen span's bytes with `replacement`. Spans are
/// processed in reverse start order so earlier byte offsets stay valid as
/// later ones are rewritten; whitespace and trivia outside each span are
/// preserved verbatim (the format-preserving guarantee).
///
/// Operates on raw bytes so that bytes outside the edited spans — including
/// any invalid-UTF-8 bytes elsewhere in the file — are copied through
/// untouched. The span offsets must index `source` directly (they come from
/// a tree parsed over these same raw bytes via [`parse_bytes`]).
pub(super) fn splice(source: &[u8], chosen: &[Span], replacement: &str) -> Vec<u8> {
    let mut by_start: Vec<&Span> = chosen.iter().collect();
    by_start.sort_by_key(|s| std::cmp::Reverse(s.start_byte));
    let mut out = source.to_vec();
    for span in by_start {
        let end = span.end_byte.min(out.len());
        let start = span.start_byte.min(end);
        out.splice(start..end, replacement.bytes());
    }
    out
}

/// Resolve which capture in `query` drives the edit.
///
/// - Explicit name match wins.
/// - Single-capture queries fall back to the only capture.
/// - Otherwise returns a diagnostic listing the available captures so
///   the caller can fix the query.
pub(super) fn resolve_target_capture(query: &Query, requested: &str) -> Result<u32, String> {
    let names = query.capture_names();
    if let Some(idx) = names.iter().position(|n| *n == requested) {
        return Ok(idx as u32);
    }
    if names.len() == 1 {
        return Ok(0);
    }
    Err(format!(
        "query has no capture named `{requested}`; available captures: [{}]",
        names
            .iter()
            .map(|n| format!("@{n}"))
            .collect::<Vec<_>>()
            .join(", ")
    ))
}

/// Render a tree-sitter [`QueryErrorKind`] as a short snake-case label
/// suitable for wire responses.
pub(super) fn query_error_kind_str(kind: &QueryErrorKind) -> &'static str {
    match kind {
        QueryErrorKind::Syntax => "syntax",
        QueryErrorKind::NodeType => "node_type",
        QueryErrorKind::Field => "field",
        QueryErrorKind::Capture => "capture",
        QueryErrorKind::Predicate => "predicate",
        QueryErrorKind::Structure => "structure",
        QueryErrorKind::Language => "language",
    }
}

/// Format a tree-sitter [`QueryError`] as the `details` string used in
/// `invalid_query` responses.
pub(super) fn format_query_error(err: &QueryError) -> String {
    format!(
        "tree-sitter rejected query at row {} col {}: {} ({})",
        err.row,
        err.column,
        err.message,
        query_error_kind_str(&err.kind),
    )
}

/// Re-parse `source` for `language` and, if the post-edit tree carries
/// any ERROR / MISSING node, return a short human-readable diagnostic
/// pinpointing the first offender. Returns `None` when the source
/// parses cleanly.
pub(super) fn first_syntax_error(source: &[u8], language: Language) -> Option<String> {
    let tree = parse_bytes(source, language).ok()?;
    let root = tree.root_node();
    if !root.has_error() {
        return None;
    }
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.is_missing() {
            let pos = node.start_position();
            return Some(format!(
                "missing `{}` at line {}, column {}",
                node.kind(),
                pos.row + 1,
                pos.column + 1
            ));
        }
        if node.is_error() {
            let pos = node.start_position();
            let snippet = node_text(node, source);
            let trimmed: String = snippet.chars().take(40).collect();
            return Some(format!(
                "unexpected `{trimmed}` at line {}, column {}",
                pos.row + 1,
                pos.column + 1
            ));
        }
        for i in (0..node.child_count()).rev() {
            if let Some(child) = node.child(i) {
                if child.has_error() || child.is_missing() {
                    stack.push(child);
                }
            }
        }
    }
    Some("post-edit source has parse errors".into())
}

pub(super) fn node_text(node: Node<'_>, source: &[u8]) -> String {
    let start = node.start_byte().min(source.len());
    let end = node.end_byte().min(source.len());
    if start >= end {
        return String::new();
    }
    std::str::from_utf8(&source[start..end])
        .map(|s| s.to_string())
        .unwrap_or_default()
}

/// Hex sha256 of the given bytes; consumed by every edit response for
/// before/after fingerprinting.
pub(super) fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

/// Read `path` through staged-fs when `session_id` is set, otherwise
/// fall back to the real filesystem. Truncates to `max_bytes` (`0` is
/// unlimited). `builtin` names the caller for error reporting.
///
/// Returns the **raw bytes** verbatim — no lossy UTF-8 decoding. The edit
/// primitives parse, splice, and write these same bytes, so any invalid
/// UTF-8 elsewhere in the file survives a targeted edit untouched. Decoding
/// to a `String` here would silently rewrite every invalid byte in the file
/// to U+FFFD on write-back, even in regions the edit never touched.
pub(super) fn read_source(
    builtin: &'static str,
    path: &Path,
    session_id: Option<&str>,
    max_bytes: usize,
) -> Result<Vec<u8>, HostlibError> {
    let bytes = if let Some(result) = crate::fs::read(path, session_id) {
        result.map_err(|err| HostlibError::Backend {
            builtin,
            message: format!("read `{}`: {err}", path.display()),
        })?
    } else {
        std::fs::read(path).map_err(|err| HostlibError::Backend {
            builtin,
            message: format!("read `{}`: {err}", path.display()),
        })?
    };
    if max_bytes != 0 && bytes.len() > max_bytes {
        Ok(bytes[..max_bytes].to_vec())
    } else {
        Ok(bytes)
    }
}

/// Best-effort UTF-8 view of raw source bytes for display/heuristic use
/// (response previews, indent detection). Lossy decoding is safe here
/// because the result never feeds back into byte offsets or the write
/// path — only [`splice`]/[`write_source`] over the raw bytes do.
pub(super) fn lossy_str(source: &[u8]) -> String {
    String::from_utf8_lossy(source).into_owned()
}

/// Write `contents` to `path`. Routes through staged-fs when active;
/// otherwise captures an auto-snapshot (#1722 fallback) and writes
/// directly. Creates the parent directory if it doesn't exist.
///
/// Takes raw bytes so the byte-preserving edit path can write back the
/// original (possibly non-UTF-8) bytes outside the edited span verbatim.
pub(super) fn write_source(
    builtin: &'static str,
    path: &Path,
    contents: &[u8],
    session_id: Option<&str>,
) -> Result<(), HostlibError> {
    if crate::fs::stage_write_or_none(builtin, path, contents, true, true, session_id)?.is_some() {
        return Ok(());
    }
    crate::fs_snapshot::auto_capture_for_write(builtin, path);
    let owned = PathBuf::from(path);
    if let Some(parent) = owned.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|err| HostlibError::Backend {
                builtin,
                message: format!("mkdir `{}`: {err}", parent.display()),
            })?;
        }
    }
    std::fs::write(path, contents).map_err(|err| HostlibError::Backend {
        builtin,
        message: format!("write `{}`: {err}", path.display()),
    })?;
    crate::fs_snapshot::record_completed_write(path);
    Ok(())
}

/// Everything a query-driven edit needs once the target file is known to be
/// parseable, structurally verified, and the query is valid for its grammar.
pub(super) struct PreparedEdit {
    pub(super) language: Language,
    pub(super) source: Vec<u8>,
    pub(super) query: Query,
    pub(super) target_index: u32,
    pub(super) tree: Tree,
    /// Health of the pre-edit parse (always verified).
    pub(super) source_health: ParserHealth,
}

/// Why [`prepare_edit`] stopped before any span was matched. Single-file
/// builtins render it with [`PrepareFailure::into_response`]; the batch
/// runner folds it into its per-file outcome instead.
pub(super) enum PrepareFailure {
    /// No grammar could be inferred from the path or hint.
    UnknownLanguage,
    /// The language is recognized but its grammar is not compiled in.
    GrammarUnavailable(Language),
    Read(Language, HostlibError),
    InvalidQuery(Language, QueryError),
    /// The query has no usable target capture; carries the diagnostic.
    NoTargetCapture(Language, String),
    /// tree-sitter could not parse the source; carries the error text.
    Parse(Language, String),
    /// The source parsed with errors; carries the observed health.
    NotVerified(ParserHealth),
}

/// Detail used when refusing to edit a source that does not parse cleanly.
pub(super) const SOURCE_NOT_VERIFIED: &str =
    "source structure is not verified; refusing structural mutation";
/// Detail used when an unvalidated edit would be written to disk.
pub(super) const UNVALIDATED_WRITE: &str = "`validate: false` is allowed only for dry-run previews";

impl PrepareFailure {
    pub(super) fn health(&self) -> ParserHealth {
        match self {
            Self::UnknownLanguage => ParserHealth::unavailable(None, ParserOperation::SafeEdit),
            Self::GrammarUnavailable(language)
            | Self::Read(language, _)
            | Self::Parse(language, _) => {
                ParserHealth::unavailable(Some(*language), ParserOperation::SafeEdit)
            }
            Self::InvalidQuery(language, _) | Self::NoTargetCapture(language, _) => {
                ParserHealth::not_observed(*language, ParserOperation::SafeEdit)
            }
            Self::NotVerified(health) => health.clone(),
        }
    }

    /// Render the failure as a single-file edit builtin's tagged response.
    /// Read and parse failures are host errors there, not edit outcomes.
    pub(super) fn into_response(
        self,
        builtin: &'static str,
        path: &str,
        language_hint: Option<&str>,
        query: &str,
        target_capture: &str,
    ) -> Result<VmValue, HostlibError> {
        let health = self.health();
        let response = match self {
            Self::UnknownLanguage | Self::GrammarUnavailable(_) => {
                unsupported_language_response(path, language_hint)
            }
            Self::Read(_, err) => return Err(err),
            Self::Parse(_, message) => return Err(HostlibError::Backend { builtin, message }),
            Self::InvalidQuery(_, err) => invalid_query_response(query, &err),
            Self::NoTargetCapture(_, detail) => {
                no_match_response(path, query, target_capture, &detail)
            }
            Self::NotVerified(_) => non_verified_response(path, SOURCE_NOT_VERIFIED),
        };
        Ok(health.attach_to(response))
    }
}

/// Shared front half of every query-driven edit: detect the language,
/// read the source, compile the query, resolve the target capture, parse,
/// and refuse sources whose structure is not verified. Callers enforce
/// path scope first.
pub(super) fn prepare_edit(
    builtin: &'static str,
    path: &Path,
    language_hint: Option<&str>,
    query_text: &str,
    target_capture: &str,
    session_id: Option<&str>,
    max_bytes: usize,
) -> Result<PreparedEdit, PrepareFailure> {
    let language = Language::detect(path, language_hint).ok_or(PrepareFailure::UnknownLanguage)?;
    let ts_language = language
        .ts_language()
        .ok_or(PrepareFailure::GrammarUnavailable(language))?;
    let source = read_source(builtin, path, session_id, max_bytes)
        .map_err(|err| PrepareFailure::Read(language, err))?;
    let query = Query::new(&ts_language, query_text)
        .map_err(|err| PrepareFailure::InvalidQuery(language, err))?;
    let target_index = resolve_target_capture(&query, target_capture)
        .map_err(|detail| PrepareFailure::NoTargetCapture(language, detail))?;
    let tree = parse_bytes(&source, language)
        .map_err(|err| PrepareFailure::Parse(language, err.to_string()))?;
    let source_health = ParserHealth::observed(
        language,
        ParserOperation::SafeEdit,
        tree.root_node().has_error(),
    );
    if !source_health.is_verified() {
        return Err(PrepareFailure::NotVerified(source_health));
    }
    Ok(PreparedEdit {
        language,
        source,
        query,
        target_index,
        tree,
        source_health,
    })
}

/// Why a spliced edit must not be applied.
pub(super) enum PostEditRejection {
    /// `validate` is on and the patched source has a parse error.
    Syntax(String),
    /// `validate` is off on a call that would write to disk.
    UnvalidatedWrite,
}

impl PostEditRejection {
    pub(super) fn health(&self, language: Language) -> ParserHealth {
        match self {
            Self::Syntax(_) => ParserHealth::observed(language, ParserOperation::SafeEdit, true),
            Self::UnvalidatedWrite => {
                ParserHealth::unavailable(Some(language), ParserOperation::SafeEdit)
            }
        }
    }
}

/// Post-edit gate shared by the edit builtins. On success returns the
/// health of the patched source (which records any tolerated parse error
/// of an unvalidated dry run).
pub(super) fn check_post_edit(
    patched: &[u8],
    language: Language,
    validate: bool,
    dry_run: bool,
) -> Result<ParserHealth, PostEditRejection> {
    let post_edit_error = first_syntax_error(patched, language);
    if validate {
        if let Some(detail) = post_edit_error {
            return Err(PostEditRejection::Syntax(detail));
        }
    } else if !dry_run {
        return Err(PostEditRejection::UnvalidatedWrite);
    }
    Ok(ParserHealth::observed(
        language,
        ParserOperation::SafeEdit,
        post_edit_error.is_some(),
    ))
}

// ---------------------------------------------------------------------------
// Responses shared by the single-file edit builtins
// ---------------------------------------------------------------------------

pub(super) fn no_match_response(
    path: &str,
    query: &str,
    target_capture: &str,
    details: &str,
) -> VmValue {
    build_dict([
        ("result", str_value("no_match")),
        ("applied", VmValue::Bool(false)),
        ("path", str_value(path)),
        ("query", str_value(query)),
        ("target_capture", str_value(target_capture)),
        ("details", str_value(details)),
    ])
}

pub(super) fn invalid_query_response(query: &str, err: &QueryError) -> VmValue {
    build_dict([
        ("result", str_value("invalid_query")),
        ("applied", VmValue::Bool(false)),
        ("query", str_value(query)),
        ("details", str_value(format_query_error(err))),
        ("error_row", VmValue::Int(err.row as i64)),
        ("error_column", VmValue::Int(err.column as i64)),
    ])
}

pub(super) fn unsupported_language_response(path: &str, hint: Option<&str>) -> VmValue {
    build_dict([
        ("result", str_value("unsupported_language")),
        ("applied", VmValue::Bool(false)),
        ("path", str_value(path)),
        (
            "details",
            str_value(unsupported_language_detail(path, hint)),
        ),
        ("fallback_suggestion", str_value(TEXT_PATCH_FALLBACK)),
    ])
}

pub(super) fn unsupported_language_detail(path: &str, hint: Option<&str>) -> String {
    format!(
        "could not infer a tree-sitter grammar for `{path}` (hint: {})",
        hint.unwrap_or("none")
    )
}

pub(super) fn non_verified_response(path: &str, details: &str) -> VmValue {
    build_dict([
        ("result", str_value("parser_not_verified")),
        ("applied", VmValue::Bool(false)),
        ("path", str_value(path)),
        ("details", str_value(details)),
    ])
}
