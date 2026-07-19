//! Canonical runtime type tags — the single source of truth for the strings
//! `type_of(x)` (i.e. `VmValue::type_name`) can produce.
//!
//! Three consumers must agree on this vocabulary:
//!
//! * `harn-vm`'s `VmValue::type_name` produces the tags at runtime (its
//!   sibling `VmValue::ALL_TYPE_NAMES` const is asserted equal to [`ALL`]
//!   by a unit test in `harn-vm`).
//! * The typechecker's `type_of` narrowing (`is_type_of_tag` in
//!   `harn-parser`) accepts exactly the [`NARROWABLE`] subset.
//! * The `unknown` exhaustive-narrowing ledger counts the
//!   [`UNKNOWN_COVERAGE`] subset.
//!
//! Historically these were three hand-maintained lists (12, 9, and the VM's
//! ~27 tags) that drifted: `type_of(x) == "duration"` silently failed to
//! narrow because `"duration"` never made it into the typechecker's copy.

/// Every tag `VmValue::type_name` can return, except harness-object names
/// (`Harness`, `HarnessFs`, …), which are open-ended and produced by
/// `HarnessValue::type_name`.
pub const ALL: &[&str] = &[
    "string",
    "bytes",
    "int",
    "float",
    "decimal",
    "bool",
    "nil",
    "list",
    "dict",
    "closure",
    "builtin",
    "duration",
    "enum",
    "struct",
    "task_handle",
    "channel",
    "atomic",
    "rng",
    "sync_permit",
    "resource_guard",
    "mcp_client",
    "verdict_receipt",
    "set",
    "generator",
    "stream",
    "range",
    "iter",
    "pair",
];

/// Tags that denote a concrete static type the checker can narrow a value
/// to. Excludes the *nominal-kind* tags (`enum`, `struct`, `builtin`): a
/// runtime "this is some enum/struct" answer carries no information about
/// *which* declared type it is, so narrowing to a phantom `Named("enum")`
/// would only manufacture spurious downstream mismatches.
pub const NARROWABLE: &[&str] = &[
    "string",
    "bytes",
    "int",
    "float",
    "decimal",
    "bool",
    "nil",
    "list",
    "dict",
    "closure",
    "duration",
    "task_handle",
    "channel",
    "atomic",
    "rng",
    "sync_permit",
    "resource_guard",
    "mcp_client",
    "verdict_receipt",
    "set",
    "generator",
    "stream",
    "range",
    "iter",
    "pair",
];

/// The tags the `unknown` exhaustive-narrowing ledger requires coverage of
/// before an `unreachable()` / `throw` sink is considered fully narrowed.
/// Deliberately the JSON-representable kinds plus `closure`/`bytes`:
/// `unknown` values come from boundary APIs (`json_parse`, `llm_call`),
/// which can only produce these — demanding coverage of `rng` or
/// `sync_permit` would make the exhaustiveness warning unusable.
pub const UNKNOWN_COVERAGE: &[&str] = &[
    "int", "string", "float", "bool", "nil", "list", "dict", "closure", "bytes",
];

/// Whether `tag` is a runtime type tag the typechecker's `type_of`
/// narrowing recognises.
pub fn is_narrowable_tag(tag: &str) -> bool {
    NARROWABLE.contains(&tag)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn narrowable_and_coverage_are_subsets_of_all() {
        for tag in NARROWABLE {
            assert!(ALL.contains(tag), "NARROWABLE tag `{tag}` missing from ALL");
        }
        for tag in UNKNOWN_COVERAGE {
            assert!(
                NARROWABLE.contains(tag),
                "UNKNOWN_COVERAGE tag `{tag}` missing from NARROWABLE"
            );
        }
    }
}
