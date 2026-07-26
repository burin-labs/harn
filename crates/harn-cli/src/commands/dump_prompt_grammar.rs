//! `harn dump-prompt-grammar` — regenerate the VS Code TextMate grammar for
//! `.harn.prompt` files.
//!
//! The grammar has to know every template keyword, filter, and section name the
//! engine accepts. Hand-maintaining that list is what let the committed grammar
//! highlight filters the engine never had while missing the whitespace the
//! engine does allow, so the whole file is generated from
//! `harn_vm::stdlib::template::vocabulary` — the same constants the engine
//! renders with.
//!
//! With `--check`, the command diffs the generated content against the file on
//! disk and exits non-zero if they differ (same idiom as `cargo fmt --check`).
//! CI runs this so a PR that adds a filter without regenerating fails.

use std::fs;
use std::path::Path;
use std::process;

use harn_vm::stdlib::template::vocabulary::{
    BLOCK_KEYWORDS, CLAUSE_KEYWORDS, FILTERS, LITERAL_KEYWORDS, OPERATOR_KEYWORDS, SECTIONS,
};
use serde_json::{json, Value};

pub(crate) fn run(output_path: &str, check_only: bool) {
    let generated = generate_file();
    let path = Path::new(output_path);

    if check_only {
        let existing = match fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("error: cannot read {}: {e}", path.display());
                eprintln!("hint: run `make gen-prompt-grammar` to regenerate.");
                process::exit(1);
            }
        };
        if normalize_line_endings(&existing) != normalize_line_endings(&generated) {
            eprintln!(
                "error: {} is stale relative to the prompt-template engine.",
                path.display()
            );
            eprintln!("hint: run `make gen-prompt-grammar` to regenerate.");
            process::exit(1);
        }
        return;
    }

    if let Some(parent) = path.parent() {
        if let Err(e) = fs::create_dir_all(parent) {
            eprintln!("error: cannot create {}: {e}", parent.display());
            process::exit(1);
        }
    }
    if let Err(e) = fs::write(path, &generated) {
        eprintln!("error: cannot write {}: {e}", path.display());
        process::exit(1);
    }
    println!("wrote {}", path.display());
}

/// A regex alternation over `words`, longest first.
///
/// Ordering matters: a TextMate engine tries alternatives left to right, so
/// listing `end` before `endsection` would let `end` claim the prefix. The
/// trailing `\b` makes that particular case backtrack correctly anyway, but
/// depending on backtracking for a generated file is a trap for whoever adds
/// the next keyword pair.
fn alternation(words: &[&str]) -> String {
    let mut sorted: Vec<&str> = words.to_vec();
    sorted.sort_by(|a, b| b.len().cmp(&a.len()).then(a.cmp(b)));
    sorted.join("|")
}

