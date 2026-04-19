//! Project, metadata, checkpoint, and store builtin signatures.

use super::{BuiltinReturn, BuiltinSig};

pub(crate) const SIGNATURES: &[BuiltinSig] = &[
    BuiltinSig {
        name: "checkpoint",
        return_type: None,
    },
    BuiltinSig {
        name: "checkpoint_clear",
        return_type: None,
    },
    BuiltinSig {
        name: "checkpoint_delete",
        return_type: None,
    },
    BuiltinSig {
        name: "checkpoint_exists",
        return_type: None,
    },
    BuiltinSig {
        name: "checkpoint_get",
        return_type: None,
    },
    BuiltinSig {
        name: "checkpoint_list",
        return_type: None,
    },
    BuiltinSig {
        name: "compute_content_hash",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "invalidate_facts",
        return_type: Some(BuiltinReturn::Named("nil")),
    },
    BuiltinSig {
        name: "metadata_entries",
        return_type: Some(BuiltinReturn::Named("list")),
    },
    BuiltinSig {
        name: "metadata_get",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "metadata_refresh_hashes",
        return_type: Some(BuiltinReturn::Named("nil")),
    },
    BuiltinSig {
        name: "metadata_resolve",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "metadata_save",
        return_type: Some(BuiltinReturn::Named("nil")),
    },
    BuiltinSig {
        name: "metadata_set",
        return_type: Some(BuiltinReturn::Named("nil")),
    },
    BuiltinSig {
        name: "metadata_stale",
        return_type: None,
    },
    BuiltinSig {
        name: "metadata_status",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "project_catalog_native",
        return_type: Some(BuiltinReturn::Named("list")),
    },
    BuiltinSig {
        name: "project_enrich_native",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "project_fingerprint",
        return_type: None,
    },
    BuiltinSig {
        name: "project_root",
        return_type: Some(BuiltinReturn::Named("string")),
    },
    BuiltinSig {
        name: "project_scan_native",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "project_scan_tree_native",
        return_type: Some(BuiltinReturn::Named("dict")),
    },
    BuiltinSig {
        name: "project_walk_tree_native",
        return_type: Some(BuiltinReturn::Named("list")),
    },
    BuiltinSig {
        name: "scan_directory",
        return_type: None,
    },
    BuiltinSig {
        name: "store_clear",
        return_type: None,
    },
    BuiltinSig {
        name: "store_delete",
        return_type: None,
    },
    BuiltinSig {
        name: "store_get",
        return_type: None,
    },
    BuiltinSig {
        name: "store_list",
        return_type: None,
    },
    BuiltinSig {
        name: "store_save",
        return_type: None,
    },
    BuiltinSig {
        name: "store_set",
        return_type: None,
    },
];
