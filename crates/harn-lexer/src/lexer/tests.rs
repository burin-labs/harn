use super::*;

#[test]
fn escape_string_literal_round_trips_through_the_lexer() {
    for original in [
        "plain text",
        "with \"quotes\" and \\backslash",
        "newline\ntab\tcr\r",
        "interpolation ${1 + 1} stays text",
        "already-escaped \\${x}",
        "plain $dollar and ${nested ${inner}}",
    ] {
        let literal = format!("\"{}\"", escape_string_literal(original));
        let mut lexer = Lexer::new(&literal);
        let tokens = lexer.tokenize().expect("escaped literal must lex");
        let TokenKind::StringLiteral(ref value) = tokens[0].kind else {
            panic!(
                "expected a plain string token for {literal:?}, got {:?}",
                tokens[0].kind
            );
        };
        assert_eq!(value, original, "round trip through {literal:?}");
    }
}

#[test]
fn shebang_at_offset_zero_is_skipped() {
    let src = "#!/usr/bin/env harn\nlet x = 1";
    let mut lexer = Lexer::new(src);
    let tokens = lexer.tokenize().expect("shebang should be skipped");
    // Expect: Newline, Let, Identifier(x), Eq, IntLiteral(1)
    assert_eq!(tokens[0].kind, TokenKind::Newline);
    assert_eq!(tokens[1].kind, TokenKind::Let);
    assert!(matches!(&tokens[2].kind, TokenKind::Identifier(n) if n == "x"));
}

#[test]
fn shebang_without_trailing_newline_is_skipped() {
    let src = "#!/usr/bin/env harn";
    let mut lexer = Lexer::new(src);
    let tokens = lexer.tokenize().expect("shebang at EOF should be skipped");
    // After the shebang there should be only the trailing EOF token.
    let non_eof: Vec<_> = tokens
        .iter()
        .filter(|t| !matches!(t.kind, TokenKind::Eof))
        .collect();
    assert!(
        non_eof.is_empty(),
        "expected only EOF after shebang-only file, got {non_eof:?}"
    );
}

#[test]
fn hash_in_middle_of_file_still_errors() {
    let src = "let x = 1\n# not a shebang\n";
    let mut lexer = Lexer::new(src);
    let result = lexer.tokenize();
    assert!(
        matches!(result, Err(LexerError::UnexpectedCharacter('#', _))),
        "got {result:?}"
    );
}

#[test]
fn test_keywords() {
    let mut lexer = Lexer::new("pipeline let var if else for in require");
    let tokens = lexer.tokenize().unwrap();
    assert_eq!(tokens[0].kind, TokenKind::Pipeline);
    assert_eq!(tokens[1].kind, TokenKind::Let);
    assert_eq!(tokens[2].kind, TokenKind::Var);
    assert_eq!(tokens[3].kind, TokenKind::If);
    assert_eq!(tokens[4].kind, TokenKind::Else);
    assert_eq!(tokens[5].kind, TokenKind::For);
    assert_eq!(tokens[6].kind, TokenKind::In);
    assert_eq!(tokens[7].kind, TokenKind::Require);
}

#[test]
fn generated_keyword_vocabulary_tokenizes_every_entry() {
    // Every string in KEYWORDS must lex as a non-identifier token.
    // If this fails, either KEYWORDS has a stale entry or the lexer
    // match in `identifier_or_keyword` is missing an arm.
    for kw in KEYWORDS {
        let mut lexer = Lexer::new(kw);
        let tokens = lexer.tokenize().expect("lex keyword");
        let first = &tokens[0].kind;
        assert!(
            !matches!(first, TokenKind::Identifier(_)),
            "keyword `{kw}` lexes as Identifier"
        );
    }
}

#[test]
fn test_parallel_keyword() {
    let mut lexer = Lexer::new("parallel defer");
    let tokens = lexer.tokenize().unwrap();
    assert_eq!(tokens[0].kind, TokenKind::Parallel);
    assert_eq!(tokens[1].kind, TokenKind::Defer);
}

