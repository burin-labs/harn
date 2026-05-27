//! Single source of truth for builtin function signatures used by the parser
//! and runtime VM: identifier resolution, typo suggestions, return-type
//! inference, static arity & per-arg type checks, runtime arity & type
//! enforcement, and lint awareness all consult the registry returned by
//! [`all_signatures`].
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
//! at driver startup via [`harn_builtin_registry::install_builtin_signatures`].
//!
//! During migration the legacy static `signatures::groups()` tables remain
//! as a fallback so unmigrated builtins still type-check. As modules port
//! to `#[harn_builtin]` their entries move out of the static tables into
//! the macro-emitted `MODULE_BUILTINS` slices. Once all signatures have
//! migrated the static tables are deleted.

mod lookup;
mod signatures;
mod types;

use std::collections::HashSet;
use std::sync::OnceLock;

pub use lookup::{
    builtin_return_type, is_builtin, is_untyped_boundary_source, iter_builtin_metadata,
    iter_builtin_names, lookup,
};
pub use types::{
    ty_to_type_expr, BuiltinMetadata, BuiltinSignature, BuiltinSignatureExt, Param,
    ShapeFieldDescriptor, Ty, TyExt, TY_ANY, TY_BOOL, TY_BYTES, TY_BYTES_OR_NIL, TY_CLOSURE,
    TY_DICT, TY_DICT_OR_NIL, TY_DURATION, TY_FLOAT, TY_INT, TY_INT_OR_NIL, TY_LIST, TY_NEVER,
    TY_NIL, TY_NUMBER, TY_STRING, TY_STRING_OR_NIL,
};

pub use harn_builtin_registry::install_builtin_signatures;

/// Every builtin known to the parser, sorted alphabetically by name.
///
/// During migration this concatenates:
///
/// 1. The runtime-installed slice (drivers populate via
///    [`install_builtin_signatures`] with the `#[harn_builtin]`-emitted defs).
/// 2. The legacy static `signatures::groups()` tables for builtins that
///    haven't yet ported to the proc-macro.
///
/// Duplicates by name are resolved in favour of the runtime-installed entry.
pub fn all_signatures() -> &'static [BuiltinSignature] {
    static ALL_SIGNATURES: OnceLock<Vec<BuiltinSignature>> = OnceLock::new();

    ALL_SIGNATURES
        .get_or_init(|| {
            let installed = harn_builtin_registry::installed_signatures();
            let installed_names: HashSet<&'static str> = installed.iter().map(|s| s.name).collect();

            let mut signatures: Vec<BuiltinSignature> = installed.iter().map(|sig| **sig).collect();

            for group in signatures::groups() {
                for sig in group {
                    if installed_names.contains(sig.name) {
                        continue;
                    }
                    signatures.push(*sig);
                }
            }
            signatures.sort_by_key(|sig| sig.name);
            signatures
        })
        .as_slice()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::TypeExpr;

    #[test]
    fn builtin_signatures_sorted() {
        let mut prev = "";
        for sig in all_signatures() {
            assert!(
                sig.name > prev,
                "BUILTIN_SIGNATURES not sorted: `{prev}` must come before `{}`",
                sig.name
            );
            prev = sig.name;
        }
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
