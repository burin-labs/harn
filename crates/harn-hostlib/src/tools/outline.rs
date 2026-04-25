//! `tools/get_file_outline` — convenience wrapper that returns a
//! structural outline (functions, classes, types) for a single source file.
//!
//! This module uses a small language-agnostic regex-backed extractor for the
//! languages `burin-code` exposes in its outline UI: Swift, Rust, JS/TS,
//! Python, Go, Ruby, Java/Kotlin, C/C++, and shell. The output shape is
//! identical to `ast.outline`'s response schema, so the extractor can be
//! replaced by a tree-sitter implementation without disturbing callers.

use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use harn_vm::VmValue;

use crate::error::HostlibError;
use crate::tools::args::{build_dict, dict_arg, optional_int, require_string, str_value};

const BUILTIN: &str = "hostlib_tools_get_file_outline";

pub(super) fn run(args: &[VmValue]) -> Result<VmValue, HostlibError> {
    let raw = dict_arg(BUILTIN, args)?;
    let dict = raw.as_ref();

    let path_str = require_string(BUILTIN, dict, "path")?;
    let max_depth = optional_int(BUILTIN, dict, "max_depth", 0)?;
    if max_depth < 0 {
        return Err(HostlibError::InvalidParameter {
            builtin: BUILTIN,
            param: "max_depth",
            message: "must be >= 0".to_string(),
        });
    }
    let _ = max_depth;

    let path = PathBuf::from(&path_str);
    let language = detect_language(&path);

    let content = fs::read_to_string(&path).map_err(|err| HostlibError::Backend {
        builtin: BUILTIN,
        message: format!("read `{path_str}`: {err}"),
    })?;

    let items = extract(&content, language);
    let items_list: Vec<VmValue> = items.into_iter().map(item_to_value).collect();

    Ok(build_dict([
        ("path", str_value(&path_str)),
        ("language", str_value(language)),
        ("items", VmValue::List(Rc::new(items_list))),
    ]))
}

fn detect_language(path: &Path) -> &'static str {
    let ext = path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "rs" => "rust",
        "swift" => "swift",
        "go" => "go",
        "py" => "python",
        "js" | "mjs" | "cjs" | "jsx" => "javascript",
        "ts" | "tsx" => "typescript",
        "rb" => "ruby",
        "java" => "java",
        "kt" | "kts" => "kotlin",
        "c" | "h" => "c",
        "cc" | "cpp" | "cxx" | "hpp" | "hxx" => "cpp",
        "sh" | "bash" | "zsh" => "shell",
        "" => "plaintext",
        other => leak_language(other),
    }
}

fn leak_language(s: &str) -> &'static str {
    Box::leak(s.to_string().into_boxed_str())
}

#[derive(Debug)]
struct OutlineItem {
    name: String,
    kind: String,
    start_row: usize,
    end_row: usize,
}

fn extract(source: &str, language: &str) -> Vec<OutlineItem> {
    let lines: Vec<&str> = source.lines().collect();
    let total = lines.len();
    let mut out = Vec::new();

    for (idx, raw_line) in lines.iter().enumerate() {
        let trimmed = raw_line.trim_start();
        if trimmed.is_empty() {
            continue;
        }
        let item = match language {
            "rust" => extract_rust(trimmed, idx, total),
            "swift" => extract_swift(trimmed, idx, total),
            "go" => extract_go(trimmed, idx, total),
            "python" => extract_python(trimmed, idx, total),
            "javascript" | "typescript" => extract_js_ts(trimmed, idx, total),
            "ruby" => extract_ruby(trimmed, idx, total),
            "java" | "kotlin" => extract_jvm(trimmed, idx, total),
            "c" | "cpp" => extract_c_cpp(trimmed, idx, total),
            "shell" => extract_shell(trimmed, idx, total),
            _ => None,
        };
        if let Some(item) = item {
            out.push(item);
        }
    }
    out
}

fn extract_rust(line: &str, idx: usize, total: usize) -> Option<OutlineItem> {
    let prefixes: &[(&str, &str)] = &[
        ("pub fn ", "function"),
        ("pub async fn ", "function"),
        ("pub(crate) fn ", "function"),
        ("pub(crate) async fn ", "function"),
        ("pub(super) fn ", "function"),
        ("fn ", "function"),
        ("async fn ", "function"),
        ("pub struct ", "struct"),
        ("struct ", "struct"),
        ("pub enum ", "enum"),
        ("enum ", "enum"),
        ("pub trait ", "trait"),
        ("trait ", "trait"),
        ("impl ", "impl"),
        ("pub mod ", "module"),
        ("mod ", "module"),
        ("pub type ", "type"),
        ("type ", "type"),
    ];
    for (prefix, kind) in prefixes {
        if let Some(rest) = line.strip_prefix(prefix) {
            let name = take_ident(rest);
            if !name.is_empty() {
                return Some(OutlineItem {
                    name,
                    kind: kind.to_string(),
                    start_row: idx,
                    end_row: idx.min(total.saturating_sub(1)),
                });
            }
        }
    }
    None
}

