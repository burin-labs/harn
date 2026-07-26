//! The words a `.harn.prompt` author can write, grouped by the role they play
//! inside `{{ ... }}`.
//!
//! Editors need this vocabulary to highlight a template, and the engine needs
//! it to render one. Keeping two copies is how the VS Code grammar drifted from
//! the engine in the first place, so this module is the single owner: the
//! engine consumes these constants, and `harn dump-prompt-grammar` generates
//! the TextMate grammar from them.
//!
//! Adding a keyword, filter, or section means adding it here. The tests at the
//! bottom of this module and of `filters.rs` fail if a name lands in the
//! engine's control flow without being declared, so the grammar cannot silently
//! fall behind.

/// Words that open, continue, or close a block: `{{ if }}`, `{{ elif }}`,
/// `{{ else }}`, `{{ end }}`, `{{ for }}`, `{{ include }}`, `{{ section }}`,
/// `{{ endsection }}`, `{{ raw }}`, `{{ endraw }}`.
///
/// Recognized by `parser.rs` (all but `raw`/`endraw`) and by `lexer.rs`, which
/// handles the verbatim `raw` block before the parser ever sees it.
pub const BLOCK_KEYWORDS: &[&str] = &[
    "if",
    "elif",
    "else",
    "end",
    "for",
    "include",
    "section",
    "endsection",
    "raw",
    "endraw",
];

/// Contextual words that separate the clauses of a block header: the `in` of
/// `{{ for x in xs }}` and the `with` of `{{ include "p" with {..} }}`.
///
/// They are keywords only in that position; elsewhere they are ordinary
/// identifiers.
pub const CLAUSE_KEYWORDS: &[&str] = &["in", "with"];

/// Word-spelled boolean operators recognized by the expression lexer.
pub const OPERATOR_KEYWORDS: &[&str] = &["and", "or", "not"];

/// Literals recognized by the expression lexer.
pub const LITERAL_KEYWORDS: &[&str] = &["true", "false", "nil"];

/// Filters callable after a `|` in an interpolation, e.g. `{{ name | upper }}`.
///
/// Must match the arms of `filters::apply_filter` exactly; a test in that
/// module enforces it.
pub const FILTERS: &[&str] = &[
    "capitalize",
    "default",
    "escape_md",
    "first",
    "indent",
    "join",
    "json",
    "last",
    "length",
    "lines",
    "lower",
    "replace",
    "reverse",
    "title",
    "trim",
    "upper",
];

