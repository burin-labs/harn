//! Capability-attenuation boundaries.

use super::*;

const ATTENUATION_RULE: &str = "capability-attenuation";

/// The falsifier: an ordinary helper with the same body must still be
/// reported. Without this the `@host_entry` assertion below would also pass
/// against a lint that had simply stopped firing.
#[test]
fn undeclared_helper_using_two_capabilities_is_reported() {
    let diagnostics = lint_source(
        "pub fn dispatch(harness: Harness, args: dict) -> dict {\n\
         \x20 const rows = harness.postgres.query(args.url, \"select 1\")\n\
         \x20 harness.net.post(args.sink, rows)\n\
         \x20 return {status: \"ok\"}\n\
         }\n",
    );
    assert!(
        has_rule(&diagnostics, ATTENUATION_RULE),
        "expected an attenuation diagnostic: {diagnostics:?}"
    );
}

#[test]
fn host_entry_suppresses_the_attenuation_diagnostic() {
    let diagnostics = lint_source(
        "@host_entry\n\
         pub fn dispatch(harness: Harness, args: dict) -> dict {\n\
         \x20 const rows = harness.postgres.query(args.url, \"select 1\")\n\
         \x20 harness.net.post(args.sink, rows)\n\
         \x20 return {status: \"ok\"}\n\
         }\n",
    );
    assert!(
        !has_rule(&diagnostics, ATTENUATION_RULE),
        "a host-entered signature is not ours to narrow: {diagnostics:?}"
    );
}

/// The attribute declares a boundary for the *attributed* function only. A
/// module-wide suppression would silently freeze every helper beside it.
#[test]
fn host_entry_does_not_suppress_a_sibling_helper() {
    let diagnostics = lint_source(
        "@host_entry\n\
         pub fn dispatch(harness: Harness, args: dict) -> dict {\n\
         \x20 const rows = harness.postgres.query(args.url, \"select 1\")\n\
         \x20 harness.net.post(args.sink, rows)\n\
         \x20 return {status: \"ok\"}\n\
         }\n\n\
         fn summarize(harness: Harness, url: string) -> dict {\n\
         \x20 return harness.postgres.query(url, \"select 2\")\n\
         }\n",
    );
    assert_eq!(
        count_rule(&diagnostics, ATTENUATION_RULE),
        1,
        "only the sibling helper should be reported: {diagnostics:?}"
    );
}