#[test]
fn test_numbers() {
    let mut lexer = Lexer::new("42 3.14");
    let tokens = lexer.tokenize().unwrap();
    assert_eq!(tokens[0].kind, TokenKind::IntLiteral(42));
    #[allow(clippy::approx_constant)]
    let expected = 3.14;
    assert_eq!(tokens[1].kind, TokenKind::FloatLiteral(expected));
}

#[test]
fn test_int_literal_max_is_exact_and_overflow_is_an_error() {
    // i64::MAX lexes exactly as an int.
    let mut lexer = Lexer::new("9223372036854775807");
    let tokens = lexer.tokenize().unwrap();
    assert_eq!(tokens[0].kind, TokenKind::IntLiteral(i64::MAX));

    // i64::MAX + 1 (and anything larger) is rejected, not silently widened
    // to a lossy float. The sign is applied by the parser, so the most
    // negative i64 is unreachable as a bare literal and overflows here too.
    for src in ["9223372036854775808", "99999999999999999999999"] {
        let mut lexer = Lexer::new(src);
        assert!(
            matches!(
                lexer.tokenize(),
                Err(LexerError::IntegerLiteralOutOfRange(lit, _)) if lit == src
            ),
            "expected out-of-range error for {src}"
        );
    }

    // A float literal of the same magnitude is still fine.
    let mut lexer = Lexer::new("9223372036854775808.0");
    let tokens = lexer.tokenize().unwrap();
    assert!(matches!(tokens[0].kind, TokenKind::FloatLiteral(_)));
}

#[test]
fn test_duration_suffix_requires_identifier_boundary() {
    let mut lexer = Lexer::new("1ms 1msfoo 2h_task 3s.ok");
    let tokens = lexer.tokenize().unwrap();
    assert_eq!(tokens[0].kind, TokenKind::DurationLiteral(1));
    assert_eq!(tokens[1].kind, TokenKind::IntLiteral(1));
    assert!(matches!(&tokens[2].kind, TokenKind::Identifier(name) if name == "msfoo"));
    assert_eq!(tokens[3].kind, TokenKind::IntLiteral(2));
    assert!(matches!(&tokens[4].kind, TokenKind::Identifier(name) if name == "h_task"));
    assert_eq!(tokens[5].kind, TokenKind::DurationLiteral(3000));
    assert_eq!(tokens[6].kind, TokenKind::Dot);
}

#[test]
fn test_duration_suffix_overflow_saturates() {
    let mut lexer = Lexer::new("18446744073709551615w");
    let tokens = lexer.tokenize().unwrap();
    assert_eq!(tokens[0].kind, TokenKind::DurationLiteral(u64::MAX));
}

