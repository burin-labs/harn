//! Standardized attribute vocabulary for the `harn.*` observability
//! schema.
//!
//! Every harn-serve primitive (session store, permission DSL, MCP host,
//! compaction policy, Postgres hostlib, HTTP codec) emits spans, metrics,
//! and logs whose attribute keys are drawn from a single published
//! vocabulary, namespaced by primitive (`harn.session.*`,
//! `harn.permission.*`, `harn.mcp.*`, `harn.compaction.*`, `harn.pg.*`,
//! `harn.http.*`).
//!
//! Centralising the keys here keeps backends and downstream dashboards
//! stable: a typo in a primitive's emit site becomes a parse-time audit
//! failure (`harn-obs-audit`) instead of a silent attribute drift.
//!
//! ## Adding keys
//!
//! 1. Pick the namespace whose primitive owns the dimension.
//! 2. Add the bare key (`tool`, not `harn.mcp.tool`) to its `KEYS` slice.
//! 3. Use [`is_known_key`] from the conformance audit and primitive emit
//!    sites; they accept either the bare key or the fully-qualified form.

use std::collections::BTreeSet;
use std::sync::OnceLock;

/// Canonical attribute schema for a single namespace.
#[derive(Debug, Clone, Copy)]
pub struct VocabNamespace {
    /// Dotted prefix shared by every key in [`Self::keys`] — e.g.
    /// `"harn.mcp"` for `harn.mcp.tool` and `harn.mcp.server`.
    pub prefix: &'static str,
    /// Bare keys (without the prefix) declared for this namespace.
    pub keys: &'static [&'static str],
}

impl VocabNamespace {
    /// `true` when `key` is declared in this namespace. Accepts both
    /// bare (`"tool"`) and prefixed (`"harn.mcp.tool"`) forms so call
    /// sites can pick whichever reads better at their layer.
    pub fn contains(&self, key: &str) -> bool {
        let bare = key
            .strip_prefix(self.prefix)
            .map_or(key, |suffix| suffix.strip_prefix('.').unwrap_or(suffix));
        self.keys.contains(&bare)
    }
}

/// Session-store primitive (A.5, harn#2502). Spans cover `put`/`get`/
/// `list`/`delete`/`subscribe`; attributes capture session identity,
/// operation type, and outcome.
pub const SESSION: VocabNamespace = VocabNamespace {
    prefix: "harn.session",
    keys: &[
        "id",
        "op",
        "outcome",
        "schema",
        "rows",
        "bytes",
        "kind",
        "duration_ms",
    ],
};

/// Permission-policy primitive (A.6, harn#2503). Spans cover
/// `evaluate`/`approve`/`deny`/`escalate`; attributes carry the action
/// name, decision, and request lineage.
pub const PERMISSION: VocabNamespace = VocabNamespace {
    prefix: "harn.permission",
    keys: &[
        "action",
        "decision",
        "rule",
        "scope",
        "actor",
        "duration_ms",
        "outcome",
    ],
};

/// MCP host primitive (A.7, harn#2504). Spans cover spawn/tools/call/
/// stop/discover/reload/status; attributes capture the supervised
/// server, the tool, restart-budget telemetry, and call outcomes.
pub const MCP: VocabNamespace = VocabNamespace {
    prefix: "harn.mcp",
    keys: &[
        "server",
        "tool",
        "outcome",
        "restart_count",
        "cache_hit",
        "duration_ms",
        "scope",
    ],
};

/// Compaction-policy primitive (A.8, harn#2505). Spans cover
/// `policy`/`check`/`run`; attributes capture the strategy, input/output
/// token counts, and the trigger that kicked the run.
pub const COMPACTION: VocabNamespace = VocabNamespace {
    prefix: "harn.compaction",
    keys: &[
        "strategy",
        "input_tokens",
        "output_tokens",
        "trigger",
        "duration_ms",
        "outcome",
    ],
};

/// Postgres hostlib primitive (A.9, harn#2506). Spans cover
/// `query`/`exec`/`transaction`; attributes capture the named query, row
/// counts, and pool dwell.
pub const PG: VocabNamespace = VocabNamespace {
    prefix: "harn.pg",
    keys: &[
        "query_name",
        "rows",
        "duration_ms",
        "pool_size",
        "wait_ms",
        "outcome",
    ],
};

/// HTTP response codec primitive (A.4, harn#2501). Spans cover the
/// `.harn` handler dispatch; attributes capture status, body kind, and
/// SSE metadata.
pub const HTTP: VocabNamespace = VocabNamespace {
    prefix: "harn.http",
    keys: &[
        "status",
        "method",
        "path",
        "body_kind",
        "duration_ms",
        "outcome",
    ],
};

