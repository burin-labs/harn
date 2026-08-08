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

/// What a block keyword does to block structure.
///
/// Highlighting only needs the spelling, but an editor that completes a
/// block needs to know what closes it — and that is exactly where the
/// language is easy to get wrong. See [`DIRECTIVES`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirectiveRole {
    /// Opens a block that `closer` ends, optionally with `continuations`
    /// in between.
    Opens {
        closer: &'static str,
        continuations: &'static [&'static str],
    },
    /// Divides a block opened by one of `opened_by`.
    Continues { opened_by: &'static [&'static str] },
    /// Ends a block opened by one of `opened_by`.
    Closes { opened_by: &'static [&'static str] },
    /// A complete directive on its own.
    Standalone,
}

/// One block keyword, with the structure it implies and prose an editor
/// can show.
#[derive(Debug, Clone, Copy)]
pub struct Directive {
    pub keyword: &'static str,
    pub role: DirectiveRole,
    /// How the directive is written, for an editor to show inline.
    pub syntax: &'static str,
    /// One line describing what it does.
    pub summary: &'static str,
}

impl Directive {
    /// The keyword that closes the block this one opens, if it opens one.
    pub fn closer(&self) -> Option<&'static str> {
        match self.role {
            DirectiveRole::Opens { closer, .. } => Some(closer),
            _ => None,
        }
    }
}

/// Words that open, continue, or close a block.
///
/// Recognized by `parser.rs` (all but `raw`/`endraw`) and by `lexer.rs`,
/// which handles the verbatim `raw` block before the parser ever sees it.
///
/// **Both `{{ if }}` and `{{ for }}` close with `{{ end }}`.** There is no
/// `{{ endif }}` or `{{ endfor }}`, and writing one is worse than an
/// error: `endif` is a valid bare identifier, so it takes the pre-v2
/// passthrough path and renders as the literal text while leaving the
/// block unclosed. Editors read this table precisely so they never offer
/// a closer the parser does not implement.
pub static DIRECTIVES: &[Directive] = &[
    Directive {
        keyword: "if",
        role: DirectiveRole::Opens {
            closer: "end",
            continuations: &["elif", "else"],
        },
        syntax: "{{ if condition }}",
        summary: "Render the body when the condition is truthy.",
    },
    Directive {
        keyword: "elif",
        role: DirectiveRole::Continues { opened_by: &["if"] },
        syntax: "{{ elif condition }}",
        summary: "Alternative branch of the enclosing `{{ if }}`.",
    },
    Directive {
        keyword: "else",
        role: DirectiveRole::Continues {
            opened_by: &["if", "for"],
        },
        syntax: "{{ else }}",
        summary: "Fallback branch. After `{{ for }}` it renders when the \
                  iterable is empty.",
    },
    Directive {
        keyword: "end",
        role: DirectiveRole::Closes {
            opened_by: &["if", "for"],
        },
        syntax: "{{ end }}",
        summary: "Close the enclosing `{{ if }}` or `{{ for }}`.",
    },
    Directive {
        keyword: "for",
        role: DirectiveRole::Opens {
            closer: "end",
            continuations: &["else"],
        },
        syntax: "{{ for item in items }}",
        summary: "Repeat the body for each item. `{{ for key, value in dict }}` \
                  iterates a dict.",
    },
    Directive {
        keyword: "include",
        role: DirectiveRole::Standalone,
        syntax: "{{ include \"partial.harn.prompt\" }}",
        summary: "Render another template here. `with { name: value }` passes \
                  bindings to it.",
    },
    Directive {
        keyword: "section",
        role: DirectiveRole::Opens {
            closer: "endsection",
            continuations: &[],
        },
        syntax: "{{ section \"name\" }}",
        summary: "Wrap the body in a capability-adaptive envelope chosen for the \
                  active model.",
    },
    Directive {
        keyword: "endsection",
        role: DirectiveRole::Closes {
            opened_by: &["section"],
        },
        syntax: "{{ endsection }}",
        summary: "Close the enclosing `{{ section }}`.",
    },
    Directive {
        keyword: "raw",
        role: DirectiveRole::Opens {
            closer: "endraw",
            continuations: &[],
        },
        syntax: "{{ raw }}",
        summary: "Emit the body verbatim, leaving `{{ }}` untouched.",
    },
    Directive {
        keyword: "endraw",
        role: DirectiveRole::Closes {
            opened_by: &["raw"],
        },
        syntax: "{{ endraw }}",
        summary: "Close the enclosing `{{ raw }}`.",
    },
];

