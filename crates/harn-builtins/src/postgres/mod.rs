use std::collections::BTreeMap;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex as SyncMutex;
use sqlx_core::connection::Connection;
use sqlx_core::executor::Executor;
use sqlx_core::query::query;
use sqlx_core::sql_str::AssertSqlSafe;
use sqlx_core::transaction::Transaction;
use sqlx_postgres::{
    PgConnectOptions, PgPool, PgPoolOptions, PgQueryResult, PgSslMode, PgTypeInfo, Postgres,
};
use tokio::sync::Mutex;

use crate::sql_common::{
    is_resource_kind, option_int, render_savepoint_sql, typed_handle, MockDb, SavepointOp,
};
pub(super) use crate::sql_common::{option_bool, resource_handle, runtime_error};
use harn_vm::stdlib::macros::{
    harn_builtin, BuiltinSignature, Param, VmBuiltinDef, TY_ANY, TY_BOOL, TY_DICT, TY_LIST,
    TY_RESOURCE,
};
use harn_vm_core::value::args::{Args, ErrorKind};
use harn_vm_core::value::{DictMap, VmDictExt, VmError, VmResourceHandle, VmValue};
use harn_vm_core::vm::{AsyncBuiltinCtx, Vm};

use self::circuit::{Allow, CircuitBreakerState};

pub(super) const HANDLE_POOL: &str = "pg_pool";
pub(super) const HANDLE_TX: &str = "pg_tx";
pub(super) const HANDLE_MOCK: &str = "pg_mock_pool";

const DEFAULT_STATEMENT_CACHE_CAPACITY: usize = 100;

pub(super) struct PoolRecord {
    pub(super) pool: Arc<PgPool>,
    pub(super) replicas: Vec<Arc<PgPool>>,
    pub(super) replica_cursor: AtomicUsize,
    pub(super) max_connections: u32,
    pub(super) statement_cache_capacity: usize,
    pub(super) read_routing_policy: ReadRoutingPolicy,
    pub(super) circuit: Arc<CircuitBreakerState>,
    pub(super) described_oids: Arc<SyncMutex<BTreeMap<String, Arc<Vec<PgTypeInfo>>>>>,
}

pub(super) struct PoolHandle {
    record: Arc<PoolRecord>,
    closed: AtomicBool,
}

pub(super) struct TxHandle {
    cell: Mutex<Option<Transaction<'static, Postgres>>>,
    parameter_cache: parameter_cache::TransactionParameterCache,
}

pub(crate) fn reset_postgres_state() {
    listen::reset_state();
}

harn_vm_core::stdlib_state_reset!(reset_postgres_state);

pub(crate) fn register_postgres_builtins(vm: &mut Vm) {
    harn_vm::stdlib::macros::register_builtin_defs(vm, MODULE_BUILTINS);
    register_postgres_namespace(vm);
}

harn_vm_core::stdlib_installer!(Host, register_postgres_builtins);

const MODULE_BUILTINS: &[&VmBuiltinDef] = &[
    // Core (v1 — issue #2500)
    &PG_POOL_IMPL_DEF,
    &PG_CONNECT_IMPL_DEF,
    &PG_CLOSE_IMPL_DEF,
    &PG_STMT_CACHE_CLEAR_IMPL_DEF,
    &PG_QUERY_IMPL_DEF,
    &PG_QUERY_ONE_IMPL_DEF,
    &PG_EXECUTE_IMPL_DEF,
    &PG_TRANSACTION_IMPL_DEF,
    &PG_SAVEPOINT_IMPL_DEF,
    &PG_RELEASE_SAVEPOINT_IMPL_DEF,
    &PG_ROLLBACK_TO_SAVEPOINT_IMPL_DEF,
    &PG_MIGRATE_IMPL_DEF,
    &PG_MOCK_POOL_IMPL_DEF,
    &PG_MOCK_CALLS_IMPL_DEF,
    // Advisory locks (v2 — issue #2512)
    &advisory::PG_ADVISORY_XACT_LOCK_IMPL_DEF,
    &advisory::PG_TRY_ADVISORY_XACT_LOCK_IMPL_DEF,
    &advisory::PG_WITH_ADVISORY_LOCK_IMPL_DEF,
    // LISTEN/NOTIFY (v2 — issue #2512)
    &listen::PG_LISTEN_IMPL_DEF,
    &listen::PG_LISTENER_RECV_IMPL_DEF,
    &listen::PG_LISTENER_CLOSE_IMPL_DEF,
    &listen::PG_NOTIFY_IMPL_DEF,
    // JSONB helpers (v2 — issue #2512)
    &jsonb::PG_JSONB_PATH_IMPL_DEF,
    &jsonb::PG_JSONB_MERGE_IMPL_DEF,
    &jsonb::PG_JSONB_CONTAINS_IMPL_DEF,
    // Schema introspection + pool observability + partitions (v2)
    &introspect::PG_INTROSPECT_TABLES_IMPL_DEF,
    &introspect::PG_INTROSPECT_COLUMNS_IMPL_DEF,
    &introspect::PG_INTROSPECT_INDEXES_IMPL_DEF,
    &introspect::PG_POOL_STATS_IMPL_DEF,
    &introspect::PG_PARTITION_ATTACH_IMPL_DEF,
    &introspect::PG_PARTITION_DETACH_IMPL_DEF,
    &introspect::PG_PARTITION_PRUNE_IMPL_DEF,
    &introspect::PG_PARTITION_RETAIN_IMPL_DEF,
    &introspect::PG_PARTITION_CREATE_FOR_WINDOW_IMPL_DEF,
];

fn register_postgres_namespace(vm: &mut Vm) {
    let jsonb = namespace(
        "pg.jsonb",
        &[
            ("path", "pg.jsonb.path"),
            ("merge", "pg.jsonb.merge"),
            ("contains", "pg.jsonb.contains"),
        ],
    );
    vm.set_global(
        "pg",
        VmValue::dict([("_namespace", VmValue::string("pg")), ("jsonb", jsonb)]),
    );
}

fn namespace(name: &str, entries: &[(&str, &str)]) -> VmValue {
    VmValue::dict(
        std::iter::once(("_namespace".to_string(), VmValue::string(name.to_string())))
            .chain(entries.iter().map(|(field, builtin)| {
                (
                    (*field).to_string(),
                    VmValue::BuiltinRef(arcstr::ArcStr::from(*builtin)),
                )
            }))
            .collect::<BTreeMap<_, _>>(),
    )
}

mod advisory;
mod circuit;
mod introspect;
mod jsonb;
mod listen;
mod migrate;
mod parameter_cache;
mod shared;
mod value_codec;

