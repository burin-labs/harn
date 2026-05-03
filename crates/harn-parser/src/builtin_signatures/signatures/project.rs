//! Project, metadata, checkpoint, and store builtin signatures.

use super::{
    BuiltinSignature, Param, Ty, TY_ANY, TY_DICT, TY_DICT_OR_NIL, TY_LIST, TY_NIL, TY_STRING,
    TY_STRING_OR_NIL,
};

pub(crate) const SIGNATURES: &[BuiltinSignature] = &[
    // checkpoint(key, value) — persist a checkpoint immediately. value may
    // be any VM value (serialized via JSON).
    BuiltinSignature {
        name: "checkpoint",
        params: &[Param::new("key", TY_STRING), Param::new("value", TY_ANY)],
        returns: TY_NIL,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    BuiltinSignature {
        name: "checkpoint_clear",
        params: &[],
        returns: TY_NIL,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    BuiltinSignature {
        name: "checkpoint_delete",
        params: &[Param::new("key", TY_STRING)],
        returns: TY_NIL,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    BuiltinSignature {
        name: "checkpoint_exists",
        params: &[Param::new("key", TY_STRING)],
        returns: Ty::Named("bool"),
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    // checkpoint_get(key) -> stored value | nil. Stored values round-trip
    // through JSON, so the static type is dynamic.
    BuiltinSignature {
        name: "checkpoint_get",
        params: &[Param::new("key", TY_STRING)],
        returns: TY_ANY,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    BuiltinSignature {
        name: "checkpoint_list",
        params: &[],
        returns: TY_LIST,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    // compute_content_hash(dir) — fnv hash of file list + sizes + mtimes.
    BuiltinSignature {
        name: "compute_content_hash",
        params: &[Param::new("dir", TY_STRING)],
        returns: TY_STRING,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    // invalidate_facts(dir?) — metadata invalidation is namespace-driven.
    BuiltinSignature {
        name: "invalidate_facts",
        params: &[Param::optional("dir", TY_STRING)],
        returns: TY_NIL,
        type_params: &[],
        has_rest: true,
        where_clauses: &[],
    },
    // metadata_entries(namespace?) -> list of {dir, local, resolved} dicts.
    BuiltinSignature {
        name: "metadata_entries",
        params: &[Param::optional("namespace", TY_STRING_OR_NIL)],
        returns: TY_LIST,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    // metadata_get(dir, namespace?) -> dict | nil
    BuiltinSignature {
        name: "metadata_get",
        params: &[
            Param::new("dir", TY_STRING),
            Param::optional("namespace", TY_STRING_OR_NIL),
        ],
        returns: TY_DICT_OR_NIL,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    BuiltinSignature {
        name: "metadata_refresh_hashes",
        params: &[],
        returns: TY_NIL,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    // metadata_resolve(dir, namespace?) -> dict | nil
    BuiltinSignature {
        name: "metadata_resolve",
        params: &[
            Param::new("dir", TY_STRING),
            Param::optional("namespace", TY_STRING_OR_NIL),
        ],
        returns: TY_DICT_OR_NIL,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    BuiltinSignature {
        name: "metadata_save",
        params: &[],
        returns: TY_NIL,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    // metadata_set(dir, namespace, data)
    BuiltinSignature {
        name: "metadata_set",
        params: &[
            Param::new("dir", TY_STRING),
            Param::new("namespace", TY_STRING),
            Param::new("data", TY_DICT),
        ],
        returns: TY_NIL,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    // metadata_stale(project?) -> {any_stale: bool, tier1: list, tier2: list}
    BuiltinSignature {
        name: "metadata_stale",
        params: &[Param::optional("project", TY_STRING)],
        returns: TY_DICT,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    // metadata_status(namespace?) -> dict
    BuiltinSignature {
        name: "metadata_status",
        params: &[Param::optional("namespace", TY_STRING_OR_NIL)],
        returns: TY_DICT,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    // project_catalog_native() -> list of catalog dict entries.
    BuiltinSignature {
        name: "project_catalog_native",
        params: &[],
        returns: TY_LIST,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    // project_enrich_native(path?, options?) -> dict (LLM-enriched evidence).
    BuiltinSignature {
        name: "project_enrich_native",
        params: &[
            Param::optional("path", TY_STRING),
            Param::optional("options", TY_DICT_OR_NIL),
        ],
        returns: TY_DICT,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    // project_fingerprint(path?) -> ProjectFingerprint-shaped dict.
    BuiltinSignature {
        name: "project_fingerprint",
        params: &[Param::optional("path", TY_STRING)],
        returns: TY_DICT,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    // project_root() -> string | nil. Resolves the repo root for the
    // currently-executing source file.
    BuiltinSignature {
        name: "project_root",
        params: &[],
        returns: TY_STRING_OR_NIL,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    // project_scan_native(path?, options?) -> dict of evidence.
    BuiltinSignature {
        name: "project_scan_native",
        params: &[
            Param::optional("path", TY_STRING),
            Param::optional("options", TY_DICT_OR_NIL),
        ],
        returns: TY_DICT,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    // project_scan_tree_native(path?, options?) -> dict keyed by relative
    // directory.
    BuiltinSignature {
        name: "project_scan_tree_native",
        params: &[
            Param::optional("path", TY_STRING),
            Param::optional("options", TY_DICT_OR_NIL),
        ],
        returns: TY_DICT,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    // project_walk_tree_native(path?, options?) -> list of tree entry dicts.
    BuiltinSignature {
        name: "project_walk_tree_native",
        params: &[
            Param::optional("path", TY_STRING),
            Param::optional("options", TY_DICT_OR_NIL),
        ],
        returns: TY_LIST,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    // scan_directory(path?, pattern_or_options?, options?) -> list of
    // {path, size, modified, is_dir} dicts. The middle arg may be either a
    // glob pattern string or an options dict.
    BuiltinSignature {
        name: "scan_directory",
        params: &[
            Param::optional("path", TY_STRING),
            Param::optional(
                "pattern_or_options",
                Ty::Union(&[TY_STRING, TY_DICT, TY_NIL]),
            ),
            Param::optional("options", TY_DICT_OR_NIL),
        ],
        returns: TY_LIST,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    BuiltinSignature {
        name: "store_clear",
        params: &[],
        returns: TY_NIL,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    BuiltinSignature {
        name: "store_delete",
        params: &[Param::new("key", TY_STRING)],
        returns: TY_NIL,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    // store_get(key) -> stored value | nil. Values round-trip through JSON
    // so the static type is dynamic.
    BuiltinSignature {
        name: "store_get",
        params: &[Param::new("key", TY_STRING)],
        returns: TY_ANY,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    BuiltinSignature {
        name: "store_list",
        params: &[],
        returns: TY_LIST,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    BuiltinSignature {
        name: "store_save",
        params: &[],
        returns: TY_NIL,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
    BuiltinSignature {
        name: "store_set",
        params: &[Param::new("key", TY_STRING), Param::new("value", TY_ANY)],
        returns: TY_NIL,
        type_params: &[],
        has_rest: false,
        where_clauses: &[],
    },
];
