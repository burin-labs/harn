//! Single source of truth for builtin function signatures used by the parser
//! and runtime VM: identifier resolution, typo suggestions, return-type
//! inference, static arity & per-arg type checks, runtime arity & type
//! enforcement, and lint awareness all consult the registry through the
//! [`lookup`] / [`is_builtin`] helpers.
//!
//! ## Architecture
//!
//! Historically every builtin lived in two places — its implementation +
//! runtime registration in `harn-vm/src/stdlib/*.rs`, and a hand-written
//! `BuiltinSignature` literal under `signatures/*.rs` here in the parser.
//! Drift between the two was caught at test time but cost a 2-file tax per
//! new builtin.
//!
//! That two-sided system has been replaced by the `#[harn_builtin]`
//! proc-macro (see `harn-builtin-macros`), which emits both the runtime
//! handler registration AND the parser `BuiltinSignature` from a single
//! annotated function. The vm crate aggregates them and installs them here
//! at driver startup via [`harn_builtin_registry::install_builtin_manifest`].
//!
//! During migration the legacy static `signatures::groups()` tables remain
//! as a fallback so unmigrated builtins still type-check. Lookups always
//! consult installed entries first and fall through to the static tables;
//! installed wins on name collisions. As modules port to `#[harn_builtin]`
//! their entries move out of the static tables into the macro-emitted
//! `MODULE_BUILTINS` slices. Once all signatures have migrated the static
//! tables are deleted.

mod lookup;
mod signatures;
mod types;

pub use lookup::{
    builtin_return_type, capability_method_entry, is_builtin, is_builtin_with_privileged_wire,
    is_untyped_boundary_capability_method, is_untyped_boundary_source, iter_builtin_metadata,
    iter_builtin_names, legacy_ambient_cap_global_entry, legacy_ambient_runtime_name,
    legacy_capability_method_entry, legacy_privileged_wire_entry, lookup, lookup_capability_method,
    lookup_with_privileged_wire, static_signature_names,
};
pub use types::{
    ty_to_type_expr, BuiltinMetadata, BuiltinSignature, BuiltinSignatureExt, Param,
    ShapeFieldDescriptor, Ty, TyExt, TY_ANY, TY_BOOL, TY_BYTES, TY_BYTES_OR_NIL, TY_CLOSURE,
    TY_DICT, TY_DICT_OR_NIL, TY_DURATION, TY_FLOAT, TY_INT, TY_INT_OR_NIL, TY_LIST, TY_NEVER,
    TY_NIL, TY_NUMBER, TY_STRING, TY_STRING_OR_NIL,
};

pub use harn_builtin_registry::{builtin_contract, install_builtin_manifest};

/// Compiler/VM opcodes that use ordinary call syntax but are implemented by
/// the language runtime rather than the stdlib builtin registry.
///
/// Keeping this set explicit lets source-callability checks distinguish
/// language intrinsics from legacy ambient builtins without an effect
/// allowlist.
pub const LANGUAGE_INTRINSICS: &[&str] = &[
    "Ok",
    "Err",
    "spawn",
    "await",
    "cancel",
    "cancel_graceful",
    "__signal_interrupted",
    "__signal_off_interrupt",
    "__signal_on_interrupt",
    "__signal_raise",
    "is_cancelled",
];

