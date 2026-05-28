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
//! at driver startup via [`harn_builtin_registry::install_builtin_signatures`].
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