pub(super) use value_codec::{bind_params, params_have_nil, row_to_value};
use value_codec::{bind_params_described, described_param_oids, map_db_error, run_described_query};
#[cfg(test)]
use value_codec::{
    point_value, points_value, range_value, reject_non_finite_floats, sqlstate_category,
};

pub use shared::install_shared_pool_registry;

#[harn_builtin(
    exposure = "runtime_internal",
    sig_expr = BuiltinSignature::variadic("pg_pool", &[Param::new("args", TY_ANY
)], TY_RESOURCE),
    kind = "async",
    category = "postgres"
)]
async fn pg_pool_impl(ctx: AsyncBuiltinCtx, args: Vec<VmValue>) -> Result<VmValue, VmError> {
    let source = args.first().ok_or_else(|| {
        runtime_error("pg_pool: url, env:, secret:, or {url|env|secret} is required")
    })?;
    let options = args.get(1).and_then(VmValue::as_dict).cloned();
    open_pool(&ctx, source, options.as_ref(), false).await
}

#[harn_builtin(
    exposure = "runtime_internal",
    sig_expr = BuiltinSignature::variadic("pg_connect", &[Param::new("args", TY_ANY
)], TY_RESOURCE),
    kind = "async",
    category = "postgres"
)]
async fn pg_connect_impl(ctx: AsyncBuiltinCtx, args: Vec<VmValue>) -> Result<VmValue, VmError> {
    let source = args.first().ok_or_else(|| {
        runtime_error("pg_connect: url, env:, secret:, or {url|env|secret} is required")
    })?;
    let options = args.get(1).and_then(VmValue::as_dict).cloned();
    open_pool(&ctx, source, options.as_ref(), true).await
}

#[harn_builtin(
    exposure = "capability_arg:0",
    effects = ["network.write@const=postgres"],
    sig_expr = BuiltinSignature::variadic("pg_close", &[Param::new("args", TY_ANY
)], TY_BOOL),
    kind = "async",
    category = "postgres"
)]
async fn pg_close_impl(_ctx: AsyncBuiltinCtx, args: Vec<VmValue>) -> Result<VmValue, VmError> {
    let handle = pool_handle(args.first(), "pg_close")?;
    Ok(VmValue::Bool(!handle.closed.swap(true, Ordering::SeqCst)))
}

#[harn_builtin(
    exposure = "capability_arg:0",
    effects = ["state.mutate@const=postgres-statement-cache"],
    sig_expr = BuiltinSignature::variadic("pg_stmt_cache_clear", &[Param::new("args", TY_ANY
)], TY_DICT),
    kind = "async",
    category = "postgres"
)]
async fn pg_stmt_cache_clear_impl(
    _ctx: AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let target = required_arg(&args, 0, "pg_stmt_cache_clear", "pool handle")?;
    if is_resource_kind(Some(target), HANDLE_MOCK) {
        mock_handle(Some(target), "pg_stmt_cache_clear")?;
        return Ok(stmt_cache_clear_result(0, 0, 0));
    }

    let record = pool_record_from_handle(target, "pg_stmt_cache_clear")?;

    let mut pools = 0_i64;
    let mut connections_cleared = 0_i64;
    let mut connections_skipped = 0_i64;
    for pool in std::iter::once(&record.pool).chain(record.replicas.iter()) {
        pools += 1;
        let (cleared, skipped) = clear_idle_statement_caches(pool, "pg_stmt_cache_clear").await?;
        connections_cleared += i64::from(cleared);
        connections_skipped += i64::from(skipped);
    }

    Ok(stmt_cache_clear_result(
        pools,
        connections_cleared,
        connections_skipped,
    ))
}

fn stmt_cache_clear_result(
    pools: i64,
    connections_cleared: i64,
    connections_skipped: i64,
) -> VmValue {
    let mut result = DictMap::new();
    result.put("pools", VmValue::Int(pools));
    result.put("connections_cleared", VmValue::Int(connections_cleared));
    result.put("connections_skipped", VmValue::Int(connections_skipped));
    VmValue::dict(result)
}

async fn clear_idle_statement_caches(
    pool: &PgPool,
    builtin: &'static str,
) -> Result<(u32, u32), VmError> {
    let size_before = pool.size();
    let mut cleared = 0_u32;
    let mut connections = Vec::new();

    while let Some(mut connection) = pool.try_acquire() {
        connection
            .clear_cached_statements()
            .await
            .map_err(|error| runtime_error(format!("{builtin}: {error}")))?;
        cleared += 1;
        connections.push(connection);
    }

    Ok((cleared, size_before.saturating_sub(cleared)))
}

/// Recycle prepared-statement state after a `pg_migrate` that ran DDL (M-5).
///
/// `pg_migrate` runs `CREATE TABLE` / `ALTER TABLE` / etc., which can change the
/// result type of a query whose plan a pooled connection has already cached. The
/// next reuse of that cached plan then fails with SQLSTATE `0A000`
/// (`cached plan must not change result type`).
///
/// We drain every connection currently in the pool — acquiring up to
/// `max_connections` of them (so we catch the one the migration ran on, which
/// sqlx returns to the pool asynchronously and which `try_acquire` can therefore
/// miss right after the run) — clear each one's cached statements (sqlx sends a
/// server-side `Close`/`DEALLOCATE`), and clear this pool authority's described
/// parameter cache (DDL can change the server-inferred type of a `$n` slot).
///
/// Best-effort and non-fatal: the migration already committed, so a clear
/// failure is logged and the run still reports success. The `acquire` is bounded
/// by `max` and each is immediately released, so this never starves the pool. A
/// connection still checked out by another task at recycle time is not drained
/// here; its first post-DDL reuse would hit `0A000` once and sqlx re-prepares —
/// the same self-healing sqlx already does for a plan invalidated out of band.
pub(super) async fn recycle_pool_after_ddl(
    pool: &PgPool,
    described_oids: &SyncMutex<BTreeMap<String, Arc<Vec<PgTypeInfo>>>>,
    max: u32,
) {
    let mut held = Vec::new();
    // Acquire (waiting, not try_acquire) so we deterministically catch the
    // just-released migration connection even though its return to the pool is
    // processed asynchronously. Bounded by `max` so we never block forever.
    for _ in 0..max.max(1) {
        match pool.try_acquire() {
            Some(conn) => held.push(conn),
            None => match pool.acquire().await {
                Ok(conn) => held.push(conn),
                Err(_) => break,
            },
        }
    }
    for mut connection in held {
        if let Err(error) = connection.clear_cached_statements().await {
            tracing::warn!(
                target: "harn_vm::postgres",
                %error,
                "pg_migrate: clearing cached statements after DDL failed (non-fatal)"
            );
        }
    }
    described_oids.lock().clear();
}