/// Burin compass tool-rewrite router (B.9, #2612). The `harn.compass.*`
/// counters (`suggested`/`rewritten`/`fell_back`) tag each routing
/// decision with the persona and the freeform/structural tool names so
/// eval dashboards can attribute the agent-loop edit-reliability win.
pub const COMPASS: VocabNamespace = VocabNamespace {
    prefix: "harn.compass",
    keys: &["persona", "tool", "target", "mode", "outcome"],
};

/// Cross-cutting attributes every primitive may emit alongside its
/// namespace-specific ones. Centralising them here keeps the conformance
/// audit's "is this key declared?" check single-pass.
pub const COMMON: VocabNamespace = VocabNamespace {
    prefix: "harn",
    keys: &[
        "tenant_id",
        "request_id",
        "trace_id",
        "span_id",
        "scope_set",
        "service",
    ],
};

/// Every published namespace, in registration order. The conformance
/// audit iterates this slice — adding a new namespace is just a one-line
/// edit here plus the `VocabNamespace` constant.
pub const ALL: &[VocabNamespace] = &[
    SESSION, PERMISSION, MCP, COMPACTION, PG, HTTP, COMPASS, COMMON,
];

/// Map a `harn.<ns>.<key>` (or `<ns>.<key>`) string to the namespace
/// that owns it. Returns `None` for any other prefix — call sites should
/// treat unknown namespaces as inert (e.g. host-tagged user attributes
/// outside the standard schema, which the audit gate ignores).
pub fn namespace_for(key: &str) -> Option<&'static VocabNamespace> {
    ALL.iter().find(|ns| {
        key.starts_with(ns.prefix) && key.as_bytes().get(ns.prefix.len()).copied() == Some(b'.')
    })
}

/// `true` when `key` is declared in any known namespace. Keys outside
/// the `harn.*` prefix never match — they're treated as user-emitted
/// tags and pass through unchanged.
pub fn is_known_key(key: &str) -> bool {
    namespace_for(key).is_some_and(|ns| ns.contains(key))
}

/// Strict subset of [`is_known_key`] that *also* rejects keys whose
/// prefix is `harn.<ns>.` for a known namespace but whose bare key is
/// not declared. The audit gate uses this; emission sites use
/// [`is_known_key`] (which is a tautology for user keys outside `harn.*`).
pub fn is_violation(key: &str) -> bool {
    namespace_for(key).is_some_and(|ns| !ns.contains(key))
}

/// Snapshot of every fully-qualified key declared across all
/// namespaces. Cached behind a `OnceLock` so the audit gate doesn't
/// rebuild it per call.
pub fn declared_keys() -> &'static BTreeSet<String> {
    static CACHE: OnceLock<BTreeSet<String>> = OnceLock::new();
    CACHE.get_or_init(|| {
        let mut set = BTreeSet::new();
        for ns in ALL {
            for key in ns.keys {
                set.insert(format!("{}.{}", ns.prefix, key));
            }
        }
        set
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_key_accepts_bare_and_qualified_forms() {
        assert!(SESSION.contains("id"));
        assert!(SESSION.contains("harn.session.id"));
        assert!(MCP.contains("tool"));
        assert!(MCP.contains("harn.mcp.tool"));
    }

    #[test]
    fn unknown_key_under_known_namespace_is_violation() {
        assert!(is_violation("harn.mcp.boops"));
        assert!(!is_violation("harn.mcp.tool"));
    }

    #[test]
    fn keys_outside_harn_prefix_are_not_violations() {
        // User-emitted attributes (e.g. business-domain tags) pass
        // through unchanged.
        assert!(!is_violation("user.id"));
        assert!(!is_violation("custom.key"));
        assert!(!is_known_key("custom.key"));
    }

    #[test]
    fn namespace_for_handles_overlapping_prefixes() {
        // The COMMON namespace has prefix `harn`; the specific ones
        // (`harn.session`, `harn.mcp`, ...) must win.
        assert_eq!(
            namespace_for("harn.session.id").map(|ns| ns.prefix),
            Some("harn.session")
        );
        assert_eq!(
            namespace_for("harn.tenant_id").map(|ns| ns.prefix),
            Some("harn")
        );
    }

    #[test]
    fn declared_keys_lists_each_namespace_entry() {
        let keys = declared_keys();
        assert!(keys.contains("harn.session.id"));
        assert!(keys.contains("harn.mcp.tool"));
        assert!(keys.contains("harn.pg.query_name"));
        assert!(keys.contains("harn.request_id"));
    }
}
