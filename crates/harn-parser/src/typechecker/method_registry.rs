//! Canonical builtin-method name tables.
//!
//! These lists mirror the method-dispatch match arms in the VM
//! (`crates/harn-vm/src/vm/methods/*.rs`). They are the single source of
//! truth the typechecker uses to decide whether a `receiver.method()` call
//! names a method that actually exists on a *concrete* builtin receiver, so
//! that `"x".frobnicate()` / `(3.14).frobnicate()` fail `harn check` instead
//! of crashing (strings/lists/sets) or silently returning `nil` (numbers) at
//! runtime.
//!
//! Drift guard: a test in `harn-vm` (`method_registry_matches_vm`) asserts
//! every name here is accepted by the corresponding VM dispatch, so a stale
//! entry can never produce a false "unknown method". When you add a method to
//! a VM dispatch, add its name to the matching list below.

/// `string` receiver methods (`call_string_method`), plus the universal
/// `.iter()` bridge that `call_method_sync` handles for every iterable.
pub const STRING_METHODS: &[&str] = &[
    "count",
    "empty",
    "contains",
    "includes",
    "replace",
    "split",
    "trim",
    "starts_with",
    "ends_with",
    "lowercase",
    "uppercase",
    "substring",
    "slice",
    "index_of",
    "chars",
    "repeat",
    "reversed",
    "reverse",
    "pad_left",
    "pad_right",
    "trim_start",
    "trim_end",
    "lines",
    "char_at",
    "last_index_of",
    "rfind",
    "lower",
    "to_lower",
    "upper",
    "to_upper",
    "len",
    "iter",
];

/// `list` receiver methods (`call_list_method_sync` + `call_list_method`),
/// plus `.iter()`.
pub const LIST_METHODS: &[&str] = &[
    "count",
    "empty",
    "map",
    "filter",
    "find",
    "flat_map",
    "sorted_by",
    "sort_by",
    "partition",
    "group_by",
    "reduce",
    "any",
    "all",
    "every",
    "all?",
    "sorted",
    "sort",
    "reversed",
    "reverse",
    "join",
    "contains",
    "includes",
    "index_of",
    "enumerate",
    "zip",
    "slice",
    "unique",
    "take",
    "skip",
    "sum",
    "min",
    "max",
    "flatten",
    "appending",
    "push",
    "dropping_last",
    "pop",
    "none",
    "none?",
    "find_index",
    "first",
    "last",
    "chunk",
    "each_slice",
    "min_by",
    "max_by",
    "compact",
    "window",
    "each_cons",
    "sliding_window",
    "tally",
    "to_list",
    "to_set",
    "take_while",
    "drop_while",
    "count_by",
    "iter",
];

/// `set` receiver methods (`call_set_method_sync` + `call_set_method`), plus
/// `.iter()`.
pub const SET_METHODS: &[&str] = &[
    "count",
    "len",
    "empty",
    "contains",
    "includes",
    "adding",
    "add",
    "removing",
    "remove",
    "delete",
    "union",
    "intersect",
    "intersection",
    "difference",
    "symmetric_difference",
    "is_subset",
    "is_superset",
    "is_disjoint",
    "to_list",
    "to_set",
    "map",
    "filter",
    "any",
    "all",
    "every",
    "iter",
];

/// `dict` receiver methods (`call_dict_method_sync` + `call_dict_method`),
/// plus `.iter()`. Dicts are *not* directly existence-checked (a dict may
/// store a callable under a key and be invoked as `d.field()`), but their
/// method names still belong to the recognized-method universe.
pub const DICT_METHODS: &[&str] = &[
    "keys",
    "values",
    "entries",
    "count",
    "has",
    "merging",
    "merge",
    "map_values",
    "rekeyed",
    "rekey",
    "map_keys",
    "filter",
    "removing",
    "remove",
    "get",
    "to_dict",
    "to_list",
    "iter",
];

/// `range` receiver methods (`call_range_method_sync`), plus `.iter()`.
pub const RANGE_METHODS: &[&str] = &[
    "len",
    "count",
    "empty",
    "contains",
    "includes",
    "first",
    "last",
    "to_string",
    "iter",
];

/// `Iter<T>` combinators and sinks (`call_iter_method` + the lazy-iter arms
/// in `infer_type`). Part of the recognized-method universe.
pub const ITER_METHODS: &[&str] = &[
    "iter",
    "map",
    "flat_map",
    "filter",
    "take",
    "skip",
    "take_while",
    "skip_while",
    "zip",
    "enumerate",
    "chain",
    "chunks",
    "windows",
    "to_list",
    "to_set",
    "to_dict",
    "count",
    "sum",
    "min",
    "max",
    "first",
    "last",
    "find",
    "any",
    "all",
    "for_each",
    "reduce",
];

/// Generator / stream drive methods (`call_generator_method`).
pub const GENERATOR_METHODS: &[&str] = &["done", "next", "value"];

/// True when `method` names a method on *any* builtin receiver. Used for the
/// permissive tier (`int` / `float` / `bool`), whose VM dispatch either
/// returns `nil` for every name (numbers) or has no methods at all (bool):
/// there is no closed per-type set to check against, so we only reject names
/// that are unknown everywhere — enough to catch typos like `frobnicate`
/// without ever flagging a real method name.
pub(super) fn is_any_known_builtin_method(method: &str) -> bool {
    STRING_METHODS.contains(&method)
        || LIST_METHODS.contains(&method)
        || SET_METHODS.contains(&method)
        || DICT_METHODS.contains(&method)
        || RANGE_METHODS.contains(&method)
        || ITER_METHODS.contains(&method)
        || GENERATOR_METHODS.contains(&method)
}