#[test]
fn test_string() {
    let mut lexer = Lexer::new(r#""hello world""#);
    let tokens = lexer.tokenize().unwrap();
    assert_eq!(
        tokens[0].kind,
        TokenKind::StringLiteral("hello world".into())
    );
}

#[test]
fn test_interpolated_string() {
    let mut lexer = Lexer::new(r#""hello ${name}!""#);
    let tokens = lexer.tokenize().unwrap();
    if let TokenKind::InterpolatedString(segs) = &tokens[0].kind {
        assert_eq!(segs.len(), 3);
        assert_eq!(segs[0], StringSegment::Literal("hello ".into()));
        assert!(matches!(&segs[1], StringSegment::Expression(e, _, _) if e == "name"));
        assert_eq!(segs[2], StringSegment::Literal("!".into()));
    } else {
        panic!("Expected interpolated string");
    }
}

/// Returns the captured text of the single interpolation hole in `src`.
fn single_interpolation_expr(src: &str) -> String {
    let mut lexer = Lexer::new(src);
    let tokens = lexer.tokenize().unwrap();
    match &tokens[0].kind {
        TokenKind::InterpolatedString(segs) => segs
            .iter()
            .find_map(|s| match s {
                StringSegment::Expression(e, _, _) => Some(e.clone()),
                _ => None,
            })
            .expect("expected an interpolation expression segment"),
        other => panic!("expected interpolated string, got {other:?}"),
    }
}

#[test]
fn test_interpolation_capture_is_string_literal_aware() {
    // A `}` (or `{`) inside a nested string literal must not end the hole.
    assert_eq!(
        single_interpolation_expr(r#""${x ?? "a}b"}""#),
        r#"x ?? "a}b""#
    );
    assert_eq!(single_interpolation_expr(r#""${f("}")}""#), r#"f("}")"#);
    assert_eq!(
        single_interpolation_expr(r#""${items["a}b"]}""#),
        r#"items["a}b"]"#
    );
    // A `\"` inside the nested string is preserved verbatim so it does not
    // close the literal early.
    assert_eq!(
        single_interpolation_expr(r#""${x ?? "a\"b"}""#),
        r#"x ?? "a\"b""#
    );
}

#[test]
fn test_interpolation_escaped_outer_quote_is_rejected() {
    // Escaping the quotes of a nested string literal (`${x ?? \"y\"}`) is a
    // common mistake: inside an interpolation hole, string literals use bare
    // double quotes. A backslash is never valid in expression position, so
    // it is reported precisely at the backslash rather than scanning to EOF.
    let mut lexer = Lexer::new(r#""${x ?? \"y\"}""#);
    assert!(matches!(
        lexer.tokenize(),
        Err(LexerError::UnexpectedCharacter('\\', _))
    ));
}

#[test]
fn test_empty_interpolation_rejected_in_single_and_multiline_strings() {
    let mut single = Lexer::new(r#""hello ${}""#);
    assert!(matches!(
        single.tokenize(),
        Err(LexerError::UnexpectedCharacter('}', _))
    ));

    let mut multiline = Lexer::new("\"\"\"\nhello ${}\n\"\"\"");
    assert!(matches!(
        multiline.tokenize(),
        Err(LexerError::UnexpectedCharacter('}', _))
    ));
}

#[test]
fn test_multiline_string_escaped_dollar_before_interpolation() {
    let mut lexer = Lexer::new("\"\"\"\n  hi \\${VAR}\n  hello ${name}\n\"\"\"");
    let tokens = lexer.tokenize().unwrap();
    if let TokenKind::InterpolatedString(segs) = &tokens[0].kind {
        assert_eq!(segs.len(), 2);
        assert_eq!(segs[0], StringSegment::Literal("hi ${VAR}\nhello ".into()));
        assert!(matches!(&segs[1], StringSegment::Expression(e, _, _) if e == "name"));
    } else {
        panic!("Expected interpolated string");
    }
}

#[test]
fn test_multiline_string_escaped_dollar_without_interpolation() {
    let mut lexer = Lexer::new("\"\"\"\n  hi \\${VAR}\n\"\"\"");
    let tokens = lexer.tokenize().unwrap();
    assert_eq!(tokens[0].kind, TokenKind::StringLiteral("hi ${VAR}".into()));
}

#[test]
fn test_multiline_string_preserves_non_interpolation_dollar_escape() {
    let mut lexer = Lexer::new("\"\"\"\n  echo \\$PATH\n\"\"\"");
    let tokens = lexer.tokenize().unwrap();
    assert_eq!(
        tokens[0].kind,
        TokenKind::StringLiteral("echo \\$PATH".into())
    );
}

#[test]
fn test_interpolated_string_multiline_expression_tracks_lines() {
    // Regression: `${...}` inside a single-line string can itself span
    // multiple lines (e.g. `${render(\n  "x",\n  {k: v},\n)}`). The
    // lexer used to consume those inner newlines without incrementing
    // the line counter, so every token after the string reported a
    // line number too low — by the number of newlines consumed inside
    // the interpolation. Downstream lint spans pointed to wrong lines.
    let src = "const x = \"${render(\n  \"a\",\n  b,\n)}\"\nconst y = 1\n";
    let mut lexer = Lexer::new(src);
    let tokens = lexer.tokenize().unwrap();
    // `const y` is on line 5 of the source.
    let const_y = tokens
        .iter()
        .skip(1) // the first `const` at line 1
        .find(|t| matches!(t.kind, TokenKind::Const))
        .expect("second `const`");
    assert_eq!(const_y.span.line, 5);
}

#[test]
fn test_two_char_operators() {
    let mut lexer = Lexer::new("== != && || |> ?? ** -> <= >=");
    let tokens = lexer.tokenize().unwrap();
    assert_eq!(tokens[0].kind, TokenKind::Eq);
    assert_eq!(tokens[1].kind, TokenKind::Neq);
    assert_eq!(tokens[2].kind, TokenKind::And);
    assert_eq!(tokens[3].kind, TokenKind::Or);
    assert_eq!(tokens[4].kind, TokenKind::Pipe);
    assert_eq!(tokens[5].kind, TokenKind::NilCoal);
    assert_eq!(tokens[6].kind, TokenKind::Pow);
    assert_eq!(tokens[7].kind, TokenKind::Arrow);
    assert_eq!(tokens[8].kind, TokenKind::Lte);
    assert_eq!(tokens[9].kind, TokenKind::Gte);
}

#[test]
fn test_block_comments() {
    let mut lexer = Lexer::new("/* outer /* nested */ still */ 42");
    let tokens = lexer.tokenize().unwrap();
    assert_eq!(tokens[0].kind, TokenKind::IntLiteral(42));
}

#[test]
fn test_multiline_block_comment_span_starts_at_open() {
    // A block comment that opens on line 2 and closes on line 4. Its span's
    // `line`/`column` must point at the opening `/*` (line 2), and `end_line`
    // at the closing `*/` (line 4) — matching how multi-line strings record
    // their span. Downstream consumers (`harn fmt`, the LSP) key comments by
    // `span.line`, so reporting the end line there misplaces the comment.
    let src = "const a = 1\n/* block\n   spanning\n   lines */\nconst b = 2";
    let mut lex = Lexer::new(src);
    let tokens = lex.tokenize_with_comments().unwrap();
    let block = tokens
        .iter()
        .find(|t| matches!(t.kind, TokenKind::BlockComment { .. }))
        .expect("block comment token");
    assert_eq!(block.span.line, 2, "start line should be the opening `/*`");
    assert_eq!(
        block.span.column, 1,
        "start column should be the `/*` column"
    );
    assert_eq!(
        block.span.end_line, 4,
        "end line should be the closing `*/`"
    );
}

#[test]
fn test_line_comment() {
    let mut lexer = Lexer::new("42 // comment\n43");
    let tokens = lexer.tokenize().unwrap();
    assert_eq!(tokens[0].kind, TokenKind::IntLiteral(42));
    assert_eq!(tokens[1].kind, TokenKind::Newline);
    assert_eq!(tokens[2].kind, TokenKind::IntLiteral(43));
}

#[test]
fn test_doc_line_comment_detection() {
    let cases = [
        ("// regular", false),
        ("/// doc", true),
        ("//// separator bar", false),
        ("///// also a bar", false),
        ("///", true), // empty doc comment
    ];
    for (src, expect_doc) in cases {
        let mut lex = Lexer::new(src);
        let tokens = lex.tokenize_with_comments().unwrap();
        match &tokens[0].kind {
            TokenKind::LineComment { is_doc, .. } => {
                assert_eq!(
                    *is_doc, expect_doc,
                    "expected is_doc={expect_doc} for input {src:?}",
                );
            }
            other => panic!("expected LineComment for {src:?}, got {other:?}"),
        }
    }
}

#[test]
fn test_doc_block_comment_detection() {
    let cases = [
        ("/* regular */", false),
        ("/** doc */", true),
        ("/*** not a doc */", false),
        ("/**/", false), // empty block comment, not a doc
    ];
    for (src, expect_doc) in cases {
        let mut lex = Lexer::new(src);
        let tokens = lex.tokenize_with_comments().unwrap();
        match &tokens[0].kind {
            TokenKind::BlockComment { is_doc, .. } => {
                assert_eq!(
                    *is_doc, expect_doc,
                    "expected is_doc={expect_doc} for input {src:?}",
                );
            }
            other => panic!("expected BlockComment for {src:?}, got {other:?}"),
        }
    }
}

#[test]
fn test_newlines() {
    let mut lexer = Lexer::new("a\nb");
    let tokens = lexer.tokenize().unwrap();
    assert_eq!(tokens[0].kind, TokenKind::Identifier("a".into()));
    assert_eq!(tokens[1].kind, TokenKind::Newline);
    assert_eq!(tokens[2].kind, TokenKind::Identifier("b".into()));
}

#[test]
fn test_unexpected_character() {
    let mut lexer = Lexer::new("`");
    let err = lexer.tokenize().unwrap_err();
    assert!(matches!(err, LexerError::UnexpectedCharacter('`', _)));
}

#[test]
fn test_at_token() {
    let mut lexer = Lexer::new("@deprecated");
    let tokens = lexer.tokenize().unwrap();
    assert_eq!(tokens[0].kind, TokenKind::At);
    assert_eq!(tokens[1].kind, TokenKind::Identifier("deprecated".into()));
}

#[test]
fn test_unterminated_string() {
    let mut lexer = Lexer::new("\"unterminated");
    let err = lexer.tokenize().unwrap_err();
    assert!(matches!(err, LexerError::UnterminatedString(_)));
}

#[test]
fn test_escape_sequences() {
    let mut lexer = Lexer::new(r#""a\nb\t\\""#);
    let tokens = lexer.tokenize().unwrap();
    assert_eq!(tokens[0].kind, TokenKind::StringLiteral("a\nb\t\\".into()));
}

#[test]
fn test_escape_carriage_return_and_null() {
    let mut lexer = Lexer::new(r#""a\rb\0c""#);
    let tokens = lexer.tokenize().unwrap();
    assert_eq!(tokens[0].kind, TokenKind::StringLiteral("a\rb\0c".into()));
}

#[test]
fn test_number_then_dot_method() {
    let mut lexer = Lexer::new("42.method");
    let tokens = lexer.tokenize().unwrap();
    assert_eq!(tokens[0].kind, TokenKind::IntLiteral(42));
    assert_eq!(tokens[1].kind, TokenKind::Dot);
    assert_eq!(tokens[2].kind, TokenKind::Identifier("method".into()));
}

#[test]
fn test_hashed_raw_string_basic() {
    let mut lexer = Lexer::new("r#\"abc\"#");
    let tokens = lexer.tokenize().unwrap();
    assert_eq!(tokens[0].kind, TokenKind::RawStringLiteral("abc".into()));
}

#[test]
fn test_hashed_raw_string_embedded_quote() {
    // r#"a"b"# — the inner quote is not followed by `#`, so it's literal.
    let mut lexer = Lexer::new("r#\"a\"b\"#");
    let tokens = lexer.tokenize().unwrap();
    assert_eq!(tokens[0].kind, TokenKind::RawStringLiteral("a\"b".into()));
}

#[test]
fn test_hashed_raw_string_regex_with_quotes() {
    // The motivating case: a regex matching quoted strings, no escaping.
    let mut lexer = Lexer::new("r#\"\"([^\"\\]*)\"\"#");
    let tokens = lexer.tokenize().unwrap();
    assert_eq!(
        tokens[0].kind,
        TokenKind::RawStringLiteral("\"([^\"\\]*)\"".into())
    );
}

#[test]
fn test_double_hashed_raw_string_holds_quote_hash() {
    // r##"a"#b"## — the `"#` run is shorter than the 2-hash delimiter,
    // so it stays literal.
    let mut lexer = Lexer::new("r##\"a\"#b\"##");
    let tokens = lexer.tokenize().unwrap();
    assert_eq!(tokens[0].kind, TokenKind::RawStringLiteral("a\"#b".into()));
}

#[test]
fn test_plain_raw_string_still_works() {
    let mut lexer = Lexer::new("r\"plain\"");
    let tokens = lexer.tokenize().unwrap();
    assert_eq!(tokens[0].kind, TokenKind::RawStringLiteral("plain".into()));
}

#[test]
fn test_hashed_raw_string_unterminated_errors() {
    let mut lexer = Lexer::new("r#\"no close\"");
    assert!(lexer.tokenize().is_err());
}

#[test]
fn test_hashed_raw_string_newline_errors() {
    let mut lexer = Lexer::new("r#\"line1\nline2\"#");
    assert!(lexer.tokenize().is_err());
}