pub fn is_language_intrinsic(name: &str) -> bool {
    LANGUAGE_INTRINSICS.contains(&name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::TypeExpr;
    use std::collections::HashSet;

    #[test]
    fn iter_builtin_names_is_unique() {
        // Sanity-check that no name is exposed twice across the
        // installed slice + static groups. Installed entries take
        // priority on collisions inside `lookup`, but the
        // `iter_builtin_names` helper already filters static names that
        // are shadowed, so the output should be deduped.
        let mut seen = HashSet::new();
        for name in iter_builtin_names() {
            assert!(seen.insert(name), "duplicate builtin name in iter: {name}");
        }
    }

    #[test]
    fn legacy_net_signatures_share_the_capability_owner() {
        let _guard = crate::legacy_ambient_capabilities_test_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let migrations = crate::diagnostic::HARNESS_NET_REPLACEMENTS;
        assert_eq!(
            migrations.len(),
            50,
            "the full migration census must be reached"
        );
        let names: HashSet<_> = iter_builtin_names().collect();
        let static_names: HashSet<_> = static_signature_names().collect();
        let metadata: HashSet<_> = iter_builtin_metadata().map(|entry| entry.name).collect();
        for &(legacy, replacement) in migrations {
            let method = replacement.strip_prefix("harness.net.").unwrap();
            let owner = capability_method_entry("net", method).expect("typed migration owner");
            assert!(
                std::ptr::eq(lookup(legacy).unwrap(), owner.signature),
                "{legacy}"
            );
            assert_eq!(
                crate::diagnostic::harness_net_replacement(legacy),
                Some(replacement)
            );
            assert!(
                names.contains(legacy) && metadata.contains(legacy),
                "{legacy}"
            );
            assert!(
                !static_names.contains(legacy),
                "duplicate static signature: {legacy}"
            );
        }
        assert!(lookup("http_definitely_not_registered").is_none());
        assert!(
            lookup("__cap_net_get").is_none(),
            "a migration is not ambient authority"
        );
        let (_, diagnostics) = crate::check_source("fn wrong() { http_get(7) }").unwrap();
        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains("expected string, found int")),
            "{diagnostics:?}"
        );
        let (_, diagnostics) = crate::check_source(
            "fn http_get(n: int) -> int { return n }\nfn local() -> int { return http_get(7) }",
        )
        .unwrap();
        assert!(
            diagnostics.is_empty(),
            "local functions must shadow migration names: {diagnostics:?}"
        );
    }

    #[test]
    fn lookup_hits_and_misses() {
        assert!(is_builtin("snake_to_camel"));
        assert!(is_builtin("log"));
        assert!(is_builtin("await"));
        assert!(!is_builtin("definitely_not_a_builtin"));
        assert!(!is_builtin(""));
    }

    #[test]
    fn every_language_intrinsic_has_a_static_type_contract() {
        for name in LANGUAGE_INTRINSICS {
            assert!(
                lookup(name).is_some(),
                "missing signature for intrinsic `{name}`"
            );
        }
    }

    #[test]
    fn return_type_named_variant() {
        assert_eq!(
            builtin_return_type("snake_to_camel"),
            Some(TypeExpr::Named("string".into()))
        );
        assert_eq!(
            builtin_return_type("log"),
            Some(TypeExpr::Named("nil".into()))
        );
        assert_eq!(
            builtin_return_type("file_exists"),
            Some(TypeExpr::Named("bool".into()))
        );
    }

    #[test]
    fn return_type_union_variant() {
        assert_eq!(
            builtin_return_type("env"),
            Some(TypeExpr::Union(vec![
                TypeExpr::Named("string".into()),
                TypeExpr::Named("nil".into()),
            ]))
        );
    }

    #[test]
    fn return_type_unknown_for_dynamic_builtins() {
        assert!(is_builtin("json_parse"));
        assert_eq!(builtin_return_type("json_parse"), None);
    }

    #[test]
    fn return_type_none_for_unknown_names() {
        assert_eq!(builtin_return_type("not_a_real_thing"), None);
    }
}

#[cfg(test)]
mod ambient_prefixed_lookup_tests {
    use super::*;

    #[test]
    fn ambient_lookup_resolves_declared_capability_global_names() {
        let _guard = crate::legacy_ambient_capabilities_test_lock()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Ensure contracts are visible the same way the CLI installs them.
        let _ = harn_capability_contracts::manifest();
        std::env::set_var("HARN_LEGACY_AMBIENT_CAPABILITIES", "1");
        crate::refresh_legacy_ambient_capabilities();
        assert!(
            lookup("runtime_context_set").is_some(),
            "ambient must resolve declared global name runtime_context_set"
        );
        assert!(
            lookup("context_set").is_some(),
            "ambient must still resolve unique short method context_set"
        );
        std::env::remove_var("HARN_LEGACY_AMBIENT_CAPABILITIES");
        crate::refresh_legacy_ambient_capabilities();
        assert!(lookup("runtime_context_set").is_none());
    }
}
