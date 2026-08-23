//! `tools/search` — ripgrep-style content search backed by `grep-searcher`
//! and `ignore`.
//!
//! Returns structured matches (path/line/column/text/context) instead of
//! a preformatted human string. The shape is locked by
//! `schemas/tools/search.{request,response}.json`.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;

use globset::{Glob, GlobSet, GlobSetBuilder};
use grep_matcher::Matcher;
use grep_regex::{RegexMatcher, RegexMatcherBuilder};
use grep_searcher::{Searcher, SearcherBuilder, Sink, SinkContext, SinkContextKind, SinkMatch};
use harn_vm::ignore_policy::{self, IgnorePolicy};
use harn_vm::process_sandbox::FsAccess;
use harn_vm::VmValue;
use ignore::WalkBuilder;

use crate::error::HostlibError;
use crate::tools::args::{
    build_dict, dict_arg, optional_bool, optional_int, optional_string, optional_string_list,
    require_string, str_value, to_agent_path,
};
use crate::tools::permissions::enforce_path_scope;

const BUILTIN: &str = "hostlib_tools_search";
const DEFAULT_MAX_LINE_BYTES: usize = 1024;
const MIN_MAX_LINE_BYTES: i64 = 64;
const HARD_MAX_LINE_BYTES: i64 = 64 * 1024;
const CLIP_PREFIX: &str = "[truncated] ... ";
const CLIP_SUFFIX: &str = " ... [truncated]";

/// Public entry point invoked by the registered builtin.
pub(super) fn run(args: &[VmValue]) -> Result<VmValue, HostlibError> {
    let raw = dict_arg(BUILTIN, args)?;
    let dict = raw.as_ref();

    let pattern = require_string(BUILTIN, dict, "pattern")?;
    if pattern.is_empty() {
        return Err(HostlibError::InvalidParameter {
            builtin: BUILTIN,
            param: "pattern",
            message: "pattern must not be empty".to_string(),
        });
    }

    let raw_path = optional_string(BUILTIN, dict, "path")?;
    let path = raw_path
        .as_deref()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    // The walk descends from `path`; workspace-root scope is a prefix
    // check, so guarding the root rejects an out-of-scope search before it
    // can enumerate any file beneath it.
    enforce_path_scope(BUILTIN, &path, FsAccess::Read)?;
    let glob = optional_string(BUILTIN, dict, "glob")?;
    let exclude_globs = optional_string_list(BUILTIN, dict, "exclude_globs")?;
    let case_insensitive = optional_bool(BUILTIN, dict, "case_insensitive", false)?;
    let fixed_strings = optional_bool(BUILTIN, dict, "fixed_strings", false)?;
    let include_hidden = optional_bool(BUILTIN, dict, "include_hidden", false)?;
    let policy = ignore_policy_arg(dict)?;
    let max_matches = optional_int(BUILTIN, dict, "max_matches", 1000)?;
    let max_line_bytes = optional_int(
        BUILTIN,
        dict,
        "max_line_bytes",
        DEFAULT_MAX_LINE_BYTES as i64,
    )?;
    let context_before = optional_int(BUILTIN, dict, "context_before", 0)?;
    let context_after = optional_int(BUILTIN, dict, "context_after", 0)?;

    if max_matches < 1 {
        return Err(HostlibError::InvalidParameter {
            builtin: BUILTIN,
            param: "max_matches",
            message: "must be >= 1".to_string(),
        });
    }
    if !(MIN_MAX_LINE_BYTES..=HARD_MAX_LINE_BYTES).contains(&max_line_bytes) {
        return Err(HostlibError::InvalidParameter {
            builtin: BUILTIN,
            param: "max_line_bytes",
            message: format!("must be between {MIN_MAX_LINE_BYTES} and {HARD_MAX_LINE_BYTES}"),
        });
    }
    if context_before < 0 {
        return Err(HostlibError::InvalidParameter {
            builtin: BUILTIN,
            param: "context_before",
            message: "must be >= 0".to_string(),
        });
    }
    if context_after < 0 {
        return Err(HostlibError::InvalidParameter {
            builtin: BUILTIN,
            param: "context_after",
            message: "must be >= 0".to_string(),
        });
    }

    let max_matches = max_matches as usize;
    let max_line_bytes = max_line_bytes as usize;
    let context_before = context_before as usize;
    let context_after = context_after as usize;

    let matcher = build_matcher(&pattern, case_insensitive, fixed_strings)?;
    // Globs select against each candidate's path *relative to the search
    // root*, so the root's own spelling is what tells a path-qualified glob
    // apart from a root-relative one. Both glob parameters normalize through
    // it; nothing downstream re-decides the question.
    let root = root_components(raw_path.as_deref().unwrap_or("."));
    let include_set = build_include_glob(glob, &root)?;
    let exclude_set = build_exclude_globs(exclude_globs, &root)?;

    let mut walker = WalkBuilder::new(&path);
    walker.sort_by_file_name(|left, right| left.cmp(right));
    // The deterministic-tools surface skips exactly what the in-VM builtins
    // skip; `harn_vm::ignore_policy` is the single owner of that decision.
    ignore_policy::configure(&mut walker, &path, policy, include_hidden).map_err(|message| {
        HostlibError::InvalidParameter {
            builtin: BUILTIN,
            param: "ignore_policy",
            message,
        }
    })?;

    let mut all_rows: Vec<RowWithPath> = Vec::new();
    let mut truncated = false;
    // Candidates actually opened and scanned. Reported so a caller can tell
    // "searched nothing" from "searched N files and found nothing" — the two
    // read identically in the matches list, and only one of them means the
    // request selected the wrong files.
    let mut files_searched: usize = 0;

    'outer: for entry in walker.build() {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }
        let file_path = entry.path().to_path_buf();
        if !included_by_globs(&path, &file_path, include_set.as_ref()) {
            continue;
        }
        if excluded_by_globs(&path, &file_path, exclude_set.as_ref()) {
            continue;
        }
        files_searched += 1;
        let mut sink = CollectorSink {
            matcher: &matcher,
            rows: Vec::new(),
            pending_before: VecDeque::new(),
            context_before,
            remaining: max_matches.saturating_sub(all_rows.len()),
            max_line_bytes,
        };
        let mut searcher = SearcherBuilder::new()
            .before_context(context_before)
            .after_context(context_after)
            .line_number(true)
            .build();
        if let Err(err) = searcher.search_path(&matcher, &file_path, &mut sink) {
            // I/O error reading one file — skip it and keep searching.
            let _ = err;
            continue;
        }
        truncated |= sink.rows.iter().any(|row| row.truncated);
        for row in sink.rows {
            all_rows.push(RowWithPath {
                path: file_path.clone(),
                row,
            });
            if all_rows.len() >= max_matches {
                truncated = true;
                break 'outer;
            }
        }
        if all_rows.len() >= max_matches {
            truncated = true;
            break 'outer;
        }
    }

    let matches: Vec<VmValue> = all_rows.into_iter().map(row_to_value).collect();

    Ok(build_dict([
        ("matches", VmValue::List(Arc::new(matches))),
        ("files_searched", VmValue::Int(files_searched as i64)),
        ("truncated", VmValue::Bool(truncated)),
    ]))
}

