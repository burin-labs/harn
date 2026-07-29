use super::*;

#[test]
fn parameterized_test_and_fixture_attributes_are_structurally_validated() {
    let valid = warnings(
        r#"
@test_fixture(scope: file)
fn shared() -> int { return 1 }

@test(cases: [{name: "one", args: [1]}], fixture: shared)
pipeline test_value(_fixture: int, value: int) {}
"#,
    );
    assert!(
        valid.iter().all(|warning| {
            !warning.contains("unknown attribute")
                && !warning.contains("InvalidAttribute")
                && !warning.contains("only applies")
                && !warning.contains("@test")
        }),
        "valid fixture and row metadata should typecheck cleanly: {valid:?}"
    );

    let invalid = warnings(
        r"
@test_fixture(scope: suite, extra: true)
pipeline wrong_target() {}

@test(cases: 1, fixture: 42, typo: [])
fn also_wrong() -> int { return 1 }
",
    );
    assert!(
        invalid
            .iter()
            .any(|warning| warning.contains("only applies to function")),
        "fixture target should be structural: {invalid:?}"
    );
    assert!(
        invalid
            .iter()
            .any(|warning| warning.contains("only applies to pipeline")),
        "test target should be structural: {invalid:?}"
    );
    assert!(
        invalid
            .iter()
            .any(|warning| warning.contains("must be one of [\"file\", \"case\"]")),
        "fixture scope should be an enum-like contract: {invalid:?}"
    );
    assert!(
        invalid
            .iter()
            .any(|warning| warning.contains("unknown `@test` argument `typo`")),
        "test metadata should reject unknown fields: {invalid:?}"
    );
}