#[harn_builtin(
    exposure = "capability_arg:0",
    effects = ["network.read@const=postgres"],
    sig_expr = BuiltinSignature::variadic("pg_query", &[Param::new("args", TY_ANY
)], TY_LIST),
    kind = "async",
    category = "postgres"
)]
async fn pg_query_impl(_ctx: AsyncBuiltinCtx, args: Vec<VmValue>) -> Result<VmValue, VmError> {
    let target = args
        .first()
        .ok_or_else(|| runtime_error("pg_query: pool or transaction handle is required"))?;
    let sql = required_string_arg(&args, 1, "pg_query", "sql")?;
    let params = params_arg(args.get(2), "pg_query")?;
    let options = args.get(3).and_then(VmValue::as_dict);
    let routing = routing_from_options(options)?;
    query_many(target, &sql, &params, routing).await
}

#[harn_builtin(
    exposure = "capability_arg:0",
    effects = ["network.read@const=postgres"],
    sig_expr = BuiltinSignature::variadic("pg_query_one", &[Param::new("args", TY_ANY
)], TY_ANY),
    kind = "async",
    category = "postgres"
)]
async fn pg_query_one_impl(_ctx: AsyncBuiltinCtx, args: Vec<VmValue>) -> Result<VmValue, VmError> {
    let target = args
        .first()
        .ok_or_else(|| runtime_error("pg_query_one: pool or transaction handle is required"))?;
    let sql = required_string_arg(&args, 1, "pg_query_one", "sql")?;
    let params = params_arg(args.get(2), "pg_query_one")?;
    let options = args.get(3).and_then(VmValue::as_dict);
    let routing = routing_from_options(options)?;
    let rows = query_rows(target, &sql, &params, routing).await?;
    Ok(rows.into_iter().next().unwrap_or(VmValue::Nil))
}

#[harn_builtin(
    exposure = "capability_arg:0",
    effects = ["network.write@const=postgres"],
    sig_expr = BuiltinSignature::variadic("pg_execute", &[Param::new("args", TY_ANY
)], TY_DICT),
    kind = "async",
    category = "postgres"
)]
async fn pg_execute_impl(_ctx: AsyncBuiltinCtx, args: Vec<VmValue>) -> Result<VmValue, VmError> {
    let target = args
        .first()
        .ok_or_else(|| runtime_error("pg_execute: pool or transaction handle is required"))?;
    let sql = required_string_arg(&args, 1, "pg_execute", "sql")?;
    let params = params_arg(args.get(2), "pg_execute")?;
    execute_stmt(target, &sql, &params).await
}

#[harn_builtin(
    exposure = "capability_arg:0",
    effects = ["network.write@const=postgres"],
    sig_expr = BuiltinSignature::variadic("pg_transaction", &[Param::new("args", TY_ANY
)], TY_ANY),
    kind = "async",
    category = "postgres"
)]
async fn pg_transaction_impl(ctx: AsyncBuiltinCtx, args: Vec<VmValue>) -> Result<VmValue, VmError> {
    let pool = pool_record_from_handle(
        args.first()
            .ok_or_else(|| runtime_error("pg_transaction: pool handle is required"))?,
        "pg_transaction",
    )?;
    let closure = match args.get(1) {
        Some(VmValue::Closure(closure)) => closure.clone(),
        _ => {
            return Err(runtime_error(
                "pg_transaction: second argument must be a closure",
            ))
        }
    };
    let options = args.get(2).and_then(VmValue::as_dict).cloned();
    let settings = options
        .as_ref()
        .and_then(|opts| opts.get("settings"))
        .and_then(VmValue::as_dict)
        .cloned();
    run_managed_transaction(&ctx, pool, "pg_transaction", closure, move |tx| {
        Box::pin(async move {
            if let Some(settings) = settings {
                apply_transaction_settings(&tx, &settings).await?;
            }
            Ok(())
        })
    })
    .await
}

/// Shared txn-with-closure wrapper used by `pg_transaction` and
/// `pg_with_advisory_lock`. Opens a transaction on the pool, registers
/// the tx in the local registry under a fresh id, runs `prepare(tx_id)`
/// so the caller can take an advisory lock / apply settings / etc., then
/// invokes the closure with the `pg_tx` handle. Commit on `Ok(...)`,
/// rollback on any error in the closure.
pub(super) async fn run_managed_transaction(
    ctx: &AsyncBuiltinCtx,
    pool: Arc<PoolRecord>,
    builtin: &'static str,
    closure: Arc<harn_vm_core::value::VmClosure>,
    prepare: impl FnOnce(
        Arc<TxHandle>,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<(), VmError>> + Send + 'static>,
    >,
) -> Result<VmValue, VmError> {
    let tx = pool
        .pool
        .begin()
        .await
        .map_err(|error| runtime_error(format!("{builtin}: begin failed: {error}")))?;
    let tx_state = Arc::new(TxHandle {
        cell: Mutex::new(Some(tx)),
        parameter_cache: parameter_cache::TransactionParameterCache::new(&pool.described_oids),
    });
    let tx_value = VmValue::resource(VmResourceHandle::from_arc(HANDLE_TX, Arc::clone(&tx_state)));

    if let Err(error) = prepare(Arc::clone(&tx_state)).await {
        if let Some(tx) = tx_state.cell.lock().await.take() {
            let _ = tx.rollback().await;
        }
        return Err(error);
    }

    let mut child_vm = ctx.child_vm();
    let result = child_vm.call_closure_pub(&closure, &[tx_value]).await;
    ctx.forward_output(&child_vm.take_output());

    let tx = tx_state
        .cell
        .lock()
        .await
        .take()
        .ok_or_else(|| runtime_error(format!("{builtin}: transaction was already consumed")))?;
    match result {
        Ok(value) => {
            tx.commit()
                .await
                .map_err(|error| runtime_error(format!("{builtin}: commit failed: {error}")))?;
            Ok(value)
        }
        Err(error) => {
            let _ = tx.rollback().await;
            Err(error)
        }
    }
}

#[harn_builtin(
    exposure = "capability_arg:0",
    effects = ["network.write@const=postgres"],
    sig_expr = BuiltinSignature::variadic("pg_savepoint", &[Param::new("args", TY_ANY
)], TY_BOOL),
    kind = "async",
    category = "postgres"
)]
async fn pg_savepoint_impl(_ctx: AsyncBuiltinCtx, args: Vec<VmValue>) -> Result<VmValue, VmError> {
    savepoint_op(&args, "pg_savepoint", SavepointOp::Create).await
}

#[harn_builtin(
    exposure = "capability_arg:0",
    effects = ["network.write@const=postgres"],
    sig_expr = BuiltinSignature::variadic("pg_release_savepoint", &[Param::new("args", TY_ANY
)], TY_BOOL),
    kind = "async",
    category = "postgres"
)]
async fn pg_release_savepoint_impl(
    _ctx: AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    savepoint_op(&args, "pg_release_savepoint", SavepointOp::Release).await
}