/// Build the grammar as structured JSON. Pure so it is easy to unit-test, and
/// built from `json!` rather than string concatenation so the output is valid
/// by construction.
fn grammar() -> Value {
    let block_and_clause: Vec<&str> = BLOCK_KEYWORDS
        .iter()
        .copied()
        .chain(CLAUSE_KEYWORDS.iter().copied())
        .collect();

    json!({
        "$schema": "https://raw.githubusercontent.com/martinring/tmlanguage/master/tmlanguage.json",
        "name": "Harn Prompt Template",
        "scopeName": "text.harn.prompt",
        "patterns": [
            { "include": "#comment" },
            { "include": "#raw-block" },
            { "include": "#directive" }
        ],
        "repository": {
            "comment": {
                "name": "comment.block.harn.prompt",
                "begin": "\\{\\{#",
                "end": "#\\}\\}",
                "patterns": []
            },
            "raw-block": {
                "name": "meta.raw.harn.prompt",
                "begin": "(\\{\\{-?)\\s*(raw)\\s*(-?\\}\\})",
                "beginCaptures": {
                    "1": { "name": "punctuation.section.embedded.begin.harn.prompt" },
                    "2": { "name": "keyword.control.raw.harn.prompt" },
                    "3": { "name": "punctuation.section.embedded.end.harn.prompt" }
                },
                "end": "(\\{\\{-?)\\s*(endraw)\\s*(-?\\}\\})",
                "endCaptures": {
                    "1": { "name": "punctuation.section.embedded.begin.harn.prompt" },
                    "2": { "name": "keyword.control.endraw.harn.prompt" },
                    "3": { "name": "punctuation.section.embedded.end.harn.prompt" }
                },
                "contentName": "string.unquoted.raw.harn.prompt"
            },
            "directive": {
                "name": "meta.directive.harn.prompt",
                "begin": "\\{\\{-?",
                "beginCaptures": {
                    "0": { "name": "punctuation.section.embedded.begin.harn.prompt" }
                },
                "end": "-?\\}\\}",
                "endCaptures": {
                    "0": { "name": "punctuation.section.embedded.end.harn.prompt" }
                },
                // `section-name` and `filter` both begin at a token that a later
                // rule would otherwise claim (the section keyword, the filter
                // name), so they are matched first.
                "patterns": [
                    { "include": "#section-name" },
                    { "include": "#filter" },
                    { "include": "#keyword" },
                    { "include": "#literal-keyword" },
                    { "include": "#literal" },
                    { "include": "#operator" },
                    { "include": "#identifier" }
                ]
            },
            "section-name": {
                "match": format!(
                    "(\\bsection\\b)(\\s*)(\"(?:{sections})\"|'(?:{sections})')",
                    sections = alternation(SECTIONS)
                ),
                "captures": {
                    "1": { "name": "keyword.control.harn.prompt" },
                    "3": { "name": "support.constant.section.harn.prompt" }
                }
            },
            // The pipe and the filter name are captured together instead of
            // using a lookbehind: the engine allows any run of whitespace after
            // `|`, and TextMate's regex engine does not reliably support
            // variable-length lookbehind.
            "filter": {
                "match": format!("(\\|)(\\s*)({filters})\\b", filters = alternation(FILTERS)),
                "captures": {
                    "1": { "name": "keyword.operator.filter.harn.prompt" },
                    "3": { "name": "entity.name.function.filter.harn.prompt" }
                }
            },
            "keyword": {
                "name": "keyword.control.harn.prompt",
                "match": format!("\\b({keywords})\\b", keywords = alternation(&block_and_clause))
            },
            "literal-keyword": {
                "name": "constant.language.harn.prompt",
                "match": format!("\\b({literals})\\b", literals = alternation(LITERAL_KEYWORDS))
            },
            "literal": {
                "patterns": [
                    {
                        "name": "string.quoted.double.harn.prompt",
                        "begin": "\"",
                        "end": "\"",
                        "patterns": [
                            { "name": "constant.character.escape.harn.prompt", "match": "\\\\." }
                        ]
                    },
                    {
                        "name": "string.quoted.single.harn.prompt",
                        "begin": "'",
                        "end": "'",
                        "patterns": [
                            { "name": "constant.character.escape.harn.prompt", "match": "\\\\." }
                        ]
                    },
                    {
                        "name": "constant.numeric.harn.prompt",
                        "match": "\\b-?[0-9]+(\\.[0-9]+)?\\b"
                    }
                ]
            },
            "operator": {
                "name": "keyword.operator.harn.prompt",
                "match": format!(
                    "==|!=|<=|>=|=|<|>|&&|\\|\\||!|\\.|\\b({words})\\b",
                    words = alternation(OPERATOR_KEYWORDS)
                )
            },
            "identifier": {
                "name": "variable.other.harn.prompt",
                "match": "\\b[a-zA-Z_][a-zA-Z0-9_]*\\b"
            }
        }
    })
}

/// The grammar plus the generated-file banner. JSON has no comment syntax, so
/// the banner lives in a `_generated` key that TextMate ignores.
fn generate_file() -> String {
    let mut value = grammar();
    let map = value
        .as_object_mut()
        .expect("grammar root is a JSON object by construction");
    map.insert(
        "_generated".to_string(),
        json!(
            "GENERATED by `harn dump-prompt-grammar` — do not edit by hand. \
             Source of truth: crates/harn-vm/src/stdlib/template/vocabulary.rs. \
             Regenerate with: make gen-prompt-grammar"
        ),
    );

    let mut out = serde_json::to_string_pretty(&value)
        .expect("grammar serializes: it contains only strings, arrays, and maps");
    out.push('\n');
    out
}