fn ignore_policy_arg(dict: &harn_vm::value::DictMap) -> Result<IgnorePolicy, HostlibError> {
    let Some(raw) = optional_string(BUILTIN, dict, IgnorePolicy::OPTION_KEY)? else {
        return Ok(IgnorePolicy::default());
    };
    IgnorePolicy::parse_for(BUILTIN, &raw).map_err(|message| HostlibError::InvalidParameter {
        builtin: BUILTIN,
        param: "ignore_policy",
        message,
    })
}

fn build_matcher(
    pattern: &str,
    case_insensitive: bool,
    fixed_strings: bool,
) -> Result<RegexMatcher, HostlibError> {
    let mut builder = RegexMatcherBuilder::new();
    builder.case_insensitive(case_insensitive);
    builder.fixed_strings(fixed_strings);
    builder
        .build(pattern)
        .map_err(|err| HostlibError::InvalidParameter {
            builtin: BUILTIN,
            param: "pattern",
            message: format!("invalid regex: {err}"),
        })
}

fn build_include_glob(
    pattern: Option<String>,
    root: &[&str],
) -> Result<Option<GlobSet>, HostlibError> {
    let Some(pattern) = pattern else {
        return Ok(None);
    };
    build_glob_set([pattern], "glob", root)
}

fn build_exclude_globs(
    patterns: Vec<String>,
    root: &[&str],
) -> Result<Option<GlobSet>, HostlibError> {
    if patterns.is_empty() {
        return Ok(None);
    }
    build_glob_set(patterns, "exclude_globs", root)
}

fn build_glob_set(
    patterns: impl IntoIterator<Item = String>,
    param: &'static str,
    root: &[&str],
) -> Result<Option<GlobSet>, HostlibError> {
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        for normalized in normalize_glob_variants(&pattern, root) {
            let glob = Glob::new(&normalized).map_err(|err| HostlibError::InvalidParameter {
                builtin: BUILTIN,
                param,
                message: format!("invalid glob `{pattern}`: {err}"),
            })?;
            builder.add(glob);
        }
    }
    builder
        .build()
        .map(Some)
        .map_err(|err| HostlibError::InvalidParameter {
            builtin: BUILTIN,
            param,
            message: format!("invalid glob set: {err}"),
        })
}

