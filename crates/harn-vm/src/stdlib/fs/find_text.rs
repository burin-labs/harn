use crate::value::VmDictExt;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use globset::{Glob, GlobSet, GlobSetBuilder};
use grep_matcher::Matcher;
use grep_regex::{RegexMatcher, RegexMatcherBuilder};
use grep_searcher::{Searcher, SearcherBuilder, Sink, SinkMatch};
use ignore::{WalkBuilder, WalkState};

use crate::stdlib::macros::harn_builtin;
use crate::value::{VmError, VmValue};

use super::{
    bool_option, int_option, resolve_fs_path, string_list_option, string_option, u64_option,
    usize_option,
};

#[derive(Clone)]
struct FindTextOptions {
    max_depth: Option<usize>,
    max_filesize: Option<u64>,
    follow_symlinks: bool,
    include_hidden: bool,
    respect_gitignore: bool,
    case_insensitive: bool,
    fixed_strings: bool,
    preset: FindTextPreset,
    include_globs: Vec<String>,
    exclude_globs: Vec<String>,
    max_matches: usize,
    long_running: bool,
    mode: FindTextMode,
    parallel: bool,
    threads: Option<usize>,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum FindTextMode {
    Hits,
    Count,
    Exists,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum FindTextPreset {
    Default,
    Source,
    All,
}

#[derive(Clone)]
struct TextMatch {
    path: String,
    line: i64,
    col: i64,
    text: String,
}

struct FindTextSummary {
    matched: bool,
    count: usize,
}

fn find_text_error(message: impl Into<String>) -> VmError {
    VmError::Thrown(VmValue::String(arcstr::ArcStr::from(message.into())))
}

fn parse_find_text_options(args: &[VmValue]) -> Result<FindTextOptions, VmError> {
    let mut options = FindTextOptions {
        max_depth: None,
        max_filesize: None,
        follow_symlinks: false,
        include_hidden: false,
        respect_gitignore: true,
        case_insensitive: false,
        fixed_strings: true,
        preset: FindTextPreset::Default,
        include_globs: Vec::new(),
        exclude_globs: Vec::new(),
        max_matches: 1000,
        long_running: false,
        mode: FindTextMode::Hits,
        parallel: false,
        threads: None,
    };
    if let Some(VmValue::Dict(opts)) = args.get(2) {
        options.preset = match string_option(opts, "preset").as_deref() {
            Some("source") | Some("sources") => FindTextPreset::Source,
            Some("all") => FindTextPreset::All,
            Some("default") | None => FindTextPreset::Default,
            Some(other) => {
                return Err(find_text_error(format!(
                    "find_text: unknown preset `{other}`"
                )));
            }
        };
        if options.preset == FindTextPreset::Source {
            options.exclude_globs = source_preset_exclude_globs();
            options.max_filesize = Some(1_048_576);
        } else if options.preset == FindTextPreset::All {
            options.include_hidden = true;
            options.respect_gitignore = false;
        }
        if let Some(v) = int_option(opts, "max_depth") {
            if v >= 0 {
                options.max_depth = Some(v as usize);
            }
        }
        if let Some(v) =
            u64_option(opts, "max_filesize").or_else(|| u64_option(opts, "max_file_size"))
        {
            options.max_filesize = Some(v);
        }
        options.follow_symlinks = bool_option(opts, "follow_symlinks").unwrap_or(false);
        options.include_hidden = bool_option(opts, "include_hidden")
            .or_else(|| bool_option(opts, "hidden"))
            .unwrap_or(options.include_hidden);
        options.respect_gitignore =
            bool_option(opts, "respect_gitignore").unwrap_or(options.respect_gitignore);
        options.case_insensitive = bool_option(opts, "case_insensitive")
            .unwrap_or_else(|| !bool_option(opts, "case_sensitive").unwrap_or(true));
        options.fixed_strings = bool_option(opts, "fixed_strings").unwrap_or(true);
        options.include_globs = string_list_option(opts, "include")
            .into_iter()
            .chain(string_list_option(opts, "include_globs"))
            .collect();
        options
            .exclude_globs
            .extend(string_list_option(opts, "exclude"));
        options
            .exclude_globs
            .extend(string_list_option(opts, "exclude_globs"));
        options
            .exclude_globs
            .extend(string_list_option(opts, "ignore"));
        options
            .exclude_globs
            .extend(string_list_option(opts, "ignore_globs"));
        if let Some(v) = int_option(opts, "max_matches") {
            options.max_matches = usize::try_from(v.max(1)).unwrap_or(usize::MAX);
        }
        options.mode = match string_option(opts, "mode").as_deref() {
            Some("count") => FindTextMode::Count,
            Some("exists") | Some("any") => FindTextMode::Exists,
            Some("hits") | Some("list") | None => FindTextMode::Hits,
            Some(other) => {
                return Err(find_text_error(format!(
                    "find_text: unknown mode `{other}`"
                )));
            }
        };
        if options.mode == FindTextMode::Exists {
            options.max_matches = 1;
        }
        options.parallel = bool_option(opts, "parallel").unwrap_or(false);
        options.threads = usize_option(opts, "threads").filter(|threads| *threads > 0);
        options.long_running = bool_option(opts, "long_running")
            .or_else(|| bool_option(opts, "background"))
            .unwrap_or(false);
    }
    Ok(options)
}

fn source_preset_exclude_globs() -> Vec<String> {
    [
        ".git/**",
        ".harn-runs/**",
        "dist/**",
        "node_modules/**",
        "target/**",
        "vendor/**",
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}

fn normalize_search_glob(glob: &str) -> String {
    if glob.contains('/') && !glob.starts_with("**/") {
        format!("**/{glob}")
    } else {
        glob.to_string()
    }
}

fn build_glob_set(patterns: &[String], option_name: &str) -> Result<Option<GlobSet>, VmError> {
    if patterns.is_empty() {
        return Ok(None);
    }
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        let normalized = normalize_search_glob(pattern);
        let glob = Glob::new(&normalized).map_err(|error| {
            find_text_error(format!(
                "find_text: invalid {option_name} glob `{pattern}`: {error}"
            ))
        })?;
        builder.add(glob);
    }
    builder.build().map(Some).map_err(|error| {
        find_text_error(format!(
            "find_text: invalid {option_name} glob set: {error}"
        ))
    })
}

fn file_matches_glob_set(
    root: &std::path::Path,
    file_path: &std::path::Path,
    set: &GlobSet,
) -> bool {
    let candidate = file_path.strip_prefix(root).unwrap_or(file_path);
    set.is_match(candidate)
}

fn find_text_file_included(
    root: &std::path::Path,
    path: &std::path::Path,
    include_set: Option<&GlobSet>,
    exclude_set: Option<&GlobSet>,
) -> bool {
    if include_set.is_some_and(|set| !file_matches_glob_set(root, path, set)) {
        return false;
    }
    if exclude_set.is_some_and(|set| file_matches_glob_set(root, path, set)) {
        return false;
    }
    true
}

fn find_text_walk_builder(root: &PathBuf, options: &FindTextOptions) -> WalkBuilder {
    let mut walker = WalkBuilder::new(root);
    walker
        .hidden(!options.include_hidden)
        .ignore(options.respect_gitignore)
        .git_ignore(options.respect_gitignore)
        .git_global(options.respect_gitignore)
        .git_exclude(options.respect_gitignore)
        .require_git(false)
        .parents(true)
        .follow_links(options.follow_symlinks)
        .sort_by_file_name(|left, right| left.cmp(right));
    if let Some(max_depth) = options.max_depth {
        walker.max_depth(Some(max_depth));
    }
    if let Some(max_filesize) = options.max_filesize {
        walker.max_filesize(Some(max_filesize));
    }
    walker
}

fn find_text_matcher(pattern: &str, options: &FindTextOptions) -> Result<RegexMatcher, VmError> {
    let mut builder = RegexMatcherBuilder::new();
    builder.case_insensitive(options.case_insensitive);
    builder.fixed_strings(options.fixed_strings);
    builder.build(pattern).map_err(|error| {
        let label = if options.fixed_strings {
            "pattern"
        } else {
            "regex"
        };
        find_text_error(format!("find_text: invalid {label}: {error}"))
    })
}

fn text_match_to_vm(hit: TextMatch) -> VmValue {
    let mut dict = BTreeMap::new();
    dict.put_str("path", hit.path);
    dict.insert("line".to_string(), VmValue::Int(hit.line));
    dict.insert("col".to_string(), VmValue::Int(hit.col));
    dict.insert("column".to_string(), VmValue::Int(hit.col));
    dict.put_str("text", hit.text);
    VmValue::dict(dict)
}

fn text_matches_to_json(hits: Vec<TextMatch>) -> serde_json::Value {
    serde_json::Value::Array(
        hits.into_iter()
            .map(|hit| {
                serde_json::json!({
                    "path": hit.path,
                    "line": hit.line,
                    "col": hit.col,
                    "column": hit.col,
                    "text": hit.text,
                })
            })
            .collect(),
    )
}

fn find_text_summary_to_vm(summary: FindTextSummary, mode: FindTextMode) -> VmValue {
    match mode {
        FindTextMode::Exists => VmValue::Bool(summary.matched),
        FindTextMode::Count => VmValue::Int(i64::try_from(summary.count).unwrap_or(i64::MAX)),
        FindTextMode::Hits => unreachable!("hits mode uses find_text_matches"),
    }
}

fn find_text_summary_to_json(summary: FindTextSummary, mode: FindTextMode) -> serde_json::Value {
    match mode {
        FindTextMode::Exists => serde_json::Value::Bool(summary.matched),
        FindTextMode::Count => {
            serde_json::Value::Number(serde_json::Number::from(summary.count as u64))
        }
        FindTextMode::Hits => unreachable!("hits mode uses find_text_matches"),
    }
}

struct FindTextSink {
    capture: FindTextCapture,
    hits: Vec<TextMatch>,
    remaining: usize,
    count: usize,
}

enum FindTextCapture {
    Hits { matcher: RegexMatcher, path: String },
    Count,
}

impl Sink for FindTextSink {
    type Error = std::io::Error;

    fn matched(
        &mut self,
        _searcher: &Searcher,
        sink_match: &SinkMatch<'_>,
    ) -> Result<bool, std::io::Error> {
        if self.remaining == 0 {
            return Ok(false);
        }
        if let FindTextCapture::Hits { matcher, path } = &self.capture {
            let line = sink_match.line_number().unwrap_or(0) as i64;
            let raw_line = std::str::from_utf8(sink_match.bytes()).unwrap_or("");
            let text = raw_line.trim_end_matches(['\n', '\r']).to_string();
            let col = matcher
                .find(sink_match.bytes())
                .ok()
                .flatten()
                .map_or(1, |m| i64::try_from(m.start()).unwrap_or(i64::MAX - 1) + 1);
            self.hits.push(TextMatch {
                path: path.clone(),
                line,
                col,
                text,
            });
        }
        self.count += 1;
        self.remaining -= 1;
        Ok(self.remaining > 0)
    }
}

fn find_text_matches(
    root: &PathBuf,
    pattern: &str,
    options: FindTextOptions,
    cancel: Option<&AtomicBool>,
) -> Result<Vec<TextMatch>, VmError> {
    let matcher = find_text_matcher(pattern, &options)?;
    let include_set = build_glob_set(&options.include_globs, "include")?;
    let exclude_set = build_glob_set(&options.exclude_globs, "exclude")?;

    let walker = find_text_walk_builder(root, &options);

    let mut hits = Vec::new();
    let mut searcher = SearcherBuilder::new().line_number(true).build();
    for entry in walker.build() {
        if cancel.is_some_and(|flag| flag.load(Ordering::Acquire)) {
            break;
        }
        if hits.len() >= options.max_matches {
            break;
        }
        let Ok(entry) = entry else {
            continue;
        };
        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }
        let path = entry.path();
        if !find_text_file_included(root, path, include_set.as_ref(), exclude_set.as_ref()) {
            continue;
        }
        if options.follow_symlinks {
            crate::stdlib::sandbox::enforce_fs_path(
                "find_text",
                path,
                crate::stdlib::sandbox::FsAccess::Read,
            )?;
        }
        let path_display = path.to_string_lossy().replace('\\', "/");
        let remaining = options.max_matches.saturating_sub(hits.len());
        let mut sink = FindTextSink {
            capture: FindTextCapture::Hits {
                matcher: matcher.clone(),
                path: path_display,
            },
            hits: Vec::new(),
            remaining,
            count: 0,
        };
        if searcher.search_path(&matcher, path, &mut sink).is_err() {
            continue;
        }
        hits.extend(sink.hits);
    }
    Ok(hits)
}

fn find_text_summary(
    root: &PathBuf,
    pattern: &str,
    options: FindTextOptions,
    cancel: Option<&AtomicBool>,
) -> Result<FindTextSummary, VmError> {
    if options.parallel {
        if options.follow_symlinks {
            return Err(find_text_error(
                "find_text: parallel searches cannot follow symlinks",
            ));
        }
        find_text_summary_parallel(root, pattern, options, cancel)
    } else {
        find_text_summary_sequential(root, pattern, options, cancel)
    }
}

fn find_text_summary_sequential(
    root: &PathBuf,
    pattern: &str,
    options: FindTextOptions,
    cancel: Option<&AtomicBool>,
) -> Result<FindTextSummary, VmError> {
    let matcher = find_text_matcher(pattern, &options)?;
    let include_set = build_glob_set(&options.include_globs, "include")?;
    let exclude_set = build_glob_set(&options.exclude_globs, "exclude")?;
    let walker = find_text_walk_builder(root, &options);
    let mut summary = FindTextSummary {
        matched: false,
        count: 0,
    };
    let mut searcher = SearcherBuilder::new().line_number(true).build();
    for entry in walker.build() {
        if cancel.is_some_and(|flag| flag.load(Ordering::Acquire)) {
            break;
        }
        if options.mode == FindTextMode::Exists && summary.matched {
            break;
        }
        if options.mode == FindTextMode::Count && summary.count >= options.max_matches {
            break;
        }
        let Ok(entry) = entry else {
            continue;
        };
        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }
        let path = entry.path();
        if !find_text_file_included(root, path, include_set.as_ref(), exclude_set.as_ref()) {
            continue;
        }
        if options.follow_symlinks {
            crate::stdlib::sandbox::enforce_fs_path(
                "find_text",
                path,
                crate::stdlib::sandbox::FsAccess::Read,
            )?;
        }
        let remaining = options.max_matches.saturating_sub(summary.count).max(1);
        let mut sink = FindTextSink {
            capture: FindTextCapture::Count,
            hits: Vec::new(),
            remaining,
            count: 0,
        };
        if searcher.search_path(&matcher, path, &mut sink).is_err() {
            continue;
        }
        if sink.count > 0 {
            summary.matched = true;
            summary.count = summary.count.saturating_add(sink.count);
        }
    }
    Ok(summary)
}