fn normalize_line_endings(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repository() -> Value {
        grammar()
            .get("repository")
            .cloned()
            .expect("grammar has a repository")
    }

    fn pattern_match(name: &str) -> String {
        repository()
            .get(name)
            .and_then(|p| p.get("match"))
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("`{name}` has a match regex"))
            .to_string()
    }

    #[test]
    fn keyword_alternation_covers_the_engine_vocabulary() {
        let keywords = pattern_match("keyword");
        for word in BLOCK_KEYWORDS.iter().chain(CLAUSE_KEYWORDS) {
            assert!(
                keywords.contains(word),
                "keyword `{word}` missing from the generated grammar"
            );
        }
    }

    #[test]
    fn filter_alternation_is_exactly_the_engine_filter_set() {
        let filters = pattern_match("filter");
        for name in FILTERS {
            assert!(
                filters.contains(name),
                "filter `{name}` missing from the generated grammar"
            );
        }
        // A plausible-looking filter the engine does not implement must not
        // highlight as one — that was the bug this generator replaces.
        assert!(
            !filters.contains("uppercase"),
            "grammar highlights filters the engine does not implement"
        );
    }

    /// The old grammar used `(?<=\|\s)`, which required exactly one whitespace
    /// character after the pipe, so `{{ x|upper }}` and `{{ x |  upper }}` both
    /// lost their highlight.
    #[test]
    fn filter_pattern_accepts_any_spacing_after_the_pipe() {
        let filters = pattern_match("filter");
        assert!(
            !filters.contains("(?<="),
            "filter pattern still uses a fixed-width lookbehind"
        );
        assert!(
            filters.starts_with("(\\|)(\\s*)"),
            "filter pattern should capture the pipe and any following whitespace, got: {filters}"
        );
    }

    #[test]
    fn section_names_come_from_the_engine() {
        let sections = pattern_match("section-name");
        for name in SECTIONS {
            assert!(
                sections.contains(name),
                "section `{name}` missing from the generated grammar"
            );
        }
    }

    #[test]
    fn alternation_lists_longer_words_first() {
        let alt = alternation(&["end", "endsection", "endraw"]);
        assert_eq!(alt, "endsection|endraw|end");
    }

    #[test]
    fn generated_file_is_valid_json_with_a_banner() {
        let out = generate_file();
        let parsed: Value = serde_json::from_str(&out).expect("generated grammar parses as JSON");
        assert_eq!(
            parsed.get("scopeName").and_then(Value::as_str),
            Some("text.harn.prompt")
        );
        assert!(parsed
            .get("_generated")
            .and_then(Value::as_str)
            .expect("banner")
            .contains("make gen-prompt-grammar"));
        assert!(out.ends_with('\n'));
    }

    /// CI backstop so a PR that changes the template vocabulary without
    /// regenerating the grammar fails `make test`, not just the audit lane.
    #[test]
    fn committed_grammar_matches_generator() {
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        let path = Path::new(manifest_dir)
            .join("..")
            .join("..")
            .join("editors")
            .join("vscode")
            .join("syntaxes")
            .join("harn-prompt.tmLanguage.json");
        let on_disk = fs::read_to_string(&path).unwrap_or_else(|e| {
            panic!(
                "failed to read {}: {e}\n\
                 hint: run `make gen-prompt-grammar` to regenerate.",
                path.display()
            )
        });
        assert_eq!(
            normalize_line_endings(&on_disk),
            normalize_line_endings(&generate_file()),
            "editors/vscode/syntaxes/harn-prompt.tmLanguage.json is stale relative to \
             the prompt-template engine.\nRun `make gen-prompt-grammar` to regenerate."
        );
    }
}
