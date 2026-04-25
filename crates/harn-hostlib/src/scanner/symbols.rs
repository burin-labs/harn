//! Symbol extraction for the scanner pipeline. Ports the regex/generic
//! arms of `Sources/BurinCore/Scanner/SymbolExtractor.swift` plus the
//! pragma extractor from `PragmaExtractor.swift`.
//!
//! The Swift implementation routes most languages through tree-sitter and
//! falls back to the regex extractor for Swift, Shell, Dart, and unknown
//! languages. This Rust scanner keeps the same public record shape so the
//! orchestrator in [`super::run_scan`] can swap extraction engines without
//! disturbing callers.
//!
//! The output shape — `Vec<SymbolRecord>` — is the canonical scanner
//! representation used by the scanner orchestrator.

use regex::Regex;
use std::sync::OnceLock;

use crate::scanner::result::{SymbolKind, SymbolRecord};

/// Extract every symbol the regex pipeline can recognize from `content`.
///
/// `language` is the lowercase file extension; `file_path` is the
/// repo-relative POSIX path (used to build stable symbol ids).
pub fn extract_symbols(content: &str, language: &str, file_path: &str) -> Vec<SymbolRecord> {
    let mut symbols = Vec::new();
    let mut current_container: Option<String> = None;
    let mut brace_depth: i64 = 0;

    for (idx, line) in content.lines().enumerate() {
        let trimmed = line.trim();

        if let Some(pragma) = extract_pragma(trimmed, language) {
            let signature = if pragma.has_separator {
                format!("--- {} ---", pragma.name)
            } else {
                pragma.name.clone()
            };
            symbols.push(make_symbol(
                pragma.name,
                pragma.kind,
                signature,
                idx + 1,
                file_path,
                None,
            ));
        }

        if trimmed.is_empty()
            || trimmed.starts_with("//")
            || trimmed.starts_with("/*")
            || trimmed.starts_with('*')
        {
            continue;
        }

        let opens = trimmed.bytes().filter(|b| *b == b'{').count() as i64;
        let closes = trimmed.bytes().filter(|b| *b == b'}').count() as i64;
        brace_depth += opens - closes;
        if brace_depth <= 0 {
            current_container = None;
            brace_depth = 0;
        }

        let extracted = extract_from_line(trimmed, language, idx + 1, file_path);
        for mut sym in extracted {
            if matches!(
                sym.kind,
                SymbolKind::Function | SymbolKind::Method | SymbolKind::Property
            ) {
                sym.container = current_container.clone();
            }
            if sym.kind.is_type_definition() {
                current_container = Some(sym.name.clone());
            }
            symbols.push(sym);
        }
    }

    symbols
}

/// Build a `SymbolRecord` with the canonical id format.
pub(super) fn make_symbol(
    name: String,
    kind: SymbolKind,
    signature: String,
    line: usize,
    file_path: &str,
    container: Option<String>,
) -> SymbolRecord {
    SymbolRecord {
        id: format!("{file_path}:{name}:{line}"),
        name,
        kind,
        file_path: file_path.to_string(),
        line,
        signature,
        container,
        reference_count: 0,
        importance_score: 0.0,
    }
}

fn extract_from_line(
    line: &str,
    language: &str,
    line_number: usize,
    file_path: &str,
) -> Vec<SymbolRecord> {
    match language {
        "dart" => extract_dart(line, line_number, file_path),
        _ => extract_generic(line, line_number, file_path),
    }
}

// MARK: - Dart (no tree-sitter grammar available on the Swift side)
fn extract_dart(line: &str, line_number: usize, file_path: &str) -> Vec<SymbolRecord> {
    let mut out = Vec::new();
    if let Some(name) = capture(dart_class(), line) {
        out.push(make_symbol(
            name.clone(),
            SymbolKind::ClassDecl,
            format!("class {name}"),
            line_number,
            file_path,
            None,
        ));
        return out;
    }
    if let Some(name) = capture(dart_enum(), line) {
        out.push(make_symbol(
            name.clone(),
            SymbolKind::EnumDecl,
            format!("enum {name}"),
            line_number,
            file_path,
            None,
        ));
        return out;
    }
    if let Some(caps) = dart_function().captures(line) {
        let name = caps.get(1).map(|m| m.as_str()).unwrap_or_default();
        let rest = caps.get(2).map(|m| m.as_str()).unwrap_or_default();
        const RESERVED: &[&str] = &[
            "if", "while", "for", "switch", "return", "catch", "class", "import", "export",
        ];
        if !RESERVED.contains(&name)
            && name
                .chars()
                .next()
                .map(|c| c.is_ascii_lowercase())
                .unwrap_or(false)
        {
            let signature = format!("{name}({}", truncate_signature(rest, 80));
            out.push(make_symbol(
                name.to_string(),
                SymbolKind::Function,
                signature,
                line_number,
                file_path,
                None,
            ));
        }
    }
    out
}

