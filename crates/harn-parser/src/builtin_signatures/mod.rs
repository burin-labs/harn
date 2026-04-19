//! Single source of truth for builtin function signatures used by the parser
//! and type checker: identifier resolution, typo suggestions, and return-type
//! inference all consult the registry returned by [`all_signatures`].
//!
//! To add a builtin: register it in the VM stdlib, then add it to the
//! appropriate namespace file under `builtin_signatures/signatures/`. The
//! registry is concatenated and sorted centrally so the parser lookup table and
//! the runtime alignment test stay in lockstep.

mod generics;
mod lookup;
mod signatures;
mod types;

use std::sync::OnceLock;

pub use types::BuiltinMetadata;
pub(crate) use types::{
    BuiltinGenericSig, BuiltinReturn, BuiltinSig, EMPTY_RETURN_TYPES, RETURN_BOOL, RETURN_DICT,
    RETURN_FLOAT, RETURN_INT, RETURN_LIST, RETURN_NEVER, RETURN_NIL, RETURN_STRING, UNION_DICT_NIL,
    UNION_STRING_NIL,
};

pub(crate) use lookup::{
    builtin_return_type, is_builtin, is_untyped_boundary_source, iter_builtin_metadata,
    iter_builtin_names, lookup_generic_builtin_sig,
};

/// Every builtin known to the parser, sorted alphabetically by name.
pub(crate) fn all_signatures() -> &'static [BuiltinSig] {
    static ALL_SIGNATURES: OnceLock<Vec<BuiltinSig>> = OnceLock::new();

    ALL_SIGNATURES
        .get_or_init(|| {
            let groups = signatures::groups();
            let mut signatures =
                Vec::with_capacity(groups.iter().map(|group| group.len()).sum::<usize>());
            for group in groups {
                signatures.extend_from_slice(group);
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
            builtin_return_type("pi"),
            Some(TypeExpr::Named("float".into()))
        );
        assert_eq!(
            builtin_return_type("sign"),
            Some(TypeExpr::Named("int".into()))
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
        assert_eq!(
            builtin_return_type("transcript_summary"),
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
        assert!(is_builtin("schema_parse"));
        assert_eq!(builtin_return_type("schema_parse"), None);
    }

    #[test]
    fn return_type_none_for_unknown_names() {
        assert_eq!(builtin_return_type("not_a_real_thing"), None);
    }
}
