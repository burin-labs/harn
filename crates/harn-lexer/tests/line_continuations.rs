use harn_lexer::{Lexer, TokenKind};

#[test]
fn crlf_backslash_continuation_matches_lf_tokens_and_positions() {
    let lf = Lexer::new("10 \\\n- 3").tokenize().unwrap();
    let crlf = Lexer::new("10 \\\r\n- 3").tokenize().unwrap();
    assert_eq!(
        lf.iter().map(|token| &token.kind).collect::<Vec<_>>(),
        crlf.iter().map(|token| &token.kind).collect::<Vec<_>>()
    );
    assert_eq!(
        lf.iter()
            .map(|token| (token.span.line, token.span.column))
            .collect::<Vec<_>>(),
        crlf.iter()
            .map(|token| (token.span.line, token.span.column))
            .collect::<Vec<_>>()
    );
}

#[test]
fn crlf_formatter_wrapped_union_tokenizes() {
    let source = "type Choice = \"one\" \\\r\n  | \"two\" \\\r\n  | \"three\"";
    let tokens = Lexer::new(source).tokenize().unwrap();
    assert_eq!(
        tokens
            .iter()
            .filter(|token| token.kind == TokenKind::Bar)
            .count(),
        2
    );
}