#[harn_builtin(
    exposure = "capability_arg:0",
    effects = ["network.write@const=postgres"],
    sig_expr = BuiltinSignature::variadic("pg_rollback_to_savepoint", &[Param::new("args", TY_ANY
)], TY_BOOL),
    kind = "async",
    category = "postgres"
)]
async fn pg_rollback_to_savepoint_impl(
    _ctx: AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    savepoint_op(&args, "pg_rollback_to_savepoint", SavepointOp::RollbackTo).await
}

#[harn_builtin(
    exposure = "capability_arg:0",
    effects = ["network.write@const=postgres"],
    sig_expr = BuiltinSignature::variadic("pg_migrate", &[Param::new("args", TY_ANY
)], TY_DICT),
    kind = "async",
    category = "postgres"
)]
async fn pg_migrate_impl(_ctx: AsyncBuiltinCtx, args: Vec<VmValue>) -> Result<VmValue, VmError> {
    migrate::run(args).await
}

#[harn_builtin(
    exposure = "harness.testing.pg_mock_pool",
    effects = ["state.mutate@dynamic"],
    sig_expr = BuiltinSignature::variadic("pg_mock_pool", &[Param::new("args", TY_ANY
)], TY_RESOURCE),
    category = "postgres"
)]
fn pg_mock_pool_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    crate::sql_common::mock_db_resource(args, "pg_mock_pool", HANDLE_MOCK)
}

#[harn_builtin(
    exposure = "capability_arg:0",
    effects = ["state.read@arg0"],
    sig_expr = BuiltinSignature::variadic("pg_mock_calls", &[Param::new("args", TY_ANY
)], TY_LIST),
    category = "postgres"
)]
fn pg_mock_calls_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let mock = mock_handle(args.first(), "pg_mock_calls")?;
    let calls = mock.lock().calls.clone();
    Ok(VmValue::list(calls))
}

async fn open_pool(
    ctx: &AsyncBuiltinCtx,
    source: &VmValue,
    options: Option<&DictMap>,
    single_connection: bool,
) -> Result<VmValue, VmError> {
    let primary_url = resolve_connection_url(ctx, source).await?;
    let stmt_cache_capacity = option_int(options, "statement_cache_capacity")
        .map(|n| n.max(0) as usize)
        .unwrap_or(DEFAULT_STATEMENT_CACHE_CAPACITY);
    let read_routing_policy = read_routing_policy_from_options(options)?;
    let max_connections = if single_connection {
        1
    } else {
        option_int(options, "max_connections")
            .unwrap_or(5)
            .clamp(1, i64::from(u32::MAX)) as u32
    };

    // Resolve replica URLs up front: they participate in the shared-pool key,
    // and resolving them is cheap (no connection opened) relative to building
    // the pools. These awaits hold no registry lock.
    let replica_urls = collect_replica_urls(ctx, options).await?;

    // When a server embedder has installed the shared registry, try to reuse an
    // existing pool whose *full* connection identity matches before building a
    // new one. This is a double-checked init: the lock is only held for the map
    // lookup (in `shared::get`), never across the pool-building await below.
    // `application_name` is reflected in the returned handle metadata (and is
    // part of the shared-pool key, so a shared hit always has the same value).
    let application_name = option_string(options, "application_name");
    let shared_key = shared::is_installed()
        .then(|| shared::PoolKey::new(&primary_url, &replica_urls, options, single_connection))
        .flatten();
    if let Some(key) = &shared_key {
        if let Some(record) = shared::get(key) {
            return Ok(register_local_pool_handle(
                record,
                single_connection,
                application_name,
            ));
        }
    }

    let primary_pool = build_pool(
        &primary_url,
        options,
        max_connections,
        stmt_cache_capacity,
        "pg_pool",
    )
    .await?;

    let mut replicas = Vec::with_capacity(replica_urls.len());
    for url in &replica_urls {
        let pool = build_pool(
            url,
            options,
            max_connections,
            stmt_cache_capacity,
            "pg_pool replica",
        )
        .await?;
        replicas.push(Arc::new(pool));
    }

    let circuit = Arc::new(build_circuit_breaker(options));

    let record = Arc::new(PoolRecord {
        pool: Arc::new(primary_pool),
        replicas,
        replica_cursor: AtomicUsize::new(0),
        max_connections,
        statement_cache_capacity: stmt_cache_capacity,
        read_routing_policy,
        circuit,
        described_oids: Arc::new(SyncMutex::new(BTreeMap::new())),
    });

    // If sharing is enabled, publish (or adopt a racing winner's) record into
    // the process-global registry. `get_or_insert` keeps the existing entry on a
    // race, so concurrent first-opens of the same identity converge on ONE pool;
    // our now-unused `record` is dropped, closing its freshly-opened pool.
    let record = match shared_key {
        Some(key) => shared::get_or_insert(key, record),
        None => record,
    };

    Ok(register_local_pool_handle(
        record,
        single_connection,
        application_name,
    ))
}

/// Mint a VM-local, unforgeable authority over a possibly shared pool record.
///
/// Sharing happens only at the [`PoolRecord`] layer. Each returned resource has
/// independent close state, so closing one request's authority cannot revoke a
/// sibling request that adopted the same explicitly shared pool.
fn register_local_pool_handle(
    record: Arc<PoolRecord>,
    _single_connection: bool,
    _application_name: Option<String>,
) -> VmValue {
    VmValue::resource(VmResourceHandle::new(
        HANDLE_POOL,
        PoolHandle {
            record,
            closed: AtomicBool::new(false),
        },
    ))
}

async fn build_pool(
    url: &str,
    options: Option<&DictMap>,
    max_connections: u32,
    stmt_cache_capacity: usize,
    label: &'static str,
) -> Result<PgPool, VmError> {
    let mut connect_options = PgConnectOptions::from_str(url).map_err(|error| {
        runtime_error(format!("{label}: invalid Postgres URL/options: {error}"))
    })?;
    if let Some(application_name) = option_string(options, "application_name") {
        connect_options = connect_options.application_name(&application_name);
    }
    if let Some(ssl_mode) =
        option_string(options, "ssl_mode").or_else(|| option_string(options, "tls_mode"))
    {
        connect_options = connect_options.ssl_mode(parse_ssl_mode(&ssl_mode)?);
    }
    connect_options = connect_options.statement_cache_capacity(stmt_cache_capacity);

    let mut pool_options = PgPoolOptions::new().max_connections(max_connections);
    if let Some(min_connections) = option_int(options, "min_connections") {
        pool_options = pool_options
            .min_connections(min_connections.clamp(0, i64::from(max_connections)) as u32);
    }
    if let Some(ms) = option_duration_ms(options, "acquire_timeout_ms")
        .or_else(|| option_duration_ms(options, "timeout_ms"))
    {
        pool_options = pool_options.acquire_timeout(Duration::from_millis(ms));
    }
    if let Some(ms) = option_duration_ms(options, "idle_timeout_ms") {
        pool_options = pool_options.idle_timeout(Duration::from_millis(ms));
    }
    if let Some(ms) = option_duration_ms(options, "max_lifetime_ms") {
        pool_options = pool_options.max_lifetime(Duration::from_millis(ms));
    }

    pool_options
        .connect_with(connect_options)
        .await
        .map_err(|error| runtime_error(format!("{label}: connect failed: {error}")))
}

