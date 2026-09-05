//! Import extraction + data-driven resolution.
//!
//! Two phases:
//!
//! 1. **Extraction** — scan source line-by-line for tokens that look like
//!    import statements: prefix-match common keywords per language and
//!    capture the trimmed line. A tree-sitter-backed extractor can replace
//!    this without changing the `code_index` public surface.
//!
//! 2. **Resolution** — for each extracted string, apply a per-language
//!    rule to produce a workspace-relative path, then look that path up
//!    in `pathToID`. The rules are stored in
//!    `data/code_index_import_rules.json` and parsed once at first use.
//!    This file is the canonical source: adding a language is a JSON edit,
//!    not a Rust edit.

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use harn_vm::text::case::to_snake_case;
use serde::Deserialize;

use super::file_table::FileId;

const RULES_JSON: &str = include_str!("../../data/code_index_import_rules.json");

/// Per-language extraction prefix keywords. A line whose trimmed start
/// matches any of the keywords is captured as an import. `None` means the
/// language has no fallback extraction.
pub(crate) fn import_keywords(language: &str) -> &'static [&'static str] {
    match language {
        "swift" => &["import "],
        "rust" => &["use ", "extern crate ", "pub use "],
        "go" => &["import "],
        "python" => &["import ", "from "],
        "java" => &["import "],
        "kotlin" => &["import "],
        "scala" => &["import "],
        "csharp" => &["using "],
        "c" | "cpp" => &["#include"],
        "ruby" => &["require ", "require_relative "],
        "php" => &["use "],
        "elixir" => &["alias ", "import ", "require ", "use "],
        "haskell" => &["import "],
        "lua" => &["require"],
        "javascript" | "typescript" => &[
            "import ",
            "import\t",
            "import{",
            "import \"",
            "import \'",
            "export * from ",
            "export {",
        ],
        "harn" => &["import ", "import{", "import\t"],
        "zig" => &["@import"],
        "r" => &["library(", "require(", "source("],
        _ => &[],
    }
}

/// Extract every import-like statement from `source`, one entry per line
/// the matcher fires on.
pub(crate) fn extract_imports(source: &str, language: &str) -> Vec<String> {
    let keywords = import_keywords(language);
    if keywords.is_empty() {
        return Vec::new();
    }
    let mut out: Vec<String> = Vec::new();
    for line in source.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        // Strip leading single-line comment markers so e.g. a commented-out
        // import doesn't polute the resolver. We don't try to parse block
        // comments — the keyword check is strict enough that false
        // positives are rare.
        if matches_comment_prefix(trimmed) {
            continue;
        }
        if keywords.iter().any(|k| trimmed.starts_with(k)) {
            out.push(trimmed.to_string());
        }
    }
    out
}

fn matches_comment_prefix(trimmed: &str) -> bool {
    trimmed.starts_with("//") || trimmed.starts_with('#') || trimmed.starts_with("--")
}

/// How a language's import strings are mapped onto workspace files.
///
/// The variant matters as much as the file set it produces. A language
/// with no resolver and a language whose import names something outside
/// the workspace both yield zero files, and treating those the same is
/// what let Rust, Swift, Go and Harn report a clean zero for years while
/// nothing had ever been attempted for them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolutionStrategy {
    /// Dotted module path (`a.b.C`) mapped onto a file path.
    Dotted,
    /// A string literal containing a dotted path.
    DottedLiteral,
    /// A path relative to the importing file.
    Relative,
    /// Rust module paths, anchored on the importing file's crate root:
    /// `crate::` resolves from the nearest `src/` ancestor, `super::`
    /// from the parent module, `self::` from the current one, with a
    /// `mod.rs` fallback for directory modules.
    RustModule,
    /// The language declares no resolver. **Not** the same as resolving
    /// to nothing: nothing was attempted, so a zero here is a measured
    /// nothing rather than a measured zero.
    UnresolvedByDesign,
}

impl ResolutionStrategy {
    /// Wire form used in the rules file and in census output.
    pub fn as_str(self) -> &'static str {
        match self {
            ResolutionStrategy::Dotted => "dotted",
            ResolutionStrategy::DottedLiteral => "dotted-literal",
            ResolutionStrategy::Relative => "relative",
            ResolutionStrategy::RustModule => "rust-module",
            ResolutionStrategy::UnresolvedByDesign => "unresolved-by-design",
        }
    }

    /// Parse a strategy declared in the rules file. An unknown spelling
    /// returns `None` so a typo fails the rules-file test rather than
    /// silently degrading that language to "resolves nothing".
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "dotted" => Some(ResolutionStrategy::Dotted),
            "dotted-literal" => Some(ResolutionStrategy::DottedLiteral),
            "relative" => Some(ResolutionStrategy::Relative),
            "rust-module" => Some(ResolutionStrategy::RustModule),
            "unresolved-by-design" => Some(ResolutionStrategy::UnresolvedByDesign),
            _ => None,
        }
    }

    /// Whether this strategy actually attempts resolution.
    pub fn attempts_resolution(self) -> bool {
        self != ResolutionStrategy::UnresolvedByDesign
    }
}

