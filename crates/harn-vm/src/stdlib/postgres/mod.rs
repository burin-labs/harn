use crate::value::VmDictExt;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::ops::Bound;
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use sqlx_core::column::Column;
use sqlx_core::connection::Connection;
use sqlx_core::executor::Executor;
use sqlx_core::query::{query, Query};
use sqlx_core::row::Row;
use sqlx_core::sql_str::AssertSqlSafe;
use sqlx_core::transaction::Transaction;
use sqlx_core::type_info::TypeInfo;
use sqlx_core::value::ValueRef;
use sqlx_postgres::{
    PgArguments, PgConnectOptions, PgPool, PgPoolOptions, PgQueryResult, PgRow, PgSslMode,
    PgTypeInfo, Postgres,
};
use tokio::sync::Mutex;

use crate::llm::vm_value_to_json;
use crate::stdlib::macros::{
    harn_builtin, BuiltinSignature, Param, VmBuiltinDef, TY_ANY, TY_BOOL, TY_DICT, TY_LIST,
};
use crate::stdlib::options::{non_negative_millis_from_value, ErrorKind};
use crate::value::{VmError, VmValue};
use crate::vm::Vm;

use self::circuit::{Allow, CircuitBreakerState};

pub(super) const HANDLE_POOL: &str = "pg_pool";
pub(super) const HANDLE_TX: &str = "pg_tx";
pub(super) const HANDLE_MOCK: &str = "pg_mock_pool";

const DEFAULT_STATEMENT_CACHE_CAPACITY: usize = 100;

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

pub(super) struct PoolRecord {
    pub(super) pool: Arc<PgPool>,
    pub(super) replicas: Vec<Arc<PgPool>>,
    pub(super) replica_cursor: AtomicUsize,
    pub(super) max_connections: u32,
    pub(super) statement_cache_capacity: usize,
    pub(super) read_routing_policy: ReadRoutingPolicy,
    pub(super) circuit: Arc<CircuitBreakerState>,
}

#[derive(Clone)]
struct MockFixture {
    sql: String,
    params: Option<serde_json::Value>,
    rows: Vec<VmValue>,
    rows_affected: u64,
    error: Option<String>,
}

#[derive(Default, Clone)]
struct MockPool {
    fixtures: Vec<MockFixture>,
    calls: Vec<VmValue>,
}

type PgTxCell = Arc<Mutex<Option<Transaction<'static, Postgres>>>>;
type PgTxRegistry = BTreeMap<String, PgTxCell>;

thread_local! {
    static POOLS: RefCell<BTreeMap<String, Arc<PoolRecord>>> = const { RefCell::new(std::collections::BTreeMap::new()) };
    static TXS: RefCell<PgTxRegistry> =
        const { RefCell::new(std::collections::BTreeMap::new()) };
    static MOCKS: RefCell<BTreeMap<String, MockPool>> = const { RefCell::new(std::collections::BTreeMap::new()) };
    /// Server-described per-slot parameter OIDs, keyed by the SQL string.
    ///
    /// Postgres infers every `$n` slot's type from the query *structure* (casts,
    /// target columns, operators) — independent of which params are `nil` at
    /// runtime — so the described OID list is stable per SQL string and can be
    /// cached and reused for all future nil-bearing executions of that SQL. This
    /// turns the describe round-trip into a one-time cost per distinct SQL rather
    /// than a per-query cost (see [`described_param_oids`]). Scoped thread-local
    /// to match the `POOLS`/`TXS`/`MOCKS` registries above (the harn VM runs on a
    /// current-thread runtime).
    static DESCRIBED_OIDS: RefCell<BTreeMap<String, Arc<Vec<PgTypeInfo>>>> =
        const { RefCell::new(std::collections::BTreeMap::new()) };
}

// Counts how many times an *uncached* server describe round-trip is performed.
// Used by tests to assert that a repeated nil-query of the same SQL hits the OID
// cache and does **not** re-describe. Lives behind `cfg(test)` so it has zero
// cost in release builds.
//
// Thread-local (not a process-global atomic): cargo runs the postgres tests in
// parallel against one live database, and several of them perform describes. A
// process-global counter would race — one test's describe would inflate
// another's absolute count. A per-thread counter, combined with the
// `current_thread` runtime the counting test uses, isolates each test's own
// describe count so the assertions are deterministic.
#[cfg(test)]
thread_local! {
    static DESCRIBE_ROUND_TRIPS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn describe_round_trips() -> u64 {
    DESCRIBE_ROUND_TRIPS.with(std::cell::Cell::get)
}

#[cfg(test)]
fn reset_describe_round_trips() {
    DESCRIBE_ROUND_TRIPS.with(|c| c.set(0));
}

#[cfg(test)]
fn bump_describe_round_trips() {
    DESCRIBE_ROUND_TRIPS.with(|c| c.set(c.get() + 1));
}

pub(crate) fn reset_postgres_state() {
    POOLS.with(|pools| pools.borrow_mut().clear());
    TXS.with(|txs| txs.borrow_mut().clear());
    MOCKS.with(|mocks| mocks.borrow_mut().clear());
    DESCRIBED_OIDS.with(|oids| oids.borrow_mut().clear());
    listen::reset_state();
}

pub(crate) fn register_postgres_builtins(vm: &mut Vm) {
    for def in MODULE_BUILTINS {
        vm.register_builtin_def(def);
    }
    register_postgres_namespace(vm);
}

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
        VmValue::dict(crate::value::DictMap::from_iter([
            ("_namespace".to_string(), VmValue::String(Arc::from("pg"))),
            ("jsonb".to_string(), jsonb),
        ])),
    );
}

fn namespace(name: &str, entries: &[(&str, &str)]) -> VmValue {
    VmValue::dict(
        std::iter::once((
            "_namespace".to_string(),
            VmValue::String(Arc::from(name.to_string())),
        ))
        .chain(entries.iter().map(|(field, builtin)| {
            (
                (*field).to_string(),
                VmValue::BuiltinRef(Arc::from(*builtin)),
            )
        }))
        .collect::<BTreeMap<_, _>>(),
    )
}

pub(crate) const MODULE_BUILTINS: &[&VmBuiltinDef] = &[
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

mod advisory;
mod circuit;
mod introspect;
mod jsonb;
mod listen;
mod migrate;
mod shared;

pub use shared::install_shared_pool_registry;

#[harn_builtin(
    sig_expr = BuiltinSignature::variadic("pg_pool", &[Param::new("args", TY_ANY)], TY_DICT),
    kind = "async",
    category = "postgres"
)]
async fn pg_pool_impl(
    ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let source = args.first().ok_or_else(|| {
        runtime_error("pg_pool: url, env:, secret:, or {url|env|secret} is required")
    })?;
    let options = args.get(1).and_then(VmValue::as_dict).cloned();
    open_pool(&ctx, source, options.as_ref(), false).await
}

#[harn_builtin(
    sig_expr = BuiltinSignature::variadic("pg_connect", &[Param::new("args", TY_ANY)], TY_DICT),
    kind = "async",
    category = "postgres"
)]
async fn pg_connect_impl(
    ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let source = args.first().ok_or_else(|| {
        runtime_error("pg_connect: url, env:, secret:, or {url|env|secret} is required")
    })?;
    let options = args.get(1).and_then(VmValue::as_dict).cloned();
    open_pool(&ctx, source, options.as_ref(), true).await
}

#[harn_builtin(
    sig_expr = BuiltinSignature::variadic("pg_close", &[Param::new("args", TY_ANY)], TY_BOOL),
    kind = "async",
    category = "postgres"
)]
async fn pg_close_impl(
    _ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let id = handle_id(args.first(), HANDLE_POOL, "pg_close")?;
    let removed = POOLS.with(|pools| pools.borrow_mut().remove(&id));
    if let Some(record) = removed {
        record.pool.close().await;
        for replica in &record.replicas {
            replica.close().await;
        }
        Ok(VmValue::Bool(true))
    } else {
        Ok(VmValue::Bool(false))
    }
}