/// Names accepted by `{{ section "..." }}`. This is the authority:
/// `sections::is_builtin_section` reads it, so an undeclared name is a template
/// error rather than a silently un-highlighted one.
pub const SECTIONS: &[&str] = &[
    "task",
    "examples",
    "output_format",
    "tools",
    "thinking_scaffold",
    "chain_of_thought",
    "system_framing",
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stdlib::template::validate_template_syntax;

    /// Every declared word must be spelled uniquely within its group and across
    /// the groups that share a TextMate scope, so the generated grammar cannot
    /// emit a duplicate regex alternative.
    #[test]
    fn declared_words_are_unique() {
        let mut seen = std::collections::BTreeSet::new();
        for word in BLOCK_KEYWORDS
            .iter()
            .chain(CLAUSE_KEYWORDS)
            .chain(OPERATOR_KEYWORDS)
            .chain(LITERAL_KEYWORDS)
        {
            assert!(seen.insert(*word), "keyword `{word}` declared twice");
        }
        let mut filters = std::collections::BTreeSet::new();
        for f in FILTERS {
            assert!(filters.insert(*f), "filter `{f}` declared twice");
        }
        let mut sections = std::collections::BTreeSet::new();
        for s in SECTIONS {
            assert!(sections.insert(*s), "section `{s}` declared twice");
        }
    }

    /// The parser must actually recognize each declared block keyword. A word
    /// that no longer means anything to the engine would otherwise keep its
    /// highlight forever.
    ///
    /// Recognition is proved two ways: a well-formed block, which only parses
    /// if every keyword in it means what it claims; and, for the closers, the
    /// *specific* "unexpected" diagnostic the parser raises for a stray one. A
    /// word the parser did not know would instead parse as a bare interpolation
    /// and produce no error at all.
    ///
    /// The proofs are checked to cover `BLOCK_KEYWORDS` exhaustively, so
    /// declaring a keyword the engine does not implement — which would give it
    /// editor highlighting it has not earned — fails here.
    #[test]
    fn parser_recognizes_every_block_keyword() {
        let mut proved: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();

        // Each source parses only if all the keywords it is credited with are
        // live. `{{ raw }}` covers `endraw` too: an unknown terminator would
        // leave the block unterminated, which is an error.
        for (keywords, well_formed) in [
            (&["if", "end"][..], "{{ if true }}x{{ end }}"),
            (&["elif"][..], "{{ if false }}x{{ elif true }}y{{ end }}"),
            (&["else"][..], "{{ if false }}x{{ else }}y{{ end }}"),
            (&["for"][..], "{{ for x in items }}{{ x }}{{ end }}"),
            // `include` resolves its target at render time, so a missing file
            // is not a syntax error here.
            (&["include"][..], "{{ include \"other.prompt\" }}"),
            (
                &["section", "endsection"][..],
                "{{ section \"task\" }}body{{ endsection }}",
            ),
            (
                &["raw", "endraw"][..],
                "{{ raw }}{{ not a directive }}{{ endraw }}",
            ),
        ] {
            let result = validate_template_syntax(well_formed);
            assert!(
                result.is_ok(),
                "declared keyword(s) {keywords:?} failed to parse: {result:?}"
            );
            proved.extend(keywords);
        }

        for (keyword, stray) in [
            ("end", "{{ end }}"),
            ("endsection", "{{ endsection }}"),
            ("else", "{{ else }}"),
            ("elif", "{{ elif true }}"),
        ] {
            let Err(err) = validate_template_syntax(stray) else {
                panic!("a stray `{keyword}` should not parse");
            };
            assert!(
                err.contains(keyword),
                "stray `{keyword}` did not raise a keyword-specific error: {err}"
            );
            proved.insert(keyword);
        }

        let declared: std::collections::BTreeSet<&str> = BLOCK_KEYWORDS.iter().copied().collect();
        assert_eq!(
            declared, proved,
            "every declared block keyword needs a case proving the engine still \
             implements it, and every case needs its keyword declared"
        );
    }

    /// `in` and `with` must still be the words that split a block header.
    #[test]
    fn parser_recognizes_every_clause_keyword() {
        assert!(CLAUSE_KEYWORDS.contains(&"in"));
        assert!(validate_template_syntax("{{ for x in items }}{{ x }}{{ end }}").is_ok());
        assert!(
            validate_template_syntax("{{ for x of items }}{{ x }}{{ end }}").is_err(),
            "`in` is no longer the for-loop separator"
        );

        assert!(CLAUSE_KEYWORDS.contains(&"with"));
        assert!(validate_template_syntax("{{ include \"p\" with { item: x } }}").is_ok());
    }

    /// The expression lexer must still spell its operators and literals this
    /// way. Each is used where only a keyword parses.
    #[test]
    fn expression_lexer_recognizes_operators_and_literals() {
        for word in OPERATOR_KEYWORDS {
            let src = match *word {
                "not" => "{{ if not true }}x{{ end }}".to_string(),
                other => format!("{{{{ if true {other} false }}}}x{{{{ end }}}}"),
            };
            assert!(
                validate_template_syntax(&src).is_ok(),
                "declared operator `{word}` no longer parses"
            );
        }
        for word in LITERAL_KEYWORDS {
            let src = format!("{{{{ if {word} }}}}x{{{{ end }}}}");
            assert!(
                validate_template_syntax(&src).is_ok(),
                "declared literal `{word}` no longer parses"
            );
        }
    }

    /// Every declared section name must be one the parser accepts, and an
    /// undeclared name must be rejected — that is what makes this list the
    /// authority rather than a mirror.
    #[test]
    fn parser_accepts_exactly_the_declared_sections() {
        for name in SECTIONS {
            let src = format!("{{{{ section \"{name}\" }}}}body{{{{ endsection }}}}");
            assert!(
                validate_template_syntax(&src).is_ok(),
                "declared section `{name}` is not accepted by the parser"
            );
        }
        let err = validate_template_syntax("{{ section \"nope\" }}b{{ endsection }}")
            .expect_err("undeclared section should be rejected");
        assert!(
            err.contains("unknown template section"),
            "unexpected error for undeclared section: {err}"
        );
    }
}
