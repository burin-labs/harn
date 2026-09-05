//! The `for` header's boundary with the body block.

use super::assert_roundtrip;
use crate::format_source;
/// A `for` header keeps its boundary with the body block visible.
///
/// An iterable that ends in `}` prints as `} {` without grouping, where the
/// brace closing the iterable and the brace opening the body are adjacent and
/// nothing in the text separates them. An iterable that ends in any other
/// token is unambiguous already and must not gain noise.
#[test]
fn for_header_groups_an_iterable_that_ends_in_a_brace() {
    let brace_tailed =
        "pipeline default(task) {\n  for entry in headers ?? {} {\n    use(entry)\n  }\n}";
    let result = format_source(brace_tailed).unwrap();
    assert!(
        result.contains("for entry in (headers ?? {}) {"),
        "brace-tailed iterable did not keep its boundary:\n{result}"
    );
    assert!(
        !result.contains("} {\n"),
        "formatted header still abuts two braces:\n{result}"
    );
    assert_roundtrip(brace_tailed);

    let bracket_tailed =
        "pipeline default(task) {\n  for span in spans ?? [] {\n    use(span)\n  }\n}";
    let result = format_source(bracket_tailed).unwrap();
    assert!(
        result.contains("for span in spans ?? [] {"),
        "unambiguous iterable gained grouping it does not need:\n{result}"
    );
    assert_roundtrip(bracket_tailed);
}