#[harn_builtin(
    sig_expr = BuiltinSignature::variadic("pg_stmt_cache_clear", &[Param::new("args", TY_ANY)], TY_DICT),
    kind = "async",
    category = "postgres"
)]
async fn pg_stmt_cache_clear_impl(
    _ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let target = required_arg(&args, 0, "pg_stmt_cache_clear", "pool handle")?;
    if handle_kind(target).as_deref() == Some(HANDLE_MOCK) {
        handle_id(Some(target), HANDLE_MOCK, "pg_stmt_cache_clear")?;
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
    let mut result = crate::value::DictMap::new();
    result.insert("pools".to_string(), VmValue::Int(pools));
    result.insert(
        "connections_cleared".to_string(),
        VmValue::Int(connections_cleared),
    );
    result.insert(
        "connections_skipped".to_string(),
        VmValue::Int(connections_skipped),
    );
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
/// server-side `Close`/`DEALLOCATE`), and clear the thread-local `DESCRIBED_OIDS`
/// cache (a DDL can change the server-inferred type of a `$n` slot).
///
/// Best-effort and non-fatal: the migration already committed, so a clear
/// failure is logged and the run still reports success. The `acquire` is bounded
/// by `max` and each is immediately released, so this never starves the pool. A
/// connection still checked out by another task at recycle time is not drained
/// here; its first post-DDL reuse would hit `0A000` once and sqlx re-prepares —
/// the same self-healing sqlx already does for a plan invalidated out of band.
pub(super) async fn recycle_pool_after_ddl(pool: &PgPool, max: u32) {
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
    DESCRIBED_OIDS.with(|oids| oids.borrow_mut().clear());
}

#[harn_builtin(
    sig_expr = BuiltinSignature::variadic("pg_query", &[Param::new("args", TY_ANY)], TY_LIST),
    kind = "async",
    category = "postgres"
)]
async fn pg_query_impl(
    _ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
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
    sig_expr = BuiltinSignature::variadic("pg_query_one", &[Param::new("args", TY_ANY)], TY_ANY),
    kind = "async",
    category = "postgres"
)]
async fn pg_query_one_impl(
    _ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
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
    sig_expr = BuiltinSignature::variadic("pg_execute", &[Param::new("args", TY_ANY)], TY_DICT),
    kind = "async",
    category = "postgres"
)]
async fn pg_execute_impl(
    _ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let target = args
        .first()
        .ok_or_else(|| runtime_error("pg_execute: pool or transaction handle is required"))?;
    let sql = required_string_arg(&args, 1, "pg_execute", "sql")?;
    let params = params_arg(args.get(2), "pg_execute")?;
    execute_stmt(target, &sql, &params).await
}

#[harn_builtin(
    sig_expr = BuiltinSignature::variadic("pg_transaction", &[Param::new("args", TY_ANY)], TY_ANY),
    kind = "async",
    category = "postgres"
)]
async fn pg_transaction_impl(
    ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let pool_id = handle_id(args.first(), HANDLE_POOL, "pg_transaction")?;
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
    run_managed_transaction(&ctx, &pool_id, "pg_transaction", closure, move |tx_id| {
        let tx_id = tx_id.to_string();
        Box::pin(async move {
            if let Some(settings) = settings {
                apply_transaction_settings(&tx_id, &settings).await?;
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
    ctx: &crate::vm::AsyncBuiltinCtx,
    pool_id: &str,
    builtin: &'static str,
    closure: Arc<crate::value::VmClosure>,
    prepare: impl FnOnce(
        &str,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<(), VmError>> + Send + '_>,
    >,
) -> Result<VmValue, VmError> {
    let pool = pool_by_id(pool_id)?;
    let tx = pool
        .begin()
        .await
        .map_err(|error| runtime_error(format!("{builtin}: begin failed: {error}")))?;
    let tx_id = next_id("pgtx");
    let tx_cell = Arc::new(Mutex::new(Some(tx)));
    register_tx(&tx_id, Arc::clone(&tx_cell));
    let tx_handle = handle_value(HANDLE_TX, &tx_id, crate::value::DictMap::new());

    if let Err(error) = prepare(&tx_id).await {
        unregister_tx(&tx_id);
        if let Some(tx) = tx_cell.lock().await.take() {
            let _ = tx.rollback().await;
        }
        return Err(error);
    }

    let mut child_vm = ctx.child_vm();
    let result = child_vm.call_closure_pub(&closure, &[tx_handle]).await;
    ctx.forward_output(&child_vm.take_output());

    unregister_tx(&tx_id);
    let tx = tx_cell
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
    sig_expr = BuiltinSignature::variadic("pg_savepoint", &[Param::new("args", TY_ANY)], TY_BOOL),
    kind = "async",
    category = "postgres"
)]
async fn pg_savepoint_impl(
    _ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    savepoint_op(&args, "pg_savepoint", SavepointOp::Create).await
}

#[harn_builtin(
    sig_expr = BuiltinSignature::variadic("pg_release_savepoint", &[Param::new("args", TY_ANY)], TY_BOOL),
    kind = "async",
    category = "postgres"
)]
async fn pg_release_savepoint_impl(
    _ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    savepoint_op(&args, "pg_release_savepoint", SavepointOp::Release).await
}

#[harn_builtin(
    sig_expr = BuiltinSignature::variadic("pg_rollback_to_savepoint", &[Param::new("args", TY_ANY)], TY_BOOL),
    kind = "async",
    category = "postgres"
)]
async fn pg_rollback_to_savepoint_impl(
    _ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    savepoint_op(&args, "pg_rollback_to_savepoint", SavepointOp::RollbackTo).await
}

#[harn_builtin(
    sig_expr = BuiltinSignature::variadic("pg_migrate", &[Param::new("args", TY_ANY)], TY_DICT),
    kind = "async",
    category = "postgres"
)]
async fn pg_migrate_impl(
    _ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    migrate::run(args).await
}

#[harn_builtin(
    sig_expr = BuiltinSignature::variadic("pg_mock_pool", &[Param::new("args", TY_ANY)], TY_DICT),
    category = "postgres"
)]
fn pg_mock_pool_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let fixtures = match args.first() {
        Some(VmValue::List(items)) => parse_mock_fixtures(items)?,
        Some(VmValue::Dict(_)) => parse_mock_fixtures(std::slice::from_ref(&args[0]))?,
        None | Some(VmValue::Nil) => Vec::new(),
        _ => {
            return Err(runtime_error(
                "pg_mock_pool: fixtures must be a list of dicts",
            ))
        }
    };
    let id = next_id("pgmock");
    MOCKS.with(|mocks| {
        mocks.borrow_mut().insert(
            id.clone(),
            MockPool {
                fixtures,
                calls: Vec::new(),
            },
        );
    });
    Ok(handle_value(HANDLE_MOCK, &id, crate::value::DictMap::new()))
}

#[harn_builtin(
    sig_expr = BuiltinSignature::variadic("pg_mock_calls", &[Param::new("args", TY_ANY)], TY_LIST),
    category = "postgres"
)]
fn pg_mock_calls_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let id = handle_id(args.first(), HANDLE_MOCK, "pg_mock_calls")?;
    let calls = MOCKS.with(|mocks| {
        mocks
            .borrow()
            .get(&id)
            .map(|mock| mock.calls.clone())
            .unwrap_or_default()
    });
    Ok(VmValue::List(std::sync::Arc::new(calls)))
}

