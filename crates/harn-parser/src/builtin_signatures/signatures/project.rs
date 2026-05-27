//! Project, metadata, checkpoint, and store builtin signatures.

use super::{
    BuiltinSignature, Param, Ty, TY_ANY, TY_DICT, TY_DICT_OR_NIL, TY_LIST, TY_NIL, TY_STRING,
    TY_STRING_OR_NIL,
};

pub(crate) const SIGNATURES: &[BuiltinSignature] = &[
    // checkpoint(key, value) — persist a checkpoint immediately. value may
    // be any VM value (serialized via JSON).
    BuiltinSignature::simple(
        "checkpoint",
        &[Param::new("key", TY_STRING), Param::new("value", TY_ANY)],
        TY_NIL,
    ),
    BuiltinSignature::simple("checkpoint_clear", &[], TY_NIL),
    BuiltinSignature::simple("checkpoint_delete", &[Param::new("key", TY_STRING)], TY_NIL),
    BuiltinSignature::simple(
        "checkpoint_exists",
        &[Param::new("key", TY_STRING)],
        Ty::Named("bool"),
    ),
    // checkpoint_get(key) -> stored value | nil. Stored values round-trip
    // through JSON, so the static type is dynamic.
    BuiltinSignature::simple("checkpoint_get", &[Param::new("key", TY_STRING)], TY_ANY),
    BuiltinSignature::simple("checkpoint_list", &[], TY_LIST),
    // compute_content_hash(dir) — fnv hash of file list + sizes + mtimes.
    BuiltinSignature::simple(
        "compute_content_hash",
        &[Param::new("dir", TY_STRING)],
        TY_STRING,
    ),
    // invalidate_facts(dir?) — metadata invalidation is namespace-driven.
    BuiltinSignature::variadic(
        "invalidate_facts",
        &[Param::optional("dir", TY_STRING)],
        TY_NIL,
    ),
    // metadata_entries(namespace?) -> list of {dir, local, resolved} dicts.
    BuiltinSignature::simple(
        "metadata_entries",
        &[Param::optional("namespace", TY_STRING_OR_NIL)],
        TY_LIST,
    ),
    // metadata_get(dir, namespace?) -> dict | nil
    BuiltinSignature::simple(
        "metadata_get",
        &[
            Param::new("dir", TY_STRING),
            Param::optional("namespace", TY_STRING_OR_NIL),
        ],
        TY_DICT_OR_NIL,
    ),
    BuiltinSignature::simple("metadata_refresh_hashes", &[], TY_NIL),
    // metadata_resolve(dir, namespace?) -> dict | nil
    BuiltinSignature::simple(
        "metadata_resolve",
        &[
            Param::new("dir", TY_STRING),
            Param::optional("namespace", TY_STRING_OR_NIL),
        ],
        TY_DICT_OR_NIL,
    ),
    BuiltinSignature::simple("metadata_save", &[], TY_NIL),
    // metadata_set(dir, namespace, data)
    BuiltinSignature::simple(
        "metadata_set",
        &[
            Param::new("dir", TY_STRING),
            Param::new("namespace", TY_STRING),
            Param::new("data", TY_DICT),
        ],
        TY_NIL,
    ),
    // metadata_stale(project?) -> {any_stale: bool, tier1: list, tier2: list}
    BuiltinSignature::simple(
        "metadata_stale",
        &[Param::optional("project", TY_STRING)],
        TY_DICT,
    ),
    // path_metadata_get(path, namespace?, opts?) -> dict | nil
    BuiltinSignature::simple(
        "path_metadata_get",
        &[
            Param::new("path", TY_STRING),
            Param::optional("namespace", TY_STRING_OR_NIL),
            Param::optional("opts", TY_DICT_OR_NIL),
        ],
        TY_DICT_OR_NIL,
    ),
    // path_metadata_set(path, namespace, data, opts?) -> nil
    BuiltinSignature::simple(
        "path_metadata_set",
        &[
            Param::new("path", TY_STRING),
            Param::new("namespace", TY_STRING),
            Param::new("data", TY_DICT),
            Param::optional("opts", TY_DICT_OR_NIL),
        ],
        TY_NIL,
    ),
    // path_metadata_entries(namespace?, opts?) -> list of {kind, path, local, resolved?}
    BuiltinSignature::simple(
        "path_metadata_entries",
        &[
            Param::optional("namespace", TY_STRING_OR_NIL),
            Param::optional("opts", TY_DICT_OR_NIL),
        ],
        TY_LIST,
    ),
    // metadata_status(namespace?) -> dict
    BuiltinSignature::simple(
        "metadata_status",
        &[Param::optional("namespace", TY_STRING_OR_NIL)],
        TY_DICT,
    ),
    // project_catalog_native() -> list of catalog dict entries.
    BuiltinSignature::simple("project_catalog_native", &[], TY_LIST),
    // project_enrich_native(path?, options?) -> dict (LLM-enriched evidence).
    BuiltinSignature::simple(
        "project_enrich_native",
        &[
            Param::optional("path", TY_STRING),
            Param::optional("options", TY_DICT_OR_NIL),
        ],
        TY_DICT,
    ),
    // project_fingerprint(path?) -> ProjectFingerprint-shaped dict.
    BuiltinSignature::simple(
        "project_fingerprint",
        &[Param::optional("path", TY_STRING)],
        TY_DICT,
    ),
    // project_scan_native(path?, options?) -> dict of evidence.
    BuiltinSignature::simple(
        "project_scan_native",
        &[
            Param::optional("path", TY_STRING),
            Param::optional("options", TY_DICT_OR_NIL),
        ],
        TY_DICT,
    ),
    // project_scan_tree_native(path?, options?) -> dict keyed by relative
    // directory.
    BuiltinSignature::simple(
        "project_scan_tree_native",
        &[
            Param::optional("path", TY_STRING),
            Param::optional("options", TY_DICT_OR_NIL),
        ],
        TY_DICT,
    ),
    // project_walk_tree_native(path?, options?) -> list of tree entry dicts.
    BuiltinSignature::simple(
        "project_walk_tree_native",
        &[
            Param::optional("path", TY_STRING),
            Param::optional("options", TY_DICT_OR_NIL),
        ],
        TY_LIST,
    ),
    // scan_directory(path?, pattern_or_options?, options?) -> list of
    // {path, size, modified, is_dir} dicts. The middle arg may be either a
    // glob pattern string or an options dict.
    BuiltinSignature::simple(
        "scan_directory",
        &[
            Param::optional("path", TY_STRING),
            Param::optional(
                "pattern_or_options",
                Ty::Union(&[TY_STRING, TY_DICT, TY_NIL]),
            ),
            Param::optional("options", TY_DICT_OR_NIL),
        ],
        TY_LIST,
    ),
    BuiltinSignature::simple("store_clear", &[], TY_NIL),
    BuiltinSignature::simple("store_delete", &[Param::new("key", TY_STRING)], TY_NIL),
    // store_get(key) -> stored value | nil. Values round-trip through JSON
    // so the static type is dynamic.
    BuiltinSignature::simple("store_get", &[Param::new("key", TY_STRING)], TY_ANY),
    BuiltinSignature::simple("store_list", &[], TY_LIST),
    BuiltinSignature::simple("store_save", &[], TY_NIL),
    BuiltinSignature::simple(
        "store_set",
        &[Param::new("key", TY_STRING), Param::new("value", TY_ANY)],
        TY_NIL,
    ),
];