async fn collect_replica_urls(
    ctx: &AsyncBuiltinCtx,
    options: Option<&DictMap>,
) -> Result<Vec<String>, VmError> {
    let Some(replicas_value) = options.and_then(|opts| opts.get("replicas")) else {
        return Ok(Vec::new());
    };
    let items = match replicas_value {
        VmValue::List(items) => items.as_ref(),
        VmValue::Nil => return Ok(Vec::new()),
        _ => {
            return Err(runtime_error(
                "pg_pool: replicas must be a list of url strings or {url|env|secret} dicts",
            ))
        }
    };
    let mut urls = Vec::with_capacity(items.len());
    for entry in items {
        urls.push(resolve_connection_url(ctx, entry).await?);
    }
    Ok(urls)
}

fn build_circuit_breaker(options: Option<&DictMap>) -> CircuitBreakerState {
    let Some(cb) = options
        .and_then(|opts| opts.get("circuit_breaker"))
        .and_then(VmValue::as_dict)
    else {
        return CircuitBreakerState::disabled();
    };
    let threshold = cb
        .get("failure_threshold")
        .and_then(VmValue::as_int)
        .filter(|n| *n > 0)
        .map(|n| n.clamp(1, i64::from(u32::MAX)) as u32);
    let Some(threshold) = threshold else {
        return CircuitBreakerState::disabled();
    };
    let reset_after_ms = cb
        .get("reset_after_ms")
        .and_then(|v| match v {
            VmValue::Int(n) => Some(*n),
            VmValue::Duration(n) => Some(*n),
            _ => None,
        })
        .filter(|n| *n >= 0)
        .unwrap_or(30_000);
    CircuitBreakerState::new(threshold, reset_after_ms)
}

async fn query_many(
    target: &VmValue,
    sql: &str,
    params: &[VmValue],
    routing: QueryRouting,
) -> Result<VmValue, VmError> {
    let rows = query_rows(target, sql, params, routing).await?;
    Ok(VmValue::list(rows))
}

pub(super) async fn query_rows(
    target: &VmValue,
    sql: &str,
    params: &[VmValue],
    routing: QueryRouting,
) -> Result<Vec<VmValue>, VmError> {
    harn_vm::call_budget::charge_pg_query()?;
    match target {
        VmValue::Resource(handle) if handle.label() == HANDLE_MOCK => {
            return mock_query(target, sql, params, false);
        }
        VmValue::Resource(handle) if handle.label() == HANDLE_TX => {
            let tx_state = tx_handle(Some(target), "pg_query")?;
            let mut tx = tx_state.cell.lock().await;
            let tx = tx
                .as_mut()
                .ok_or_else(|| runtime_error("pg_query: transaction is closed"))?;
            let has_nil = params_have_nil(params);
            let initial_cache = tx_state.parameter_cache.begin_statement();
            let rows = if has_nil {
                let oids = tx_state
                    .parameter_cache
                    .describe(initial_cache, tx, sql, "pg_query")
                    .await?;
                bind_params_described(sql, &oids, params)?
                    .fetch_all(&mut **tx)
                    .await
            } else {
                bind_params(query(AssertSqlSafe(sql)), params)?
                    .fetch_all(&mut **tx)
                    .await
            }
            .map_err(|error| map_db_error("pg_query", error))?;
            return rows.into_iter().map(row_to_value).collect();
        }
        _ => {}
    }

    let record = pool_record_from_handle(target, "pg_query")?;
    let pool = pool_for_routing(&record, routing, "pg_query")?;
    let (probe, _) = enter_circuit(&record.circuit, "pg_query")?;
    let result = if params_have_nil(params) {
        run_described_query(
            &pool,
            &record.described_oids,
            sql,
            params,
            "pg_query",
            |q, conn| Box::pin(async move { q.fetch_all(conn).await }),
        )
        .await
    } else {
        bind_params(query(AssertSqlSafe(sql)), params)?
            .fetch_all(pool.as_ref())
            .await
            .map_err(|error| map_db_error("pg_query", error))
    };
    match result {
        Ok(rows) => {
            record.circuit.record_success(probe);
            rows.into_iter().map(row_to_value).collect()
        }
        Err(error) => {
            record.circuit.record_failure(probe);
            Err(error)
        }
    }
}

pub(super) async fn execute_stmt(
    target: &VmValue,
    sql: &str,
    params: &[VmValue],
) -> Result<VmValue, VmError> {
    harn_vm::call_budget::charge_pg_query()?;
    let started = std::time::Instant::now();
    if is_resource_kind(Some(target), HANDLE_MOCK) {
        let rows = mock_query(target, sql, params, true)?;
        let rows_affected = rows
            .first()
            .and_then(VmValue::as_dict)
            .and_then(|dict| dict.get("rows_affected"))
            .and_then(VmValue::as_int)
            .unwrap_or(0)
            .max(0) as u64;
        return Ok(execute_result_value(rows_affected, started.elapsed()));
    }
    if is_resource_kind(Some(target), HANDLE_TX) {
        let tx_state = tx_handle(Some(target), "pg_execute")?;
        let mut tx = tx_state.cell.lock().await;
        let tx = tx
            .as_mut()
            .ok_or_else(|| runtime_error("pg_execute: transaction is closed"))?;
        let has_nil = params_have_nil(params);
        let initial_cache = tx_state.parameter_cache.begin_statement();
        let result = if has_nil {
            let oids = tx_state
                .parameter_cache
                .describe(initial_cache, tx, sql, "pg_execute")
                .await?;
            bind_params_described(sql, &oids, params)?
                .execute(&mut **tx)
                .await
        } else {
            bind_params(query(AssertSqlSafe(sql)), params)?
                .execute(&mut **tx)
                .await
        }
        .map_err(|error| map_db_error("pg_execute", error))?;
        return Ok(query_result_value(result, started.elapsed()));
    }
    let record = pool_record_from_handle(target, "pg_execute")?;
    let (probe, _) = enter_circuit(&record.circuit, "pg_execute")?;
    let result = if params_have_nil(params) {
        run_described_query(
            &record.pool,
            &record.described_oids,
            sql,
            params,
            "pg_execute",
            |q, conn| Box::pin(async move { q.execute(conn).await }),
        )
        .await
    } else {
        bind_params(query(AssertSqlSafe(sql)), params)?
            .execute(record.pool.as_ref())
            .await
            .map_err(|error| map_db_error("pg_execute", error))
    };
    match result {
        Ok(query_result) => {
            record.circuit.record_success(probe);
            Ok(query_result_value(query_result, started.elapsed()))
        }
        Err(error) => {
            record.circuit.record_failure(probe);
            Err(error)
        }
    }
}

