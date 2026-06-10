//! Server-lifetime shared Postgres pool registry.
//!
//! # Why this exists
//!
//! Under `harn serve`, a fresh [`Vm`](crate::vm::Vm) is constructed per request,
//! and dispatch runs on a multi-thread tokio runtime. The per-VM Postgres state
//! ([`super::POOLS`] etc.) is **thread-local**, which is exactly right for the
//! CLI one-shot model (one VM, one thread, deterministic teardown) but means a
//! server re-opens a brand-new connection pool on every single request — the
//! pool can never be reused across requests because the registry holding it
//! lives on a thread the next request may not even run on.
//!
//! This module provides an **opt-in, process-lifetime** registry that an
//! embedder (e.g. the harn-serve `SiteServer` wiring) installs **once** at
//! startup via [`install_shared_pool_registry`]. When installed,
//! [`super::open_pool`] consults it: repeated `pg_pool(same source, same
//! options)` calls — across requests and across worker threads — return the
//! **same** underlying [`PoolRecord`] (a cheap `Arc` clone of the sqlx `Pool`).
//! When **not** installed (the default, and the entire CLI surface), behavior is
//! byte-identical to before: every `pg_pool` builds its own pool in the
//! thread-local registry.
//!
//! # Security: the pool key is the full connection identity
//!
//! Two `pg_pool` calls share a pool **only** when their entire connection
//! identity matches: the resolved connection string (host, port, database,
//! user, **password**, and every query parameter), the SSL mode, the
//! application name, the replica set, and every pool-shaping option
//! (sizing/timeouts/statement-cache/routing/circuit-breaker). The key is the
//! SHA-256 of a canonical serialization of all of these (see [`PoolKey::new`]).
//! Crucially the key is built from the **resolved** URL — after `env:`/`secret:`
//! indirection — never from a user-supplied alias, so two callers cannot
//! collide on a pool by naming the same alias while resolving to different
//! credentials.
//!
//! This is safe to share across tenants because harn-cloud scopes RLS
//! **per-transaction** via `set_config('app.current_tenant_id', …, /*local*/
//! true)` (see `ALLOWED_TRANSACTION_SETTINGS` in the parent module) — tenancy is
//! *never* encoded in the pool/connection itself. A shared pool therefore only
//! ever multiplexes transactions that each set their own tenant scope; it can
//! never leak one tenant's rows to another. If a future caller ever moved
//! tenant scoping onto the connection (e.g. a per-tenant `SET ROLE` at connect
//! time), that tenant discriminator would have to become part of the pool key —
//! today it is not, because it is not on the connection.
//!
//! # Lock discipline (no deadlock across await)
//!
//! The registry is a `std::sync::Mutex<HashMap<PoolKey, Arc<PoolRecord>>>`. We
//! **never** hold the mutex across an `.await`. Opening a pool is a double-
//! checked init: (1) lock, look up the key, clone-and-return on hit, else drop
//! the lock; (2) build the pool *outside* any lock (the only `.await`); (3)
//! re-lock and insert, but if a racing request inserted the same key while we
//! were connecting, we keep the existing entry and drop ours (its pool closes on
//! `Drop`). The guard returned from step 1/3 is a plain `MutexGuard` that is
//! dropped before any await — enforced by structure, not convention.

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use sha2::{Digest, Sha256};

use crate::value::VmValue;

use super::PoolRecord;

/// Opaque, collision-resistant key for a fully-resolved connection identity.
///
/// Wraps the hex SHA-256 of a canonical description of everything that makes
/// two pools interchangeable. Two `PoolKey`s are equal iff every identity- and
/// shape-affecting input matched. We hash (rather than store the raw string)
/// so the in-memory key never retains the plaintext password from the resolved
/// URL.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub(super) struct PoolKey(String);

