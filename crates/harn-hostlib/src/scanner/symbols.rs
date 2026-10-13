//! Symbol extraction for the scanner pipeline.
//!
//! Every language with a tree-sitter grammar goes through the same
//! extractor as `ast.symbols`. [`regex_fallback_symbols`] is the one
//! line-oriented fallback, used only for languages the registry knows but
//! no grammar covers in this build (Dart, Objective-C, Vue, …). Section
//! pragmas (`// MARK:`, `# TODO:`, `#pragma mark`) are comment conventions
//! rather than syntax, so they are matched per line for every language.
//!
//! The output shape — `Vec<SymbolRecord>` — is the canonical scanner
//! representation used by the scanner orchestrator.

use harn_languages::Language;
use regex::Regex;
use std::sync::OnceLock;

use crate::scanner::result::{SymbolKind, SymbolRecord};

/// Extract every symbol in `content`.
///
/// `file_path` is the repo-relative POSIX path (used to build stable symbol
/// ids).
pub fn extract_symbols(content: &str, language: Language, file_path: &str) -> Vec<SymbolRecord> {
    let mut symbols = pragma_symbols(content, language, file_path);
    match structural_symbols(content, language, file_path) {
        Some(found) => symbols.extend(found),
        None => symbols.extend(regex_fallback_symbols(content, language, file_path)),
    }
    symbols.sort_by_key(|symbol| symbol.line);
    symbols
}

/// Tree-sitter symbols, or `None` when no grammar is bound for `language`
/// in this build.
#[cfg(feature = "ast")]
fn structural_symbols(
    content: &str,
    language: Language,
    file_path: &str,
) -> Option<Vec<SymbolRecord>> {
    use crate::ast::SymbolKind as AstKind;

    let grammar = crate::ast::Language::from_registry(language)?;
    grammar.ts_language()?;
    let symbols = crate::ast::api::symbols_from_source(content, grammar).ok()?;
    Some(
        symbols
            .into_iter()
            .map(|symbol| {
                let kind = match symbol.kind {
                    AstKind::Function => SymbolKind::Function,
                    AstKind::Method => SymbolKind::Method,
                    AstKind::Class => SymbolKind::ClassDecl,
                    AstKind::Struct => SymbolKind::StructDecl,
                    AstKind::Enum => SymbolKind::EnumDecl,
                    AstKind::Interface => SymbolKind::InterfaceDecl,
                    AstKind::Protocol => SymbolKind::ProtocolDecl,
                    AstKind::Type => SymbolKind::TypeAlias,
                    AstKind::Field => SymbolKind::Property,
                    AstKind::Variable => SymbolKind::Variable,
                    AstKind::Module => SymbolKind::Module,
                    AstKind::EnumCase | AstKind::Other => SymbolKind::Other,
                };
                make_symbol(
                    symbol.name,
                    kind,
                    symbol.signature,
                    symbol.start_row as usize + 1,
                    file_path,
                    symbol.container,
                )
            })
            .collect(),
    )
}

#[cfg(not(feature = "ast"))]
fn structural_symbols(
    _content: &str,
    _language: Language,
    _file_path: &str,
) -> Option<Vec<SymbolRecord>> {
    None
}

fn pragma_symbols(content: &str, language: Language, file_path: &str) -> Vec<SymbolRecord> {
    content
        .lines()
        .enumerate()
        .filter_map(|(idx, line)| {
            let pragma = extract_pragma(line.trim(), language)?;
            let signature = if pragma.has_separator {
                format!("--- {} ---", pragma.name)
            } else {
                pragma.name.clone()
            };
            Some(make_symbol(
                pragma.name,
                pragma.kind,
                signature,
                idx + 1,
                file_path,
                None,
            ))
        })
        .collect()
}

