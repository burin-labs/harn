//! Parser-agreement contract gate.
//!
//! For a small checked-in polyglot fixture corpus, assert that the host's
//! BUNDLED tree-sitter parser facts AGREE with declared ground truth for each
//! `(fixture, language)` pair. The facts checked are the ones the agent loop
//! ships to the model:
//!
//!   * `hostlib_ast_extract_imports` — the import set
//!   * `hostlib_ast_parse_errors`    — whether the file parses cleanly
//!
//! ## Why this exists
//!
//! A bundled-grammar regression (a tree-sitter grammar bump that mis-lexes valid
//! modern source) silently ships PHANTOM facts to the model — a phantom import
//! the model "fixes", or a phantom parse-error storm that steers it to rewrite
//! correct code. A downstream host caught one such bug after it shipped: the
//! tree-sitter-zig grammar mis-lexed a valid `\\` multiline string into a storm
//! of `unexpected '...'` errors (observed in an IDE host), and the only defense was a
//! per-language special-case. This corpus is the GENERALIZED, pre-ship defense:
//! a grammar regression that disagrees with ground truth fails THIS test in CI
//! instead of reaching an agent.
//!
//! ## Extending the corpus
//!
//! Add one fixture file under `tests/fixtures/parser_agreement/` and one
//! [`Case`] row in [`corpus`]. Keep fixtures tiny — this is an agreement gate,
//! not a parser conformance suite. Ground-truth values are what the CURRENTLY
//! bundled grammars produce; a future grammar bump that changes them is exactly
//! the regression this gate is meant to surface (re-pin deliberately, with a
//! note, when a change is intended).
//!
//! ## Seed regression
//!
//! `sample.zig` carries a `\\` multiline string — the exact construct that bit
//! us. Its ground truth is `expect_clean_parse: true`: if a grammar bump
//! re-introduces the mislex, the parse errors come back and this case fails.

use std::path::PathBuf;
use std::sync::Arc;

use harn_hostlib::{ast::AstCapability, BuiltinRegistry, HostlibCapability};
use harn_vm::VmValue;

/// One corpus row: a fixture file + the language to parse it as + the parser
/// facts that must agree with ground truth.
struct Case {
    /// Fixture filename under `tests/fixtures/parser_agreement/`.
    fixture: &'static str,
    /// Language wire-name (or extension alias) handed to the builtins.
    language: &'static str,
    /// The exact import-statement texts the bundled grammar must extract, in
    /// document order. Drawn from what the currently shipped grammars produce.
    expected_imports: &'static [&'static str],
    /// Whether the fixture must parse with ZERO `ERROR`/`MISSING` nodes. The
    /// fixtures are all valid modern source, so this is `true` everywhere — a
    /// `false` here would mean ground truth is itself broken.
    expect_clean_parse: bool,
}

/// The polyglot corpus — one row per language. Tiny and extensible.
fn corpus() -> Vec<Case> {
    vec![
        // SEED REGRESSION: the zig `\\` multiline string that the bundled
        // grammar once mis-lexed into a phantom error storm (#3010). Zig
        // `@import` builtins are not import DECLARATIONS in the grammar, so the
        // import set is empty — the load-bearing fact here is the CLEAN parse.
        Case {
            fixture: "sample.zig",
            language: "zig",
            expected_imports: &[],
            expect_clean_parse: true,
        },
        Case {
            fixture: "sample.py",
            language: "python",
            expected_imports: &["import os", "from typing import List, Optional"],
            expect_clean_parse: true,
        },
        Case {
            fixture: "sample.rs",
            language: "rust",
            expected_imports: &["use std::collections::HashMap;"],
            expect_clean_parse: true,
        },
        Case {
            fixture: "sample.go",
            language: "go",
            // tree-sitter-go surfaces the grouped `import (...)` block as one
            // `import_declaration` statement.
            expected_imports: &["import (\n\t\"fmt\"\n\t\"os\"\n)"],
            expect_clean_parse: true,
        },
        Case {
            fixture: "sample.ts",
            language: "typescript",
            expected_imports: &[
                "import { readFile } from 'fs';",
                "import path from \"path\";",
            ],
            expect_clean_parse: true,
        },
        Case {
            fixture: "sample.c",
            language: "c",
            expected_imports: &["#include <stdio.h>", "#include \"local.h\""],
            expect_clean_parse: true,
        },
    ]
}

// ---------------------------------------------------------------------------
// Harness plumbing
// ---------------------------------------------------------------------------

fn ast_registry() -> BuiltinRegistry {
    let mut registry = BuiltinRegistry::new();
    AstCapability.register_builtins(&mut registry);
    registry
}

fn dict(pairs: &[(&str, VmValue)]) -> VmValue {
    let mut map: harn_vm::value::DictMap = Default::default();
    for (k, v) in pairs {
        map.insert((*k).into(), v.clone());
    }
    VmValue::dict(map)
}

