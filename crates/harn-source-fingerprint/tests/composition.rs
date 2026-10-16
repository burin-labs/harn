use harn_source_fingerprint::{compose, SourceContribution};

const LEFT: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const RIGHT: &str = "fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210";
const fn part<'a>(name: &'a str, value: &'a str) -> SourceContribution<'a> {
    SourceContribution::crate_sources(name, value)
}
const COMPILED: &str = compose("compiler", &[part("parser", LEFT), part("kernel", RIGHT)]).as_str();

#[test]
fn compiled_contributions_have_named_ordered_identity() {
    assert_eq!(COMPILED.len(), 64);
    assert_ne!(COMPILED, "0".repeat(64));
    for changed in [
        compose("check", &[part("parser", LEFT), part("kernel", RIGHT)]),
        compose("compiler", &[part("kernel", RIGHT), part("parser", LEFT)]),
        compose("compiler", &[part("parser", RIGHT), part("kernel", RIGHT)]),
        compose("compiler", &[part("parser", LEFT), part("kernel", LEFT)]),
    ] {
        assert_ne!(COMPILED, changed.as_str());
    }
}

#[test]
fn incomplete_or_ambiguous_contributions_are_refused() {
    assert!(std::panic::catch_unwind(|| compose("compiler", &[])).is_err());
    assert!(std::panic::catch_unwind(|| compose("compiler", &[part("parser", "")])).is_err());
    assert!(std::panic::catch_unwind(|| compose(
        "compiler",
        &[part("parser", LEFT), part("parser", RIGHT)]
    ))
    .is_err());
}
