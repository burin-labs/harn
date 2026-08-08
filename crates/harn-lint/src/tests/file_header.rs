//! `require-file-header` opt-in rule plus the `derive_file_header_title`
//! helper used by its autofix.

use super::*;

#[test]
fn test_require_file_header_fires_when_missing() {
    let path = std::path::PathBuf::from("foo.harn");
    let source = "fn main() -> int {\n  return 0\n}\n";
    let diags = lint_with_require_header(source, Some(&path));
    let hit = diags
        .iter()
        .find(|d| d.rule == "require-file-header")
        .expect("expected require-file-header");
    let fix = hit.fix.as_ref().expect("autofix expected");
    assert_eq!(fix.len(), 1);
    assert!(
        fix[0].replacement.starts_with("/**\n * Foo."),
        "expected 'Foo.' title, got: {:?}",
        fix[0].replacement
    );
}

#[test]
fn test_require_file_header_ok_when_present() {
    let path = std::path::PathBuf::from("foo.harn");
    let source = "/**\n * Some header.\n */\nfn main() -> int {\n  return 0\n}\n";
    let diags = lint_with_require_header(source, Some(&path));
    assert!(
        !has_rule(&diags, "require-file-header"),
        "should not fire when header present, got: {diags:?}"
    );
}

#[test]
fn test_require_file_header_fires_when_only_line_comment() {
    let path = std::path::PathBuf::from("foo.harn");
    let source = "// not a header\nfn main() -> int {\n  return 0\n}\n";
    let diags = lint_with_require_header(source, Some(&path));
    assert!(
        has_rule(&diags, "require-file-header"),
        "// comment does not count as header, got: {diags:?}"
    );
}

#[test]
fn test_require_file_header_off_by_default() {
    // Without LintOptions { require_file_header: true }, the rule must
    // not fire — this protects the existing repo from sudden regressions.
    let diags = lint_source("fn main() -> int {\n  return 0\n}\n");
    assert!(
        !has_rule(&diags, "require-file-header"),
        "rule should be opt-in, got: {diags:?}"
    );
}

#[test]
fn test_derive_file_header_title_cases() {
    use std::path::PathBuf;
    let cases = [
        ("foo.harn", "Foo."),
        ("foo_bar.harn", "Foo bar."),
        ("foo-bar.harn", "Foo bar."),
        ("Foo.harn", "Foo."),
        ("data_pipeline.harn", "Data pipeline."),
        ("llm-cost.harn", "Llm cost."),
    ];
    for (name, expected) in cases {
        let path = PathBuf::from(name);
        let got = derive_file_header_title(Some(&path));
        assert_eq!(got, expected, "title for {name}");
    }
}

#[test]
fn test_derive_file_header_title_no_path_fallback() {
    let got = derive_file_header_title(None);
    assert_eq!(got, "Module.");
}