impl PoolKey {
    /// Build a key from the resolved primary URL, resolved replica URLs (order
    /// preserved — it affects round-robin identity), and the raw options dict
    /// the caller passed to `pg_pool`/`pg_connect`.
    ///
    /// `single_connection` distinguishes `pg_connect` (max 1) from `pg_pool`
    /// even when the options dict is identical, so the two never alias.
    ///
    /// The options dict is folded into the key in a canonical, order-independent
    /// way (sorted keys; values stringified). This is intentionally a superset
    /// of the options `build_pool` actually consults: hashing *every* provided
    /// option is conservatively safe (it can only ever cause two calls that
    /// differ in some exotic/no-op option to NOT share — never the reverse), and
    /// it future-proofs the key against new options gaining pool semantics
    /// without anyone remembering to update this list.
    pub(super) fn new(
        primary_url: &str,
        replica_urls: &[String],
        options: Option<&BTreeMap<String, VmValue>>,
        single_connection: bool,
    ) -> Self {
        let mut hasher = Sha256::new();
        // Domain separator + version, so the key scheme can evolve without
        // silently colliding with a future variant.
        hasher.update(b"harn-pg-shared-pool-key\x01");
        hasher.update(b"single_connection:");
        hasher.update([u8::from(single_connection)]);
        hasher.update(b"\x00primary:");
        hash_len_prefixed(&mut hasher, primary_url.as_bytes());
        hasher.update(b"\x00replicas:");
        hasher.update((replica_urls.len() as u64).to_le_bytes());
        for url in replica_urls {
            hash_len_prefixed(&mut hasher, url.as_bytes());
        }
        hasher.update(b"\x00options:");
        if let Some(options) = options {
            // BTreeMap iterates in sorted key order already, but be explicit:
            // collect into a BTreeMap of canonical (key -> display) so the
            // serialization is deterministic regardless of insertion order.
            let canonical: BTreeMap<&str, String> = options
                .iter()
                .map(|(key, value)| (key.as_str(), canonical_option_value(value)))
                .collect();
            hasher.update((canonical.len() as u64).to_le_bytes());
            for (key, value) in canonical {
                hash_len_prefixed(&mut hasher, key.as_bytes());
                hash_len_prefixed(&mut hasher, value.as_bytes());
            }
        } else {
            hasher.update(0u64.to_le_bytes());
        }
        PoolKey(hex::encode(hasher.finalize()))
    }
}

/// Length-prefix then write `bytes`, so two adjacent fields can never be
/// ambiguously concatenated (`"ab" + "c"` vs `"a" + "bc"`).
fn hash_len_prefixed(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update((bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}

/// Canonical, deterministic stringification of an option value for the key.
///
/// Nested lists/dicts (e.g. `replicas`, `circuit_breaker`) are recursively
/// serialized so two structurally-identical option dicts produce the same key
/// and two different ones do not.
fn canonical_option_value(value: &VmValue) -> String {
    match value {
        VmValue::List(items) => {
            let mut out = String::from("[");
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&canonical_option_value(item));
            }
            out.push(']');
            out
        }
        VmValue::Dict(dict) => {
            // Sort keys for order-independence.
            let sorted: BTreeMap<&String, &VmValue> = dict.iter().collect();
            let mut out = String::from("{");
            for (i, (key, val)) in sorted.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(key);
                out.push('=');
                out.push_str(&canonical_option_value(val));
            }
            out.push('}');
            out
        }
        other => other.display(),
    }
}

type SharedRegistry = Mutex<HashMap<PoolKey, Arc<PoolRecord>>>;

/// The process-global shared registry. `None` until an embedder installs it.
/// `OnceLock` so installation is one-shot and lock-free to read.
static SHARED_POOLS: OnceLock<SharedRegistry> = OnceLock::new();

/// Install the process-lifetime shared pool registry. Idempotent: calling it
/// more than once is harmless (subsequent calls are no-ops and keep the first
/// registry). Intended to be called **once** by a long-lived server embedder at
/// startup, before serving requests.
///
/// After this is installed, `pg_pool`/`pg_connect` calls whose full connection
/// identity matches an already-open pool reuse it instead of opening a new one,
/// across requests and worker threads. The CLI never calls this, so the CLI
/// one-shot path is unchanged.
pub fn install_shared_pool_registry() {
    let _ = SHARED_POOLS.get_or_init(|| Mutex::new(HashMap::new()));
}