/// The block keywords, spelling only — what the grammar generator needs.
/// Derived from [`DIRECTIVES`] so there is one list, not two.
pub fn block_keywords() -> Vec<&'static str> {
    DIRECTIVES.iter().map(|d| d.keyword).collect()
}

/// The directive named `keyword`, if the parser recognises one.
pub fn directive(keyword: &str) -> Option<&'static Directive> {
    DIRECTIVES.iter().find(|d| d.keyword == keyword)
}

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
/// Derived from [`super::filters::FILTERS`], which `apply_filter`
/// dispatches through — so the spelling an editor highlights and the
/// filter the engine runs cannot disagree. The full table carries each
/// filter's parameters and description too; this is the spelling-only
/// view the grammar generator needs.
pub fn filter_names() -> Vec<&'static str> {
    super::filters::FILTERS.iter().map(|f| f.name).collect()
}

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
        for word in block_keywords()
            .into_iter()
            .chain(CLAUSE_KEYWORDS.iter().copied())
            .chain(OPERATOR_KEYWORDS.iter().copied())
            .chain(LITERAL_KEYWORDS.iter().copied())
        {
            assert!(seen.insert(word), "keyword `{word}` declared twice");
        }
        let mut filters = std::collections::BTreeSet::new();
        for f in filter_names() {
            assert!(filters.insert(f), "filter `{f}` declared twice");
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

        let declared: std::collections::BTreeSet<&str> = block_keywords().into_iter().collect();
        assert_eq!(
            declared, proved,
            "every declared block keyword needs a case proving the engine still \
             implements it, and every case needs its keyword declared"
        );
    }

    /// Recognising a keyword is not the same as knowing what closes it,
    /// and the pairings are what an editor completes from. Each declared
    /// opener is checked against a real parse of the block it claims.
    #[test]
    fn every_declared_pairing_is_what_the_parser_accepts() {
        for (keyword, body) in [
            ("if", "{{ if a }}\nx\n{{ end }}"),
            ("for", "{{ for x in xs }}\nx\n{{ end }}"),
            ("section", "{{ section \"task\" }}\nx\n{{ endsection }}"),
            ("raw", "{{ raw }}\nx\n{{ endraw }}"),
        ] {
            let closer = directive(keyword)
                .and_then(Directive::closer)
                .unwrap_or_else(|| panic!("`{keyword}` should declare a closer"));
            assert!(
                body.contains(&format!("{{{{ {closer} }}}}")),
                "`{keyword}` declares `{closer}`, which the sample does not use"
            );
            assert!(
                validate_template_syntax(body).is_ok(),
                "`{keyword}` closed by `{closer}` should parse"
            );
        }
    }

    /// The trap the pairings exist to close. `{{ endif }}` is the reflex
    /// from other template languages; here `endif` is an ordinary
    /// identifier, so a stray one parses silently and renders as literal
    /// text while the block it looks like it closed stays open.
    #[test]
    fn end_prefixed_spellings_close_nothing() {
        for wrong in ["endif", "endfor"] {
            assert!(
                directive(wrong).is_none(),
                "`{wrong}` must not be suggestable"
            );
            assert!(
                validate_template_syntax(&format!("intro\n{{{{ {wrong} }}}}\n")).is_ok(),
                "a stray `{wrong}` parses as a variable — that is the trap"
            );
        }
        let err = validate_template_syntax("{{ if a }}\nx\n{{ endif }}")
            .expect_err("`{{ endif }}` does not close `{{ if }}`");
        assert!(err.contains("missing matching"), "unexpected error: {err}");
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
