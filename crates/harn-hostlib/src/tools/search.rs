//! `tools/search` — ripgrep-style content search backed by `grep-searcher`
//! and `ignore`.
//!
//! Returns structured matches (path/line/column/text/context) instead of
//! a preformatted human string. The shape is locked by
//! `schemas/tools/search.{request,response}.json`.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::rc::Rc;

use grep_matcher::Matcher;
use grep_regex::{RegexMatcher, RegexMatcherBuilder};
use grep_searcher::{Searcher, SearcherBuilder, Sink, SinkContext, SinkContextKind, SinkMatch};
use harn_vm::VmValue;
use ignore::WalkBuilder;

use crate::error::HostlibError;
use crate::tools::args::{
    build_dict, dict_arg, optional_bool, optional_int, optional_string, require_string, str_value,
};

const BUILTIN: &str = "hostlib_tools_search";

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

    let path = optional_string(BUILTIN, dict, "path")?
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    let glob = optional_string(BUILTIN, dict, "glob")?;
    let case_insensitive = optional_bool(BUILTIN, dict, "case_insensitive", false)?;
    let fixed_strings = optional_bool(BUILTIN, dict, "fixed_strings", false)?;
    let include_hidden = optional_bool(BUILTIN, dict, "include_hidden", false)?;
    let max_matches = optional_int(BUILTIN, dict, "max_matches", 1000)?;
    let context_before = optional_int(BUILTIN, dict, "context_before", 0)?;
    let context_after = optional_int(BUILTIN, dict, "context_after", 0)?;

    if max_matches < 1 {
        return Err(HostlibError::InvalidParameter {
            builtin: BUILTIN,
            param: "max_matches",
            message: "must be >= 1".to_string(),
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
    let context_before = context_before as usize;
    let context_after = context_after as usize;

    let matcher = build_matcher(&pattern, case_insensitive, fixed_strings)?;

    let mut walker = WalkBuilder::new(&path);
    walker
        .hidden(!include_hidden)
        .ignore(true)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        // Honor `.gitignore` even outside a git repo. The deterministic-tools
        // surface should match developer expectation: a `.gitignore` next to
        // the search root filters results regardless of whether `.git/`
        // exists. (Without this, `ignore` requires a `.git/` ancestor.)
        .require_git(false)
        .parents(true);
    if let Some(glob_pat) = glob.as_deref() {
        let mut builder = ignore::overrides::OverrideBuilder::new(&path);
        let normalized = normalize_glob(glob_pat);
        builder
            .add(&normalized)
            .map_err(|err| HostlibError::InvalidParameter {
                builtin: BUILTIN,
                param: "glob",
                message: format!("invalid glob `{glob_pat}`: {err}"),
            })?;
        let overrides = builder
            .build()
            .map_err(|err| HostlibError::InvalidParameter {
                builtin: BUILTIN,
                param: "glob",
                message: format!("invalid glob `{glob_pat}`: {err}"),
            })?;
        walker.overrides(overrides);
    }

    let mut all_rows: Vec<RowWithPath> = Vec::new();
    let mut truncated = false;

    'outer: for entry in walker.build() {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }
        let file_path = entry.path().to_path_buf();
        let mut sink = CollectorSink {
            matcher: matcher.clone(),
            rows: Vec::new(),
            pending_before: VecDeque::new(),
            context_before,
            remaining: max_matches.saturating_sub(all_rows.len()),
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
        ("matches", VmValue::List(Rc::new(matches))),
        ("truncated", VmValue::Bool(truncated)),
    ]))
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

/// Normalize a user-supplied glob so callers writing
/// `internal/manifest/*.go` get matches at any depth.
fn normalize_glob(glob: &str) -> String {
    if glob.contains('/') && !glob.starts_with("**/") {
        format!("**/{glob}")
    } else {
        glob.to_string()
    }
}

#[derive(Debug, Clone)]
struct MatchRow {
    line: u64,
    column: u64,
    text: String,
    context_before: VecDeque<String>,
    context_after: VecDeque<String>,
}

struct RowWithPath {
    path: PathBuf,
    row: MatchRow,
}

struct CollectorSink {
    matcher: RegexMatcher,
    rows: Vec<MatchRow>,
    /// Sliding window of recent before-context lines published by
    /// [`Sink::context`] before each [`Sink::matched`] call.
    pending_before: VecDeque<String>,
    context_before: usize,
    remaining: usize,
}

impl Sink for CollectorSink {
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
        if let Ok(Some(m)) = self.matcher.find(sink_match.bytes()) {
            column = (m.start() as u64) + 1;
        }

        let before = std::mem::take(&mut self.pending_before);
        self.rows.push(MatchRow {
            line: line_number,
            column,
            text: trimmed.to_string(),
            context_before: before,
            context_after: VecDeque::new(),
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
        let trimmed = line.trim_end_matches(['\n', '\r']).to_string();

        match ctx.kind() {
            SinkContextKind::Before => {
                self.pending_before.push_back(trimmed);
                while self.pending_before.len() > self.context_before {
                    self.pending_before.pop_front();
                }
            }
            SinkContextKind::After => {
                if let Some(last) = self.rows.last_mut() {
                    last.context_after.push_back(trimmed);
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
    } = row;

    let before: Vec<VmValue> = context_before.into_iter().map(str_value).collect();
    let after: Vec<VmValue> = context_after.into_iter().map(str_value).collect();

    build_dict([
        ("path", str_value(path.to_string_lossy())),
        ("line", VmValue::Int(line as i64)),
        ("column", VmValue::Int(column as i64)),
        ("text", str_value(text)),
        ("context_before", VmValue::List(Rc::new(before))),
        ("context_after", VmValue::List(Rc::new(after))),
    ])
}