/// True when the shared registry has been installed (server mode).
pub(super) fn is_installed() -> bool {
    SHARED_POOLS.get().is_some()
}

/// Look up an already-shared pool by key, returning a cheap `Arc` clone.
///
/// Holds the mutex only for the map lookup; never across an await.
pub(super) fn get(key: &PoolKey) -> Option<Arc<PoolRecord>> {
    let registry = SHARED_POOLS.get()?;
    let guard = registry.lock().expect("shared pg pool registry poisoned");
    guard.get(key).map(Arc::clone)
}

/// Insert `record` under `key`, returning the record that ends up canonical.
///
/// If a concurrent caller already inserted this key while we were connecting
/// (the classic double-checked-init race), we keep the **existing** entry and
/// return it; the just-built `record`'s pool is dropped by the caller (its sqlx
/// `Pool` closes its connections on `Drop`). Holds the mutex only for the
/// insert/lookup; never across an await.
pub(super) fn get_or_insert(key: PoolKey, record: Arc<PoolRecord>) -> Arc<PoolRecord> {
    let Some(registry) = SHARED_POOLS.get() else {
        // Not installed: caller should not have reached here, but be safe and
        // just hand back the record unshared.
        return record;
    };
    let mut guard = registry.lock().expect("shared pg pool registry poisoned");
    Arc::clone(guard.entry(key).or_insert(record))
}

/// Test-only: clear and uninstall semantics are not possible on a `OnceLock`,
/// so for test isolation we expose a way to empty the map. The `OnceLock`
/// itself stays installed, which is fine: an emptied registry behaves like a
/// freshly installed one.
#[cfg(test)]
pub(super) fn clear_for_test() {
    if let Some(registry) = SHARED_POOLS.get() {
        registry
            .lock()
            .expect("shared pg pool registry poisoned")
            .clear();
    }
}