/// What one import string names inside the workspace.
///
/// `files` is a set because an import does not always name a single
/// file: a Go import names a directory and a Swift import names a
/// module. Today's strategies each yield at most one, but the contract
/// is the set so those languages do not have to change it again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleTarget {
    /// Workspace files this import names. Empty under a real strategy
    /// means the target lives outside the workspace.
    pub files: Vec<FileId>,
    /// The strategy that produced `files`.
    pub strategy: ResolutionStrategy,
}

impl ModuleTarget {
    /// A target for a language that declares no resolver.
    fn unattempted() -> Self {
        ModuleTarget {
            files: Vec::new(),
            strategy: ResolutionStrategy::UnresolvedByDesign,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ImportRule {
    strategy: String,
    #[serde(default)]
    strip_prefixes: Vec<String>,
    #[serde(default)]
    strip_suffixes: Vec<String>,
    #[serde(default)]
    take_first_after_strip: bool,
    #[serde(default)]
    alias_separator: Option<String>,
    #[serde(default)]
    separator: Option<String>,
    #[serde(default)]
    replace_separator: Option<String>,
    #[serde(default)]
    candidate_suffixes: Vec<String>,
    #[serde(default)]
    allow_suffix_match: bool,
    #[serde(default)]
    last_segment_only: bool,
    #[serde(default)]
    camel_to_snake: bool,
    #[serde(default)]
    require_prefixes: Vec<String>,
    #[serde(default)]
    candidate_extensions: Vec<String>,
    #[serde(default)]
    index_fallbacks: Vec<String>,
    #[serde(default)]
    skip_if_contains_angle_bracket: bool,
    #[serde(default)]
    require_literal_contains: Vec<String>,
    #[serde(default)]
    relative_only_if_contains: Vec<String>,
    #[serde(default)]
    append_extension_if_missing: Option<String>,
    /// Token returned in the `imports_for` response so callers can render
    /// "use" vs "import" vs "require" vs "include" consistently.
    #[serde(default)]
    kind: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ImportRulesFile {
    languages: HashMap<String, ImportRule>,
}

fn rules() -> &'static HashMap<String, ImportRule> {
    static CELL: OnceLock<HashMap<String, ImportRule>> = OnceLock::new();
    CELL.get_or_init(|| {
        let parsed: ImportRulesFile =
            serde_json::from_str(RULES_JSON).expect("bundled import-rules.json must be valid JSON");
        parsed.languages
    })
}

/// Outcome of resolving the import strings for one file.
#[derive(Debug, Default)]
pub(crate) struct Resolved {
    /// Workspace files these imports name.
    pub resolved: HashSet<FileId>,
    /// Import strings with no workspace target, whatever the reason.
    /// The dep graph stores these verbatim; ask [`strategy_for`] whether
    /// the language even has a resolver, because "tried and found
    /// nothing" and "never tried" both land here.
    pub unresolved: Vec<String>,
}

/// Resolve the import strings for one file against `path_to_id`.
pub(crate) fn resolve(
    imports: &[String],
    from_relative_path: &str,
    language: &str,
    path_to_id: &HashMap<String, FileId>,
) -> Resolved {
    let mut out = Resolved::default();
    for raw in imports {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        let target = resolve_target(trimmed, language, from_relative_path, path_to_id);
        if target.files.is_empty() {
            out.unresolved.push(raw.clone());
        } else {
            out.resolved.extend(target.files);
        }
    }
    out
}

/// Every language this module can extract import strings from. The
/// rules-coverage test walks this list, so a language added to
/// [`import_keywords`] without a rules entry fails loudly instead of
/// quietly extracting imports it can never resolve.
#[cfg(test)]
pub(crate) const EXTRACTABLE_LANGUAGES: &[&str] = &[
    "c",
    "cpp",
    "csharp",
    "elixir",
    "go",
    "harn",
    "haskell",
    "java",
    "javascript",
    "kotlin",
    "lua",
    "php",
    "python",
    "r",
    "ruby",
    "rust",
    "scala",
    "swift",
    "typescript",
    "zig",
];

/// The strategy a language declares, or
/// [`ResolutionStrategy::UnresolvedByDesign`] when it declares none.
pub(crate) fn strategy_for(language: &str) -> ResolutionStrategy {
    rules()
        .get(language)
        .and_then(|r| ResolutionStrategy::parse(&r.strategy))
        .unwrap_or(ResolutionStrategy::UnresolvedByDesign)
}

/// Resolve one import string into a [`ModuleTarget`].
///
/// `from_relative_path` is the importing file, not its directory: Rust
/// module resolution needs the full path to find the crate root, while
/// the relative strategies only need the parent.
pub(crate) fn resolve_target(
    raw: &str,
    language: &str,
    from_relative_path: &str,
    path_to_id: &HashMap<String, FileId>,
) -> ModuleTarget {
    let strategy = strategy_for(language);
    if !strategy.attempts_resolution() {
        return ModuleTarget::unattempted();
    }
    let Some(rule) = rules().get(language) else {
        return ModuleTarget::unattempted();
    };
    let base_dir = parent_relative(from_relative_path);
    let files = match strategy {
        ResolutionStrategy::RustModule => {
            resolve_rust_module(raw.trim(), from_relative_path, path_to_id)
        }
        _ => apply_rule(rule, raw.trim(), &base_dir, path_to_id)
            .map(|id| vec![id])
            .unwrap_or_default(),
    };
    ModuleTarget { files, strategy }
}

/// Lookup the language-specific kind tag (`"use"`, `"import"`, etc.) used
/// in the `imports_for` response. Defaults to `"import"` for unknown
/// languages.
pub(crate) fn import_kind(language: &str) -> &str {
    rules()
        .get(language)
        .and_then(|r| r.kind.as_deref())
        .unwrap_or("import")
}

/// Try to resolve a single import string against `path_to_id`. `base_dir`
/// is the workspace-relative directory of the *importing* file (with `/`
/// separators, no trailing slash); pass an empty string when the resolver
/// shouldn't attempt relative resolution.
pub(crate) fn resolve_module(
    module: &str,
    language: &str,
    from_relative_path: &str,
    path_to_id: &HashMap<String, FileId>,
) -> Option<FileId> {
    resolve_target(module, language, from_relative_path, path_to_id)
        .files
        .into_iter()
        .next()
}

fn apply_rule(
    rule: &ImportRule,
    raw: &str,
    base_dir: &str,
    path_to_id: &HashMap<String, FileId>,
) -> Option<FileId> {
    match rule.strategy.as_str() {
        "dotted" => resolve_dotted(rule, raw, path_to_id),
        "dotted-literal" => {
            let lit = extract_string_literal(raw)?;
            resolve_dotted(rule, &lit, path_to_id)
        }
        "relative" => resolve_relative(rule, raw, base_dir, path_to_id),
        // `rust-module` is anchored on the importing file, so
        // `resolve_target` routes it before reaching this data-driven
        // matcher. `unresolved-by-design` never gets here either.
        _ => None,
    }
}

/// Resolve a Rust `use` / `pub use` path against the importing file's
/// crate.
///
/// Rust module paths are anchored, not relative, so this needs the
/// importing file's own path rather than just its directory:
///
/// - `crate::a::b` anchors on the nearest `src/` ancestor of the
///   importing file, which is that crate's root.
/// - `super::a` anchors on the parent of the importing file's module.
/// - `self::a` anchors on the importing file's own module.
///
/// A module is either `<anchor>/<segments>.rs` or
/// `<anchor>/<segments>/mod.rs`, and the trailing segment of a `use` is
/// usually an *item* rather than a module, so the parent path is tried
/// too. Everything else — `std::`, a third-party crate, `extern crate` —
/// correctly resolves to nothing, because it names no file in this
/// workspace.
fn resolve_rust_module(
    raw: &str,
    from_relative_path: &str,
    path_to_id: &HashMap<String, FileId>,
) -> Vec<FileId> {
    let mut files: Vec<FileId> = Vec::new();
    for use_path in rust_use_paths(raw) {
        if let Some(id) = resolve_one_rust_path(&use_path, from_relative_path, path_to_id) {
            if !files.contains(&id) {
                files.push(id);
            }
        }
    }
    files
}

/// One path named by a `use` statement, after brace expansion.
struct RustUsePath {
    /// `::`-joined path with `use` / `pub use` and any rename removed.
    path: String,
    /// True when the path is certainly a module rather than possibly an
    /// item, which is the case for a glob (`use a::b::*`). The trailing
    /// segment must then not be dropped.
    exact_module: bool,
}

fn resolve_one_rust_path(
    use_path: &RustUsePath,
    from_relative_path: &str,
    path_to_id: &HashMap<String, FileId>,
) -> Option<FileId> {
    let mut segments: Vec<&str> = use_path
        .path
        .split("::")
        .filter(|s| !s.is_empty())
        .collect();
    if segments.is_empty() {
        return None;
    }

    let (self_anchor, super_anchor) = rust_module_anchors(from_relative_path);
    let anchor = match segments[0] {
        "crate" => {
            segments.remove(0);
            rust_crate_root(from_relative_path)?
        }
        "self" => {
            segments.remove(0);
            self_anchor
        }
        "super" => {
            // The first `super` names the parent module; each further one
            // climbs another level.
            segments.remove(0);
            let mut dir = super_anchor;
            while segments.first() == Some(&"super") {
                segments.remove(0);
                dir = parent_relative(&dir);
            }
            dir
        }
        // `std::`, `core::`, `alloc::`, a third-party crate, or another
        // workspace crate. Cross-crate resolution needs a crate-name to
        // path map that this slice does not build.
        _ => return None,
    };

    // A glob already names the module, so its path is exact. Anything
    // else may end in an item name (`use crate::a::b::Thing`) more often
    // than a module, so the parent is tried as well.
    let takes: Vec<usize> = if use_path.exact_module {
        vec![segments.len()]
    } else {
        vec![segments.len(), segments.len().saturating_sub(1)]
    };
    for take in takes {
        if take == 0 {
            // `use super::*` with nothing left: the anchor directory is
            // itself the module.
            if use_path.exact_module {
                for candidate in [format!("{anchor}.rs"), format!("{anchor}/mod.rs")] {
                    if let Some(id) = path_to_id.get(&candidate) {
                        return Some(*id);
                    }
                }
            }
            continue;
        }
        let joined = segments[..take].join("/");
        let base = if anchor.is_empty() {
            joined
        } else {
            format!("{anchor}/{joined}")
        };
        for candidate in [format!("{base}.rs"), format!("{base}/mod.rs")] {
            if let Some(id) = path_to_id.get(&candidate) {
                return Some(*id);
            }
        }
    }
    None
}

/// Expand a `use` statement into the paths it names.
///
/// One statement is not one path. `use a::{b, c}` names two, `use a::*`
/// names the module `a`, and `use a::b as c` names `a::b` under another
/// name. Returning a list is why [`ModuleTarget::files`] is a set.
///
/// A nested brace group (`use a::{b::{c, d}, e}`) is rare enough that it
/// is skipped rather than parsed; it resolves to nothing instead of to
/// something wrong.
fn rust_use_paths(raw: &str) -> Vec<RustUsePath> {
    let trimmed = raw.trim();
    let mut rest = trimmed;
    let mut matched = false;
    for prefix in ["pub use ", "use "] {
        if let Some(stripped) = rest.strip_prefix(prefix) {
            rest = stripped;
            matched = true;
            break;
        }
    }
    if !matched {
        // `extern crate foo;` and anything else names no workspace file.
        return Vec::new();
    }
    let rest = rest.trim().trim_end_matches(';').trim();
    if rest.is_empty() {
        return Vec::new();
    }

    let Some((before, after_open)) = rest.split_once('{') else {
        return vec![single_rust_use_path(rest)];
    };
    let Some((inner, _)) = after_open.rsplit_once('}') else {
        return Vec::new();
    };
    if inner.contains('{') {
        // Nested group: decline rather than mis-parse.
        return Vec::new();
    }
    let prefix = before.trim().trim_end_matches("::").trim();
    inner
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .filter_map(|item| {
            let item = strip_rename(item);
            if item == "self" {
                if prefix.is_empty() {
                    return None;
                }
                return Some(RustUsePath {
                    path: prefix.to_string(),
                    exact_module: true,
                });
            }
            if item == "*" {
                if prefix.is_empty() {
                    return None;
                }
                return Some(RustUsePath {
                    path: prefix.to_string(),
                    exact_module: true,
                });
            }
            let joined = if prefix.is_empty() {
                item.to_string()
            } else {
                format!("{prefix}::{item}")
            };
            Some(single_rust_use_path(&joined))
        })
        .collect()
}

/// Classify one brace-free path, treating a trailing `::*` as naming the
/// module it globs.
fn single_rust_use_path(raw: &str) -> RustUsePath {
    let raw = strip_rename(raw.trim());
    match raw.strip_suffix("::*") {
        Some(module) => RustUsePath {
            path: module.to_string(),
            exact_module: true,
        },
        None => RustUsePath {
            path: raw.trim_end_matches("::").to_string(),
            exact_module: false,
        },
    }
}

/// Drop an ` as alias` suffix; the path before it is what names a file.
fn strip_rename(item: &str) -> &str {
    match item.split_once(" as ") {
        Some((path, _alias)) => path.trim(),
        None => item,
    }
}

/// Where `self::` and `super::` resolve from, for a file at
/// `from_relative_path`.
///
/// Rust's module tree is not the directory tree. A file module owns a
/// directory named after itself, so `src/deep/leaf.rs` is the module
/// `crate::deep::leaf` and its children live in `src/deep/leaf/`, while
/// `src/deep/mod.rs` *is* `crate::deep` and its children live in
/// `src/deep/`. Getting this wrong resolves `super::util` from
/// `deep/leaf.rs` to `src/util.rs` when the language means
/// `src/deep/util.rs` — a confident edge to the wrong file.
///
/// Returns `(self_anchor, super_anchor)`: the directory this module's own
/// children live in, and the one its parent's children live in.
fn rust_module_anchors(from_relative_path: &str) -> (String, String) {
    let dir = parent_relative(from_relative_path);
    let basename = from_relative_path
        .rsplit_once('/')
        .map(|(_, b)| b)
        .unwrap_or(from_relative_path);
    // `mod.rs` is its directory's module; `main.rs` and `lib.rs` are the
    // crate root and own `src/` directly. Every other file owns a
    // same-named subdirectory.
    let owns_its_directory = matches!(basename, "mod.rs" | "main.rs" | "lib.rs");
    if owns_its_directory {
        (dir.clone(), parent_relative(&dir))
    } else {
        let stem = basename.strip_suffix(".rs").unwrap_or(basename);
        let self_anchor = if dir.is_empty() {
            stem.to_string()
        } else {
            format!("{dir}/{stem}")
        };
        (self_anchor, dir)
    }
}

/// The crate root for a file, i.e. the nearest ancestor directory named
/// `src`. `crates/harn-hostlib/src/code_index/imports.rs` anchors on
/// `crates/harn-hostlib/src`. A file outside any `src/` has no crate
/// root this resolver can name.
fn rust_crate_root(from_relative_path: &str) -> Option<String> {
    let segments: Vec<&str> = from_relative_path.split('/').collect();
    let idx = segments.iter().rposition(|s| *s == "src")?;
    Some(segments[..=idx].join("/"))
}

fn resolve_dotted(
    rule: &ImportRule,
    raw: &str,
    path_to_id: &HashMap<String, FileId>,
) -> Option<FileId> {
    let mut cleaned = raw.to_string();
    for prefix in &rule.strip_prefixes {
        if let Some(stripped) = cleaned.strip_prefix(prefix) {
            cleaned = stripped.to_string();
        }
    }
    // Second pass — handles chained prefixes (e.g. `import qualified`).
    for prefix in &rule.strip_prefixes {
        if let Some(stripped) = cleaned.strip_prefix(prefix) {
            cleaned = stripped.to_string();
        }
    }
    for suffix in &rule.strip_suffixes {
        if cleaned.ends_with(suffix) {
            cleaned.truncate(cleaned.len() - suffix.len());
        }
    }
    if let Some(alias) = rule.alias_separator.as_deref() {
        if let Some(idx) = cleaned.find(alias) {
            cleaned.truncate(idx);
        }
    }
    cleaned = cleaned.trim().to_string();
    if rule.take_first_after_strip {
        cleaned = cleaned
            .split_whitespace()
            .next()
            .unwrap_or(&cleaned)
            .to_string();
        cleaned = cleaned.split(',').next().unwrap_or(&cleaned).to_string();
    }
    if cleaned.is_empty() {
        return None;
    }
    let mut candidate = cleaned;
    let separator = rule.separator.as_deref().unwrap_or(".");
    if rule.last_segment_only {
        if let Some(last) = candidate.split(separator).last() {
            candidate = last.to_string();
        }
    }
    if rule.camel_to_snake {
        candidate = to_snake_case(&candidate);
    }
    let replace = rule.replace_separator.as_deref().unwrap_or("/");
    let joined = candidate.replace(separator, replace);

    for suffix in &rule.candidate_suffixes {
        let needle = format!("{joined}{suffix}");
        if let Some(id) = path_to_id.get(&needle) {
            return Some(*id);
        }
        if rule.allow_suffix_match {
            for (path, id) in path_to_id {
                if path.ends_with(&format!("/{needle}")) || path == &needle {
                    return Some(*id);
                }
            }
        }
    }
    None
}

fn resolve_relative(
    rule: &ImportRule,
    raw: &str,
    base_dir: &str,
    path_to_id: &HashMap<String, FileId>,
) -> Option<FileId> {
    if !rule.relative_only_if_contains.is_empty()
        && !rule
            .relative_only_if_contains
            .iter()
            .any(|n| raw.contains(n))
    {
        return None;
    }
    if rule.skip_if_contains_angle_bracket && raw.contains('<') {
        return None;
    }
    let mut literal = extract_string_literal(raw).unwrap_or_else(|| raw.to_string());
    if !rule.require_literal_contains.is_empty()
        && !rule
            .require_literal_contains
            .iter()
            .any(|n| literal.contains(n))
    {
        return None;
    }
    if !rule.require_prefixes.is_empty()
        && !rule.require_prefixes.iter().any(|p| literal.starts_with(p))
    {
        return None;
    }
    if let Some(ext) = rule.append_extension_if_missing.as_deref() {
        if !literal.ends_with(&format!(".{ext}")) {
            literal = format!("{literal}.{ext}");
        }
    }
    let joined = if base_dir.is_empty() {
        literal.clone()
    } else {
        format!("{base_dir}/{literal}")
    };
    let normalized = normalize_relative(&joined);
    if let Some(id) = path_to_id.get(&normalized) {
        return Some(*id);
    }
    for ext in &rule.candidate_extensions {
        if let Some(id) = path_to_id.get(&format!("{normalized}.{ext}")) {
            return Some(*id);
        }
    }
    for fallback in &rule.index_fallbacks {
        let candidate = if normalized.is_empty() {
            fallback.clone()
        } else {
            format!("{normalized}/{fallback}")
        };
        if let Some(id) = path_to_id.get(&candidate) {
            return Some(*id);
        }
    }
    if rule.allow_suffix_match {
        for (path, id) in path_to_id {
            if path.ends_with(&format!("/{literal}")) {
                return Some(*id);
            }
        }
    }
    None
}

#[expect(
    clippy::string_slice,
    reason = "both bounds are positions of ASCII quote bytes, so they are char boundaries"
)]
fn extract_string_literal(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let first = bytes.iter().position(|b| *b == b'"' || *b == b'\'')?;
    let quote = bytes[first];
    let after = first + 1;
    let second_offset = bytes[after..].iter().position(|b| *b == quote)?;
    let second = after + second_offset;
    Some(text[after..second].to_string())
}

fn parent_relative(rel: &str) -> String {
    match rel.rsplit_once('/') {
        Some((parent, _)) => parent.to_string(),
        None => String::new(),
    }
}

fn normalize_relative(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for comp in path.split('/').filter(|c| !c.is_empty()) {
        match comp {
            "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    parts.join("/")
}

#[cfg(test)]
mod tests {
    /// Every language we can extract imports from must declare a
    /// strategy, and every declared strategy must parse. A language that
    /// extracts imports it can never resolve is the exact shape of
    /// #8114: the extractor worked, the resolver was `noop`, and the
    /// aggregate read zero for years.
    #[test]
    fn every_extractable_language_declares_a_parsable_strategy() {
        for language in EXTRACTABLE_LANGUAGES {
            assert!(
                !import_keywords(language).is_empty(),
                "{language} is listed as extractable but has no keywords"
            );
            let rule = rules()
                .get(*language)
                .unwrap_or_else(|| panic!("{language} extracts imports but declares no rule"));
            assert!(
                ResolutionStrategy::parse(&rule.strategy).is_some(),
                "{language} declares unknown strategy `{}`",
                rule.strategy
            );
        }
        // Negative control: the check can fail. An unknown spelling must
        // not quietly become "resolves nothing".
        assert!(ResolutionStrategy::parse("noop").is_none());
    }

    /// The distinction the whole contract exists for. Both of these
    /// resolve to zero files; only one of them tried.
    #[test]
    fn no_resolver_and_no_match_are_different_answers() {
        let mut paths: HashMap<String, FileId> = HashMap::new();
        paths.insert("src/util.rs".to_string(), 2);

        let unattempted = resolve_target("import Foundation", "swift", "src/a.swift", &paths);
        assert_eq!(unattempted.strategy, ResolutionStrategy::UnresolvedByDesign);
        assert!(unattempted.files.is_empty());
        assert!(!unattempted.strategy.attempts_resolution());

        let attempted = resolve_target("use std::fmt::Debug;", "rust", "src/main.rs", &paths);
        assert_eq!(attempted.strategy, ResolutionStrategy::RustModule);
        assert!(attempted.files.is_empty());
        assert!(
            attempted.strategy.attempts_resolution(),
            "a std:: import was tried and genuinely names no workspace file"
        );
    }

    fn rust_paths() -> HashMap<String, FileId> {
        let mut m: HashMap<String, FileId> = HashMap::new();
        m.insert("crates/pkg/src/util.rs".to_string(), 1);
        m.insert("crates/pkg/src/deep/mod.rs".to_string(), 2);
        m.insert("crates/pkg/src/deep/leaf.rs".to_string(), 3);
        m.insert("crates/pkg/src/main.rs".to_string(), 4);
        m.insert("crates/pkg/src/deep/sibling.rs".to_string(), 5);
        m.insert("crates/pkg/src/util/inner.rs".to_string(), 6);
        m
    }

    #[test]
    fn rust_crate_path_anchors_on_the_nearest_src_ancestor() {
        let paths = rust_paths();
        // The item `helper` is not a module; the parent path is.
        let t = resolve_target(
            "use crate::util::helper;",
            "rust",
            "crates/pkg/src/main.rs",
            &paths,
        );
        assert_eq!(t.files, vec![1]);
        // Anchoring is on the importer's crate, from a nested module too.
        let t = resolve_target(
            "use crate::util;",
            "rust",
            "crates/pkg/src/deep/leaf.rs",
            &paths,
        );
        assert_eq!(t.files, vec![1]);
    }

    #[test]
    fn rust_resolves_a_directory_module_through_mod_rs() {
        let paths = rust_paths();
        let t = resolve_target(
            "use crate::deep::leaf;",
            "rust",
            "crates/pkg/src/main.rs",
            &paths,
        );
        assert_eq!(t.files, vec![3], "a file module wins over its parent");
        let t = resolve_target("use crate::deep;", "rust", "crates/pkg/src/main.rs", &paths);
        assert_eq!(t.files, vec![2], "a directory module resolves via mod.rs");
    }

    /// `super` is the parent *module*, which is not the parent
    /// directory. From `deep/leaf.rs`, `super::x` means
    /// `crate::deep::x`, so it must find `deep/sibling.rs` and must not
    /// reach `src/util.rs` one level further up.
    #[test]
    fn rust_super_names_the_parent_module_not_the_parent_directory() {
        let paths = rust_paths();
        let t = resolve_target(
            "use super::sibling;",
            "rust",
            "crates/pkg/src/deep/leaf.rs",
            &paths,
        );
        assert_eq!(
            t.files,
            vec![5],
            "`super::sibling` is `crate::deep::sibling`"
        );

        let t = resolve_target(
            "use super::util;",
            "rust",
            "crates/pkg/src/deep/leaf.rs",
            &paths,
        );
        assert!(
            t.files.is_empty(),
            "`crate::deep::util` does not exist; `crate::util` is one level too far"
        );

        // From `deep/mod.rs`, which *is* `crate::deep`, `super` is the
        // crate root, so the same name now does resolve.
        let t = resolve_target(
            "use super::util;",
            "rust",
            "crates/pkg/src/deep/mod.rs",
            &paths,
        );
        assert_eq!(t.files, vec![1]);
    }

    #[test]
    fn rust_self_names_the_importing_modules_own_children() {
        let paths = rust_paths();
        // `deep/mod.rs` is `crate::deep`; its children live in `deep/`.
        let t = resolve_target(
            "use self::leaf;",
            "rust",
            "crates/pkg/src/deep/mod.rs",
            &paths,
        );
        assert_eq!(t.files, vec![3]);
        // `util.rs` is a file module, so its children live in `util/`.
        let t = resolve_target("use self::inner;", "rust", "crates/pkg/src/util.rs", &paths);
        assert_eq!(t.files, vec![6]);
    }

    #[test]
    fn rust_declines_what_names_nothing_in_this_workspace() {
        let paths = rust_paths();
        for raw in [
            "use std::collections::HashMap;",
            "use serde::Serialize;",
            "extern crate alloc;",
            // A nested brace group is declined rather than mis-parsed.
            "use crate::util::{a::{b, c}, d};",
        ] {
            let t = resolve_target(raw, "rust", "crates/pkg/src/main.rs", &paths);
            assert!(
                t.files.is_empty(),
                "`{raw}` must name no workspace file, got {:?}",
                t.files
            );
            assert!(
                t.strategy.attempts_resolution(),
                "`{raw}` was still attempted; only the answer is empty"
            );
        }
    }

    /// One `use` statement can name several paths, which is why
    /// `ModuleTarget::files` is a set rather than one optional id.
    #[test]
    fn rust_brace_groups_name_every_path_in_them() {
        let paths = rust_paths();
        let t = resolve_target(
            "use crate::{util, deep};",
            "rust",
            "crates/pkg/src/main.rs",
            &paths,
        );
        let mut got = t.files;
        got.sort_unstable();
        assert_eq!(got, vec![1, 2], "both braced paths must resolve");

        // `self` inside a group names the prefix module itself.
        let t = resolve_target(
            "use crate::deep::{self, leaf};",
            "rust",
            "crates/pkg/src/main.rs",
            &paths,
        );
        let mut got = t.files;
        got.sort_unstable();
        assert_eq!(got, vec![2, 3]);
    }

    #[test]
    fn rust_globs_name_the_module_they_glob() {
        let paths = rust_paths();
        let t = resolve_target(
            "use crate::deep::*;",
            "rust",
            "crates/pkg/src/main.rs",
            &paths,
        );
        assert_eq!(t.files, vec![2], "a glob names the module, not a parent");
        // The very common test-module form.
        let t = resolve_target(
            "use super::*;",
            "rust",
            "crates/pkg/src/deep/leaf.rs",
            &paths,
        );
        assert_eq!(t.files, vec![2], "`super::*` names the parent module");
    }

    #[test]
    fn rust_renames_resolve_to_the_path_before_the_alias() {
        let paths = rust_paths();
        let t = resolve_target(
            "use crate::util as helpers;",
            "rust",
            "crates/pkg/src/main.rs",
            &paths,
        );
        assert_eq!(t.files, vec![1]);
        let t = resolve_target(
            "use crate::{util as u, deep as d};",
            "rust",
            "crates/pkg/src/main.rs",
            &paths,
        );
        let mut got = t.files;
        got.sort_unstable();
        assert_eq!(got, vec![1, 2]);
    }

    #[test]
    fn harn_relative_imports_resolve_and_std_does_not() {
        let mut paths: HashMap<String, FileId> = HashMap::new();
        paths.insert("pipelines/lib/host/runtime.harn".to_string(), 7);

        let t = resolve_target(
            "import { host_set_result } from \"../host/runtime\"",
            "harn",
            "pipelines/lib/agent/loop.harn",
            &paths,
        );
        assert_eq!(t.files, vec![7]);
        assert_eq!(t.strategy, ResolutionStrategy::Relative);

        let bare = resolve_target(
            "import \"../host/runtime\"",
            "harn",
            "pipelines/lib/agent/loop.harn",
            &paths,
        );
        assert_eq!(bare.files, vec![7], "a side-effect import resolves too");

        let std_import = resolve_target(
            "import { with_temp_dir } from \"std/testing\"",
            "harn",
            "pipelines/lib/agent/loop.harn",
            &paths,
        );
        assert!(std_import.files.is_empty());
        assert!(
            std_import.strategy.attempts_resolution(),
            "std/ was tried and correctly names nothing in the workspace"
        );
    }

    #[test]
    fn harn_import_strings_are_extracted_at_all() {
        let src = "import { a } from \"../x\"\nimport \"./y\"\nfn main() {}\n";
        let found = extract_imports(src, "harn");
        assert_eq!(found.len(), 2, "got {found:?}");
    }

    use super::*;

    fn ids() -> HashMap<String, FileId> {
        let mut m = HashMap::new();
        m.insert("src/util.ts".into(), 1);
        m.insert("src/index.ts".into(), 2);
        m.insert("foo/bar.py".into(), 3);
        m.insert("com/example/Foo.java".into(), 4);
        m.insert("Bar.cs".into(), 5);
        m
    }

    #[test]
    fn extract_handles_swift_and_rust() {
        let swift = "import Foundation\nimport SwiftUI\nlet x = 1\n";
        assert_eq!(
            extract_imports(swift, "swift"),
            vec!["import Foundation", "import SwiftUI"]
        );
        let rust = "use std::sync::Arc;\nuse crate::foo;\nfn main() {}";
        assert_eq!(
            extract_imports(rust, "rust"),
            vec!["use std::sync::Arc;", "use crate::foo;"]
        );
    }

    #[test]
    fn resolve_typescript_relative() {
        let map = ids();
        let r = resolve(
            &["import x from \"./util\"".to_string()],
            "src/index.ts",
            "typescript",
            &map,
        );
        assert!(r.resolved.contains(&1));
    }

    #[test]
    fn resolve_python_dotted() {
        let map = ids();
        // Python rule resolves the *package* (`foo.bar` -> `foo/bar.py`),
        // not the imported symbol.
        let r = resolve(
            &["from foo.bar import baz".to_string()],
            "src/main.py",
            "python",
            &map,
        );
        assert!(r.resolved.contains(&3));
        let r = resolve(
            &["import foo.bar".to_string()],
            "src/main.py",
            "python",
            &map,
        );
        assert!(r.resolved.contains(&3));
    }

    #[test]
    fn resolve_java_suffix_match() {
        let map = ids();
        let r = resolve(
            &["import com.example.Foo;".to_string()],
            "src/Main.java",
            "java",
            &map,
        );
        assert!(r.resolved.contains(&4));
    }

    #[test]
    fn unresolved_imports_are_kept() {
        let map = ids();
        let r = resolve(
            &["import com.unknown.Foo;".to_string()],
            "src/Main.java",
            "java",
            &map,
        );
        assert!(r.resolved.is_empty());
        assert_eq!(r.unresolved, vec!["import com.unknown.Foo;".to_string()]);
    }

    #[test]
    fn comment_lines_are_skipped() {
        let src = "// import Foundation\nimport UIKit\n# import Foo\n";
        assert_eq!(extract_imports(src, "swift"), vec!["import UIKit"]);
    }

    #[test]
    fn import_kind_falls_back_to_import() {
        assert_eq!(import_kind("rust"), "use");
        assert_eq!(import_kind("c"), "include");
        assert_eq!(import_kind("totally-unknown"), "import");
    }
}
