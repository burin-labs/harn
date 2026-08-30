//! Generate the portable Harn language vocabulary and projection adapters.
//!
//! The mdBook documentation site (`docs/`) uses a custom highlight.js language
//! definition to render ```` ```harn ```` code blocks. TypeScript, browser,
//! docs, and editor consumers share the machine-readable JSON artifact rather
//! than embedding their own keyword or builtin subsets.
//!
//! Sources of truth:
//!
//! - `harn_lexer::KEYWORDS` — language keywords.
//! - `harn_vm::stdlib::stdlib_builtin_names()` — globally-available stdlib
//!   builtins (all three tiers are registered unconditionally on a Harn VM,
//!   so everything this function returns is reachable without an `import`).
//!
//! With `--check`, the command diffs the generated content against the file
//! on disk and exits non-zero if they differ (same idiom as `cargo fmt
//! --check`). CI runs this to fail any PR that changes a keyword or a builtin
//! name without regenerating.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::process;

use harn_lexer::{KEYWORDS, LITERAL_KEYWORDS};
use harn_vm::stdlib::stdlib_builtin_names;
use serde::Serialize;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LanguageVocabulary {
    schema_version: u32,
    keywords: Vec<String>,
    literals: Vec<String>,
    builtins: Vec<String>,
    token_categories: TokenCategories,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TokenCategories {
    keyword: &'static str,
    literal: &'static str,
    builtin: &'static str,
    type_name: &'static str,
    string: &'static str,
    number: &'static str,
    comment: &'static str,
}

pub(crate) fn run(
    output_path: &str,
    json_output_path: &str,
    wasm_json_output_path: &str,
    check_only: bool,
) {
    let vocabulary = language_vocabulary();
    let json = format!(
        "{}\n",
        serde_json::to_string_pretty(&vocabulary)
            .expect("language vocabulary is JSON serializable")
    );
    let outputs = [
        (Path::new(output_path), generate_file(&vocabulary)),
        (Path::new(json_output_path), json.clone()),
        (Path::new(wasm_json_output_path), json),
    ];

    for (path, generated) in outputs {
        write_or_check(path, &generated, check_only);
    }
}

fn write_or_check(path: &Path, generated: &str, check_only: bool) {
    if check_only {
        let existing = match fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("error: cannot read {}: {e}", path.display());
                eprintln!("hint: run `make gen-highlight` to regenerate.");
                process::exit(1);
            }
        };
        if normalize_line_endings(&existing) != normalize_line_endings(generated) {
            eprintln!(
                "error: {} is stale relative to the lexer/stdlib.",
                path.display()
            );
            eprintln!("hint: run `make gen-highlight` to regenerate.");
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
    if let Err(e) = fs::write(path, generated) {
        eprintln!("error: cannot write {}: {e}", path.display());
        process::exit(1);
    }
    println!("wrote {}", path.display());
}

/// Build the full file contents. Pure so it's easy to unit-test.
fn language_vocabulary() -> LanguageVocabulary {
    let literals: BTreeSet<&str> = LITERAL_KEYWORDS.iter().copied().collect();

    let keywords: Vec<String> = KEYWORDS
        .iter()
        .copied()
        .filter(|k| !literals.contains(k))
        .map(str::to_string)
        .collect();

    // Builtins: names registered on a fully-initialized VM, minus anything
    // that's already a keyword (highlight.js treats those as keywords) and
    // compiler-internal `__*` names users never call directly.
    let keyword_set: BTreeSet<&str> = KEYWORDS.iter().copied().collect();
    let builtin_owned: Vec<String> = stdlib_builtin_names()
        .into_iter()
        .filter(|name| !name.starts_with("__"))
        .filter(|name| !keyword_set.contains(name.as_str()))
        .collect();
    let mut builtins: BTreeSet<&str> = builtin_owned.iter().map(String::as_str).collect();
    builtins.remove("");

    LanguageVocabulary {
        schema_version: 1,
        keywords,
        literals: LITERAL_KEYWORDS
            .iter()
            .map(|value| (*value).to_string())
            .collect(),
        builtins: builtins.into_iter().map(str::to_string).collect(),
        token_categories: TokenCategories {
            keyword: "harn-keyword",
            literal: "harn-literal",
            builtin: "harn-builtin",
            type_name: "harn-type",
            string: "harn-string",
            number: "harn-number",
            comment: "harn-comment",
        },
    }
}

fn generate_file(vocabulary: &LanguageVocabulary) -> String {
    let keyword_line = vocabulary.keywords.join(" ");
    let literal_line = vocabulary.literals.join(" ");
    let builtin_line = vocabulary.builtins.join(" ");

    format!(
        "// GENERATED by `harn dump-highlight-keywords` — do not edit by hand.\n\
         //\n\
         // Sources of truth:\n\
         //   crates/harn-lexer/src/token.rs  (KEYWORDS)\n\
         //   crates/harn-vm/src/stdlib.rs    (stdlib_builtin_names)\n\
         //\n\
         // Regenerate with: make gen-highlight\n\
         // CI guard:        cargo run -p harn-cli -- dump-highlight-keywords --check\n\
         window.__HARN_KEYWORDS = {{\n\
         \x20\x20keyword: {keyword_line:?},\n\
         \x20\x20literal: {literal_line:?},\n\
         \x20\x20built_in: {builtin_line:?}\n\
         }};\n",
    )
}

fn normalize_line_endings(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_file_contains_core_keywords() {
        let out = generate_file(&language_vocabulary());
        assert!(out.contains("pipeline"));
        assert!(out.contains("parallel"));
        assert!(out.contains("defer"));
        assert!(out.contains("window.__HARN_KEYWORDS"));
    }

    #[test]
    fn generated_file_contains_known_builtins() {
        let out = generate_file(&language_vocabulary());
        for name in &["log", "read_file", "llm_call", "http_choose"] {
            assert!(
                out.contains(name),
                "expected builtin `{name}` in generated file"
            );
        }
        let builtin_line = out
            .lines()
            .find_map(|line| line.trim().strip_prefix("built_in: \""))
            .and_then(|line| line.strip_suffix('"'))
            .expect("generated built_in line");
        let builtins: std::collections::BTreeSet<&str> = builtin_line.split_whitespace().collect();
        for name in &["http_get", "println", "prompt_user"] {
            assert!(
                !builtins.contains(name),
                "removed ambient builtin `{name}` should not be highlighted"
            );
        }
    }

    /// CI backstop so PRs that change a keyword or stdlib builtin name
    /// without regenerating `docs/theme/harn-keywords.js` fail `make test`.
    #[test]
    fn committed_keyword_file_matches_generator() {
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        let path = std::path::Path::new(manifest_dir)
            .join("..")
            .join("..")
            .join("docs")
            .join("theme")
            .join("harn-keywords.js");
        let on_disk = std::fs::read_to_string(&path).unwrap_or_else(|e| {
            panic!(
                "failed to read {}: {e}\n\
                 hint: run `make gen-highlight` to regenerate.",
                path.display()
            )
        });
        let generated = generate_file(&language_vocabulary());
        assert_eq!(
            normalize_line_endings(&on_disk),
            normalize_line_endings(&generated),
            "docs/theme/harn-keywords.js is stale relative to the lexer/stdlib.\n\
             Run `make gen-highlight` to regenerate."
        );
    }

    #[test]
    fn committed_vocabulary_manifest_matches_generator() {
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        let path = std::path::Path::new(manifest_dir)
            .join("..")
            .join("..")
            .join("spec")
            .join("language-vocabulary.json");
        let on_disk = std::fs::read_to_string(&path).unwrap_or_else(|error| {
            panic!(
                "failed to read {}: {error}\n\
                 hint: run `make gen-highlight` to regenerate.",
                path.display()
            )
        });
        let generated = format!(
            "{}\n",
            serde_json::to_string_pretty(&language_vocabulary()).unwrap()
        );
        assert_eq!(normalize_line_endings(&on_disk), generated);

        let wasm_path = std::path::Path::new(manifest_dir)
            .join("..")
            .join("harn-wasm")
            .join("demo")
            .join("language-vocabulary.json");
        let wasm_projection = std::fs::read_to_string(&wasm_path)
            .unwrap_or_else(|error| panic!("failed to read {}: {error}", wasm_path.display()));
        assert_eq!(normalize_line_endings(&wasm_projection), generated);
    }

    #[test]
    #[expect(
        clippy::string_slice,
        reason = "generated keyword vocabulary output is ASCII"
    )]
    fn literals_are_not_also_keywords() {
        let out = generate_file(&language_vocabulary());
        // Literals must live in the literal field, not bleed into the
        // keyword string.
        let keyword_section_start = out.find("keyword: \"").expect("keyword field");
        let keyword_section_end = out[keyword_section_start..]
            .find('"')
            .and_then(|i| out[keyword_section_start + i + 1..].find('"'))
            .unwrap();
        let keyword_section =
            &out[keyword_section_start..keyword_section_start + keyword_section_end + 20];
        for lit in LITERAL_KEYWORDS {
            assert!(
                !keyword_section.contains(&format!(" {lit} ")),
                "literal `{lit}` leaked into keyword list"
            );
        }
    }
}