#[cfg(test)]
pub(super) fn len_for_test() -> usize {
    SHARED_POOLS
        .get()
        .map(|registry| registry.lock().expect("poisoned").len())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(value: &str) -> VmValue {
        VmValue::String(std::sync::Arc::from(value))
    }

    fn opts(pairs: &[(&str, VmValue)]) -> BTreeMap<String, VmValue> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), v.clone()))
            .collect()
    }

    const URL_A: &str = "postgres://app:secret@db.internal:5432/tenants";
    const URL_B: &str = "postgres://app:secret@db.internal:5432/other";
    const URL_A_DIFF_PW: &str = "postgres://app:HUNTER2@db.internal:5432/tenants";

    /// Identical resolved URL + identical options + identical mode share a key.
    #[test]
    fn same_identity_same_key() {
        let o = opts(&[("max_connections", VmValue::Int(5))]);
        let k1 = PoolKey::new(URL_A, &[], Some(&o), false);
        let k2 = PoolKey::new(URL_A, &[], Some(&o), false);
        assert_eq!(k1, k2);
    }

    /// A different database in the resolved URL must NOT collide.
    #[test]
    fn different_database_different_key() {
        let k1 = PoolKey::new(URL_A, &[], None, false);
        let k2 = PoolKey::new(URL_B, &[], None, false);
        assert_ne!(k1, k2);
    }

    /// SECURITY: a different password (credential) must NOT collide, even though
    /// host/port/db/user are identical.
    #[test]
    fn different_credentials_different_key() {
        let k1 = PoolKey::new(URL_A, &[], None, false);
        let k2 = PoolKey::new(URL_A_DIFF_PW, &[], None, false);
        assert_ne!(k1, k2);
    }

    /// `pg_pool` and `pg_connect` (single_connection) on the same URL must not
    /// alias even with identical options.
    #[test]
    fn single_connection_flag_distinguishes_key() {
        let k_pool = PoolKey::new(URL_A, &[], None, false);
        let k_conn = PoolKey::new(URL_A, &[], None, true);
        assert_ne!(k_pool, k_conn);
    }

    /// Option dict ordering does not affect the key (canonical, sorted).
    #[test]
    fn option_order_is_canonical() {
        let o1 = opts(&[
            ("max_connections", VmValue::Int(5)),
            ("application_name", s("svc")),
        ]);
        let o2 = opts(&[
            ("application_name", s("svc")),
            ("max_connections", VmValue::Int(5)),
        ]);
        assert_eq!(
            PoolKey::new(URL_A, &[], Some(&o1), false),
            PoolKey::new(URL_A, &[], Some(&o2), false)
        );
    }

    /// A differing pool-shaping option (max_connections) yields a different key,
    /// so two calls that asked for differently-sized pools never silently share.
    #[test]
    fn different_pool_shape_different_key() {
        let o1 = opts(&[("max_connections", VmValue::Int(5))]);
        let o2 = opts(&[("max_connections", VmValue::Int(20))]);
        assert_ne!(
            PoolKey::new(URL_A, &[], Some(&o1), false),
            PoolKey::new(URL_A, &[], Some(&o2), false)
        );
    }

    /// A differing application_name yields a different key.
    #[test]
    fn different_application_name_different_key() {
        let o1 = opts(&[("application_name", s("svc-a"))]);
        let o2 = opts(&[("application_name", s("svc-b"))]);
        assert_ne!(
            PoolKey::new(URL_A, &[], Some(&o1), false),
            PoolKey::new(URL_A, &[], Some(&o2), false)
        );
    }

    /// Replica set membership and order are part of the key.
    #[test]
    fn replica_set_is_part_of_key() {
        let r1 = vec![URL_B.to_string()];
        let r2 = vec![URL_B.to_string(), URL_A.to_string()];
        assert_ne!(
            PoolKey::new(URL_A, &[], None, false),
            PoolKey::new(URL_A, &r1, None, false)
        );
        assert_ne!(
            PoolKey::new(URL_A, &r1, None, false),
            PoolKey::new(URL_A, &r2, None, false)
        );
    }

    /// Nested option dicts (e.g. `circuit_breaker`) participate structurally.
    #[test]
    fn nested_option_dicts_affect_key() {
        let cb1 = VmValue::Dict(std::sync::Arc::new(opts(&[(
            "failure_threshold",
            VmValue::Int(3),
        )])));
        let cb2 = VmValue::Dict(std::sync::Arc::new(opts(&[(
            "failure_threshold",
            VmValue::Int(9),
        )])));
        let o1 = opts(&[("circuit_breaker", cb1)]);
        let o2 = opts(&[("circuit_breaker", cb2)]);
        assert_ne!(
            PoolKey::new(URL_A, &[], Some(&o1), false),
            PoolKey::new(URL_A, &[], Some(&o2), false)
        );
    }

    /// The key never retains the plaintext password (it is a SHA-256 hex digest).
    #[test]
    fn key_does_not_leak_plaintext_credentials() {
        let key = PoolKey::new(URL_A_DIFF_PW, &[], None, false);
        assert!(!key.0.contains("HUNTER2"));
        assert_eq!(key.0.len(), 64); // 32-byte SHA-256, hex-encoded.
        assert!(key.0.chars().all(|c| c.is_ascii_hexdigit()));
    }

    /// When the registry is NOT installed, `get`/`is_installed` report empty —
    /// this is the CLI default path and the precondition for byte-identical
    /// behavior. (Asserting the *not-installed* state requires a process where
    /// nobody installed; under nextest each test is its own process.)
    #[test]
    fn not_installed_returns_none_by_default() {
        // We cannot assert globally because another test in the same process may
        // have installed; instead assert the lookup contract: an absent key is
        // None regardless of install state.
        let key = PoolKey::new(
            "postgres://nobody@nowhere/db_never_inserted",
            &[],
            None,
            true,
        );
        assert!(get(&key).is_none());
    }
}
