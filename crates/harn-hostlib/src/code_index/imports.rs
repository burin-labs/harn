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

use serde::Deserialize;

use super::file_table::FileId;

const RULES_JSON: &str = include_str!("../../data/code_index_import_rules.json");

/// Per-language extraction prefix keywords. A line whose trimmed start
/// matches any of the keywords is captured as an import. `None` means the
/// language has no fallback extraction.
fn import_keywords(language: &str) -> &'static [&'static str] {
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
    pub resolved: HashSet<FileId>,
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
    let rule = rules().get(language);
    let base_dir = parent_relative(from_relative_path);
    for raw in imports {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(rule) = rule {
            if let Some(id) = apply_rule(rule, trimmed, &base_dir, path_to_id) {
                out.resolved.insert(id);
                continue;
            }
        }
        out.unresolved.push(raw.clone());
    }
    out
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
    base_dir: &str,
    path_to_id: &HashMap<String, FileId>,
) -> Option<FileId> {
    let rule = rules().get(language)?;
    apply_rule(rule, module.trim(), base_dir, path_to_id)
}

/// Compute the workspace-relative parent directory for a relative path.
pub(crate) fn parent_dir(rel: &str) -> String {
    parent_relative(rel)
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
        "noop" => None,
        _ => None,
    }
}

fn resolve_dotted(
    rule: &ImportRule,
    raw: &str,
    path_to_id: &HashMap<String, FileId>,
) -> Option<FileId> {
    let mut cleaned = raw.to_string();
    for prefix in &rule.strip_prefixes {
        if cleaned.starts_with(prefix) {
            cleaned = cleaned[prefix.len()..].to_string();
        }
    }
    // Second pass — handles chained prefixes (e.g. `import qualified`).
    for prefix in &rule.strip_prefixes {
        if cleaned.starts_with(prefix) {
            cleaned = cleaned[prefix.len()..].to_string();
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
        candidate = camel_to_snake(&candidate);
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

fn extract_string_literal(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let first = bytes.iter().position(|b| *b == b'"' || *b == b'\'')?;
    let quote = bytes[first];
    let after = first + 1;
    let second_offset = bytes[after..].iter().position(|b| *b == quote)?;
    let second = after + second_offset;
    Some(text[after..second].to_string())
}

fn camel_to_snake(input: &str) -> String {
    let mut out = String::with_capacity(input.len() + 4);
    for (i, ch) in input.chars().enumerate() {
        if ch.is_uppercase() && i > 0 {
            out.push('_');
        }
        for lc in ch.to_lowercase() {
            out.push(lc);
        }
    }
    out
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