async fn open_pool(
    ctx: &crate::vm::AsyncBuiltinCtx,
    source: &VmValue,
    options: Option<&crate::value::DictMap>,
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
        .then(|| shared::PoolKey::new(&primary_url, &replica_urls, options, single_connection));
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

/// Register `record` in the thread-local [`POOLS`] registry under a fresh
/// opaque id and build the `pg_pool` handle the VM hands back to `.harn` code.
///
/// The thread-local map is still the per-request lookup table that the query
/// builtins consult by handle id; sharing happens at the [`PoolRecord`] `Arc`
/// level, so two requests get distinct ids that resolve to the *same* underlying
/// pool. The handle metadata is derived from the (possibly shared) record so it
/// always reflects the live pool rather than this caller's requested options.
fn register_local_pool_handle(
    record: Arc<PoolRecord>,
    single_connection: bool,
    application_name: Option<String>,
) -> VmValue {
    let id = next_id(if single_connection {
        "pgconn"
    } else {
        "pgpool"
    });
    let mut meta = crate::value::DictMap::new();
    meta.insert(
        "max_connections".to_string(),
        VmValue::Int(i64::from(record.max_connections)),
    );
    meta.insert(
        "single_connection".to_string(),
        VmValue::Bool(single_connection),
    );
    meta.insert(
        "replicas".to_string(),
        VmValue::Int(record.replicas.len() as i64),
    );
    meta.insert(
        "statement_cache_capacity".to_string(),
        VmValue::Int(record.statement_cache_capacity as i64),
    );
    meta.put_str("read_routing_policy", record.read_routing_policy.as_str());
    if let Some(application_name) = application_name {
        meta.put_str("application_name", application_name);
    }
    POOLS.with(|pools| {
        pools.borrow_mut().insert(id.clone(), record);
    });
    handle_value(HANDLE_POOL, &id, meta)
}

async fn build_pool(
    url: &str,
    options: Option<&crate::value::DictMap>,
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
    ctx: &crate::vm::AsyncBuiltinCtx,
    options: Option<&crate::value::DictMap>,
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

fn build_circuit_breaker(options: Option<&crate::value::DictMap>) -> CircuitBreakerState {
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
    Ok(VmValue::List(std::sync::Arc::new(rows)))
}

pub(super) async fn query_rows(
    target: &VmValue,
    sql: &str,
    params: &[VmValue],
    routing: QueryRouting,
) -> Result<Vec<VmValue>, VmError> {
    crate::call_budget::charge_pg_query()?;
    match handle_kind(target).as_deref() {
        Some(HANDLE_MOCK) => return mock_query(target, sql, params, false),
        Some(HANDLE_TX) => {
            let id = handle_id(Some(target), HANDLE_TX, "pg_query")?;
            let tx = tx_by_id(&id)?;
            let mut tx = tx.lock().await;
            let tx = tx
                .as_mut()
                .ok_or_else(|| runtime_error("pg_query: transaction is closed"))?;
            let rows = if params_have_nil(params) {
                // describe-then-bind: learn the server-inferred parameter OIDs
                // on the tx connection (cached per SQL), then bind typed NULLs
                // for the nils.
                let oids = described_param_oids(tx, sql, "pg_query", true).await?;
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
        run_described_query(&pool, sql, params, "pg_query", |q, conn| {
            Box::pin(async move { q.fetch_all(conn).await })
        })
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
    crate::call_budget::charge_pg_query()?;
    let started = std::time::Instant::now();
    if handle_kind(target).as_deref() == Some(HANDLE_MOCK) {
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
    if handle_kind(target).as_deref() == Some(HANDLE_TX) {
        let id = handle_id(Some(target), HANDLE_TX, "pg_execute")?;
        let tx = tx_by_id(&id)?;
        let mut tx = tx.lock().await;
        let tx = tx
            .as_mut()
            .ok_or_else(|| runtime_error("pg_execute: transaction is closed"))?;
        let result = if params_have_nil(params) {
            // describe-then-bind: learn the server-inferred parameter OIDs on
            // the tx connection (cached per SQL), then bind typed NULLs for the
            // nils.
            let oids = described_param_oids(tx, sql, "pg_execute", true).await?;
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
        run_described_query(&record.pool, sql, params, "pg_execute", |q, conn| {
            Box::pin(async move { q.execute(conn).await })
        })
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

#[derive(Clone, Copy)]
enum SavepointOp {
    Create,
    Release,
    RollbackTo,
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
    if handle_kind(target).as_deref() == Some(HANDLE_MOCK) {
        let sql = render_savepoint_sql(op, &name);
        let _ = mock_query(target, &sql, &[], true);
        return Ok(VmValue::Bool(true));
    }
    let id = handle_id(Some(target), HANDLE_TX, builtin)?;
    let tx = tx_by_id(&id)?;
    let mut tx = tx.lock().await;
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

fn render_savepoint_sql(op: SavepointOp, name: &str) -> String {
    let quoted = format!("\"{}\"", name.replace('"', "\"\""));
    match op {
        SavepointOp::Create => format!("SAVEPOINT {quoted}"),
        SavepointOp::Release => format!("RELEASE SAVEPOINT {quoted}"),
        SavepointOp::RollbackTo => format!("ROLLBACK TO SAVEPOINT {quoted}"),
    }
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

async fn apply_transaction_settings(
    tx_id: &str,
    settings: &crate::value::DictMap,
) -> Result<(), VmError> {
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
            VmValue::String(std::sync::Arc::from(key.as_str())),
            VmValue::String(std::sync::Arc::from(value.display())),
        ];
        let sql = "select set_config($1, $2, true)";
        let tx = tx_by_id(tx_id)?;
        let mut tx = tx.lock().await;
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

/// Error message for a non-finite float that can't be safely bound to Postgres.
fn non_finite_float_error() -> VmError {
    runtime_error(
        "pg bind: non-finite float (NaN/Infinity) cannot be bound to a Postgres parameter",
    )
}

/// Reject a non-finite `VmValue::Float` anywhere inside a value that will be
/// bound. Direct `float8` binds of NaN/Infinity are legal for the column type
/// but corrupt any downstream JSON, and a non-finite float that falls through
/// to the jsonb (`vm_value_to_json`) path serializes to a JSON `null`
/// (serde_json's representation of non-finite f64), silently dropping the
/// value. We reject both up front with a clear error instead.
fn reject_non_finite_floats(value: &VmValue) -> Result<(), VmError> {
    match value {
        VmValue::Float(f) if !f.is_finite() => Err(non_finite_float_error()),
        VmValue::List(list) => list.iter().try_for_each(reject_non_finite_floats),
        VmValue::Dict(dict) => dict.values().try_for_each(reject_non_finite_floats),
        VmValue::StructInstance { .. } => value
            .struct_fields_map()
            .unwrap_or_default()
            .values()
            .try_for_each(reject_non_finite_floats),
        _ => Ok(()),
    }
}

/// A SQL `NULL` that declares a *specific* Postgres type OID.
///
/// A dynamic `nil` from harn has no static Rust type, so we cannot pick a
/// sqlx `Type` for it the way every other `VmValue` does. Binding it as
/// `None::<String>` declares the parameter as `text` (OID 25), which is wrong
/// in two ways:
///
///   * it poisons sqlx's *per-connection*, *SQL-keyed* prepared-statement
///     cache — the first execution caches `$n = text`, so a later execution
///     that binds a non-text value at `$n` sends binary data that no longer
///     matches the cached parameter type (→ `invalid byte sequence ... 0x00`);
///   * it fails outright against a non-text typed column / cast
///     (→ `column is of type integer but expression is of type text`).
///
/// `TypedNull` carries the *server-described* OID for that parameter slot (see
/// [`bind_params_described`]) so the NULL declares exactly the type a non-null
/// value at that slot would. `encode` writes no bytes and reports `IsNull::Yes`,
/// while `produces()` overrides the declared type to our chosen OID — matching
/// the cached statement's parameter list and eliminating both failure modes.
struct TypedNull(PgTypeInfo);

impl sqlx_core::types::Type<Postgres> for TypedNull {
    fn type_info() -> PgTypeInfo {
        // Fallback only; `produces()` overrides this for the actual bind.
        // `Void` (OID 2278) is the most neutral built-in. This is never the
        // declared type in practice because `produces()` always returns `Some`.
        PgTypeInfo::with_oid(sqlx_postgres::types::Oid(2278))
    }

    fn compatible(_ty: &PgTypeInfo) -> bool {
        true
    }
}

impl sqlx_core::encode::Encode<'_, Postgres> for TypedNull {
    fn encode_by_ref(
        &self,
        _buf: &mut <Postgres as sqlx_core::database::Database>::ArgumentBuffer,
    ) -> Result<sqlx_core::encode::IsNull, sqlx_core::error::BoxDynError> {
        // A NULL writes no payload bytes; the buffer machinery records the
        // `-1` length when it sees `IsNull::Yes`.
        Ok(sqlx_core::encode::IsNull::Yes)
    }

    fn produces(&self) -> Option<PgTypeInfo> {
        // This is the whole point: declare the parameter with the
        // server-described OID rather than the `Type::type_info()` fallback.
        Some(self.0.clone())
    }
}

/// True when any bound param is a dynamic `nil`. Only these queries need the
/// extra describe round-trip; all-non-null queries take the fast path
/// (`bind_params`) unchanged.
pub(super) fn params_have_nil(params: &[VmValue]) -> bool {
    params.iter().any(|p| matches!(p, VmValue::Nil))
}

/// Bind a single `VmValue` onto a `Query`. `nil_type`, when present, is the
/// server-described OID for *this* parameter slot and is used to bind a typed
/// `NULL` (see [`TypedNull`]); otherwise a `nil` falls back to the legacy
/// `None::<String>` (`text`) bind.
fn bind_one<'q>(
    query: Query<'q, Postgres, PgArguments>,
    param: &'q VmValue,
    nil_type: Option<&PgTypeInfo>,
) -> Result<Query<'q, Postgres, PgArguments>, VmError> {
    // Guard every param (including floats nested in the jsonb path) before it
    // reaches sqlx, so NaN/Infinity fail cleanly rather than corrupting a
    // float8 column or emitting invalid JSON.
    reject_non_finite_floats(param)?;
    Ok(match param {
        VmValue::Nil => match nil_type {
            Some(ty) => query.bind(TypedNull(ty.clone())),
            None => query.bind(None::<String>),
        },
        VmValue::Bool(value) => query.bind(*value),
        VmValue::Int(value) => query.bind(*value),
        VmValue::Float(value) => query.bind(*value),
        VmValue::String(value) => query.bind(value.to_string()),
        VmValue::Bytes(value) => query.bind((**value).clone()),
        VmValue::Duration(ms) => query.bind(*ms),
        // Bind exact decimals to NUMERIC/DECIMAL columns without going through
        // a lossy float — sqlx encodes `rust_decimal::Decimal` natively.
        VmValue::Decimal(value) => query.bind(*value),
        value => query.bind(sqlx_core::types::Json(vm_value_to_json(value))),
    })
}

pub(super) fn bind_params<'q>(
    mut query: Query<'q, Postgres, PgArguments>,
    params: &'q [VmValue],
) -> Result<Query<'q, Postgres, PgArguments>, VmError> {
    for param in params {
        query = bind_one(query, param, None)?;
    }
    Ok(query)
}

/// Build a query bound to `sql`, encoding any dynamic `nil` as a [`TypedNull`]
/// carrying the server-described OID for that slot (`described`).
///
/// Non-null params are bound by sqlx in their natural type/format, so the
/// resulting `PgArguments` declares, per slot, either sqlx's natural type
/// (non-null) or the described OID (nil). The exact per-slot type list therefore
/// depends on *which* slots are `nil` at runtime — and sqlx's prepared-statement
/// cache is keyed by the SQL string **alone**, so two different nil-patterns of
/// the same SQL would collide on a single cached statement (a non-null slot
/// reusing a sibling pattern's `NULL`-declared OID → `incorrect binary data
/// format in bind parameter N`). We therefore mark the described query
/// [`persistent(false)`], so in sqlx 0.9.0 it Parses an **unnamed** statement
/// and is **never inserted** into the SQL-keyed cache (verified in
/// `sqlx_postgres::connection::executor::{prepare, get_or_prepare}` — the cache
/// insert is gated on `persistent == true`, and a non-persistent prepare uses
/// `StatementId::UNNAMED`). Each described execute thus Parses exactly this
/// self-consistent type list (in the *same* network round-trip as the Bind +
/// Execute — sqlx pipelines Parse/Bind/Execute/Sync behind one flush),
/// eliminating both the binary-format mismatch (the OID-0 failure mode) and the
/// text-NULL failure modes, and it cannot poison sibling patterns or the
/// all-non-null fast path. A `nil` reusing an all-non-null cached statement is
/// harmless: a SQL `NULL` carries no payload bytes, so the declared OID is
/// irrelevant for it.
fn bind_params_described<'q>(
    sql: &'q str,
    described: &[PgTypeInfo],
    params: &'q [VmValue],
) -> Result<Query<'q, Postgres, PgArguments>, VmError> {
    let mut query = query(AssertSqlSafe(sql)).persistent(false);
    for (index, param) in params.iter().enumerate() {
        query = bind_one(query, param, described.get(index))?;
    }
    Ok(query)
}

/// Server-described per-slot parameter OIDs for `sql`, looked up from the
/// process-stable [`DESCRIBED_OIDS`] cache and computed on a miss.
///
/// The described OID list is a pure function of the SQL *structure* (Postgres
/// infers each `$n` from casts/target columns/operators, not from the runtime
/// param values), so it is stable per SQL string and never needs invalidation:
///
///   * **HIT** — return the cached `Arc<Vec<PgTypeInfo>>` with **no** describe
///     round-trip and **no** statement-cache clear. The caller then binds the
///     nils with these OIDs and executes via the *normal* path, which Parses its
///     own self-consistent type list and caches it under `sql` like any plain
///     query. After warmup, a nil-query therefore costs exactly the same
///     round-trips as a plain bind.
///   * **MISS** — perform a single describe via
///     [`describe_param_oids_uncached`] (one prepare + one cache clear), store
///     the result, and return it.
///
/// Schema changes that would alter a slot's inferred type are extremely rare and
/// are already handled by the execute path: Postgres raises `cached plan must
/// not change result type` / re-parse on its own, and sqlx surfaces it as a
/// query error — the cached OIDs only seed the NULL *declaration*, they do not
/// pin server-side plans.
///
/// `in_transaction` tells the describe probe whether `conn` is currently inside
/// a caller-owned transaction (the `HANDLE_TX` paths pass `true`; the
/// autocommit pool path passes `false`). It controls whether the probe wraps its
/// `prepare_with` in a `SAVEPOINT` so a failed probe cannot abort the caller's
/// transaction — see [`describe_param_oids_uncached`].
async fn described_param_oids(
    conn: &mut sqlx_postgres::PgConnection,
    sql: &str,
    builtin: &str,
    in_transaction: bool,
) -> Result<Arc<Vec<PgTypeInfo>>, VmError> {
    if let Some(cached) = DESCRIBED_OIDS.with(|oids| oids.borrow().get(sql).cloned()) {
        return Ok(cached);
    }
    let oids = Arc::new(describe_param_oids_uncached(conn, sql, builtin, in_transaction).await?);
    DESCRIBED_OIDS.with(|cache| {
        cache
            .borrow_mut()
            .insert(sql.to_string(), Arc::clone(&oids));
    });
    Ok(oids)
}

/// Perform the actual (uncached) server describe for `sql` on `conn`, returning
/// one [`PgTypeInfo`] per `$n`.
///
/// In sqlx 0.9.0, `prepare_with` is the only public way to obtain per-slot
/// parameter OIDs, and it *always* prepares persistently — it inserts the
/// *server-inferred* statement into the connection's SQL-keyed cache (see
/// `sqlx_postgres::connection::executor::get_or_prepare`, which gates the cache
/// insert on `persistent == true` and offers no per-key eviction). There is no
/// non-caching describe-only entry point: `Executor::describe` is gated behind
/// the `offline` feature (which harn does not enable) and is likewise
/// persistent. For a query whose non-null param's natural sqlx type differs from
/// the inferred column type (e.g. an `i64` against an `int4` column), that
/// inferred cache entry would poison a later execute, so we
/// [`clear_cached_statements`] once here. Because the result is cached by
/// [`described_param_oids`], this prepare+clear happens **at most once per
/// distinct SQL** (on the first nil-bearing execution); every subsequent
/// nil-query of that SQL is a pure cache hit with no describe and no clear.
///
/// **Best-effort, never worse than the legacy bind.** `prepare_with(sql, &[])`
/// forces Postgres to infer *every* `$n` from query structure alone, so a query
/// with a genuinely ambiguous slot (e.g. `could not determine data type of
/// parameter $3`) makes the probe itself fail — even though binding that nil as
/// a legacy `text` NULL would have worked. We therefore treat a probe failure as
/// "no per-slot OIDs available" rather than an error: we return an **empty**
/// `Vec`, which the bind path turns into the legacy `None::<String>` (text) NULL
/// for each nil — strictly ≥ the pre-describe behavior. The empty result is
/// cached by [`described_param_oids`] like any other (the failure is
/// deterministic per SQL structure, so re-probing every call would be wasted
/// round-trips).
///
/// Inside a caller transaction (`in_transaction`), a failed `prepare_with`
/// aborts the whole transaction (`current transaction is aborted`), which would
/// then break the caller's own statements. We guard the probe with `SAVEPOINT
/// _harn_describe_probe`: on success we `RELEASE` it and proceed normally; on
/// failure we `ROLLBACK TO` it, which discards the aborted sub-state and leaves
/// the transaction usable. Outside a transaction (autocommit pool conn) a
/// `SAVEPOINT` is itself an error, so we skip it and simply catch the prepare
/// failure. The savepoint round-trips are added **only** around a tx-path probe
/// — the all-non-null fast path and the successful-describe path are unchanged.
async fn describe_param_oids_uncached(
    conn: &mut sqlx_postgres::PgConnection,
    sql: &str,
    builtin: &str,
    in_transaction: bool,
) -> Result<Vec<PgTypeInfo>, VmError> {
    use sqlx_core::connection::Connection as _;
    use sqlx_core::sql_str::SqlSafeStr as _;
    use sqlx_core::statement::Statement as _;

    const SAVEPOINT: &str = "_harn_describe_probe";

    #[cfg(test)]
    bump_describe_round_trips();

    // In a caller transaction, a failed probe aborts the whole tx. A savepoint
    // lets us roll back *only* the failed probe and keep the tx alive. Outside a
    // tx, SAVEPOINT is an error, so we skip it.
    if in_transaction {
        conn.execute(AssertSqlSafe(format!("SAVEPOINT {SAVEPOINT}")))
            .await
            .map_err(|error| runtime_error(format!("{builtin}: savepoint failed: {error}")))?;
    }

    let prepared = conn
        .prepare_with(AssertSqlSafe(sql.to_string()).into_sql_str(), &[])
        .await;

    let stmt = match prepared {
        Ok(stmt) => stmt,
        Err(_) => {
            // The describe probe failed (e.g. an ambiguous `$n` Postgres cannot
            // infer from structure alone). Don't propagate — fall back to legacy
            // text NULLs by returning no per-slot OIDs. Inside a tx, undo the
            // aborted sub-state so the caller's transaction stays usable.
            if in_transaction {
                conn.execute(AssertSqlSafe(format!("ROLLBACK TO SAVEPOINT {SAVEPOINT}")))
                    .await
                    .map_err(|error| {
                        runtime_error(format!("{builtin}: rollback to savepoint failed: {error}"))
                    })?;
            }
            return Ok(Vec::new());
        }
    };

    let oids = match stmt.parameters() {
        Some(sqlx_core::Either::Left(types)) => types.to_vec(),
        // Count-only or no parameter metadata: no per-slot OIDs to apply; the
        // bind path will fall back to legacy text NULLs.
        _ => Vec::new(),
    };
    drop(stmt);

    if in_transaction {
        conn.execute(AssertSqlSafe(format!("RELEASE SAVEPOINT {SAVEPOINT}")))
            .await
            .map_err(|error| {
                runtime_error(format!("{builtin}: release savepoint failed: {error}"))
            })?;
    }

    conn.clear_cached_statements()
        .await
        .map_err(|error| runtime_error(format!("{builtin}: clear cache failed: {error}")))?;
    Ok(oids)
}

/// Run a nil-containing query against `pool` via the describe-then-bind path on
/// a single acquired pool connection.
///
/// On the first nil-bearing execution of `sql`, [`described_param_oids`] does one
/// describe (and one statement-cache clear) and caches the per-slot OIDs; the
/// describe-bound execute then re-parses with its own self-consistent type list
/// and the connection returns to the pool with the *correct* cached entry for
/// `sql`. Every subsequent nil-query of the same SQL is an OID-cache hit: no
/// describe, no clear — it binds the cached OIDs and executes via the normal
/// prepared-statement cache, so it costs the same round-trips as a plain bind
/// (and on a fresh pool connection it simply Parses+caches its self-consistent
/// statement once, exactly like any plain query would). This work happens
/// **only** when a `nil` is present; the all-non-null fast path never reaches
/// here and is fully unchanged. `run` performs the actual `fetch_all`/`execute`
/// and is generic over the result so both `pg_query` and `pg_execute` can share
/// this flow.
async fn run_described_query<T>(
    pool: &PgPool,
    sql: &str,
    params: &[VmValue],
    builtin: &str,
    run: impl for<'a> FnOnce(
        Query<'a, Postgres, PgArguments>,
        &'a mut sqlx_postgres::PgConnection,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<T, sqlx_core::error::Error>> + Send + 'a>,
    >,
) -> Result<T, VmError> {
    let mut conn = pool
        .acquire()
        .await
        .map_err(|error| runtime_error(format!("{builtin}: {error}")))?;
    // Autocommit pool connection: not inside a caller transaction, so no
    // savepoint (it would error outside a tx) — the probe just catches its own
    // failure and falls back to legacy text NULLs.
    let oids = described_param_oids(&mut conn, sql, builtin, false).await?;
    let query = bind_params_described(sql, &oids, params)?;
    run(query, &mut conn)
        .await
        .map_err(|error| map_db_error(builtin, error))
}

pub(super) fn row_to_value(row: PgRow) -> Result<VmValue, VmError> {
    let mut map = crate::value::DictMap::new();
    for (index, column) in row.columns().iter().enumerate() {
        let name = column.name().to_string();
        let value = column_value(&row, index, column.type_info().name())?;
        map.insert(name, value);
    }
    Ok(VmValue::dict(map))
}

fn column_value(row: &PgRow, index: usize, type_name: &str) -> Result<VmValue, VmError> {
    let raw = row
        .try_get_raw(index)
        .map_err(|error| runtime_error(format!("pg_query: row decode failed: {error}")))?;
    if raw.is_null() {
        return Ok(VmValue::Nil);
    }
    let value = match type_name {
        "BOOL" => VmValue::Bool(row.try_get::<bool, _>(index).map_err(decode_error)?),
        "INT2" => VmValue::Int(i64::from(
            row.try_get::<i16, _>(index).map_err(decode_error)?,
        )),
        "INT4" => VmValue::Int(i64::from(
            row.try_get::<i32, _>(index).map_err(decode_error)?,
        )),
        "INT8" => VmValue::Int(row.try_get::<i64, _>(index).map_err(decode_error)?),
        "FLOAT4" => VmValue::Float(f64::from(
            row.try_get::<f32, _>(index).map_err(decode_error)?,
        )),
        "FLOAT8" => VmValue::Float(row.try_get::<f64, _>(index).map_err(decode_error)?),
        // Harn has no Decimal type — surface NUMERIC as its canonical
        // textual representation so downstream JSON / Decimal callers can
        // round-trip without precision loss.
        "NUMERIC" => VmValue::String(std::sync::Arc::from(
            row.try_get::<rust_decimal::Decimal, _>(index)
                .map_err(decode_error)?
                .to_string(),
        )),
        "TEXT" | "VARCHAR" | "BPCHAR" | "NAME" => VmValue::String(std::sync::Arc::from(
            row.try_get::<String, _>(index).map_err(decode_error)?,
        )),
        "UUID" => VmValue::String(std::sync::Arc::from(
            row.try_get::<uuid::Uuid, _>(index)
                .map_err(decode_error)?
                .to_string(),
        )),
        "JSON" | "JSONB" => {
            let json = row
                .try_get::<serde_json::Value, _>(index)
                .map_err(decode_error)?;
            crate::stdlib::json_to_vm_value(&json)
        }
        "BYTEA" => VmValue::Bytes(std::sync::Arc::new(
            row.try_get::<Vec<u8>, _>(index).map_err(decode_error)?,
        )),
        "DATE" => VmValue::String(std::sync::Arc::from(
            row.try_get::<time::Date, _>(index)
                .map_err(decode_error)?
                .to_string(),
        )),
        "TIME" => VmValue::String(std::sync::Arc::from(
            row.try_get::<time::Time, _>(index)
                .map_err(decode_error)?
                .to_string(),
        )),
        "TIMESTAMP" => VmValue::String(std::sync::Arc::from(
            row.try_get::<time::PrimitiveDateTime, _>(index)
                .map_err(decode_error)?
                .to_string(),
        )),
        "TIMESTAMPTZ" => VmValue::String(std::sync::Arc::from(
            row.try_get::<time::OffsetDateTime, _>(index)
                .map_err(decode_error)?
                .to_string(),
        )),
        // Postgres array types. sqlx exposes these as `<ELEMENT>[]`. Common
        // element types map directly; anything else (e.g. user-defined
        // enum arrays) falls back to text below.
        "BOOL[]" => decode_array::<bool>(row, index, VmValue::Bool)?,
        "INT2[]" => decode_array::<i16>(row, index, |v| VmValue::Int(i64::from(v)))?,
        "INT4[]" => decode_array::<i32>(row, index, |v| VmValue::Int(i64::from(v)))?,
        "INT8[]" => decode_array::<i64>(row, index, VmValue::Int)?,
        "FLOAT4[]" => decode_array::<f32>(row, index, |v| VmValue::Float(f64::from(v)))?,
        "FLOAT8[]" => decode_array::<f64>(row, index, VmValue::Float)?,
        "TEXT[]" | "VARCHAR[]" => {
            decode_array::<String>(row, index, |v| VmValue::String(std::sync::Arc::from(v)))?
        }
        "UUID[]" => decode_array::<uuid::Uuid>(row, index, |v| {
            VmValue::String(std::sync::Arc::from(v.to_string()))
        })?,
        "JSON[]" | "JSONB[]" => {
            let values: Vec<serde_json::Value> = row.try_get(index).map_err(decode_error)?;
            VmValue::List(std::sync::Arc::new(
                values.iter().map(crate::stdlib::json_to_vm_value).collect(),
            ))
        }
        "INT4RANGE" => range_value(
            row.try_get::<sqlx_postgres::types::PgRange<i32>, _>(index)
                .map_err(decode_error)?,
            |v| VmValue::Int(i64::from(v)),
        ),
        "INT8RANGE" => range_value(
            row.try_get::<sqlx_postgres::types::PgRange<i64>, _>(index)
                .map_err(decode_error)?,
            VmValue::Int,
        ),
        "NUMRANGE" => range_value(
            row.try_get::<sqlx_postgres::types::PgRange<rust_decimal::Decimal>, _>(index)
                .map_err(decode_error)?,
            |v| VmValue::String(Arc::from(v.to_string())),
        ),
        "DATERANGE" => range_value(
            row.try_get::<sqlx_postgres::types::PgRange<time::Date>, _>(index)
                .map_err(decode_error)?,
            |v| VmValue::String(Arc::from(v.to_string())),
        ),
        "TSRANGE" => range_value(
            row.try_get::<sqlx_postgres::types::PgRange<time::PrimitiveDateTime>, _>(index)
                .map_err(decode_error)?,
            |v| VmValue::String(Arc::from(v.to_string())),
        ),
        "TSTZRANGE" => range_value(
            row.try_get::<sqlx_postgres::types::PgRange<time::OffsetDateTime>, _>(index)
                .map_err(decode_error)?,
            |v| VmValue::String(Arc::from(v.to_string())),
        ),
        // HSTORE decodes as a Harn dict<string, string|nil>. sqlx surfaces
        // it as `BTreeMap<String, Option<String>>` already.
        "HSTORE" => {
            let map: sqlx_postgres::types::PgHstore = row.try_get(index).map_err(decode_error)?;
            let mut dict = crate::value::DictMap::new();
            for (key, value) in map.0 {
                dict.insert(
                    key,
                    value
                        .map(|v| VmValue::String(std::sync::Arc::from(v)))
                        .unwrap_or(VmValue::Nil),
                );
            }
            VmValue::dict(dict)
        }
        // Postgres geometric types decode into dictionaries that preserve
        // the native shape while staying idiomatic to Harn callers.
        "POINT" => {
            let point: sqlx_postgres::types::PgPoint = row.try_get(index).map_err(decode_error)?;
            point_value(point.x, point.y)
        }
        "LINE" => {
            let line: sqlx_postgres::types::PgLine = row.try_get(index).map_err(decode_error)?;
            dict_value([
                ("a", VmValue::Float(line.a)),
                ("b", VmValue::Float(line.b)),
                ("c", VmValue::Float(line.c)),
            ])
        }
        "LSEG" => {
            let segment: sqlx_postgres::types::PgLSeg = row.try_get(index).map_err(decode_error)?;
            dict_value([
                ("start", point_value(segment.start_x, segment.start_y)),
                ("end", point_value(segment.end_x, segment.end_y)),
            ])
        }
        "BOX" => {
            let pg_box: sqlx_postgres::types::PgBox = row.try_get(index).map_err(decode_error)?;
            dict_value([
                (
                    "upper_right",
                    point_value(pg_box.upper_right_x, pg_box.upper_right_y),
                ),
                (
                    "lower_left",
                    point_value(pg_box.lower_left_x, pg_box.lower_left_y),
                ),
            ])
        }
        "PATH" => {
            let path: sqlx_postgres::types::PgPath = row.try_get(index).map_err(decode_error)?;
            dict_value([
                ("closed", VmValue::Bool(path.closed)),
                ("points", points_value(path.points)),
            ])
        }
        "POLYGON" => {
            let polygon: sqlx_postgres::types::PgPolygon =
                row.try_get(index).map_err(decode_error)?;
            dict_value([("points", points_value(polygon.points))])
        }
        "CIRCLE" => {
            let circle: sqlx_postgres::types::PgCircle =
                row.try_get(index).map_err(decode_error)?;
            dict_value([
                ("center", point_value(circle.x, circle.y)),
                ("radius", VmValue::Float(circle.radius)),
            ])
        }
        _ => VmValue::String(std::sync::Arc::from(
            row.try_get::<String, _>(index).map_err(|error| {
                runtime_error(format!(
                    "pg_query: unsupported column type {type_name}: {error}"
                ))
            })?,
        )),
    };
    Ok(value)
}

fn decode_array<T>(
    row: &PgRow,
    index: usize,
    map: impl Fn(T) -> VmValue,
) -> Result<VmValue, VmError>
where
    T: for<'r> sqlx_core::decode::Decode<'r, Postgres>
        + sqlx_core::types::Type<Postgres>
        + sqlx_postgres::PgHasArrayType
        + Send
        + Unpin
        + 'static,
{
    let values: Vec<T> = row.try_get(index).map_err(decode_error)?;
    Ok(VmValue::List(std::sync::Arc::new(
        values.into_iter().map(map).collect(),
    )))
}

fn range_value<T>(range: sqlx_postgres::types::PgRange<T>, map: impl Fn(T) -> VmValue) -> VmValue {
    let (start, start_inclusive) = range_bound_value(range.start, &map);
    let (end, end_inclusive) = range_bound_value(range.end, &map);
    dict_value([
        ("start", start),
        ("end", end),
        ("start_inclusive", VmValue::Bool(start_inclusive)),
        ("end_inclusive", VmValue::Bool(end_inclusive)),
    ])
}

fn range_bound_value<T>(bound: Bound<T>, map: &impl Fn(T) -> VmValue) -> (VmValue, bool) {
    match bound {
        Bound::Included(value) => (map(value), true),
        Bound::Excluded(value) => (map(value), false),
        Bound::Unbounded => (VmValue::Nil, false),
    }
}

fn points_value(points: Vec<sqlx_postgres::types::PgPoint>) -> VmValue {
    VmValue::List(Arc::new(
        points
            .into_iter()
            .map(|point| point_value(point.x, point.y))
            .collect(),
    ))
}

fn point_value(x: f64, y: f64) -> VmValue {
    dict_value([("x", VmValue::Float(x)), ("y", VmValue::Float(y))])
}

fn dict_value<const N: usize>(pairs: [(&'static str, VmValue); N]) -> VmValue {
    VmValue::dict(
        pairs
            .into_iter()
            .map(|(key, value)| (key.to_string(), value))
            .collect::<crate::value::DictMap>(),
    )
}

fn decode_error(error: sqlx_core::error::Error) -> VmError {
    runtime_error(format!("pg_query: row decode failed: {error}"))
}

/// Map a SQLSTATE code to a stable, schema-free category string.
///
/// Returns `Some(category)` for the error classes whose *raw* Postgres message
/// embeds sensitive schema detail (constraint names, column names, relation
/// names) that should not cross the hostlib boundary to a `.harn` caller. The
/// SQLSTATE itself is stable and safe to surface. Unknown codes return `None`,
/// and those fall back to a generic `"database error"` category (still without
/// the raw message). The five-character SQLSTATE class (first two chars) is used
/// so we cover whole families, e.g. `23xxx` integrity-constraint violations.
fn sqlstate_category(code: &str) -> Option<&'static str> {
    match code {
        // 23xxx — integrity constraint violation (names tables/constraints).
        "23505" => Some("unique_violation"),
        "23503" => Some("foreign_key_violation"),
        "23502" => Some("not_null_violation"),
        "23514" => Some("check_violation"),
        "23P01" => Some("exclusion_violation"),
        // 22xxx — data exceptions (overflow, etc.) name columns/values.
        "22003" => Some("numeric_out_of_range"),
        "22001" => Some("string_too_long"),
        "22P02" => Some("invalid_text_representation"),
        // 42xxx — syntax / access-rule (relation/column names).
        "42501" => Some("insufficient_privilege"),
        "42P01" => Some("undefined_table"),
        "42703" => Some("undefined_column"),
        // 40xxx — transaction rollback (serialization / deadlock).
        "40001" => Some("serialization_failure"),
        "40P01" => Some("deadlock_detected"),
        _ => {
            // Cover the whole 23xxx integrity-violation family by class so a
            // newer constraint code still gets a stable, schema-free category.
            if code.starts_with("23") {
                Some("constraint_violation")
            } else {
                None
            }
        }
    }
}

/// Map a sqlx error from a user-facing `pg_query`/`pg_execute` into a stable
/// `VmError` whose message does **not** leak raw Postgres detail (constraint
/// names, schema/relation names, column names). The full original error —
/// including any constraint name — is recorded via `tracing` at the boundary so
/// operators keep the detail server-side (M-2).
///
/// For a database error we surface `{builtin}: <category> (SQLSTATE <code>)`.
/// Non-database errors (pool/IO/protocol) carry no schema detail, so their text
/// is preserved behind the `{builtin}:` prefix as before.
fn map_db_error(builtin: &str, error: sqlx_core::error::Error) -> VmError {
    if let sqlx_core::error::Error::Database(db) = &error {
        let code = db.code().map(|c| c.into_owned());
        let category = code
            .as_deref()
            .and_then(sqlstate_category)
            .unwrap_or("database error");
        // Keep the full, detail-bearing error in server-side tracing only.
        tracing::warn!(
            target: "harn_vm::postgres",
            builtin,
            sqlstate = code.as_deref().unwrap_or("none"),
            constraint = db.constraint().unwrap_or(""),
            table = db.table().unwrap_or(""),
            error = %error,
            "postgres error (detail withheld from caller)"
        );
        return match code {
            Some(code) => runtime_error(format!("{builtin}: {category} (SQLSTATE {code})")),
            None => runtime_error(format!("{builtin}: {category}")),
        };
    }
    runtime_error(format!("{builtin}: {error}"))
}

fn query_result_value(result: PgQueryResult, duration: std::time::Duration) -> VmValue {
    execute_result_value(result.rows_affected(), duration)
}

fn execute_result_value(rows_affected: u64, duration: std::time::Duration) -> VmValue {
    let mut map = crate::value::DictMap::new();
    map.insert(
        "rows_affected".to_string(),
        VmValue::Int(rows_affected as i64),
    );
    map.insert(
        "duration_ms".to_string(),
        VmValue::Int(duration.as_millis() as i64),
    );
    VmValue::dict(map)
}

async fn resolve_connection_url(
    ctx: &crate::vm::AsyncBuiltinCtx,
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

async fn secret_url(ctx: &crate::vm::AsyncBuiltinCtx, secret_id: &str) -> Result<String, VmError> {
    let mut child_vm = ctx.child_vm();
    let value = child_vm
        .call_named_builtin(
            "secret_get",
            vec![VmValue::String(std::sync::Arc::from(
                secret_id.trim().to_string(),
            ))],
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

pub(super) fn pool_by_id(id: &str) -> Result<Arc<PgPool>, VmError> {
    pool_record_by_id(id).map(|record| Arc::clone(&record.pool))
}

pub(super) fn pool_record_by_id(id: &str) -> Result<Arc<PoolRecord>, VmError> {
    POOLS.with(|pools| {
        pools
            .borrow()
            .get(id)
            .cloned()
            .ok_or_else(|| runtime_error(format!("pg_pool: unknown or closed pool `{id}`")))
    })
}

pub(super) fn pool_record_from_handle(
    value: &VmValue,
    builtin: &str,
) -> Result<Arc<PoolRecord>, VmError> {
    let id = handle_id(Some(value), HANDLE_POOL, builtin)?;
    pool_record_by_id(&id)
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
    let id = handle_id(Some(handle), HANDLE_POOL, builtin)?;
    pool_by_id(&id)
}

pub(super) fn tx_by_id(id: &str) -> Result<PgTxCell, VmError> {
    TXS.with(|txs| {
        txs.borrow()
            .get(id)
            .cloned()
            .ok_or_else(|| runtime_error(format!("pg_transaction: unknown transaction `{id}`")))
    })
}

pub(super) fn register_tx(id: &str, cell: PgTxCell) {
    TXS.with(|txs| {
        txs.borrow_mut().insert(id.to_string(), cell);
    });
}

pub(super) fn unregister_tx(id: &str) {
    TXS.with(|txs| {
        txs.borrow_mut().remove(id);
    });
}

pub(super) fn handle_value(kind: &str, id: &str, mut extra: crate::value::DictMap) -> VmValue {
    extra.put_str("_type", kind);
    extra.put_str("id", id);
    VmValue::dict(extra)
}

pub(super) fn handle_kind(value: &VmValue) -> Option<String> {
    value
        .as_dict()
        .and_then(|dict| dict.get("_type"))
        .map(VmValue::display)
}

pub(super) fn handle_id(
    value: Option<&VmValue>,
    expected: &str,
    builtin: &str,
) -> Result<String, VmError> {
    let dict = value
        .and_then(VmValue::as_dict)
        .ok_or_else(|| runtime_error(format!("{builtin}: expected {expected} handle")))?;
    let kind = dict.get("_type").map(VmValue::display).unwrap_or_default();
    if kind != expected {
        return Err(runtime_error(format!(
            "{builtin}: expected {expected} handle"
        )));
    }
    let id = dict.get("id").map(VmValue::display).unwrap_or_default();
    if id.is_empty() {
        return Err(runtime_error(format!("{builtin}: handle is missing id")));
    }
    Ok(id)
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
    let value = args.get(index).map(VmValue::display).unwrap_or_default();
    if value.trim().is_empty() {
        return Err(runtime_error(format!("{builtin}: {label} is required")));
    }
    Ok(value)
}

/// Tenant namespace used by `pg_advisory_*lock(... {tenant_namespace: true})`
/// to XOR salt into lock keys. Reads the harness-tenant scope (same
/// source `emit_channel` consults) so two tenants colliding on the same
/// numeric key see distinct lock-key pairs server-side.
pub(super) fn current_tenant_namespace() -> String {
    crate::harness_tenant::current_tenant_id()
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
    options: Option<&crate::value::DictMap>,
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
    options: Option<&crate::value::DictMap>,
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

pub(super) fn routing_from_options(
    options: Option<&crate::value::DictMap>,
) -> Result<QueryRouting, VmError> {
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
    match value {
        None | Some(VmValue::Nil) => Ok(Vec::new()),
        Some(VmValue::List(items)) => Ok((**items).clone()),
        Some(_) => Err(runtime_error(format!(
            "{builtin}: params must be a list when provided"
        ))),
    }
}

fn option_string(options: Option<&crate::value::DictMap>, key: &str) -> Option<String> {
    options
        .and_then(|opts| opts.get(key))
        .map(VmValue::display)
        .filter(|value| !value.trim().is_empty())
}

pub(super) fn option_bool(value: Option<&VmValue>) -> Option<bool> {
    match value? {
        VmValue::Bool(b) => Some(*b),
        _ => None,
    }
}

fn option_int(options: Option<&crate::value::DictMap>, key: &str) -> Option<i64> {
    options
        .and_then(|opts| opts.get(key))
        .and_then(|value| match value {
            VmValue::Int(number) => Some(*number),
            VmValue::Float(number)
                if number.is_finite()
                    && *number >= i64::MIN as f64
                    && *number <= i64::MAX as f64 =>
            {
                Some(*number as i64)
            }
            _ => None,
        })
}

fn option_duration_ms(options: Option<&crate::value::DictMap>, key: &str) -> Option<u64> {
    options.and_then(|opts| opts.get(key)).and_then(|value| {
        non_negative_millis_from_value(value, "postgres", key, ErrorKind::Runtime).ok()
    })
}

pub(super) fn next_id(prefix: &str) -> String {
    format!("{prefix}-{}", NEXT_ID.fetch_add(1, Ordering::Relaxed))
}

pub(super) fn runtime_error(message: impl Into<String>) -> VmError {
    VmError::Runtime(message.into())
}

fn parse_mock_fixtures(items: &[VmValue]) -> Result<Vec<MockFixture>, VmError> {
    items
        .iter()
        .map(|item| {
            let dict = item
                .as_dict()
                .ok_or_else(|| runtime_error("pg_mock_pool: each fixture must be a dict"))?;
            let sql = dict
                .get("sql")
                .map(VmValue::display)
                .filter(|sql| !sql.trim().is_empty())
                .ok_or_else(|| runtime_error("pg_mock_pool: fixture.sql is required"))?;
            let params = dict.get("params").map(vm_value_to_json);
            let rows = match dict.get("rows") {
                Some(VmValue::List(rows)) => (**rows).clone(),
                None | Some(VmValue::Nil) => Vec::new(),
                Some(_) => return Err(runtime_error("pg_mock_pool: fixture.rows must be a list")),
            };
            let rows_affected = dict
                .get("rows_affected")
                .and_then(VmValue::as_int)
                .unwrap_or(rows.len() as i64)
                .max(0) as u64;
            let error = dict
                .get("error")
                .map(VmValue::display)
                .filter(|value| !value.is_empty());
            Ok(MockFixture {
                sql,
                params,
                rows,
                rows_affected,
                error,
            })
        })
        .collect()
}

fn mock_query(
    target: &VmValue,
    sql: &str,
    params: &[VmValue],
    execute: bool,
) -> Result<Vec<VmValue>, VmError> {
    let id = handle_id(Some(target), HANDLE_MOCK, "pg_mock")?;
    let params_json = serde_json::Value::Array(params.iter().map(vm_value_to_json).collect());
    MOCKS.with(|mocks| {
        let mut mocks = mocks.borrow_mut();
        let mock = mocks
            .get_mut(&id)
            .ok_or_else(|| runtime_error(format!("pg_mock: unknown mock pool `{id}`")))?;
        let call = crate::stdlib::json_to_vm_value(&serde_json::json!({
            "sql": sql,
            "params": params_json,
            "execute": execute,
        }));
        mock.calls.push(call);
        let fixture = mock
            .fixtures
            .iter()
            .find(|fixture| {
                fixture.sql.trim() == sql.trim()
                    && fixture
                        .params
                        .as_ref()
                        .is_none_or(|expected| expected == &params_json)
            })
            .ok_or_else(|| runtime_error(format!("pg_mock: no fixture matched `{sql}`")))?;
        if let Some(error) = &fixture.error {
            return Err(runtime_error(format!("pg_mock: {error}")));
        }
        if execute {
            // Internal scaffold: `execute_stmt` only reads `rows_affected`
            // out of this synthetic row, then constructs its own dict
            // with the real call-site duration. The placeholder duration
            // here never surfaces to callers.
            Ok(vec![execute_result_value(
                fixture.rows_affected,
                std::time::Duration::ZERO,
            )])
        } else {
            Ok(fixture.rows.clone())
        }
    })
}

#[cfg(test)]
mod tests;
