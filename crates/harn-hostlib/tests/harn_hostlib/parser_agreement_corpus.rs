//! Versioned grammar-fitness contract gate.
//!
//! The JSON corpus is the checked-in authority. Every row proves parse,
//! structural-search, and post-edit validation behavior for the exact grammar
//! artifact recorded in `receipt.v1.json`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use harn_hostlib::ast::Language;
use serde::Deserialize;
use streaming_iterator::StreamingIterator;
use tree_sitter::{Parser, Query, QueryCursor};

const CORPUS_JSON: &str = include_str!("../../data/grammar-fitness/corpus.v1.json");
const CORPUS_SCHEMA_JSON: &str = include_str!("../../data/grammar-fitness/corpus.schema.json");

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Corpus {
    schema_version: u32,
    rows: Vec<CorpusRow>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CorpusRow {
    language: String,
    fixture: String,
    authority: String,
    operations: Vec<Operation>,
    expected: ExpectedFacts,
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
enum Operation {
    Parse,
    Search,
    SafeEdit,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExpectedFacts {
    clean_parse: bool,
    minimum_search_matches: usize,
    safe_edit_suffix: String,
}

fn corpus() -> Corpus {
    serde_json::from_str(CORPUS_JSON).expect("grammar fitness corpus must match its typed schema")
}

fn fixture_path(row: &CorpusRow) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("data/grammar-fitness")
        .join(&row.fixture)
}

fn parser_for(language: Language) -> Parser {
    let ts_language = language
        .ts_language()
        .unwrap_or_else(|| panic!("grammar for {} must be compiled", language.name()));
    let mut parser = Parser::new();
    parser
        .set_language(&ts_language)
        .unwrap_or_else(|err| panic!("set grammar for {}: {err}", language.name()));
    parser
}

#[test]
fn corpus_is_versioned_closed_and_covers_every_language() {
    let schema: serde_json::Value =
        serde_json::from_str(CORPUS_SCHEMA_JSON).expect("corpus JSON Schema must be valid JSON");
    let instance: serde_json::Value =
        serde_json::from_str(CORPUS_JSON).expect("corpus must be valid JSON");
    let validator = jsonschema::validator_for(&schema).expect("corpus JSON Schema must compile");
    if let Err(error) = validator.validate(&instance) {
        panic!("grammar fitness corpus violates its JSON Schema: {error}");
    }
    let corpus = corpus();
    assert_eq!(corpus.schema_version, 1);

    let mut seen = BTreeSet::new();
    for row in &corpus.rows {
        assert!(
            seen.insert(row.language.clone()),
            "duplicate corpus row for {}",
            row.language
        );
        assert!(
            !row.authority.trim().is_empty(),
            "authority must be explicit"
        );
        assert_eq!(
            row.operations.iter().copied().collect::<BTreeSet<_>>(),
            BTreeSet::from([Operation::Parse, Operation::Search, Operation::SafeEdit]),
            "{} must cover every operation class",
            row.language
        );
        assert!(
            fixture_path(row).is_file(),
            "missing fixture {}",
            fixture_path(row).display()
        );
    }

    let registered = Language::all()
        .iter()
        .map(|language| language.name().to_string())
        .collect::<BTreeSet<_>>();
    assert_eq!(seen, registered, "corpus and language registry must agree");
}

#[test]
fn resolved_grammars_pass_the_versioned_fitness_corpus() {
    for row in corpus().rows {
        let language = Language::from_name(&row.language)
            .unwrap_or_else(|| panic!("unknown corpus language {}", row.language));
        let source = std::fs::read(fixture_path(&row))
            .unwrap_or_else(|err| panic!("read {}: {err}", fixture_path(&row).display()));
        let mut parser = parser_for(language);
        let tree = parser
            .parse(&source, None)
            .unwrap_or_else(|| panic!("parser returned no tree for {}", row.language));
        if row.expected.clean_parse {
            assert!(
                !tree.root_node().has_error(),
                "{} parse fitness failed for {} ({})",
                row.language,
                row.fixture,
                row.authority
            );
        }

        let ts_language = language.ts_language().expect("grammar compiled");
        let query = Query::new(&ts_language, "(_) @node").expect("portable wildcard query");
        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&query, tree.root_node(), source.as_slice());
        let mut match_count = 0;
        let mut first_span = None;
        while let Some(query_match) = matches.next() {
            match_count += 1;
            if first_span.is_none() {
                first_span = query_match
                    .captures
                    .first()
                    .map(|capture| (capture.node.start_byte(), capture.node.end_byte()));
            }
        }
        assert!(
            match_count >= row.expected.minimum_search_matches,
            "{} search fitness returned {match_count} matches, expected at least {}",
            row.language,
            row.expected.minimum_search_matches
        );

        let (start, end) = first_span.expect("search fitness must bind a node");
        let mut edited = Vec::with_capacity(source.len() + row.expected.safe_edit_suffix.len());
        edited.extend_from_slice(&source[..start]);
        edited.extend_from_slice(&source[start..end]);
        edited.extend_from_slice(row.expected.safe_edit_suffix.as_bytes());
        edited.extend_from_slice(&source[end..]);
        let edited_tree = parser
            .parse(&edited, None)
            .unwrap_or_else(|| panic!("post-edit parser returned no tree for {}", row.language));
        assert!(
            !edited_tree.root_node().has_error(),
            "{} safe-edit validation fitness failed after a trivia-only structural splice",
            row.language
        );
    }
}

#[test]
fn swift_optional_chain_cast_and_fallback_stays_clean() {
    let row = corpus()
        .rows
        .into_iter()
        .find(|row| row.language == "swift")
        .expect("Swift corpus row");
    let source = std::fs::read_to_string(fixture_path(&row)).expect("read Swift fixture");
    assert!(source
        .contains("notification.userInfo?[\"message\"] as? String ?? \"Collaboration error\""));
    let tree = parser_for(Language::Swift)
        .parse(source.as_bytes(), None)
        .expect("Swift parse tree");
    assert!(
        !tree.root_node().has_error(),
        "the approved Swift artifact regressed optional-chain/cast/coalesce syntax"
    );
}

#[test]
fn zig_multiline_string_stays_clean() {
    let row = corpus()
        .rows
        .into_iter()
        .find(|row| row.language == "zig")
        .expect("Zig corpus row");
    let source = std::fs::read_to_string(fixture_path(&row)).expect("read Zig fixture");
    assert!(source.contains("\\\\SELECT"));
    let tree = parser_for(Language::Zig)
        .parse(source.as_bytes(), None)
        .expect("Zig parse tree");
    assert!(
        !tree.root_node().has_error(),
        "the approved Zig artifact regressed multiline strings"
    );
}