fn extract_swift(line: &str, idx: usize, total: usize) -> Option<OutlineItem> {
    let prefixes: &[(&str, &str)] = &[
        ("public func ", "function"),
        ("private func ", "function"),
        ("internal func ", "function"),
        ("func ", "function"),
        ("public class ", "class"),
        ("class ", "class"),
        ("public struct ", "struct"),
        ("struct ", "struct"),
        ("public enum ", "enum"),
        ("enum ", "enum"),
        ("public protocol ", "protocol"),
        ("protocol ", "protocol"),
        ("extension ", "extension"),
    ];
    extract_with_prefixes(line, idx, total, prefixes)
}

fn extract_go(line: &str, idx: usize, total: usize) -> Option<OutlineItem> {
    let prefixes: &[(&str, &str)] = &[
        ("func ", "function"),
        ("type ", "type"),
        ("var ", "variable"),
        ("const ", "constant"),
    ];
    extract_with_prefixes(line, idx, total, prefixes)
}

fn extract_python(line: &str, idx: usize, total: usize) -> Option<OutlineItem> {
    let prefixes: &[(&str, &str)] = &[
        ("def ", "function"),
        ("async def ", "function"),
        ("class ", "class"),
    ];
    extract_with_prefixes(line, idx, total, prefixes)
}

fn extract_js_ts(line: &str, idx: usize, total: usize) -> Option<OutlineItem> {
    let prefixes: &[(&str, &str)] = &[
        ("export function ", "function"),
        ("export async function ", "function"),
        ("export default function ", "function"),
        ("function ", "function"),
        ("async function ", "function"),
        ("export class ", "class"),
        ("class ", "class"),
        ("export interface ", "interface"),
        ("interface ", "interface"),
        ("export type ", "type"),
        ("type ", "type"),
        ("export enum ", "enum"),
        ("enum ", "enum"),
    ];
    extract_with_prefixes(line, idx, total, prefixes)
}

fn extract_ruby(line: &str, idx: usize, total: usize) -> Option<OutlineItem> {
    let prefixes: &[(&str, &str)] = &[
        ("def ", "function"),
        ("class ", "class"),
        ("module ", "module"),
    ];
    extract_with_prefixes(line, idx, total, prefixes)
}

fn extract_jvm(line: &str, idx: usize, total: usize) -> Option<OutlineItem> {
    let prefixes: &[(&str, &str)] = &[
        ("public class ", "class"),
        ("class ", "class"),
        ("public interface ", "interface"),
        ("interface ", "interface"),
        ("public enum ", "enum"),
        ("enum ", "enum"),
        ("fun ", "function"),
        ("object ", "object"),
    ];
    extract_with_prefixes(line, idx, total, prefixes)
}

fn extract_c_cpp(line: &str, idx: usize, total: usize) -> Option<OutlineItem> {
    let prefixes: &[(&str, &str)] = &[
        ("class ", "class"),
        ("struct ", "struct"),
        ("namespace ", "namespace"),
        ("template ", "template"),
        ("typedef ", "type"),
    ];
    extract_with_prefixes(line, idx, total, prefixes)
}

fn extract_shell(line: &str, idx: usize, total: usize) -> Option<OutlineItem> {
    if let Some(rest) = line.strip_prefix("function ") {
        let name = take_ident(rest);
        if !name.is_empty() {
            return Some(OutlineItem {
                name,
                kind: "function".to_string(),
                start_row: idx,
                end_row: idx.min(total.saturating_sub(1)),
            });
        }
    }
    if let Some((name, rest)) = line.split_once("()") {
        let trimmed_rest = rest.trim();
        if trimmed_rest.starts_with('{') || trimmed_rest.is_empty() {
            let candidate: String = name.trim().chars().take_while(is_ident_char).collect();
            if !candidate.is_empty() {
                return Some(OutlineItem {
                    name: candidate,
                    kind: "function".to_string(),
                    start_row: idx,
                    end_row: idx.min(total.saturating_sub(1)),
                });
            }
        }
    }
    None
}

fn extract_with_prefixes(
    line: &str,
    idx: usize,
    total: usize,
    prefixes: &[(&str, &str)],
) -> Option<OutlineItem> {
    for (prefix, kind) in prefixes {
        if let Some(rest) = line.strip_prefix(prefix) {
            let name = take_ident(rest);
            if !name.is_empty() {
                return Some(OutlineItem {
                    name,
                    kind: kind.to_string(),
                    start_row: idx,
                    end_row: idx.min(total.saturating_sub(1)),
                });
            }
        }
    }
    None
}

fn take_ident(s: &str) -> String {
    s.chars().take_while(is_ident_char).collect()
}

fn is_ident_char(c: &char) -> bool {
    c.is_alphanumeric() || *c == '_'
}

fn item_to_value(item: OutlineItem) -> VmValue {
    let OutlineItem {
        name,
        kind,
        start_row,
        end_row,
    } = item;
    build_dict([
        ("name", str_value(&name)),
        ("kind", str_value(&kind)),
        ("start_row", VmValue::Int(start_row as i64)),
        ("end_row", VmValue::Int(end_row as i64)),
        ("children", VmValue::List(Rc::new(Vec::new()))),
    ])
}