async fn savepoint_op(
    args: &[VmValue],
    builtin: &'static str,
    op: SavepointOp,
) -> Result<VmValue, VmError> {
    let target = args
        .first()
        .ok_or_else(|| runtime_error(format!("{builtin}: transaction handle is required")))?;
    let name = required_string_arg(args, 1, builtin, "name")?;
    validate_savepoint_name(&name, builtin)?;
    // Savepoints are no-ops against mock pools; record the dispatched SQL
    // so tests can assert on it but skip the (nonexistent) transaction
    // state machine.
    if is_resource_kind(Some(target), HANDLE_MOCK) {
        let sql = render_savepoint_sql(op, &name);
        let _ = mock_query(target, &sql, &[], true);
        return Ok(VmValue::Bool(true));
    }
    let tx = tx_handle(Some(target), builtin)?;
    let mut tx = tx.cell.lock().await;
    let tx = tx
        .as_mut()
        .ok_or_else(|| runtime_error(format!("{builtin}: transaction is closed")))?;
    let sql = render_savepoint_sql(op, &name);
    (&mut **tx)
        .execute(AssertSqlSafe(sql))
        .await
        .map_err(|error| runtime_error(format!("{builtin}: {error}")))?;
    Ok(VmValue::Bool(true))
}

fn validate_savepoint_name(name: &str, builtin: &'static str) -> Result<(), VmError> {
    // Savepoints can be scoped with `.` so `migration.0042` is valid.
    validate_pg_identifier(name, builtin, "savepoint name", &['_', '.'])
}

/// Shared Postgres-identifier validator. `extras` lists characters that
/// are accepted in addition to ASCII alphanumerics; `_` is the standard
/// PG-identifier extra, `.` is accepted by savepoint/channel callers.
///
/// Used by savepoints (mod.rs), partition + introspection identifiers
/// (introspect.rs), and LISTEN/NOTIFY channel names (listen.rs) so they
/// share one canonical reject set. The 63-byte ceiling matches Postgres'
/// `NAMEDATALEN - 1` default.
pub(super) fn validate_pg_identifier(
    name: &str,
    builtin: &'static str,
    label: &'static str,
    extras: &[char],
) -> Result<(), VmError> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(runtime_error(format!(
            "{builtin}: {label} must not be empty"
        )));
    }
    if name.len() > 63 {
        return Err(runtime_error(format!(
            "{builtin}: {label} exceeds Postgres identifier length (63 bytes)"
        )));
    }
    let first = name.chars().next().expect("non-empty checked above");
    if !(first.is_ascii_alphabetic() || first == '_') {
        return Err(runtime_error(format!(
            "{builtin}: {label} must start with a letter or underscore"
        )));
    }
    for ch in name.chars() {
        if !(ch.is_ascii_alphanumeric() || ch == '_' || extras.contains(&ch)) {
            return Err(runtime_error(format!(
                "{builtin}: {label} `{name}` contains disallowed character `{ch}`"
            )));
        }
    }
    Ok(())
}

/// GUC keys that `pg_transaction(settings)` is permitted to `set_config`.
///
/// These are the *non-`app.`* runtime parameters Harn callers legitimately
/// tune per transaction: the statement/lock/idle timeouts. Everything in the
/// `app.*` namespace is allowed by prefix (see [`is_allowed_transaction_setting`])
/// because that namespace is the application's own RLS contract — RLS policies
/// are written against `app.current_tenant_id` / `app.bypass_rls`, so those keys
/// are part of the intended security model, not an escape from it.
///
/// Crucially, this allowlist does **not** include privileged Postgres GUCs that
/// would let `.harn` code escalate *beyond* the app's RLS model at the Postgres
/// level — `role`, `session_authorization`, `is_superuser`, `search_path`, etc.
/// Those are rejected so a `pg_transaction(settings)` cannot, for example,
/// `SET ROLE` to a superuser and read every tenant's rows regardless of policy.
const ALLOWED_TRANSACTION_SETTINGS: &[&str] = &[
    "statement_timeout",
    "lock_timeout",
    "idle_in_transaction_session_timeout",
];

/// True when `key` is a GUC that `pg_transaction(settings)` may set.
///
/// Allowed: any `app.*` key (the application's own settable namespace, which is
/// what RLS policies read) and the explicit timeout GUCs in
/// [`ALLOWED_TRANSACTION_SETTINGS`]. Everything else — notably `role`,
/// `session_authorization`, `is_superuser`, `search_path` and any other
/// privileged backend GUC — is rejected so untrusted `.harn` code cannot use a
/// transaction setting to escape row-level security at the Postgres level.
///
/// The comparison is case-insensitive on the *non*-`app.` keys because Postgres
/// GUC names are case-insensitive; `app.*` is matched by prefix on the original
/// (custom GUC namespaces are conventionally lower-case and case-sensitive after
/// the first `.`, so we keep the user's spelling there).
fn is_allowed_transaction_setting(key: &str) -> bool {
    let key = key.trim();
    if key.is_empty() {
        return false;
    }
    // App-scoped settings (`app.current_tenant_id`, `app.bypass_rls`, …) are the
    // application's own contract that RLS policies read; allow the whole prefix.
    if let Some(rest) = key.strip_prefix("app.") {
        // Reject `app.` with nothing after it or with an embedded NUL.
        return !rest.is_empty() && !rest.contains('\0');
    }
    let lower = key.to_ascii_lowercase();
    ALLOWED_TRANSACTION_SETTINGS.contains(&lower.as_str())
}

