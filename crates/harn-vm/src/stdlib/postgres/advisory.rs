//! Postgres advisory locks.
//!
//! Surface (issue #2512 A.9):
//!
//! ```harn
//! // Transaction-scoped (auto-released at commit/rollback):
//! pg_transaction(db, { tx ->
//!   pg_advisory_xact_lock(tx, 0x1234)
//!   …
//! })
//!
//! // Non-blocking probe — returns `true` if acquired:
//! pg_transaction(db, { tx ->
//!   if (pg_try_advisory_xact_lock(tx, 0x1234)) { … }
//! })
//!
//! // RAII helper — opens an internal transaction, takes the lock, runs
//! // the closure with the bound `tx`, commits on Ok / rolls back on Err.
//! pg_with_advisory_lock(db, 0x1234, { tx -> … })
//! ```
//!
//! Cross-tenant safety: when `opts.tenant_namespace` is true, the caller's
//! key is XORed with `hashtext(tenant_id)::int8`. Two tenants colliding on
//! key `42` then see distinct lock keys server-side — they can't deadlock
//! each other.
//!
//! Keys may be `int` (mapped to `pg_advisory_lock(key)`) or a 2-tuple
//! `{class: int, instance: int}` (mapped to the `(int, int)` variant).
//! String keys are SHA-256 hashed into the `(int, int)` slot so common
//! identifiers ("migrations", "release-cut") work without callers picking
//! a magic number.

use std::collections::BTreeMap;

use sqlx_core::query::query;

use crate::stdlib::macros::{harn_builtin, BuiltinSignature, Param, TY_ANY, TY_BOOL};
use crate::value::{VmError, VmValue};

use super::{
    bind_params, handle_id, required_arg, runtime_error, tx_by_id, HANDLE_POOL, HANDLE_TX,
};

#[harn_builtin(
    sig_expr = BuiltinSignature::variadic(
        "pg_advisory_xact_lock",
        &[Param::new("args", TY_ANY)],
        TY_BOOL,
    ),
    kind = "async",
    category = "postgres"
)]
async fn pg_advisory_xact_lock_impl(
    _ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    advisory_xact_op(&args, false).await
}

#[harn_builtin(
    sig_expr = BuiltinSignature::variadic(
        "pg_try_advisory_xact_lock",
        &[Param::new("args", TY_ANY)],
        TY_BOOL,
    ),
    kind = "async",
    category = "postgres"
)]
async fn pg_try_advisory_xact_lock_impl(
    _ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    advisory_xact_op(&args, true).await
}

#[harn_builtin(
    sig_expr = BuiltinSignature::variadic(
        "pg_with_advisory_lock",
        &[Param::new("args", TY_ANY)],
        TY_ANY,
    ),
    kind = "async",
    category = "postgres"
)]
async fn pg_with_advisory_lock_impl(
    ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let pool_handle = required_arg(&args, 0, "pg_with_advisory_lock", "pool handle")?;
    let pool_id = handle_id(Some(pool_handle), HANDLE_POOL, "pg_with_advisory_lock")?;
    let key_value = required_arg(&args, 1, "pg_with_advisory_lock", "key")?;
    let closure = match args.get(2) {
        Some(VmValue::Closure(closure)) => closure.clone(),
        _ => {
            return Err(runtime_error(
                "pg_with_advisory_lock: third argument must be a closure",
            ))
        }
    };
    let options = args.get(3).and_then(VmValue::as_dict).cloned();
    let key = resolve_key(key_value, options.as_ref(), "pg_with_advisory_lock")?;

    super::run_managed_transaction(
        &ctx,
        &pool_id,
        "pg_with_advisory_lock",
        closure,
        move |tx_id| {
            let key = key;
            let tx_id_owned = tx_id.to_string();
            Box::pin(async move { take_xact_lock(&tx_id_owned, &key).await })
        },
    )
    .await
}

async fn advisory_xact_op(args: &[VmValue], try_only: bool) -> Result<VmValue, VmError> {
    let builtin = if try_only {
        "pg_try_advisory_xact_lock"
    } else {
        "pg_advisory_xact_lock"
    };
    let target = required_arg(args, 0, builtin, "transaction handle")?;
    let tx_id = handle_id(Some(target), HANDLE_TX, builtin)?;
    let key_value = required_arg(args, 1, builtin, "key")?;
    let options = args.get(2).and_then(VmValue::as_dict).cloned();
    let key = resolve_key(key_value, options.as_ref(), builtin)?;
    if try_only {
        try_take_xact_lock(&tx_id, &key).await
    } else {
        take_xact_lock(&tx_id, &key).await?;
        Ok(VmValue::Bool(true))
    }
}