fn find_text_summary_parallel(
    root: &PathBuf,
    pattern: &str,
    options: FindTextOptions,
    cancel: Option<&AtomicBool>,
) -> Result<FindTextSummary, VmError> {
    let matcher = find_text_matcher(pattern, &options)?;
    let include_set = Arc::new(build_glob_set(&options.include_globs, "include")?);
    let exclude_set = Arc::new(build_glob_set(&options.exclude_globs, "exclude")?);
    let matched = Arc::new(AtomicBool::new(false));
    let count = Arc::new(AtomicUsize::new(0));
    let stop = Arc::new(AtomicBool::new(false));
    let external_cancel = cancel
        .map(|flag| flag.load(Ordering::Acquire))
        .unwrap_or(false);
    if external_cancel {
        return Ok(FindTextSummary {
            matched: false,
            count: 0,
        });
    }

    let mut walker = find_text_walk_builder(root, &options);
    if let Some(threads) = options.threads {
        walker.threads(threads);
    }
    walker.build_parallel().run(|| {
        let root = root.clone();
        let matcher = matcher.clone();
        let include_set = include_set.clone();
        let exclude_set = exclude_set.clone();
        let matched = matched.clone();
        let count = count.clone();
        let stop = stop.clone();
        let mode = options.mode;
        let max_matches = options.max_matches;
        Box::new(move |entry| {
            if cancel
                .map(|flag| flag.load(Ordering::Acquire))
                .unwrap_or(false)
            {
                stop.store(true, Ordering::Release);
                return WalkState::Quit;
            }
            if stop.load(Ordering::Acquire)
                || (mode == FindTextMode::Exists && matched.load(Ordering::Acquire))
                || count.load(Ordering::Acquire) >= max_matches
            {
                stop.store(true, Ordering::Release);
                return WalkState::Quit;
            }
            let Ok(entry) = entry else {
                return WalkState::Continue;
            };
            if !entry.file_type().is_some_and(|ft| ft.is_file()) {
                return WalkState::Continue;
            }
            let path = entry.path();
            if !find_text_file_included(
                &root,
                path,
                include_set.as_ref().as_ref(),
                exclude_set.as_ref().as_ref(),
            ) {
                return WalkState::Continue;
            }
            let mut sink = FindTextSink {
                capture: FindTextCapture::Count,
                hits: Vec::new(),
                remaining: max_matches
                    .saturating_sub(count.load(Ordering::Acquire))
                    .max(1),
                count: 0,
            };
            let mut searcher = SearcherBuilder::new().line_number(true).build();
            if searcher.search_path(&matcher, path, &mut sink).is_err() || sink.count == 0 {
                return WalkState::Continue;
            }
            matched.store(true, Ordering::Release);
            let previous = count.fetch_add(sink.count, Ordering::AcqRel);
            if previous >= max_matches {
                stop.store(true, Ordering::Release);
                return WalkState::Quit;
            }
            if mode == FindTextMode::Exists || previous + sink.count >= max_matches {
                stop.store(true, Ordering::Release);
                WalkState::Quit
            } else {
                WalkState::Continue
            }
        })
    });

    let count = count.load(Ordering::Acquire).min(options.max_matches);
    Ok(FindTextSummary {
        matched: matched.load(Ordering::Acquire),
        count,
    })
}