async fn apply_transaction_settings(tx: &Arc<TxHandle>, settings: &DictMap) -> Result<(), VmError> {
    for (key, value) in settings {
        if !is_allowed_transaction_setting(key) {
            return Err(runtime_error(format!(
                "pg_transaction: setting `{key}` is not permitted; allowed settings are \
                 `app.*` keys and the timeouts {ALLOWED_TRANSACTION_SETTINGS:?}. Privileged \
                 GUCs such as `role`, `session_authorization`, `is_superuser` and \
                 `search_path` are rejected because they could bypass row-level security."
            )));
        }
        // A `nil` settings value would otherwise be stringified to the literal
        // `"nil"` (set_config takes text), silently setting the GUC to a bogus
        // value instead of resetting it. That is never what a caller means, so
        // reject it explicitly (M-3).
        if matches!(value, VmValue::Nil) {
            return Err(runtime_error(format!(
                "pg_transaction: setting `{key}` has a nil value; provide a string/number \
                 value (nil would be set as the literal text \"nil\", not a reset)"
            )));
        }
        let params = vec![
            VmValue::string(key.as_str()),
            VmValue::string(value.display()),
        ];
        let sql = "select set_config($1, $2, true)";
        let mut tx = tx.cell.lock().await;
        let tx = tx
            .as_mut()
            .ok_or_else(|| runtime_error("pg_transaction: transaction is closed"))?;
        bind_params(query(sql), &params)?
            .execute(&mut **tx)
            .await
            .map_err(|error| {
                runtime_error(format!("pg_transaction: set_config({key}) failed: {error}"))
            })?;
    }
    Ok(())
}

fn query_result_value(result: PgQueryResult, duration: std::time::Duration) -> VmValue {
    execute_result_value(result.rows_affected(), duration)
}

fn execute_result_value(rows_affected: u64, duration: std::time::Duration) -> VmValue {
    let mut map = DictMap::new();
    map.put("rows_affected", VmValue::Int(rows_affected as i64));
    map.put("duration_ms", VmValue::Int(duration.as_millis() as i64));
    VmValue::dict(map)
}

async fn resolve_connection_url(
    ctx: &AsyncBuiltinCtx,
    source: &VmValue,
) -> Result<String, VmError> {
    match source {
        VmValue::Dict(dict) => {
            if let Some(url) = dict.get("url") {
                return match url {
                    VmValue::String(url) if !url.trim().is_empty() => Ok(url.to_string()),
                    _ => Err(runtime_error("pg_pool: url must be a non-empty string")),
                };
            }
            if let Some(env) = dict.get("env") {
                return env_url(&env.display(), "pg_pool");
            }
            if let Some(secret) = dict.get("secret") {
                return secret_url(ctx, &secret.display()).await;
            }
            Err(runtime_error(
                "pg_pool: connection dict must contain url, env, or secret",
            ))
        }
        VmValue::String(text) => {
            let text = text.trim();
            if let Some(name) = text.strip_prefix("env:") {
                env_url(name, "pg_pool")
            } else if let Some(id) = text.strip_prefix("secret:") {
                secret_url(ctx, id).await
            } else {
                Ok(text.to_string())
            }
        }
        _ => Err(runtime_error(
            "pg_pool: connection source must be a string or dict",
        )),
    }
}

fn env_url(name: &str, builtin: &str) -> Result<String, VmError> {
    std::env::var(name.trim()).map_err(|_| {
        runtime_error(format!(
            "{builtin}: environment variable `{}` is not set",
            name.trim()
        ))
    })
}

async fn secret_url(ctx: &AsyncBuiltinCtx, secret_id: &str) -> Result<String, VmError> {
    let mut child_vm = ctx.child_vm();
    let value = child_vm
        .call_named_builtin(
            "secret_get",
            vec![VmValue::string(secret_id.trim().to_string())],
        )
        .await?;
    ctx.forward_output(&child_vm.take_output());
    match value {
        VmValue::String(value) if !value.trim().is_empty() => Ok(value.to_string()),
        _ => Err(runtime_error(
            "pg_pool: secret value must be a non-empty UTF-8 string",
        )),
    }
}

fn parse_ssl_mode(mode: &str) -> Result<PgSslMode, VmError> {
    match mode.trim().to_ascii_lowercase().as_str() {
        "disable" | "disabled" => Ok(PgSslMode::Disable),
        "prefer" => Ok(PgSslMode::Prefer),
        "require" | "required" => Ok(PgSslMode::Require),
        "verify-ca" | "verify_ca" => Ok(PgSslMode::VerifyCa),
        "verify-full" | "verify_full" => Ok(PgSslMode::VerifyFull),
        other => Err(runtime_error(format!(
            "pg_pool: unsupported ssl_mode `{other}`"
        ))),
    }
}

pub(super) fn pool_record_from_handle(
    value: &VmValue,
    builtin: &str,
) -> Result<Arc<PoolRecord>, VmError> {
    let handle = pool_handle(Some(value), builtin)?;
    if handle.closed.load(Ordering::SeqCst) {
        return Err(runtime_error(format!("{builtin}: pool is closed")));
    }
    Ok(Arc::clone(&handle.record))
}

/// First-arg extractor for builtins that operate on the primary pool:
/// validates that args\[0\] is a `pg_pool` handle and returns the live
/// connection pool. Replaces the
/// `required_arg + handle_id + pool_by_id` preamble across every
/// introspection / partition / lock builtin.
///
/// `handle_id` already validates the handle kind, so we don't need
/// `ensure_handle_kind` ahead of it — that helper exists for callers
/// that want a kind check without also reading the id.
pub(super) fn pool_arg(args: &[VmValue], builtin: &'static str) -> Result<Arc<PgPool>, VmError> {
    let handle = required_arg(args, 0, builtin, "pool handle")?;
    pool_record_from_handle(handle, builtin).map(|record| Arc::clone(&record.pool))
}

pub(super) fn pool_handle(
    value: Option<&VmValue>,
    builtin: &str,
) -> Result<Arc<PoolHandle>, VmError> {
    typed_handle(value, HANDLE_POOL, builtin, "Postgres pool")
}

pub(super) fn tx_handle(value: Option<&VmValue>, builtin: &str) -> Result<Arc<TxHandle>, VmError> {
    typed_handle(value, HANDLE_TX, builtin, "Postgres transaction")
}

fn mock_handle(value: Option<&VmValue>, builtin: &str) -> Result<Arc<SyncMutex<MockDb>>, VmError> {
    typed_handle(value, HANDLE_MOCK, builtin, "Postgres mock")
}

pub(super) fn required_arg<'a>(
    args: &'a [VmValue],
    index: usize,
    builtin: &str,
    label: &str,
) -> Result<&'a VmValue, VmError> {
    args.get(index)
        .ok_or_else(|| runtime_error(format!("{builtin}: {label} is required")))
}

fn required_string_arg(
    args: &[VmValue],
    index: usize,
    builtin: &str,
    label: &str,
) -> Result<String, VmError> {
    Args::runtime(builtin, args)
        .non_empty_string(index, label)
        .map(str::to_string)
}