/// Components of the search root as the caller spelled it, most-significant
/// first. `.` segments and the empty segments an absolute or trailing-slash
/// path produces carry no directory name, so they drop out.
fn root_components(path: &str) -> Vec<&str> {
    path.split(['/', '\\'])
        .filter(|part| !part.is_empty() && *part != ".")
        .collect()
}

/// The root-relative spelling of a glob the caller qualified with the
/// directory they also passed as `path` — `path: "src"` with
/// `glob: "src/registry.ts"` means `registry.ts` under the search root.
/// `None` when the glob is not so qualified.
///
/// `path` is often absolute by the time it reaches here (hosts resolve agent
/// paths against a workspace root) while the glob stays relative, so the match
/// runs against a *trailing run* of the root's components rather than the whole
/// spelling. The longest run wins, and at least one glob component always
/// survives — a glob that names only the root directory is a filename pattern,
/// not a prefix.
fn root_relative_glob(glob: &str, root: &[&str]) -> Option<String> {
    let parts: Vec<&str> = glob.split('/').collect();
    let overlap = (1..=root.len().min(parts.len().saturating_sub(1)))
        .rev()
        .find(|depth| root[root.len() - depth..] == parts[..*depth])?;
    Some(parts[overlap..].join("/"))
}

/// Normalize user-supplied globs so `*.rs` and `internal/*.go` behave like
/// ripgrep-style path filters at any depth while still matching root files.
///
/// Globs select against each candidate's path relative to the search root, so
/// a caller who qualified the glob with the same directory they passed as
/// `path` was testing a pattern no candidate could ever carry — zero files
/// selected, reported as an ordinary content miss (#7018). Both spellings are
/// accepted here, the single boundary that owns the question. Accepting rather
/// than rewriting keeps a genuine `src/src/...` layout reachable; the cost is
/// that a path-qualified exclude also drops the nested spelling, which is the
/// reading a caller who wrote it that way intended.
fn normalize_glob_variants(glob: &str, root: &[&str]) -> Vec<String> {
    let glob = glob.replace('\\', "/");
    let glob = glob.strip_prefix("./").unwrap_or(&glob).to_string();
    let mut spellings = vec![glob.clone()];
    spellings.extend(root_relative_glob(&glob, root));

    let mut variants = Vec::with_capacity(spellings.len() * 2);
    for spelling in spellings {
        if spelling == "*" || spelling.starts_with("**/") {
            variants.push(spelling);
            continue;
        }
        let at_any_depth = format!("**/{spelling}");
        variants.push(spelling);
        variants.push(at_any_depth);
    }
    variants
}

fn included_by_globs(
    root: &std::path::Path,
    file_path: &std::path::Path,
    set: Option<&GlobSet>,
) -> bool {
    let Some(set) = set else {
        return true;
    };
    let candidate = file_path.strip_prefix(root).unwrap_or(file_path);
    set.is_match(candidate)
}

fn excluded_by_globs(
    root: &std::path::Path,
    file_path: &std::path::Path,
    set: Option<&GlobSet>,
) -> bool {
    let Some(set) = set else {
        return false;
    };
    let candidate = file_path.strip_prefix(root).unwrap_or(file_path);
    set.is_match(candidate)
}

#[derive(Debug, Clone)]
struct MatchRow {
    line: u64,
    column: u64,
    text: String,
    context_before: VecDeque<String>,
    context_after: VecDeque<String>,
    truncated: bool,
}

struct RowWithPath {
    path: PathBuf,
    row: MatchRow,
}

struct ContextLine {
    text: String,
    truncated: bool,
}

struct CollectorSink<'a> {
    // Borrowed rather than owned: the compiled matcher is reused across every
    // file in the walk, and `grep_regex::RegexMatcher::clone` deep-copies the
    // compiled program — cloning it per file made a repo-wide scan pay that
    // cost N times for no reason.
    matcher: &'a RegexMatcher,
    rows: Vec<MatchRow>,
    /// Sliding window of recent before-context lines published by
    /// [`Sink::context`] before each [`Sink::matched`] call.
    pending_before: VecDeque<ContextLine>,
    context_before: usize,
    remaining: usize,
    max_line_bytes: usize,
}

