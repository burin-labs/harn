use harn_lexer::{Lexer, TokenKind};

#[test]
fn backslash_continuation_accepts_lf_and_crlf() {
    for source in [concat!("10 \\", "\n- 3"), concat!("10 \\", "\r\n- 3")] {
        let kinds = Lexer::new(source)
            .tokenize()
            .unwrap()
            .into_iter()
            .map(|token| token.kind)
            .collect::<Vec<_>>();
        assert_eq!(
            kinds,
            vec![
                TokenKind::IntLiteral(10),
                TokenKind::Minus,
                TokenKind::IntLiteral(3),
                TokenKind::Eof,
            ]
        );
    }
}
