//! Closed builtin vocabulary implemented by the portable kernel.
//!
//! Source-visible signatures live in `harn-builtin-meta`; the parser fallback,
//! native handlers, compiler contract registry, and this dispatch table all
//! project those same const values. Internal bytecode helpers remain private.

use std::sync::OnceLock;

use harn_builtin_meta::BuiltinSignature;
use harn_builtin_registry::BuiltinManifestEntry;

macro_rules! define_portable_builtins {
    (
        source { $($source_variant:ident => $signature:path),+ $(,)? }
        internal { $($internal_variant:ident => $name:literal),+ $(,)? }
    ) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub(crate) enum PortableBuiltin {
            $($source_variant,)+
            $($internal_variant,)+
        }

        impl PortableBuiltin {
            pub(crate) const ALL: &'static [Self] = &[
                $(Self::$source_variant,)+
                $(Self::$internal_variant,)+
            ];

            pub(crate) fn from_name(name: &str) -> Option<Self> {
                Self::ALL
                    .iter()
                    .copied()
                    .find(|builtin| builtin.name() == name)
            }

            pub(crate) const fn name(self) -> &'static str {
                match self {
                    $(Self::$source_variant => $signature.name,)+
                    $(Self::$internal_variant => $name,)+
                }
            }

            const fn source_signature(self) -> Option<&'static BuiltinSignature> {
                match self {
                    $(Self::$source_variant => Some(&$signature),)+
                    $(Self::$internal_variant => None,)+
                }
            }
        }
    };
}

define_portable_builtins! {
    source {
        Len => harn_builtin_meta::signatures::PORTABLE_LEN,
        ToString => harn_builtin_meta::signatures::PORTABLE_TO_STRING,
        HexEncode => harn_builtin_meta::signatures::PORTABLE_HEX_ENCODE,
        HexDecode => harn_builtin_meta::signatures::PORTABLE_HEX_DECODE,
        Trim => harn_builtin_meta::signatures::PORTABLE_TRIM,
        Replace => harn_builtin_meta::signatures::PORTABLE_REPLACE,
        StartsWith => harn_builtin_meta::signatures::PORTABLE_STARTS_WITH,
        JsonStringify => harn_builtin_meta::signatures::PORTABLE_JSON_STRINGIFY,
        RegexMatch => harn_builtin_meta::signatures::PORTABLE_REGEX_MATCH,
        RegexReplace => harn_builtin_meta::signatures::PORTABLE_REGEX_REPLACE,
        RegexCaptures => harn_builtin_meta::signatures::PORTABLE_REGEX_CAPTURES,
        RegexSplit => harn_builtin_meta::signatures::PORTABLE_REGEX_SPLIT,
        Sha256 => harn_builtin_meta::signatures::PORTABLE_SHA256,
        SecretScan => harn_builtin_meta::signatures::PORTABLE_SECRET_SCAN,
        PathJoin => harn_builtin_meta::signatures::PORTABLE_PATH_JOIN,
    }
    internal {
        Count => "count",
        String => "string",
        MakeStruct => "__make_struct",
        AssertList => "__assert_list",
        AssertSchema => "__assert_schema",
        DictFilterNil => "__dict_filter_nil",
    }
}

/// Install the portable source contract projection before parsing or lowering.
/// A native VM may later contribute the same contracts plus its hostful
/// surface; structurally identical projections are idempotent.
pub(crate) fn install_source_contracts() {
    static MANIFEST: OnceLock<Vec<&'static BuiltinManifestEntry>> = OnceLock::new();
    let manifest = MANIFEST.get_or_init(|| {
        PortableBuiltin::ALL
            .iter()
            .filter_map(|builtin| {
                let signature = builtin.source_signature()?;
                Some(Box::leak(Box::new(BuiltinManifestEntry {
                    name: signature.name,
                    canonical_name: signature.name,
                    signature,
                    contract: harn_builtin_meta::BuiltinContract::PURE,
                })) as &'static BuiltinManifestEntry)
            })
            .collect()
    });
    harn_builtin_registry::install_builtin_manifest(manifest);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_contracts_and_execution_dispatch_share_one_vocabulary() {
        install_source_contracts();
        for signature in harn_builtin_meta::signatures::PORTABLE_SOURCE_BUILTINS {
            let builtin = PortableBuiltin::from_name(signature.name)
                .unwrap_or_else(|| panic!("{} has no portable dispatch", signature.name));
            assert_eq!(builtin.source_signature(), Some(signature));
            let entry = harn_builtin_registry::builtin_entry(signature.name)
                .unwrap_or_else(|| panic!("{} has no compiler contract", signature.name));
            assert_eq!(entry.signature, signature);
            assert_eq!(entry.contract, harn_builtin_meta::BuiltinContract::PURE);
        }
    }
}