// MARK: - Generic regex fallback (matches Swift `extractGeneric`)
fn extract_generic(line: &str, line_number: usize, file_path: &str) -> Vec<SymbolRecord> {
    if let Some(name) = capture(generic_function(), line) {
        return vec![make_symbol(
            name,
            SymbolKind::Function,
            String::new(),
            line_number,
            file_path,
            None,
        )];
    }
    if let Some(name) = capture(generic_struct(), line) {
        return vec![make_symbol(
            name,
            SymbolKind::StructDecl,
            String::new(),
            line_number,
            file_path,
            None,
        )];
    }
    if let Some(name) = capture(generic_enum(), line) {
        return vec![make_symbol(
            name,
            SymbolKind::EnumDecl,
            String::new(),
            line_number,
            file_path,
            None,
        )];
    }
    if let Some(name) = capture(generic_protocol(), line) {
        return vec![make_symbol(
            name,
            SymbolKind::ProtocolDecl,
            String::new(),
            line_number,
            file_path,
            None,
        )];
    }
    if let Some(name) = capture(generic_class(), line) {
        return vec![make_symbol(
            name,
            SymbolKind::ClassDecl,
            String::new(),
            line_number,
            file_path,
            None,
        )];
    }
    Vec::new()
}

// MARK: - Pragma extraction

#[derive(Clone, Debug)]
struct PragmaMatch {
    name: String,
    kind: SymbolKind,
    has_separator: bool,
}

struct PragmaPattern {
    prefix: &'static str,
    kind: SymbolKind,
    has_separator: bool,
}

const SLASH_PATTERNS: &[PragmaPattern] = &[
    PragmaPattern {
        prefix: "// MARK: - ",
        kind: SymbolKind::Mark,
        has_separator: true,
    },
    PragmaPattern {
        prefix: "// MARK: -",
        kind: SymbolKind::Mark,
        has_separator: true,
    },
    PragmaPattern {
        prefix: "// MARK: ",
        kind: SymbolKind::Mark,
        has_separator: false,
    },
    PragmaPattern {
        prefix: "// MARK:",
        kind: SymbolKind::Mark,
        has_separator: false,
    },
    PragmaPattern {
        prefix: "// TODO: ",
        kind: SymbolKind::Todo,
        has_separator: false,
    },
    PragmaPattern {
        prefix: "// TODO:",
        kind: SymbolKind::Todo,
        has_separator: false,
    },
    PragmaPattern {
        prefix: "// FIXME: ",
        kind: SymbolKind::Fixme,
        has_separator: false,
    },
    PragmaPattern {
        prefix: "// FIXME:",
        kind: SymbolKind::Fixme,
        has_separator: false,
    },
];

const PRAGMA_PATTERNS: &[PragmaPattern] = &[
    PragmaPattern {
        prefix: "#pragma mark - ",
        kind: SymbolKind::Mark,
        has_separator: true,
    },
    PragmaPattern {
        prefix: "#pragma mark -",
        kind: SymbolKind::Mark,
        has_separator: true,
    },
    PragmaPattern {
        prefix: "#pragma mark ",
        kind: SymbolKind::Mark,
        has_separator: false,
    },
];

const HASH_PATTERNS: &[PragmaPattern] = &[
    PragmaPattern {
        prefix: "# MARK: - ",
        kind: SymbolKind::Mark,
        has_separator: true,
    },
    PragmaPattern {
        prefix: "# MARK: -",
        kind: SymbolKind::Mark,
        has_separator: true,
    },
    PragmaPattern {
        prefix: "# MARK: ",
        kind: SymbolKind::Mark,
        has_separator: false,
    },
    PragmaPattern {
        prefix: "# MARK:",
        kind: SymbolKind::Mark,
        has_separator: false,
    },
    PragmaPattern {
        prefix: "# TODO: ",
        kind: SymbolKind::Todo,
        has_separator: false,
    },
    PragmaPattern {
        prefix: "# TODO:",
        kind: SymbolKind::Todo,
        has_separator: false,
    },
    PragmaPattern {
        prefix: "# FIXME: ",
        kind: SymbolKind::Fixme,
        has_separator: false,
    },
    PragmaPattern {
        prefix: "# FIXME:",
        kind: SymbolKind::Fixme,
        has_separator: false,
    },
];