impl Sink for CollectorSink<'_> {
    type Error = std::io::Error;

    fn matched(
        &mut self,
        _searcher: &Searcher,
        sink_match: &SinkMatch<'_>,
    ) -> Result<bool, std::io::Error> {
        if self.remaining == 0 {
            return Ok(false);
        }

        let line_number = sink_match.line_number().unwrap_or(0);
        let raw_line = std::str::from_utf8(sink_match.bytes()).unwrap_or("");
        let trimmed = raw_line.trim_end_matches(['\n', '\r']);

        let mut column = 1u64;
        let mut match_start = None;
        if let Ok(Some(m)) = self.matcher.find(sink_match.bytes()) {
            column = (m.start() as u64) + 1;
            match_start = Some(m.start().min(trimmed.len()));
        }

        let before = std::mem::take(&mut self.pending_before);
        let truncated = before.iter().any(|line| line.truncated);
        let before = before
            .into_iter()
            .map(|line| line.text)
            .collect::<VecDeque<_>>();
        let (text, text_truncated) = clip_text(trimmed, self.max_line_bytes, match_start);
        self.rows.push(MatchRow {
            line: line_number,
            column,
            text,
            context_before: before,
            context_after: VecDeque::new(),
            truncated: truncated || text_truncated,
        });
        self.remaining -= 1;
        Ok(self.remaining > 0)
    }

    fn context(
        &mut self,
        _searcher: &Searcher,
        ctx: &SinkContext<'_>,
    ) -> Result<bool, std::io::Error> {
        let line = std::str::from_utf8(ctx.bytes()).unwrap_or("");
        let trimmed = line.trim_end_matches(['\n', '\r']);
        let (text, truncated) = clip_text(trimmed, self.max_line_bytes, None);

        match ctx.kind() {
            SinkContextKind::Before => {
                self.pending_before
                    .push_back(ContextLine { text, truncated });
                while self.pending_before.len() > self.context_before {
                    self.pending_before.pop_front();
                }
            }
            SinkContextKind::After => {
                if let Some(last) = self.rows.last_mut() {
                    last.context_after.push_back(text);
                    last.truncated |= truncated;
                }
            }
            SinkContextKind::Other => {}
        }
        Ok(true)
    }
}

fn row_to_value(rwp: RowWithPath) -> VmValue {
    let RowWithPath { path, row } = rwp;
    let MatchRow {
        line,
        column,
        text,
        context_before,
        context_after,
        truncated: _,
    } = row;

    let before: Vec<VmValue> = context_before.into_iter().map(str_value).collect();
    let after: Vec<VmValue> = context_after.into_iter().map(str_value).collect();

    build_dict([
        ("path", str_value(to_agent_path(&path))),
        ("line", VmValue::Int(line as i64)),
        ("column", VmValue::Int(column as i64)),
        ("text", str_value(text)),
        ("context_before", VmValue::List(Arc::new(before))),
        ("context_after", VmValue::List(Arc::new(after))),
    ])
}

#[expect(
    clippy::string_slice,
    reason = "start/end come from floor/next_char_boundary, so they are char boundaries"
)]
fn clip_text(value: &str, max_bytes: usize, anchor_byte: Option<usize>) -> (String, bool) {
    if value.len() <= max_bytes {
        return (value.to_string(), false);
    }

    let Some(anchor_byte) = anchor_byte else {
        let keep = max_bytes.saturating_sub(CLIP_SUFFIX.len()).max(1);
        let end = floor_char_boundary(value, keep);
        return (format!("{}{}", &value[..end], CLIP_SUFFIX), true);
    };

    let content_budget = max_bytes
        .saturating_sub(CLIP_PREFIX.len())
        .saturating_sub(CLIP_SUFFIX.len())
        .max(1);
    let anchor_byte = anchor_byte.min(value.len());
    let mut start = anchor_byte.saturating_sub(content_budget / 2);
    if start.saturating_add(content_budget) > value.len() {
        start = value.len().saturating_sub(content_budget);
    }
    start = floor_char_boundary(value, start);
    let mut end = (start + content_budget).min(value.len());
    end = floor_char_boundary(value, end);
    if end <= start {
        end = next_char_boundary(value, start);
    }

    let mut out = String::with_capacity(max_bytes);
    if start > 0 {
        out.push_str(CLIP_PREFIX);
    }
    out.push_str(&value[start..end]);
    if end < value.len() {
        out.push_str(CLIP_SUFFIX);
    }
    (out, true)
}

fn floor_char_boundary(value: &str, mut index: usize) -> usize {
    index = index.min(value.len());
    while index > 0 && !value.is_char_boundary(index) {
        index -= 1;
    }
    index
}

#[expect(
    clippy::string_slice,
    reason = "the only caller passes an index already floored to a char boundary"
)]
fn next_char_boundary(value: &str, index: usize) -> usize {
    if index >= value.len() {
        return value.len();
    }
    value[index..]
        .chars()
        .next()
        .map(|ch| index + ch.len_utf8())
        .unwrap_or(value.len())
}