fn invoke(registry: &BuiltinRegistry, name: &str, payload: VmValue) -> VmValue {
    let entry = registry
        .find(name)
        .unwrap_or_else(|| panic!("builtin {name} not registered"));
    (entry.handler)(&[payload]).unwrap_or_else(|err| panic!("{name} failed: {err}"))
}

fn vstring(s: &str) -> VmValue {
    VmValue::String(arcstr::ArcStr::from(s))
}

fn dict_field(value: &VmValue, key: &str) -> VmValue {
    match value {
        VmValue::Dict(d) => d
            .get(key)
            .cloned()
            .unwrap_or_else(|| panic!("missing field `{key}` on {value:?}")),
        other => panic!("expected dict, got {other:?}"),
    }
}

fn list_value(value: &VmValue) -> Arc<Vec<VmValue>> {
    match value {
        VmValue::List(l) => l.clone(),
        other => panic!("expected list, got {other:?}"),
    }
}

fn string_value(value: &VmValue) -> String {
    match value {
        VmValue::String(s) => s.to_string(),
        other => panic!("expected string, got {other:?}"),
    }
}

fn bool_value(value: &VmValue) -> bool {
    match value {
        VmValue::Bool(b) => *b,
        other => panic!("expected bool, got {other:?}"),
    }
}

fn fixture_source(name: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/parser_agreement")
        .join(name);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("read fixture {}: {err}", path.display()))
}

fn extracted_imports(registry: &BuiltinRegistry, source: &str, language: &str) -> Vec<String> {
    let result = invoke(
        registry,
        "hostlib_ast_extract_imports",
        dict(&[("source", vstring(source)), ("language", vstring(language))]),
    );
    assert!(
        bool_value(&dict_field(&result, "supported")),
        "language `{language}` must be a supported tree-sitter grammar"
    );
    list_value(&dict_field(&result, "statements"))
        .iter()
        .map(|stmt| string_value(&dict_field(stmt, "text")))
        .collect()
}

fn parse_error_messages(registry: &BuiltinRegistry, source: &str, language: &str) -> Vec<String> {
    let result = invoke(
        registry,
        "hostlib_ast_parse_errors",
        dict(&[
            ("content", vstring(source)),
            ("language", vstring(language)),
        ]),
    );
    assert!(
        bool_value(&dict_field(&result, "supported")),
        "language `{language}` must be a supported tree-sitter grammar"
    );
    list_value(&dict_field(&result, "errors"))
        .iter()
        .map(|e| string_value(&dict_field(e, "message")))
        .collect()
}

// ---------------------------------------------------------------------------
// The contract
// ---------------------------------------------------------------------------

/// Every corpus fixture's BUNDLED parser facts must agree with ground truth. A
/// grammar regression that disagrees fails here, in CI, before it can ship a
/// phantom fact to the model.
#[test]
fn bundled_parser_facts_agree_with_ground_truth() {
    let registry = ast_registry();
    for case in corpus() {
        let source = fixture_source(case.fixture);

        // Fact 1 — the import set the host extracts must match ground truth
        // exactly (a phantom or dropped import is a steering hazard).
        let imports = extracted_imports(&registry, &source, case.language);
        let expected: Vec<String> = case
            .expected_imports
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(
            imports, expected,
            "parser-agreement MISMATCH on `{}` ({}): extracted imports diverged from ground truth — \
             a bundled grammar bump is shipping phantom/dropped import facts to the model",
            case.fixture, case.language,
        );

        // Fact 2 — valid modern source must parse with no ERROR/MISSING nodes.
        // This is the direct guard on the zig multiline-string regression: a
        // re-introduced mislex makes the parse-error storm reappear here.
        let errors = parse_error_messages(&registry, &source, case.language);
        if case.expect_clean_parse {
            assert!(
                errors.is_empty(),
                "parser-agreement MISMATCH on `{}` ({}): expected a CLEAN parse but the bundled \
                 grammar reported {} error(s): {:?} — a grammar regression is mis-lexing valid \
                 source into phantom parse errors",
                case.fixture,
                case.language,
                errors.len(),
                errors,
            );
        }
    }
}

/// Guard the SEED regression on its own so a failure names it unambiguously: the
/// zig `\\` multiline string must parse clean AND surface no phantom imports.
#[test]
fn zig_multiline_string_does_not_regress_to_phantom_facts() {
    let registry = ast_registry();
    let source = fixture_source("sample.zig");
    assert!(
        source.contains("\\\\SELECT"),
        "the seed fixture must contain a `\\\\` multiline string"
    );
    let errors = parse_error_messages(&registry, &source, "zig");
    assert!(
        errors.is_empty(),
        "REGRESSION: the bundled tree-sitter-zig grammar mis-lexed the `\\\\` multiline string \
         into {} phantom parse error(s): {:?} (this is the #3010 class)",
        errors.len(),
        errors,
    );
    let imports = extracted_imports(&registry, &source, "zig");
    assert!(
        imports.is_empty(),
        "zig `@import` builtins are not import declarations; the grammar must surface none, got {imports:?}"
    );
}
