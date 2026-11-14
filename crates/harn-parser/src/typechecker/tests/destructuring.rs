use super::*;

#[rstest::rstest]
#[case::constant("const {x = 0} = p")]
#[case::mutable("let {x = 0} = p")]
#[case::typed_list("for p in points { const {x = 0} = p }")]
#[case::loop_pattern("for {x = 0} in points {}")]
fn dict_patterns_reject_struct_values(#[case] binding: &str) {
    let source = format!(
        "struct Point {{ x: int }}\nfn show(p: Point, points: list<Point>) {{\n{binding}\n}}"
    );
    let diagnostics = diagnostics_with_code(
        &source,
        Code::InvalidPatternBinding,
        DiagnosticSeverity::Error,
    );
    assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
    let diagnostic = &diagnostics[0];
    assert_eq!(diagnostic.span.unwrap().line, 3);
    assert!(
        diagnostic.message.contains("struct `Point`"),
        "{diagnostic:?}"
    );
    assert!(
        diagnostic.message.contains("field access"),
        "{diagnostic:?}"
    );
    if !binding.starts_with("for {x") {
        assert!(diagnostic.message.contains("`p.x`"), "{diagnostic:?}");
    } else {
        assert!(!diagnostic.message.contains("points.x"), "{diagnostic:?}");
    }
}

#[rstest::rstest]
#[case::alias("type Position = Point", "Position")]
#[case::generic("", "Box<int>")]
fn dict_patterns_resolve_struct_types(#[case] declaration: &str, #[case] ty: &str) {
    let source = format!(
        "struct Point {{ x: int }}\nstruct Box<T> {{ x: T }}\n{declaration}\nfn show(p: {ty}) {{ const {{x}} = p }}"
    );
    let diagnostics = diagnostics_with_code(
        &source,
        Code::InvalidPatternBinding,
        DiagnosticSeverity::Error,
    );
    assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
}

#[test]
fn dict_patterns_keep_dict_record_and_dynamic_sources() {
    let diagnostics = check_source(
        "type Point = {x: int}\nfn show(p: Point, d: dict, value: any) {\nconst {x} = p\nlet {x: other = 0} = d\nconst {x: dynamic} = value\n}",
    );
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
}
