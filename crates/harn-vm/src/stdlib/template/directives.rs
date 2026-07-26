//! The directive vocabulary of the prompt-template language.
//!
//! Both `{{ if }}` and `{{ for }}` close with `{{ end }}`. There is no
//! `{{ endif }}` or `{{ endfor }}`, and writing one is worse than an
//! error: `endif` is a valid bare identifier, so it takes the pre-v2
//! passthrough path and renders as the literal text `{{ endif }}`. The
//! author sees a block they believe is closed and a prompt that quietly
//! carries a stray brace into the model.
//!
//! A closed vocabulary is what lets an editor refuse to suggest it.
//! [`DIRECTIVES`] is that vocabulary, read by tooling for completion and
//! hover. The tests at the bottom drive the real parser to prove every
//! relationship declared here, so the table cannot claim a pairing the
//! engine does not implement.

/// What a keyword does to block structure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirectiveRole {
    /// Opens a block that `closer` ends, optionally with
    /// `continuations` in between.
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

/// One keyword an author can write inside `{{ }}`.
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

/// Every keyword the template parser recognises inside `{{ }}`.
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
        keyword: "end",
        role: DirectiveRole::Closes {
            opened_by: &["if", "for"],
        },
        syntax: "{{ end }}",
        summary: "Close the enclosing `{{ if }}` or `{{ for }}`.",
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
    Directive {
        keyword: "include",
        role: DirectiveRole::Standalone,
        syntax: "{{ include \"partial.harn.prompt\" }}",
        summary: "Render another template here. `with { name: value }` passes \
                  bindings to it.",
    },
];

/// The directive named `keyword`, if the parser recognises one.
pub fn lookup(keyword: &str) -> Option<&'static Directive> {
    DIRECTIVES
        .iter()
        .find(|directive| directive.keyword == keyword)
}

#[cfg(test)]
mod tests {
    use super::{lookup, Directive, DirectiveRole, DIRECTIVES};
    use crate::stdlib::template::outline::{self, OutlineBlockKind};

    /// A minimal well-formed body for a block-opening keyword, so the
    /// claims below can be checked against a real parse.
    fn opener_body(keyword: &str) -> (String, OutlineBlockKind) {
        match keyword {
            "if" => ("{{ if a }}\nx\n{{ end }}".into(), OutlineBlockKind::If),
            "for" => (
                "{{ for x in xs }}\nx\n{{ end }}".into(),
                OutlineBlockKind::For,
            ),
            "section" => (
                "{{ section \"task\" }}\nx\n{{ endsection }}".into(),
                OutlineBlockKind::Section,
            ),
            "raw" => ("{{ raw }}\nx\n{{ endraw }}".into(), OutlineBlockKind::Raw),
            other => panic!("no sample for opener `{other}`"),
        }
    }

    fn openers() -> impl Iterator<Item = &'static Directive> {
        DIRECTIVES
            .iter()
            .filter(|d| matches!(d.role, DirectiveRole::Opens { .. }))
    }

    #[test]
    fn every_declared_pairing_is_what_the_parser_accepts() {
        for directive in openers() {
            let (source, expected) = opener_body(directive.keyword);
            let blocks = outline::parse(&source).unwrap_or_else(|error| {
                panic!(
                    "`{{{{ {} }}}}` closed by `{{{{ {} }}}}` should parse: {error}",
                    directive.keyword,
                    directive.closer().unwrap(),
                )
            });
            assert!(
                blocks.iter().any(|block| block.kind == expected),
                "`{}` did not produce a {expected:?} block",
                directive.keyword,
            );
        }
    }

    #[test]
    fn no_opener_accepts_an_end_prefixed_spelling_of_itself() {
        // `{{ endif }}` / `{{ endfor }}` are the reflex from other
        // template languages. Neither closes anything here.
        for (opener, wrong) in [("if", "endif"), ("for", "endfor")] {
            let (source, _) = opener_body(opener);
            let closer = lookup(opener).unwrap().closer().unwrap();
            let swapped = source.replace(closer, wrong);
            let error = outline::parse(&swapped)
                .expect_err("a block closed with the wrong keyword must not parse");
            assert!(
                error.message.contains("missing matching"),
                "unexpected message for `{{{{ {wrong} }}}}`: {}",
                error.message,
            );
        }
    }

    #[test]
    fn a_stray_end_prefixed_keyword_is_silently_a_variable() {
        // The trap this vocabulary exists to close: with no block open,
        // `{{ endif }}` is a bare identifier, so it parses cleanly and
        // contributes no block. Nothing tells the author it did nothing.
        let blocks = outline::parse("intro\n{{ endif }}\n").expect("parses as a variable lookup");
        assert!(blocks.is_empty());
        assert!(lookup("endif").is_none(), "`endif` must not be suggestable");
        assert!(
            lookup("endfor").is_none(),
            "`endfor` must not be suggestable"
        );
    }

    #[test]
    fn continuations_parse_inside_the_blocks_that_declare_them() {
        assert!(outline::parse("{{ if a }}\nx\n{{ elif b }}\ny\n{{ end }}").is_ok());
        assert!(outline::parse("{{ if a }}\nx\n{{ else }}\ny\n{{ end }}").is_ok());
        assert!(outline::parse("{{ for x in xs }}\nx\n{{ else }}\nnone\n{{ end }}").is_ok());
    }

    #[test]
    fn every_referenced_keyword_is_itself_in_the_vocabulary() {
        for directive in DIRECTIVES {
            let referenced: Vec<&str> = match directive.role {
                DirectiveRole::Opens {
                    closer,
                    continuations,
                } => std::iter::once(closer)
                    .chain(continuations.iter().copied())
                    .collect(),
                DirectiveRole::Continues { opened_by } | DirectiveRole::Closes { opened_by } => {
                    opened_by.to_vec()
                }
                DirectiveRole::Standalone => Vec::new(),
            };
            for keyword in referenced {
                assert!(
                    lookup(keyword).is_some(),
                    "`{}` references `{keyword}`, which is not in the vocabulary",
                    directive.keyword,
                );
            }
        }
    }

    #[test]
    fn keywords_are_unique() {
        let mut keywords: Vec<&str> = DIRECTIVES.iter().map(|d| d.keyword).collect();
        let before = keywords.len();
        keywords.sort_unstable();
        keywords.dedup();
        assert_eq!(
            before,
            keywords.len(),
            "duplicate keyword in the vocabulary"
        );
    }
}