/// The line-oriented fallback for languages without a grammar binding.
/// Containers are tracked by brace depth, so it is approximate by design.
fn regex_fallback_symbols(content: &str, language: Language, file_path: &str) -> Vec<SymbolRecord> {
    let mut symbols = Vec::new();
    let mut current_container: Option<String> = None;
    let mut brace_depth: i64 = 0;

    for (idx, line) in content.lines().enumerate() {
        let trimmed = line.trim();
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

        let extracted = match language {
            Language::Dart => extract_dart(trimmed, idx + 1, file_path),
            _ => extract_generic(trimmed, idx + 1, file_path),
        };
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

// MARK: - Dart (regex fallback)
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

// MARK: - Generic regex fallback
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

fn extract_pragma(line: &str, language: Language) -> Option<PragmaMatch> {
    let is_python = language == Language::Python;
    let is_cpp = matches!(
        language,
        Language::C | Language::Cpp | Language::ObjectiveC | Language::ObjectiveCpp
    );

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
    #[expect(
        clippy::string_slice,
        reason = "`end` is `i + ch.len_utf8()` from char_indices, a char boundary"
    )]
    let candidate = if end > 0 { &rest[..end] } else { rest };
    harn_vm::text::truncate_end(candidate, max_len)
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

    #[cfg(feature = "grammars-all")]
    #[test]
    fn grammar_languages_use_tree_sitter_symbols() {
        let src = "class Foo {\n    func bar() {}\n}\n";
        let symbols = extract_symbols(src, Language::Swift, "Foo.swift");
        let names: Vec<_> = symbols
            .iter()
            .map(|s| (s.name.as_str(), s.kind, s.container.as_deref()))
            .collect();
        assert!(
            names.contains(&("Foo", SymbolKind::ClassDecl, None)),
            "{names:?}"
        );
        assert!(
            symbols
                .iter()
                .any(|s| s.name == "bar" && s.container.as_deref() == Some("Foo")),
            "{names:?}"
        );
        // The regex fallback would read a call inside a body as a
        // declaration; the grammar does not.
        let src = "fn one() {\n    helper(1);\n}\n";
        let symbols = extract_symbols(src, Language::Rust, "lib.rs");
        let names: Vec<_> = symbols.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["one"]);
    }

    #[cfg(feature = "grammars-all")]
    #[test]
    fn extracts_scala_sealed_trait_symbol() {
        let src = "package rulekit\n\nsealed trait Rule:\n  def name: String\n";
        let symbols = extract_symbols(src, Language::Scala, "Rule.scala");
        assert!(
            symbols.iter().any(|s| s.name == "Rule"),
            "expected sealed trait Rule to be indexed, got {symbols:?}"
        );
    }

    #[cfg(feature = "grammars-all")]
    #[test]
    fn data_grammars_have_no_symbols_and_skip_the_fallback() {
        let symbols = extract_symbols("{\"type Foo\": 1}\n", Language::Json, "a.json");
        assert!(symbols.is_empty(), "{symbols:?}");
    }

    #[test]
    fn extracts_pragmas_in_python() {
        let src = "# MARK: - Section\n# TODO: write me\n";
        let symbols = extract_symbols(src, Language::Python, "x.py");
        assert!(symbols.iter().any(|s| s.kind == SymbolKind::Mark));
        assert!(symbols.iter().any(|s| s.kind == SymbolKind::Todo));
    }

    #[test]
    fn extracts_dart_function_with_signature() {
        // Dart has no grammar, so it takes the regex fallback.
        let src = "void greet(String name) {\n  return name.length;\n}\n";
        let symbols = extract_symbols(src, Language::Dart, "main.dart");
        let fns: Vec<_> = symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::Function && s.name == "greet")
            .collect();
        assert_eq!(fns.len(), 1);
        assert!(fns[0].signature.starts_with("greet("));
    }

    #[test]
    fn languages_without_a_grammar_use_the_generic_fallback() {
        let src = "@interface Widget : NSObject\n@end\nstruct Point { int x; };\n";
        let symbols = extract_symbols(src, Language::ObjectiveC, "Widget.m");
        assert!(
            symbols
                .iter()
                .any(|s| s.name == "Point" && s.kind == SymbolKind::StructDecl),
            "{symbols:?}"
        );
    }

    #[test]
    fn ids_are_stable_per_file_name_line() {
        let src = "fn one() {}\nfn two() {}\n";
        let symbols = extract_symbols(src, Language::Rust, "lib.rs");
        let ids: Vec<_> = symbols.iter().map(|s| s.id.as_str()).collect();
        assert!(ids.contains(&"lib.rs:one:1"));
        assert!(ids.contains(&"lib.rs:two:2"));
    }
}