/// Tenant namespace used by `pg_advisory_*lock(... {tenant_namespace: true})`
/// to XOR salt into lock keys. Reads the harness-tenant scope (same
/// source `emit_channel` consults) so two tenants colliding on the same
/// numeric key see distinct lock-key pairs server-side.
pub(super) fn current_tenant_namespace() -> String {
    harn_vm::harness_tenant::current_tenant_id()
        .map(|t| t.0)
        .unwrap_or_default()
}

/// Selects the primary or a replica for a query according to the explicit
/// query route or the pool's read-only routing policy.
pub(super) fn pool_for_routing(
    record: &Arc<PoolRecord>,
    routing: QueryRouting,
    builtin: &'static str,
) -> Result<Arc<PgPool>, VmError> {
    let policy = match routing {
        QueryRouting::Primary => return Ok(Arc::clone(&record.pool)),
        QueryRouting::ReadOnly => record.read_routing_policy,
        QueryRouting::Policy(policy) => policy,
    };
    let pool = match policy {
        ReadRoutingPolicy::Primary => Arc::clone(&record.pool),
        ReadRoutingPolicy::Replica => record
            .replicas
            .first()
            .cloned()
            .ok_or_else(|| no_replica_error(builtin, policy))?,
        ReadRoutingPolicy::ReplicaOrPrimary => {
            next_replica(record).unwrap_or_else(|| Arc::clone(&record.pool))
        }
        ReadRoutingPolicy::RoundRobinReplica => {
            next_replica(record).ok_or_else(|| no_replica_error(builtin, policy))?
        }
    };
    Ok(pool)
}

fn next_replica(record: &Arc<PoolRecord>) -> Option<Arc<PgPool>> {
    if record.replicas.is_empty() {
        None
    } else {
        let idx = record.replica_cursor.fetch_add(1, Ordering::Relaxed) % record.replicas.len();
        Some(Arc::clone(&record.replicas[idx]))
    }
}

fn no_replica_error(builtin: &'static str, policy: ReadRoutingPolicy) -> VmError {
    runtime_error(format!(
        "{builtin}: read routing policy `{}` requires at least one replica",
        policy.as_str()
    ))
}

/// Per-query routing policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum QueryRouting {
    Primary,
    ReadOnly,
    Policy(ReadRoutingPolicy),
}

/// Pool-level policy used when a query opts into read-only routing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ReadRoutingPolicy {
    Primary,
    Replica,
    ReplicaOrPrimary,
    RoundRobinReplica,
}

impl ReadRoutingPolicy {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            ReadRoutingPolicy::Primary => "primary",
            ReadRoutingPolicy::Replica => "replica",
            ReadRoutingPolicy::ReplicaOrPrimary => "replica_or_primary",
            ReadRoutingPolicy::RoundRobinReplica => "round_robin_replica",
        }
    }
}

fn read_routing_policy_from_options(
    options: Option<&DictMap>,
) -> Result<ReadRoutingPolicy, VmError> {
    Ok(parse_read_routing_policy(
        options
            .and_then(|opts| opts.get("read_routing_policy"))
            .or_else(|| options.and_then(|opts| opts.get("routing_policy"))),
        "pg_pool",
    )?
    .unwrap_or(ReadRoutingPolicy::ReplicaOrPrimary))
}

fn query_routing_policy_from_options(
    options: Option<&DictMap>,
) -> Result<Option<ReadRoutingPolicy>, VmError> {
    parse_read_routing_policy(
        options
            .and_then(|opts| opts.get("read_routing_policy"))
            .or_else(|| options.and_then(|opts| opts.get("routing_policy")))
            .or_else(|| options.and_then(|opts| opts.get("route"))),
        "pg_query",
    )
}

fn parse_read_routing_policy(
    value: Option<&VmValue>,
    builtin: &'static str,
) -> Result<Option<ReadRoutingPolicy>, VmError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let text = value.display();
    let policy = match text.trim() {
        "" => return Ok(None),
        "primary" => ReadRoutingPolicy::Primary,
        "replica" => ReadRoutingPolicy::Replica,
        "replica_or_primary" => ReadRoutingPolicy::ReplicaOrPrimary,
        "round_robin_replica" => ReadRoutingPolicy::RoundRobinReplica,
        other => {
            return Err(runtime_error(format!(
                "{builtin}: unsupported read routing policy `{other}`"
            )))
        }
    };
    Ok(Some(policy))
}

pub(super) fn routing_from_options(options: Option<&DictMap>) -> Result<QueryRouting, VmError> {
    if let Some(policy) = query_routing_policy_from_options(options)? {
        Ok(QueryRouting::Policy(policy))
    } else if option_bool(options.and_then(|opts| opts.get("read_only"))) == Some(true) {
        Ok(QueryRouting::ReadOnly)
    } else {
        Ok(QueryRouting::Primary)
    }
}

/// Returns `(probe, ())` where `probe` is true if the call is a half-open
/// probe. Errors fast when the circuit is open.
pub(super) fn enter_circuit(
    circuit: &CircuitBreakerState,
    builtin: &str,
) -> Result<(bool, ()), VmError> {
    match circuit.admit() {
        Allow::Closed => Ok((false, ())),
        Allow::Probe => Ok((true, ())),
        Allow::Open => Err(runtime_error(format!(
            "{builtin}: circuit open — pool is throttling after repeated failures"
        ))),
    }
}

pub(super) fn params_arg(value: Option<&VmValue>, builtin: &str) -> Result<Vec<VmValue>, VmError> {
    Ok(Args::single(builtin, ErrorKind::Runtime, value)
        .opt_list(0, "params")?
        .unwrap_or_default()
        .to_vec())
}

/// A string connection option, or `None` when absent.
///
/// Non-string values are ignored rather than rendered: `display` would have
/// turned a dict into its own text and put that in the connection string.
fn option_string(options: Option<&DictMap>, key: &str) -> Option<String> {
    match options?.get(key)? {
        VmValue::String(text) if !text.trim().is_empty() => Some(text.trim().to_string()),
        _ => None,
    }
}

fn option_duration_ms(options: Option<&DictMap>, key: &str) -> Option<u64> {
    let reader = Args::single("postgres", ErrorKind::Runtime, options?.get(key));
    reader.millis(0, key).ok()
}

fn mock_query(
    target: &VmValue,
    sql: &str,
    params: &[VmValue],
    execute: bool,
) -> Result<Vec<VmValue>, VmError> {
    let mock = mock_handle(Some(target), "pg_mock")?;
    // Internal scaffold: `execute_stmt` only reads `rows_affected` out of
    // this synthetic row, then constructs its own dict with the real
    // call-site duration. The placeholder duration never surfaces.
    crate::sql_common::mock_query(&mock, "pg_mock", sql, params, execute, |rows| {
        execute_result_value(rows, std::time::Duration::ZERO)
    })
}

#[cfg(test)]
mod tests;