async fn take_xact_lock(tx_id: &str, key: &LockKey) -> Result<(), VmError> {
    let tx = tx_by_id(tx_id)?;
    let mut tx = tx.lock().await;
    let tx = tx
        .as_mut()
        .ok_or_else(|| runtime_error("pg_advisory_xact_lock: transaction is closed"))?;
    let result = match key {
        LockKey::Single(value) => {
            let params = [VmValue::Int(*value)];
            bind_params(query("SELECT pg_advisory_xact_lock($1)"), &params)?
                .execute(&mut **tx)
                .await
        }
        LockKey::Pair(a, b) => {
            // Bind the two halves as i32 so Postgres resolves the
            // `pg_advisory_xact_lock(int4, int4)` overload. Binding i64 here
            // would ask for a nonexistent `(int8, int8)` function (see the
            // matching i32 binds in `try_take_xact_lock`).
            query("SELECT pg_advisory_xact_lock($1, $2)")
                .bind(*a)
                .bind(*b)
                .execute(&mut **tx)
                .await
        }
    };
    result.map_err(|error| {
        runtime_error(format!("pg_advisory_xact_lock: acquire failed: {error}"))
    })?;
    Ok(())
}

async fn try_take_xact_lock(tx_id: &str, key: &LockKey) -> Result<VmValue, VmError> {
    let tx = tx_by_id(tx_id)?;
    let mut tx = tx.lock().await;
    let tx = tx
        .as_mut()
        .ok_or_else(|| runtime_error("pg_try_advisory_xact_lock: transaction is closed"))?;
    let row: bool = match key {
        LockKey::Single(value) => {
            sqlx_core::query_scalar::query_scalar("SELECT pg_try_advisory_xact_lock($1)")
                .bind(*value)
                .fetch_one(&mut **tx)
                .await
        }
        LockKey::Pair(a, b) => {
            sqlx_core::query_scalar::query_scalar("SELECT pg_try_advisory_xact_lock($1, $2)")
                .bind(*a)
                .bind(*b)
                .fetch_one(&mut **tx)
                .await
        }
    }
    .map_err(|error| runtime_error(format!("pg_try_advisory_xact_lock: {error}")))?;
    Ok(VmValue::Bool(row))
}

#[derive(Clone, Copy)]
enum LockKey {
    Single(i64),
    Pair(i32, i32),
}

fn resolve_key(
    value: &VmValue,
    options: Option<&BTreeMap<String, VmValue>>,
    builtin: &'static str,
) -> Result<LockKey, VmError> {
    let mut key = match value {
        VmValue::Int(n) => LockKey::Single(*n),
        VmValue::String(text) => {
            let hash = sha256_to_i64_pair(text);
            LockKey::Pair(hash.0, hash.1)
        }
        VmValue::Dict(dict) => {
            let class = dict
                .get("class")
                .and_then(VmValue::as_int)
                .ok_or_else(|| runtime_error(format!("{builtin}: key.class must be int")))?;
            let instance = dict
                .get("instance")
                .and_then(VmValue::as_int)
                .ok_or_else(|| runtime_error(format!("{builtin}: key.instance must be int")))?;
            LockKey::Pair(class as i32, instance as i32)
        }
        _ => {
            return Err(runtime_error(format!(
                "{builtin}: key must be int, string, or {{class, instance}} dict"
            )))
        }
    };

    let tenant_namespace =
        super::option_bool(options.and_then(|opts| opts.get("tenant_namespace"))).unwrap_or(false);
    if tenant_namespace {
        let tenant = super::current_tenant_namespace();
        let salt = if tenant.is_empty() {
            0
        } else {
            sha256_to_i64(&tenant)
        };
        key = match key {
            LockKey::Single(value) => LockKey::Single(value ^ salt),
            LockKey::Pair(a, b) => {
                let salt_a = (salt >> 32) as i32;
                let salt_b = salt as i32;
                LockKey::Pair(a ^ salt_a, b ^ salt_b)
            }
        };
    }
    Ok(key)
}

#[cfg(test)]
pub(super) fn tenant_salt_for_test() -> i64 {
    let tenant = super::current_tenant_namespace();
    if tenant.is_empty() {
        0
    } else {
        sha256_to_i64(&tenant)
    }
}

fn sha256_to_i64(text: &str) -> i64 {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    let digest = hasher.finalize();
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&digest[0..8]);
    i64::from_be_bytes(bytes)
}

fn sha256_to_i64_pair(text: &str) -> (i32, i32) {
    let raw = sha256_to_i64(text);
    ((raw >> 32) as i32, raw as i32)
}