fn extract_pragma(line: &str, language: &str) -> Option<PragmaMatch> {
    let is_python = language == "py";
    let is_cpp = matches!(language, "cpp" | "cc" | "cxx" | "hpp" | "h" | "c" | "hxx");

    if !is_python {
        if let Some(m) = match_patterns(SLASH_PATTERNS, line) {
            return Some(m);
        }
    }
    if is_cpp {
        if let Some(m) = match_patterns(PRAGMA_PATTERNS, line) {
            return Some(m);
        }
    }
    if is_python {
        if let Some(m) = match_patterns(HASH_PATTERNS, line) {
            return Some(m);
        }
    }
    None
}

fn match_patterns(patterns: &[PragmaPattern], line: &str) -> Option<PragmaMatch> {
    for pat in patterns {
        if let Some(rest) = line.strip_prefix(pat.prefix) {
            let trimmed = rest.trim();
            let name = if trimmed.is_empty() {
                pat.kind.keyword().to_ascii_uppercase()
            } else {
                trimmed.to_string()
            };
            return Some(PragmaMatch {
                name,
                kind: pat.kind,
                has_separator: pat.has_separator,
            });
        }
    }
    None
}

// MARK: - Helpers

fn capture(regex: &Regex, line: &str) -> Option<String> {
    regex.captures(line)?.get(1).map(|m| m.as_str().to_string())
}

fn truncate_signature(rest: &str, max_len: usize) -> String {
    let mut depth = 0i32;
    let mut end = 0usize;
    for (i, ch) in rest.char_indices() {
        if ch == '(' {
            depth += 1;
        }
        if ch == ')' {
            depth -= 1;
            if depth <= 0 {
                end = i + ch.len_utf8();
                break;
            }
        }
    }
    let candidate = if end > 0 { &rest[..end] } else { rest };
    if candidate.chars().count() > max_len {
        let truncated: String = candidate.chars().take(max_len).collect();
        format!("{truncated}…")
    } else {
        candidate.to_string()
    }
}

macro_rules! pattern {
    ($name:ident, $expr:expr) => {
        fn $name() -> &'static Regex {
            static R: OnceLock<Regex> = OnceLock::new();
            R.get_or_init(|| {
                Regex::new($expr).expect(concat!("invalid pattern: ", stringify!($name)))
            })
        }
    };
}

pattern!(generic_function, r"(?:function|func|def|fn)\s+(\w+)");
pattern!(generic_struct, r"(?:^|\s)struct\s+(\w+)");
pattern!(generic_enum, r"(?:^|\s)enum\s+(\w+)");
pattern!(generic_protocol, r"(?:^|\s)(?:protocol|interface)\s+(\w+)");
pattern!(generic_class, r"(?:^|\s)(?:class|trait|type)\s+(\w+)");
pattern!(dart_class, r"^(?:abstract\s+)?class\s+(\w+)");
pattern!(dart_enum, r"^enum\s+(\w+)");
pattern!(dart_function, r"^\s*(?:\w+\s+)?(\w+)\s*\((.*)");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_swift_class_with_method_container() {
        let src = "class Foo {\n    func bar() {}\n}\n";
        let symbols = extract_symbols(src, "swift", "Foo.swift");
        let names: Vec<_> = symbols
            .iter()
            .map(|s| (s.name.as_str(), s.kind, s.container.as_deref()))
            .collect();
        // Class on line 1, function on line 2 with container "Foo"
        assert!(names.contains(&("Foo", SymbolKind::ClassDecl, None)));
        assert!(names.contains(&("bar", SymbolKind::Function, Some("Foo"))));
    }

    #[test]
    fn extracts_pragmas_in_python() {
        let src = "# MARK: - Section\n# TODO: write me\n";
        let symbols = extract_symbols(src, "py", "x.py");
        assert!(symbols.iter().any(|s| s.kind == SymbolKind::Mark));
        assert!(symbols.iter().any(|s| s.kind == SymbolKind::Todo));
    }

    #[test]
    fn extracts_dart_function_with_signature() {
        // The Swift regex extractor matches any `<word> <ident>(` line, so
        // a function-call inside a body shows up as another function entry
        // — match that behavior exactly. Production callers de-dupe at the
        // tree-sitter / outline layer.
        let src = "void greet(String name) {\n  return name.length;\n}\n";
        let symbols = extract_symbols(src, "dart", "main.dart");
        let fns: Vec<_> = symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::Function && s.name == "greet")
            .collect();
        assert_eq!(fns.len(), 1);
        assert!(fns[0].signature.starts_with("greet("));
    }

    #[test]
    fn ids_are_stable_per_file_name_line() {
        let src = "fn one() {}\nfn two() {}\n";
        let symbols = extract_symbols(src, "rs", "lib.rs");
        let ids: Vec<_> = symbols.iter().map(|s| s.id.as_str()).collect();
        assert!(ids.contains(&"lib.rs:one:1"));
        assert!(ids.contains(&"lib.rs:two:2"));
    }
}
