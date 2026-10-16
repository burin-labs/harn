use harn_parser::diagnostic_codes::Code;

fn sources_with_carriage_returns<'a>(
    sources: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> Vec<&'a str> {
    sources
        .into_iter()
        .filter_map(|(name, source)| source.contains('\r').then_some(name))
        .collect()
}

#[test]
fn embedded_diagnostic_explanations_use_lf_line_endings() {
    assert!(!Code::ALL.is_empty());
    let offenders = sources_with_carriage_returns(
        Code::ALL
            .iter()
            .map(|code| (code.as_str(), code.explanation())),
    );
    assert!(
        offenders.is_empty(),
        "embedded explanations must use LF: {offenders:?}"
    );
}

#[test]
fn embedded_source_line_ending_check_rejects_crlf() {
    assert_eq!(
        sources_with_carriage_returns([
            ("lf.md", "first\nsecond\n"),
            ("crlf.md", "first\r\nsecond\r\n"),
        ]),
        ["crlf.md"]
    );
}