#[harn_builtin(
    sig = "find_text(root: string, pattern: string, options?: dict) -> any",
    category = "fs",
    doc = "Search files under a root for text hits, existence, or capped counts."
)]
fn find_text_builtin(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let root = args.first().map(|a| a.display()).unwrap_or_default();
    if root.is_empty() {
        return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
            "find_text: root path is required",
        ))));
    }
    let pattern = args.get(1).map(|a| a.display()).unwrap_or_default();
    if pattern.is_empty() {
        return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
            "find_text: pattern is required",
        ))));
    }
    let resolved = resolve_fs_path(&root);
    crate::stdlib::sandbox::enforce_fs_path(
        "find_text",
        &resolved,
        crate::stdlib::sandbox::FsAccess::Read,
    )?;
    let options = parse_find_text_options(args)?;
    if options.long_running {
        let session_id = crate::llm::current_agent_session_id().unwrap_or_default();
        let descriptor = format!("find_text {} in {}", pattern, resolved.display());
        let handle = crate::stdlib::long_running::spawn_json_operation(
            "find_text",
            descriptor,
            session_id,
            move |cancel| {
                if options.mode == FindTextMode::Hits {
                    find_text_matches(&resolved, &pattern, options, Some(&cancel))
                        .map(text_matches_to_json)
                        .map_err(|error| error.to_string())
                } else {
                    let mode = options.mode;
                    find_text_summary(&resolved, &pattern, options, Some(&cancel))
                        .map(|summary| find_text_summary_to_json(summary, mode))
                        .map_err(|error| error.to_string())
                }
            },
        )
        .map_err(VmError::Runtime)?;
        return Ok(handle.into_vm_value());
    }
    if options.mode == FindTextMode::Hits {
        let hits = find_text_matches(&resolved, &pattern, options, None)?
            .into_iter()
            .map(text_match_to_vm)
            .collect::<Vec<_>>();
        Ok(VmValue::List(std::sync::Arc::new(hits)))
    } else {
        let mode = options.mode;
        let summary = find_text_summary(&resolved, &pattern, options, None)?;
        Ok(find_text_summary_to_vm(summary, mode))
    }
}
