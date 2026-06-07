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
    PgArguments, PgConnectOptions, PgPool, PgPoolOptions, PgQueryResult, PgRow, PgSslMode, Postgres,
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
    static POOLS: RefCell<BTreeMap<String, Arc<PoolRecord>>> = const { RefCell::new(BTreeMap::new()) };
    static TXS: RefCell<PgTxRegistry> =
        const { RefCell::new(BTreeMap::new()) };
    static MOCKS: RefCell<BTreeMap<String, MockPool>> = const { RefCell::new(BTreeMap::new()) };
}

pub(crate) fn reset_postgres_state() {
    POOLS.with(|pools| pools.borrow_mut().clear());
    TXS.with(|txs| txs.borrow_mut().clear());
    MOCKS.with(|mocks| mocks.borrow_mut().clear());
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
        VmValue::Dict(Arc::new(BTreeMap::from([
            ("_namespace".to_string(), VmValue::String(Arc::from("pg"))),
            ("jsonb".to_string(), jsonb),
        ]))),
    );
}

fn namespace(name: &str, entries: &[(&str, &str)]) -> VmValue {
    VmValue::Dict(Arc::new(
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
    ))
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
    let mut result = BTreeMap::new();
    result.insert("pools".to_string(), VmValue::Int(pools));
    result.insert(
        "connections_cleared".to_string(),
        VmValue::Int(connections_cleared),
    );
    result.insert(
        "connections_skipped".to_string(),
        VmValue::Int(connections_skipped),
    );
    VmValue::Dict(std::sync::Arc::new(result))
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
    let tx_handle = handle_value(HANDLE_TX, &tx_id, BTreeMap::new());

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
    Ok(handle_value(HANDLE_MOCK, &id, BTreeMap::new()))
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
    options: Option<&BTreeMap<String, VmValue>>,
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
    let primary_pool = build_pool(
        &primary_url,
        options,
        max_connections,
        stmt_cache_capacity,
        "pg_pool",
    )
    .await?;

    let replica_urls = collect_replica_urls(ctx, options).await?;
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

    let id = next_id(if single_connection {
        "pgconn"
    } else {
        "pgpool"
    });
    let mut meta = BTreeMap::new();
    meta.insert(
        "max_connections".to_string(),
        VmValue::Int(i64::from(max_connections)),
    );
    meta.insert(
        "single_connection".to_string(),
        VmValue::Bool(single_connection),
    );
    meta.insert("replicas".to_string(), VmValue::Int(replicas.len() as i64));
    meta.insert(
        "statement_cache_capacity".to_string(),
        VmValue::Int(stmt_cache_capacity as i64),
    );
    meta.insert(
        "read_routing_policy".to_string(),
        VmValue::String(Arc::from(read_routing_policy.as_str())),
    );
    if let Some(application_name) = option_string(options, "application_name") {
        meta.insert(
            "application_name".to_string(),
            VmValue::String(std::sync::Arc::from(application_name)),
        );
    }
    POOLS.with(|pools| {
        pools.borrow_mut().insert(
            id.clone(),
            Arc::new(PoolRecord {
                pool: Arc::new(primary_pool),
                replicas,
                replica_cursor: AtomicUsize::new(0),
                max_connections,
                statement_cache_capacity: stmt_cache_capacity,
                read_routing_policy,
                circuit,
            }),
        );
    });
    Ok(handle_value(HANDLE_POOL, &id, meta))
}

async fn build_pool(
    url: &str,
    options: Option<&BTreeMap<String, VmValue>>,
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
    options: Option<&BTreeMap<String, VmValue>>,
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

fn build_circuit_breaker(options: Option<&BTreeMap<String, VmValue>>) -> CircuitBreakerState {
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
            let query = bind_params(query(AssertSqlSafe(sql)), params);
            let rows = query
                .fetch_all(&mut **tx)
                .await
                .map_err(|error| runtime_error(format!("pg_query: {error}")))?;
            return rows.into_iter().map(row_to_value).collect();
        }
        _ => {}
    }

    let record = pool_record_from_handle(target, "pg_query")?;
    let pool = pool_for_routing(&record, routing, "pg_query")?;
    let (probe, _) = enter_circuit(&record.circuit, "pg_query")?;
    let query = bind_params(query(AssertSqlSafe(sql)), params);
    let result = query.fetch_all(pool.as_ref()).await;
    match result {
        Ok(rows) => {
            record.circuit.record_success(probe);
            rows.into_iter().map(row_to_value).collect()
        }
        Err(error) => {
            record.circuit.record_failure(probe);
            Err(runtime_error(format!("pg_query: {error}")))
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
        let result = bind_params(query(AssertSqlSafe(sql)), params)
            .execute(&mut **tx)
            .await
            .map_err(|error| runtime_error(format!("pg_execute: {error}")))?;
        return Ok(query_result_value(result, started.elapsed()));
    }
    let record = pool_record_from_handle(target, "pg_execute")?;
    let (probe, _) = enter_circuit(&record.circuit, "pg_execute")?;
    let result = bind_params(query(AssertSqlSafe(sql)), params)
        .execute(record.pool.as_ref())
        .await;
    match result {
        Ok(query_result) => {
            record.circuit.record_success(probe);
            Ok(query_result_value(query_result, started.elapsed()))
        }
        Err(error) => {
            record.circuit.record_failure(probe);
            Err(runtime_error(format!("pg_execute: {error}")))
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

async fn apply_transaction_settings(
    tx_id: &str,
    settings: &BTreeMap<String, VmValue>,
) -> Result<(), VmError> {
    for (key, value) in settings {
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
        bind_params(query(sql), &params)
            .execute(&mut **tx)
            .await
            .map_err(|error| {
                runtime_error(format!("pg_transaction: set_config({key}) failed: {error}"))
            })?;
    }
    Ok(())
}

pub(super) fn bind_params<'q>(
    mut query: Query<'q, Postgres, PgArguments>,
    params: &'q [VmValue],
) -> Query<'q, Postgres, PgArguments> {
    for param in params {
        query = match param {
            VmValue::Nil => query.bind(None::<String>),
            VmValue::Bool(value) => query.bind(*value),
            VmValue::Int(value) => query.bind(*value),
            VmValue::Float(value) => query.bind(*value),
            VmValue::String(value) => query.bind(value.to_string()),
            VmValue::Bytes(value) => query.bind((**value).clone()),
            VmValue::Duration(ms) => query.bind(*ms),
            value => query.bind(sqlx_core::types::Json(vm_value_to_json(value))),
        };
    }
    query
}

pub(super) fn row_to_value(row: PgRow) -> Result<VmValue, VmError> {
    let mut map = BTreeMap::new();
    for (index, column) in row.columns().iter().enumerate() {
        let name = column.name().to_string();
        let value = column_value(&row, index, column.type_info().name())?;
        map.insert(name, value);
    }
    Ok(VmValue::Dict(std::sync::Arc::new(map)))
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
            let mut dict = BTreeMap::new();
            for (key, value) in map.0 {
                dict.insert(
                    key,
                    value
                        .map(|v| VmValue::String(std::sync::Arc::from(v)))
                        .unwrap_or(VmValue::Nil),
                );
            }
            VmValue::Dict(std::sync::Arc::new(dict))
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
    VmValue::Dict(Arc::new(
        pairs
            .into_iter()
            .map(|(key, value)| (key.to_string(), value))
            .collect(),
    ))
}

fn decode_error(error: sqlx_core::error::Error) -> VmError {
    runtime_error(format!("pg_query: row decode failed: {error}"))
}

fn query_result_value(result: PgQueryResult, duration: std::time::Duration) -> VmValue {
    execute_result_value(result.rows_affected(), duration)
}

fn execute_result_value(rows_affected: u64, duration: std::time::Duration) -> VmValue {
    let mut map = BTreeMap::new();
    map.insert(
        "rows_affected".to_string(),
        VmValue::Int(rows_affected as i64),
    );
    map.insert(
        "duration_ms".to_string(),
        VmValue::Int(duration.as_millis() as i64),
    );
    VmValue::Dict(std::sync::Arc::new(map))
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

pub(super) fn handle_value(kind: &str, id: &str, mut extra: BTreeMap<String, VmValue>) -> VmValue {
    extra.insert(
        "_type".to_string(),
        VmValue::String(std::sync::Arc::from(kind)),
    );
    extra.insert(
        "id".to_string(),
        VmValue::String(std::sync::Arc::from(id.to_string())),
    );
    VmValue::Dict(std::sync::Arc::new(extra))
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
    options: Option<&BTreeMap<String, VmValue>>,
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
    options: Option<&BTreeMap<String, VmValue>>,
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
    options: Option<&BTreeMap<String, VmValue>>,
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

fn option_string(options: Option<&BTreeMap<String, VmValue>>, key: &str) -> Option<String> {
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

fn option_int(options: Option<&BTreeMap<String, VmValue>>, key: &str) -> Option<i64> {
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

fn option_duration_ms(options: Option<&BTreeMap<String, VmValue>>, key: &str) -> Option<u64> {
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
mod tests {
    use super::*;

    use crate::{compile_source, register_vm_stdlib, Vm};

    fn s(value: &str) -> VmValue {
        VmValue::String(std::sync::Arc::from(value))
    }

    fn dict(pairs: &[(&str, VmValue)]) -> VmValue {
        VmValue::Dict(std::sync::Arc::new(
            pairs
                .iter()
                .map(|(key, value)| ((*key).to_string(), value.clone()))
                .collect(),
        ))
    }

    fn lazy_pool_for_test() -> Arc<PgPool> {
        let options = PgConnectOptions::from_str("postgres://postgres@localhost/postgres").unwrap();
        Arc::new(
            PgPoolOptions::new()
                .max_connections(1)
                .connect_lazy_with(options),
        )
    }

    fn routing_record(replicas: usize, policy: ReadRoutingPolicy) -> Arc<PoolRecord> {
        Arc::new(PoolRecord {
            pool: lazy_pool_for_test(),
            replicas: (0..replicas).map(|_| lazy_pool_for_test()).collect(),
            replica_cursor: AtomicUsize::new(0),
            max_connections: 1,
            statement_cache_capacity: DEFAULT_STATEMENT_CACHE_CAPACITY,
            read_routing_policy: policy,
            circuit: Arc::new(CircuitBreakerState::disabled()),
        })
    }

    #[test]
    fn read_routing_policy_options_parse_named_modes() {
        let pool_options =
            BTreeMap::from([("read_routing_policy".to_string(), s("round_robin_replica"))]);
        assert_eq!(
            read_routing_policy_from_options(Some(&pool_options)).unwrap(),
            ReadRoutingPolicy::RoundRobinReplica
        );

        let query_options = BTreeMap::from([("route".to_string(), s("replica"))]);
        assert_eq!(
            routing_from_options(Some(&query_options)).unwrap(),
            QueryRouting::Policy(ReadRoutingPolicy::Replica)
        );

        let read_only_options = BTreeMap::from([("read_only".to_string(), VmValue::Bool(true))]);
        assert_eq!(
            routing_from_options(Some(&read_only_options)).unwrap(),
            QueryRouting::ReadOnly
        );

        let bad_options = BTreeMap::from([("routing_policy".to_string(), s("nearby"))]);
        assert!(routing_from_options(Some(&bad_options)).is_err());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn read_routing_policy_selects_replicas_or_errors_deterministically() {
        let record = routing_record(2, ReadRoutingPolicy::RoundRobinReplica);
        let first = pool_for_routing(&record, QueryRouting::ReadOnly, "pg_query").unwrap();
        let second = pool_for_routing(&record, QueryRouting::ReadOnly, "pg_query").unwrap();
        assert!(Arc::ptr_eq(&first, &record.replicas[0]));
        assert!(Arc::ptr_eq(&second, &record.replicas[1]));

        let fallback = routing_record(0, ReadRoutingPolicy::ReplicaOrPrimary);
        let pool = pool_for_routing(&fallback, QueryRouting::ReadOnly, "pg_query").unwrap();
        assert!(Arc::ptr_eq(&pool, &fallback.pool));

        let strict = routing_record(0, ReadRoutingPolicy::RoundRobinReplica);
        assert!(pool_for_routing(&strict, QueryRouting::ReadOnly, "pg_query").is_err());
    }

    #[test]
    fn range_value_preserves_bounds_and_inclusivity() {
        let value = range_value(
            sqlx_postgres::types::PgRange {
                start: Bound::Included(10_i64),
                end: Bound::Excluded(20_i64),
            },
            VmValue::Int,
        );
        let dict = value.as_dict().expect("range dict");
        assert_eq!(dict.get("start").and_then(VmValue::as_int), Some(10));
        assert_eq!(dict.get("end").and_then(VmValue::as_int), Some(20));
        assert!(matches!(
            dict.get("start_inclusive"),
            Some(VmValue::Bool(true))
        ));
        assert!(matches!(
            dict.get("end_inclusive"),
            Some(VmValue::Bool(false))
        ));
    }

    #[test]
    fn geometry_helpers_return_structured_dicts() {
        let point = point_value(1.5, 2.5);
        let point = point.as_dict().expect("point dict");
        assert!(matches!(point.get("x"), Some(VmValue::Float(1.5))));
        assert!(matches!(point.get("y"), Some(VmValue::Float(2.5))));

        let points = points_value(vec![sqlx_postgres::types::PgPoint { x: 3.0, y: 4.0 }]);
        let VmValue::List(items) = points else {
            panic!("points should be a list");
        };
        let first = items[0].as_dict().expect("nested point");
        assert!(matches!(first.get("x"), Some(VmValue::Float(3.0))));
        assert!(matches!(first.get("y"), Some(VmValue::Float(4.0))));
    }

    #[test]
    fn mock_pool_matches_parameterized_query_and_records_calls() {
        reset_postgres_state();
        let fixtures = VmValue::List(std::sync::Arc::new(vec![dict(&[
            ("sql", s("select * from claims where tenant_id = $1")),
            (
                "params",
                VmValue::List(std::sync::Arc::new(vec![s("tenant-a")])),
            ),
            (
                "rows",
                VmValue::List(std::sync::Arc::new(vec![dict(&[("claim_id", s("c1"))])])),
            ),
        ])]));
        let fixture_list = match &fixtures {
            VmValue::List(items) => items,
            _ => unreachable!(),
        };
        let id = next_id("pgmock");
        MOCKS.with(|mocks| {
            mocks.borrow_mut().insert(
                id.clone(),
                MockPool {
                    fixtures: parse_mock_fixtures(fixture_list).unwrap(),
                    calls: Vec::new(),
                },
            );
        });
        let handle = handle_value(HANDLE_MOCK, &id, BTreeMap::new());
        let rows = mock_query(
            &handle,
            "select * from claims where tenant_id = $1",
            &[s("tenant-a")],
            false,
        )
        .unwrap();
        assert_eq!(
            VmValue::List(std::sync::Arc::new(rows)).display(),
            "[{claim_id: c1}]"
        );
        let calls = MOCKS.with(|mocks| mocks.borrow().values().next().unwrap().calls.clone());
        assert_eq!(calls.len(), 1);
    }

    #[test]
    fn mock_execute_returns_rows_affected() {
        reset_postgres_state();
        let fixtures = parse_mock_fixtures(&[dict(&[
            ("sql", s("update receipts set status = $1")),
            ("rows_affected", VmValue::Int(3)),
        ])])
        .unwrap();
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
        let handle = handle_value(HANDLE_MOCK, &id, BTreeMap::new());
        let rows = mock_query(
            &handle,
            "update receipts set status = $1",
            &[s("done")],
            true,
        )
        .unwrap();
        // mock_query stuffs Duration::ZERO into the internal scaffold row;
        // the real `duration_ms` lands on the dict execute_stmt returns
        // (see `pg_execute_reports_duration_ms_on_real_pool` smoke test).
        assert_eq!(rows[0].display(), "{duration_ms: 0, rows_affected: 3}");
    }

    #[test]
    fn savepoint_names_are_validated() {
        assert!(validate_savepoint_name("step_one", "pg_savepoint").is_ok());
        assert!(validate_savepoint_name("step.one", "pg_savepoint").is_ok());
        assert!(validate_savepoint_name("1bad", "pg_savepoint").is_err());
        assert!(validate_savepoint_name("bad name", "pg_savepoint").is_err());
        assert!(validate_savepoint_name("bad;name", "pg_savepoint").is_err());
        assert!(validate_savepoint_name("", "pg_savepoint").is_err());
    }

    #[test]
    fn savepoint_sql_double_quotes_identifier() {
        assert_eq!(
            render_savepoint_sql(SavepointOp::Create, "sp1"),
            "SAVEPOINT \"sp1\""
        );
        assert_eq!(
            render_savepoint_sql(SavepointOp::Release, "sp1"),
            "RELEASE SAVEPOINT \"sp1\""
        );
        assert_eq!(
            render_savepoint_sql(SavepointOp::RollbackTo, "sp1"),
            "ROLLBACK TO SAVEPOINT \"sp1\""
        );
    }

    #[test]
    fn execute_result_value_includes_duration() {
        let value = execute_result_value(7, std::time::Duration::from_millis(42));
        let dict = value.as_dict().expect("dict");
        assert_eq!(dict.get("rows_affected").unwrap().display(), "7");
        let duration_ms = dict.get("duration_ms").unwrap().as_int().unwrap();
        assert!((40..=50).contains(&duration_ms), "got {duration_ms}");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn postgres_round_trip_when_env_url_is_set() {
        let Ok(url) = std::env::var("HARN_TEST_POSTGRES_URL") else {
            return;
        };
        reset_postgres_state();
        let mut options = BTreeMap::new();
        options.insert("max_connections".to_string(), VmValue::Int(1));
        options.insert(
            "application_name".to_string(),
            s("harn-postgres-stdlib-test"),
        );
        let ctx = crate::vm::AsyncBuiltinCtx::for_test(crate::Vm::new());
        let handle = open_pool(&ctx, &s(&url), Some(&options), false)
            .await
            .unwrap();
        assert_eq!(handle.as_dict().unwrap()["max_connections"].display(), "1");
        let row = query_rows(
            &handle,
            "select $1::uuid as id, $2::jsonb as payload, $3::timestamptz as observed_at, $4::numeric as amount",
            &[
                s("00000000-0000-0000-0000-000000000001"),
                dict(&[("ok", VmValue::Bool(true))]),
                s("2024-01-02T03:04:05Z"),
                s("12345.6789"),
            ],
            QueryRouting::Primary,
        )
        .await
        .unwrap()
        .remove(0);
        let row = row.as_dict().unwrap();
        assert_eq!(
            row.get("id").unwrap().display(),
            "00000000-0000-0000-0000-000000000001"
        );
        assert_eq!(row.get("payload").unwrap().display(), "{ok: true}");
        assert!(row
            .get("observed_at")
            .unwrap()
            .display()
            .contains("2024-01-02"));
        assert_eq!(row.get("amount").unwrap().display(), "12345.6789");
    }

    #[test]
    fn harn_transaction_commits_rolls_back_and_applies_settings_when_env_url_is_set() {
        if std::env::var("HARN_TEST_POSTGRES_URL").is_err() {
            return;
        }
        reset_postgres_state();
        let source = r#"
import "std/postgres"

let db = pg_pool("env:HARN_TEST_POSTGRES_URL", {max_connections: 1})
pg_execute(db, "create temporary table if not exists harn_pg_tx_test(value int) on commit preserve rows", [])
pg_execute(db, "truncate table harn_pg_tx_test", [])

let tenant = pg_transaction(
  db,
  { tx ->
    pg_execute(tx, "insert into harn_pg_tx_test(value) values ($1)", [1])
    return pg_query_one(tx, "select current_setting('app.current_tenant_id', true) as tenant", []).tenant
  },
  {settings: {"app.current_tenant_id": "tenant-a"}},
)
__io_println(tenant)

let rolled = try {
  pg_transaction(db, { tx ->
    pg_execute(tx, "insert into harn_pg_tx_test(value) values ($1)", [2])
    throw_error("force rollback")
  })
} catch (e) {
  "rolled back"
}
__io_println(rolled)
__io_println(pg_query_one(db, "select count(*)::int8 as count from harn_pg_tx_test", []).count)
pg_close(db)
"#;
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let local = tokio::task::LocalSet::new();
            local
                .run_until(async {
                    let chunk =
                        compile_source(source).expect("compile postgres transaction source");
                    let mut vm = Vm::new();
                    register_vm_stdlib(&mut vm);
                    vm.execute(&chunk)
                        .await
                        .expect("execute postgres transaction source");
                    assert_eq!(vm.output().trim(), "tenant-a\nrolled back\n1");
                })
                .await;
        });
    }

    /// Drives `pg_savepoint` / `pg_rollback_to_savepoint` /
    /// `pg_release_savepoint` against a real Postgres so we cover the
    /// transaction-state-machine path the mocks can't exercise.
    #[test]
    fn savepoint_rollback_preserves_outer_writes_when_env_url_is_set() {
        if std::env::var("HARN_TEST_POSTGRES_URL").is_err() {
            return;
        }
        reset_postgres_state();
        let source = r#"
import "std/postgres"

let db = pg_pool("env:HARN_TEST_POSTGRES_URL", {max_connections: 1})
pg_execute(db, "DROP TABLE IF EXISTS harn_pg_sp_test", [])
pg_execute(db, "CREATE TABLE harn_pg_sp_test (id int PRIMARY KEY, label text NOT NULL)", [])

pg_transaction(db, { tx ->
  pg_execute(tx, "INSERT INTO harn_pg_sp_test (id, label) VALUES ($1, $2)", [1, "outer"])
  pg_savepoint(tx, "before_inner")
  pg_execute(tx, "INSERT INTO harn_pg_sp_test (id, label) VALUES ($1, $2)", [2, "inner"])
  pg_rollback_to_savepoint(tx, "before_inner")
  pg_release_savepoint(tx, "before_inner")
  pg_execute(tx, "INSERT INTO harn_pg_sp_test (id, label) VALUES ($1, $2)", [3, "after_release"])
  return 0
})

let rows = pg_query(db, "SELECT id, label FROM harn_pg_sp_test ORDER BY id", [])
for row in rows {
  __io_println(to_string(row.id) + ":" + row.label)
}
pg_execute(db, "DROP TABLE harn_pg_sp_test", [])
pg_close(db)
"#;
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let local = tokio::task::LocalSet::new();
            local
                .run_until(async {
                    let chunk = compile_source(source).expect("compile postgres savepoint source");
                    let mut vm = Vm::new();
                    register_vm_stdlib(&mut vm);
                    vm.execute(&chunk)
                        .await
                        .expect("execute postgres savepoint source");
                    assert_eq!(vm.output().trim(), "1:outer\n3:after_release");
                })
                .await;
        });
    }

    /// `pg_migrate` applies a synthetic two-file directory exactly once
    /// then no-ops on a second run. `.down.sql` siblings must be ignored.
    /// Requires `HARN_TEST_POSTGRES_URL`; runs against a unique scratch
    /// schema so concurrent invocations don't conflict.
    #[test]
    fn migrate_applies_synthetic_dir_and_is_idempotent_when_env_url_is_set() {
        if std::env::var("HARN_TEST_POSTGRES_URL").is_err() {
            return;
        }
        reset_postgres_state();
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path();
        std::fs::write(
            dir.join("0001_create_widgets.sql"),
            "CREATE TABLE widgets (id INT PRIMARY KEY, label TEXT NOT NULL)",
        )
        .unwrap();
        std::fs::write(
            dir.join("0002_seed_widget.sql"),
            "INSERT INTO widgets (id, label) VALUES (1, 'alpha')",
        )
        .unwrap();
        // The runner must ignore .down.sql siblings even when their up
        // counterpart would otherwise share a prefix.
        std::fs::write(
            dir.join("0001_create_widgets.down.sql"),
            "DROP TABLE widgets",
        )
        .unwrap();

        let schema = format!("harn_pg_mig_{}", uuid::Uuid::new_v4().simple());
        let migration_dir = dir.to_string_lossy().into_owned();
        let source = format!(
            r#"
import "std/postgres"

let admin = pg_pool("env:HARN_TEST_POSTGRES_URL", {{max_connections: 1}})
pg_execute(admin, "DROP SCHEMA IF EXISTS \"{schema}\" CASCADE", [])
pg_execute(admin, "CREATE SCHEMA \"{schema}\"", [])
pg_close(admin)

let db = pg_pool("env:HARN_TEST_POSTGRES_URL", {{max_connections: 1}})
pg_execute(db, "SET search_path TO \"{schema}\"", [])

let first = pg_migrate(db, {{dir: "{migration_dir}"}})
__io_println(len(first.applied))
__io_println(len(first.skipped))

let second = pg_migrate(db, {{dir: "{migration_dir}"}})
__io_println(len(second.applied))
__io_println(len(second.skipped))

let count = pg_query_one(db, "SELECT count(*)::int8 AS c FROM widgets", [])
__io_println(count.c)

pg_execute(db, "DROP SCHEMA \"{schema}\" CASCADE", [])
pg_close(db)
"#,
        );
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let local = tokio::task::LocalSet::new();
            local
                .run_until(async {
                    let chunk = compile_source(&source).expect("compile migrate source");
                    let mut vm = Vm::new();
                    register_vm_stdlib(&mut vm);
                    vm.execute(&chunk).await.expect("execute migrate source");
                    let lines: Vec<&str> = vm.output().lines().collect();
                    assert_eq!(lines, vec!["2", "0", "0", "2", "1"]);
                })
                .await;
        });
    }

    /// `pg_migrate` against the canonical `harn-cloud-store/migrations/`
    /// directory. Opt-in via `HARN_TEST_CLOUD_MIGRATIONS_DIR`; verifies
    /// that the runner consumes the full ledger without errors.
    #[test]
    fn migrate_loads_harn_cloud_store_migrations_when_env_set() {
        if std::env::var("HARN_TEST_POSTGRES_URL").is_err() {
            return;
        }
        let Ok(dir) = std::env::var("HARN_TEST_CLOUD_MIGRATIONS_DIR") else {
            return;
        };
        if !std::path::Path::new(&dir).exists() {
            return;
        }
        reset_postgres_state();
        let schema = format!("harn_pg_cloud_{}", uuid::Uuid::new_v4().simple());
        let source = format!(
            r#"
import "std/postgres"

let admin = pg_pool("env:HARN_TEST_POSTGRES_URL", {{max_connections: 1}})
pg_execute(admin, "DROP SCHEMA IF EXISTS \"{schema}\" CASCADE", [])
pg_execute(admin, "CREATE SCHEMA \"{schema}\"", [])
pg_close(admin)

let db = pg_pool("env:HARN_TEST_POSTGRES_URL", {{max_connections: 1}})
pg_execute(db, "SET search_path TO \"{schema}\"", [])

let result = pg_migrate(db, {{dir: "{dir}"}})
__io_println(len(result.applied))
__io_println(len(result.skipped))

let tables = pg_query(
  db,
  "SELECT table_name FROM information_schema.tables WHERE table_schema = $1",
  ["{schema}"],
)
__io_println(len(tables))

pg_execute(db, "DROP SCHEMA \"{schema}\" CASCADE", [])
pg_close(db)
"#,
        );
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let local = tokio::task::LocalSet::new();
            local
                .run_until(async {
                    let chunk = compile_source(&source).expect("compile cloud-migrate source");
                    let mut vm = Vm::new();
                    register_vm_stdlib(&mut vm);
                    vm.execute(&chunk)
                        .await
                        .expect("execute cloud-migrate source");
                    let lines: Vec<&str> = vm.output().lines().collect();
                    assert_eq!(lines.len(), 3, "unexpected output: {}", vm.output());
                    let applied: usize = lines[0].parse().expect("applied count");
                    let tables: usize = lines[2].parse().expect("table count");
                    assert!(applied > 0, "no migrations applied: {}", vm.output());
                    assert!(
                        tables >= applied,
                        "fewer tables than migrations applied: tables={tables}, applied={applied}",
                    );
                })
                .await;
        });
    }

    /// Build a synthetic SQLx-style migrations directory (with `.up.sql`
    /// and ignorable `.down.sql` siblings) and return (tmpdir, dir-string).
    /// Keep the `TempDir` alive for the duration of the test.
    fn sqlx_synthetic_migrations() -> (tempfile::TempDir, String) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path();
        let files: &[(&str, &str)] = &[
            (
                "20260419170000_bootstrap.up.sql",
                "CREATE TABLE widgets (id INT PRIMARY KEY, label TEXT NOT NULL)",
            ),
            ("20260419170000_bootstrap.down.sql", "DROP TABLE widgets"),
            (
                "20260423100000_seed_widget.up.sql",
                "INSERT INTO widgets (id, label) VALUES (1, 'alpha')",
            ),
            (
                "20260423100000_seed_widget.down.sql",
                "DELETE FROM widgets WHERE id = 1",
            ),
            (
                "20260424000000_add_gadgets.up.sql",
                "CREATE TABLE gadgets (id INT PRIMARY KEY)",
            ),
            ("20260424000000_add_gadgets.down.sql", "DROP TABLE gadgets"),
        ];
        for (name, body) in files {
            std::fs::write(dir.join(name), body).unwrap();
        }
        let s = dir.to_string_lossy().into_owned();
        (tmp, s)
    }

    fn run_harn_source(source: &str) -> String {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let local = tokio::task::LocalSet::new();
            local
                .run_until(async {
                    let chunk = compile_source(source).expect("compile source");
                    let mut vm = Vm::new();
                    register_vm_stdlib(&mut vm);
                    vm.execute(&chunk).await.expect("execute source");
                    vm.output().to_string()
                })
                .await
        })
    }

    fn run_harn_source_expect_err(source: &str) -> String {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let local = tokio::task::LocalSet::new();
            local
                .run_until(async {
                    let chunk = compile_source(source).expect("compile source");
                    let mut vm = Vm::new();
                    register_vm_stdlib(&mut vm);
                    let err = vm
                        .execute(&chunk)
                        .await
                        .expect_err("expected source to error");
                    format!("{err:?}")
                })
                .await
        })
    }

    /// SQLx ledger mode applies all forward files into `_sqlx_migrations`
    /// with the exact 6-column schema, 48-byte SHA-384 checksums, and
    /// `success = true`.
    #[test]
    fn migrate_sqlx_applies_into_sqlx_migrations_table_when_env_url_is_set() {
        if std::env::var("HARN_TEST_POSTGRES_URL").is_err() {
            return;
        }
        reset_postgres_state();
        let (_tmp, dir) = sqlx_synthetic_migrations();
        let schema = format!("harn_pg_sqlx_{}", uuid::Uuid::new_v4().simple());
        let source = format!(
            r#"
import "std/postgres"

let admin = pg_pool("env:HARN_TEST_POSTGRES_URL", {{max_connections: 1}})
pg_execute(admin, "DROP SCHEMA IF EXISTS \"{schema}\" CASCADE", [])
pg_execute(admin, "CREATE SCHEMA \"{schema}\"", [])
pg_close(admin)

let db = pg_pool("env:HARN_TEST_POSTGRES_URL", {{max_connections: 1}})
pg_execute(db, "SET search_path TO \"{schema}\"", [])

let result = pg_migrate(db, {{dir: "{dir}", ledger: "sqlx"}})
__io_println(len(result.applied))
__io_println(len(result.available))
__io_println(result.table)

let cols = pg_query(db, "SELECT column_name FROM information_schema.columns WHERE table_schema=$1 AND table_name='_sqlx_migrations' ORDER BY column_name", ["{schema}"])
__io_println(len(cols))

let rows = pg_query_one(db, "SELECT count(*)::int8 AS c FROM _sqlx_migrations", [])
__io_println(rows.c)

let badlen = pg_query_one(db, "SELECT count(*)::int8 AS c FROM _sqlx_migrations WHERE octet_length(checksum) <> 48", [])
__io_println(badlen.c)

let failed = pg_query_one(db, "SELECT count(*)::int8 AS c FROM _sqlx_migrations WHERE success = false", [])
__io_println(failed.c)

let versions = pg_query(db, "SELECT version FROM _sqlx_migrations ORDER BY version", [])
__io_println(len(versions))

pg_execute(db, "DROP SCHEMA \"{schema}\" CASCADE", [])
pg_close(db)
"#,
        );
        let out = run_harn_source(&source);
        let lines: Vec<&str> = out.lines().collect();
        // applied=3, available=3, table name, 6 columns, 3 rows, 0 bad
        // checksum lengths, 0 failed, 3 versions.
        assert_eq!(
            lines,
            vec!["3", "3", "_sqlx_migrations", "6", "3", "0", "0", "3"],
            "unexpected output: {out}"
        );
    }

    /// SQLx ledger mode is idempotent: a second run applies 0, skips all,
    /// and leaves the row count unchanged.
    #[test]
    fn migrate_sqlx_is_idempotent_when_env_url_is_set() {
        if std::env::var("HARN_TEST_POSTGRES_URL").is_err() {
            return;
        }
        reset_postgres_state();
        let (_tmp, dir) = sqlx_synthetic_migrations();
        let schema = format!("harn_pg_sqlxidem_{}", uuid::Uuid::new_v4().simple());
        let source = format!(
            r#"
import "std/postgres"

let admin = pg_pool("env:HARN_TEST_POSTGRES_URL", {{max_connections: 1}})
pg_execute(admin, "DROP SCHEMA IF EXISTS \"{schema}\" CASCADE", [])
pg_execute(admin, "CREATE SCHEMA \"{schema}\"", [])
pg_close(admin)

let db = pg_pool("env:HARN_TEST_POSTGRES_URL", {{max_connections: 1}})
pg_execute(db, "SET search_path TO \"{schema}\"", [])

let first = pg_migrate(db, {{dir: "{dir}", ledger: "sqlx"}})
__io_println(len(first.applied))
__io_println(len(first.skipped))

let count1 = pg_query_one(db, "SELECT count(*)::int8 AS c FROM _sqlx_migrations", [])
__io_println(count1.c)

let second = pg_migrate(db, {{dir: "{dir}", ledger: "sqlx"}})
__io_println(len(second.applied))
__io_println(len(second.skipped))

let count2 = pg_query_one(db, "SELECT count(*)::int8 AS c FROM _sqlx_migrations", [])
__io_println(count2.c)

pg_execute(db, "DROP SCHEMA \"{schema}\" CASCADE", [])
pg_close(db)
"#,
        );
        let out = run_harn_source(&source);
        let lines: Vec<&str> = out.lines().collect();
        // first: applied 3 skipped 0, count 3; second: applied 0 skipped 3,
        // count still 3.
        assert_eq!(
            lines,
            vec!["3", "0", "3", "0", "3", "3"],
            "unexpected output: {out}"
        );
    }

    /// No-fork against a "real" SQLx ledger: pre-seed `_sqlx_migrations`
    /// with rows whose checksums are computed the SAME way SQLx does
    /// (SHA-384 of the file body) — exactly what `sqlx migrate run` would
    /// have written — then run `pg_migrate(ledger: "sqlx")`. It must apply
    /// 0 and the checksums stay byte-identical.
    #[test]
    fn migrate_sqlx_no_fork_against_preseeded_ledger_when_env_url_is_set() {
        if std::env::var("HARN_TEST_POSTGRES_URL").is_err() {
            return;
        }
        reset_postgres_state();
        let (_tmp, dir) = sqlx_synthetic_migrations();

        // Compute SHA-384 the same way sqlx (and our runner) does: over the
        // file body read as a string.
        let checksum_hex = |name: &str| -> String {
            use sha2::{Digest, Sha384};
            let body =
                std::fs::read_to_string(std::path::Path::new(&dir).join(name)).expect("read file");
            let digest = Sha384::digest(body.as_bytes());
            digest.iter().map(|b| format!("{b:02x}")).collect()
        };
        let bootstrap_sum = checksum_hex("20260419170000_bootstrap.up.sql");
        let seed_sum = checksum_hex("20260423100000_seed_widget.up.sql");
        let gadgets_sum = checksum_hex("20260424000000_add_gadgets.up.sql");

        let schema = format!("harn_pg_sqlxnofork_{}", uuid::Uuid::new_v4().simple());
        let source = format!(
            r#"
import "std/postgres"

let admin = pg_pool("env:HARN_TEST_POSTGRES_URL", {{max_connections: 1}})
pg_execute(admin, "DROP SCHEMA IF EXISTS \"{schema}\" CASCADE", [])
pg_execute(admin, "CREATE SCHEMA \"{schema}\"", [])
pg_close(admin)

let db = pg_pool("env:HARN_TEST_POSTGRES_URL", {{max_connections: 1}})
pg_execute(db, "SET search_path TO \"{schema}\"", [])

// Replicate exactly what `sqlx migrate run` would have written, including
// the schema and the three rows with SHA-384 checksums, then create the
// objects those migrations would have created.
pg_execute(db, "CREATE TABLE _sqlx_migrations (version BIGINT PRIMARY KEY, description TEXT NOT NULL, installed_on TIMESTAMPTZ NOT NULL DEFAULT now(), success BOOLEAN NOT NULL, checksum BYTEA NOT NULL, execution_time BIGINT NOT NULL)", [])
pg_execute(db, "CREATE TABLE widgets (id INT PRIMARY KEY, label TEXT NOT NULL)", [])
pg_execute(db, "INSERT INTO widgets (id, label) VALUES (1, 'alpha')", [])
pg_execute(db, "CREATE TABLE gadgets (id INT PRIMARY KEY)", [])
pg_execute(db, "INSERT INTO _sqlx_migrations (version, description, success, checksum, execution_time) VALUES (20260419170000, 'bootstrap', TRUE, decode('{bootstrap_sum}', 'hex'), 1)", [])
pg_execute(db, "INSERT INTO _sqlx_migrations (version, description, success, checksum, execution_time) VALUES (20260423100000, 'seed widget', TRUE, decode('{seed_sum}', 'hex'), 1)", [])
pg_execute(db, "INSERT INTO _sqlx_migrations (version, description, success, checksum, execution_time) VALUES (20260424000000, 'add gadgets', TRUE, decode('{gadgets_sum}', 'hex'), 1)", [])

let before = pg_query_one(db, "SELECT md5(string_agg(encode(checksum,'hex'), ',' ORDER BY version)) AS h FROM _sqlx_migrations", [])

let result = pg_migrate(db, {{dir: "{dir}", ledger: "sqlx"}})
__io_println(len(result.applied))
__io_println(len(result.skipped))

let after = pg_query_one(db, "SELECT md5(string_agg(encode(checksum,'hex'), ',' ORDER BY version)) AS h FROM _sqlx_migrations", [])
if before.h == after.h {{ __io_println("checksums-identical") }} else {{ __io_println("checksums-CHANGED") }}

let count = pg_query_one(db, "SELECT count(*)::int8 AS c FROM _sqlx_migrations", [])
__io_println(count.c)

pg_execute(db, "DROP SCHEMA \"{schema}\" CASCADE", [])
pg_close(db)
"#,
        );
        let out = run_harn_source(&source);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(
            lines,
            vec!["0", "3", "checksums-identical", "3"],
            "unexpected output: {out}"
        );
    }

    /// Checksum-mismatch detection: corrupt one recorded checksum then run;
    /// the runner must error and name the offending version.
    #[test]
    fn migrate_sqlx_detects_checksum_mismatch_when_env_url_is_set() {
        if std::env::var("HARN_TEST_POSTGRES_URL").is_err() {
            return;
        }
        reset_postgres_state();
        let (_tmp, dir) = sqlx_synthetic_migrations();
        let schema = format!("harn_pg_sqlxmismatch_{}", uuid::Uuid::new_v4().simple());
        let source = format!(
            r#"
import "std/postgres"

let admin = pg_pool("env:HARN_TEST_POSTGRES_URL", {{max_connections: 1}})
pg_execute(admin, "DROP SCHEMA IF EXISTS \"{schema}\" CASCADE", [])
pg_execute(admin, "CREATE SCHEMA \"{schema}\"", [])
pg_close(admin)

let db = pg_pool("env:HARN_TEST_POSTGRES_URL", {{max_connections: 1}})
pg_execute(db, "SET search_path TO \"{schema}\"", [])

let first = pg_migrate(db, {{dir: "{dir}", ledger: "sqlx"}})
__io_println(len(first.applied))

// Corrupt the recorded checksum for the first migration.
pg_execute(db, "UPDATE _sqlx_migrations SET checksum = decode('deadbeef', 'hex') WHERE version = 20260419170000", [])

let second = pg_migrate(db, {{dir: "{dir}", ledger: "sqlx"}})
__io_println(len(second.applied))

pg_execute(db, "DROP SCHEMA \"{schema}\" CASCADE", [])
pg_close(db)
"#,
        );
        let err = run_harn_source_expect_err(&source);
        assert!(
            err.contains("checksum mismatch") && err.contains("20260419170000"),
            "expected checksum-mismatch error naming the version, got: {err}"
        );
    }

    /// Dirty-ledger detection: a `success = false` row blocks the run.
    #[test]
    fn migrate_sqlx_detects_dirty_ledger_when_env_url_is_set() {
        if std::env::var("HARN_TEST_POSTGRES_URL").is_err() {
            return;
        }
        reset_postgres_state();
        let (_tmp, dir) = sqlx_synthetic_migrations();
        let schema = format!("harn_pg_sqlxdirty_{}", uuid::Uuid::new_v4().simple());
        let source = format!(
            r#"
import "std/postgres"

let admin = pg_pool("env:HARN_TEST_POSTGRES_URL", {{max_connections: 1}})
pg_execute(admin, "DROP SCHEMA IF EXISTS \"{schema}\" CASCADE", [])
pg_execute(admin, "CREATE SCHEMA \"{schema}\"", [])
pg_close(admin)

let db = pg_pool("env:HARN_TEST_POSTGRES_URL", {{max_connections: 1}})
pg_execute(db, "SET search_path TO \"{schema}\"", [])

pg_execute(db, "CREATE TABLE _sqlx_migrations (version BIGINT PRIMARY KEY, description TEXT NOT NULL, installed_on TIMESTAMPTZ NOT NULL DEFAULT now(), success BOOLEAN NOT NULL, checksum BYTEA NOT NULL, execution_time BIGINT NOT NULL)", [])
pg_execute(db, "INSERT INTO _sqlx_migrations (version, description, success, checksum, execution_time) VALUES (20260419170000, 'bootstrap', FALSE, decode('deadbeef', 'hex'), -1)", [])

let result = pg_migrate(db, {{dir: "{dir}", ledger: "sqlx"}})
__io_println(len(result.applied))

pg_execute(db, "DROP SCHEMA \"{schema}\" CASCADE", [])
pg_close(db)
"#,
        );
        let err = run_harn_source_expect_err(&source);
        assert!(
            err.contains("dirty migration") && err.contains("20260419170000"),
            "expected dirty-ledger error naming the version, got: {err}"
        );
    }

    /// SQLx ledger mode against the canonical harn-cloud `migrations/`
    /// directory: applies the full forward history, then a second run is a
    /// no-op (every version skipped, no checksum drift). This is the
    /// retire-`migrations.rs` acceptance test. Opt-in via
    /// `HARN_TEST_CLOUD_MIGRATIONS_DIR`.
    #[test]
    fn migrate_sqlx_applies_real_cloud_dir_and_is_idempotent_when_env_set() {
        if std::env::var("HARN_TEST_POSTGRES_URL").is_err() {
            return;
        }
        let Ok(dir) = std::env::var("HARN_TEST_CLOUD_MIGRATIONS_DIR") else {
            return;
        };
        if !std::path::Path::new(&dir).exists() {
            return;
        }
        reset_postgres_state();
        let schema = format!("harn_pg_sqlxcloud_{}", uuid::Uuid::new_v4().simple());
        let source = format!(
            r#"
import "std/postgres"

let admin = pg_pool("env:HARN_TEST_POSTGRES_URL", {{max_connections: 1}})
pg_execute(admin, "DROP SCHEMA IF EXISTS \"{schema}\" CASCADE", [])
pg_execute(admin, "CREATE SCHEMA \"{schema}\"", [])
pg_close(admin)

let db = pg_pool("env:HARN_TEST_POSTGRES_URL", {{max_connections: 1}})
pg_execute(db, "SET search_path TO \"{schema}\"", [])

let first = pg_migrate(db, {{dir: "{dir}", ledger: "sqlx"}})
__io_println(len(first.applied))
__io_println(len(first.skipped))

let count1 = pg_query_one(db, "SELECT count(*)::int8 AS c FROM _sqlx_migrations", [])
__io_println(count1.c)

let badlen = pg_query_one(db, "SELECT count(*)::int8 AS c FROM _sqlx_migrations WHERE octet_length(checksum) <> 48", [])
__io_println(badlen.c)

let second = pg_migrate(db, {{dir: "{dir}", ledger: "sqlx"}})
__io_println(len(second.applied))
__io_println(len(second.skipped))

pg_execute(db, "DROP SCHEMA \"{schema}\" CASCADE", [])
pg_close(db)
"#,
        );
        let out = run_harn_source(&source);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 6, "unexpected output: {out}");
        let applied: usize = lines[0].parse().expect("applied count");
        let skipped_first: usize = lines[1].parse().expect("skipped count");
        let count: usize = lines[2].parse().expect("row count");
        let bad_checksums: usize = lines[3].parse().expect("bad checksum count");
        let applied_second: usize = lines[4].parse().expect("second applied");
        let skipped_second: usize = lines[5].parse().expect("second skipped");
        assert!(applied > 0, "no migrations applied: {out}");
        assert_eq!(skipped_first, 0, "first run should skip nothing: {out}");
        assert_eq!(count, applied, "ledger rows != applied: {out}");
        assert_eq!(bad_checksums, 0, "all checksums must be 48 bytes: {out}");
        assert_eq!(applied_second, 0, "second run must apply nothing: {out}");
        assert_eq!(
            skipped_second, applied,
            "second run must skip everything: {out}"
        );
    }

    /// Confirms `duration_ms` lives on every real execute result. The
    /// synthetic `Instant` path is covered by
    /// `execute_result_value_includes_duration`.
    #[test]
    fn execute_reports_duration_ms_on_real_pool_when_env_url_is_set() {
        if std::env::var("HARN_TEST_POSTGRES_URL").is_err() {
            return;
        }
        reset_postgres_state();
        let source = r#"
import "std/postgres"

let db = pg_pool("env:HARN_TEST_POSTGRES_URL", {max_connections: 1})
let result = pg_execute(db, "SELECT pg_sleep(0.05)", [])
__io_println(result.duration_ms)
pg_close(db)
"#;
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let local = tokio::task::LocalSet::new();
            local
                .run_until(async {
                    let chunk = compile_source(source).expect("compile duration source");
                    let mut vm = Vm::new();
                    register_vm_stdlib(&mut vm);
                    vm.execute(&chunk).await.expect("execute duration source");
                    let duration_ms: i64 = vm
                        .output()
                        .trim()
                        .parse()
                        .unwrap_or_else(|_| panic!("expected int, got `{}`", vm.output()));
                    assert!(duration_ms >= 50, "expected ≥50ms, got {duration_ms}");
                })
                .await;
        });
    }

    /// End-to-end smoke for the v2 surface against a real Postgres:
    /// pool stats reflect live connections, advisory locks succeed inside
    /// a transaction, schema introspection finds the test table, an
    /// `int[]` column round-trips through the array decoder, and
    /// LISTEN/NOTIFY delivers the payload back through pg_listener_recv.
    #[test]
    fn v2_surface_smoke_when_env_url_is_set() {
        if std::env::var("HARN_TEST_POSTGRES_URL").is_err() {
            return;
        }
        reset_postgres_state();
        let schema = format!("harn_pg_v2_{}", uuid::Uuid::new_v4().simple());
        let source = format!(
            r#"
import "std/postgres"

let db = pg_pool("env:HARN_TEST_POSTGRES_URL", {{max_connections: 2}})

// --- Pool observability --------------------------------------------------
let stats = pg_pool_stats(db)
__io_println(stats.circuit_state)
__io_println(stats.max_connections)
__io_println(stats.read_routing_policy)
__io_println(stats.replicas)

let clear_result = pg_stmt_cache_clear(db)
__io_println(clear_result.pools)
__io_println(clear_result.connections_cleared >= 1)
__io_println(clear_result.connections_skipped)

// --- Schema setup --------------------------------------------------------
pg_execute(db, "CREATE SCHEMA IF NOT EXISTS \"{schema}\"", [])
pg_execute(db, "SET search_path TO \"{schema}\"", [])
pg_execute(db, "CREATE TABLE widgets (id int4 PRIMARY KEY, tags text[] NOT NULL DEFAULT '{{}}')", [])
pg_execute(db, "CREATE UNIQUE INDEX widgets_id_uniq ON widgets (id)", [])
pg_execute(db, "INSERT INTO widgets (id, tags) VALUES (1, ARRAY['alpha','beta'])", [])
pg_execute(db, "INSERT INTO widgets (id, tags) VALUES (2, ARRAY[]::text[])", [])

// --- Advisory lock inside a transaction ----------------------------------
let locked_label = pg_transaction(db, {{ tx ->
  pg_advisory_xact_lock(tx, 0x4861_726E_5632_AABB)
  return pg_query_one(tx, "SELECT 'locked' AS label", []).label
}})
__io_println(locked_label)

// --- pg_with_advisory_lock (RAII helper, exercises run_managed_transaction) ----
let with_label = pg_with_advisory_lock(db, "release-cut", {{ tx ->
  return pg_query_one(tx, "SELECT 'raii' AS label", []).label
}})
__io_println(with_label)

// --- Schema introspection ------------------------------------------------
let tables = pg_introspect_tables(db, {{schema: "{schema}"}})
__io_println(len(tables))
__io_println(tables[0].kind)

let cols = pg_introspect_columns(db, "{schema}.widgets")
__io_println(len(cols))
__io_println(cols[0].column + ":" + cols[0].type)
__io_println(cols[1].column + ":" + cols[1].type)

let idx = pg_introspect_indexes(db, "{schema}.widgets")
__io_println(len(idx))

// --- Array decoding ------------------------------------------------------
let row = pg_query_one(db, "SELECT tags FROM widgets WHERE id = $1", [1])
__io_println(row.tags[0] + "," + row.tags[1])

let empty = pg_query_one(db, "SELECT tags FROM widgets WHERE id = $1", [2])
__io_println(len(empty.tags))

// --- LISTEN/NOTIFY round-trip --------------------------------------------
let listener = pg_listen(db, "harn_v2_test")
pg_notify(db, "harn_v2_test", "hello")
let notification = pg_listener_recv(listener, 5000)
__io_println(notification.channel + ":" + notification.payload)
pg_listener_close(listener)

// --- Teardown ------------------------------------------------------------
pg_execute(db, "DROP SCHEMA \"{schema}\" CASCADE", [])
pg_close(db)
"#,
        );
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let local = tokio::task::LocalSet::new();
            local
                .run_until(async {
                    let chunk = compile_source(&source).expect("compile v2 smoke source");
                    let mut vm = Vm::new();
                    register_vm_stdlib(&mut vm);
                    vm.execute(&chunk).await.expect("execute v2 smoke source");
                    let lines: Vec<&str> = vm.output().lines().collect();
                    // Expected (in order):
                    //   disabled            // circuit_state
                    //   2                   // max_connections
                    //   replica_or_primary  // read_routing_policy
                    //   0                   // replicas
                    //   1                   // primary pool cache clear
                    //   true                // at least one idle connection cleared
                    //   0                   // no checked-out connections skipped
                    //   locked              // pg_advisory_xact_lock path label
                    //   raii                // pg_with_advisory_lock path label
                    //   1                   // tables in schema
                    //   table               // kind
                    //   2                   // columns count
                    //   id:int4             // column 0 type
                    //   tags:_text          // column 1 type (PG type is _text)
                    //   2                   // PK + explicit UNIQUE indexes
                    //   alpha,beta          // array decoding
                    //   0                   // empty array length
                    //   harn_v2_test:hello  // notification
                    assert_eq!(lines[0], "disabled");
                    assert_eq!(lines[1], "2");
                    assert_eq!(lines[2], "replica_or_primary");
                    assert_eq!(lines[3], "0");
                    assert_eq!(lines[4], "1");
                    assert_eq!(lines[5], "true");
                    assert_eq!(lines[6], "0");
                    assert_eq!(lines[7], "locked");
                    assert_eq!(lines[8], "raii");
                    assert_eq!(lines[9], "1");
                    assert_eq!(lines[10], "table");
                    assert_eq!(lines[11], "2");
                    assert_eq!(lines[12], "id:int4");
                    assert!(
                        lines[13] == "tags:_text" || lines[13] == "tags:text[]",
                        "tags column type unexpected: {}",
                        lines[13]
                    );
                    // PK index + the explicit UNIQUE = 2 indexes
                    assert_eq!(lines[14], "2");
                    assert_eq!(lines[15], "alpha,beta");
                    assert_eq!(lines[16], "0");
                    assert_eq!(lines[17], "harn_v2_test:hello");
                })
                .await;
        });
    }

    /// Advisory locks must isolate distinct tenants when
    /// `tenant_namespace: true` is set: the same caller-supplied key
    /// resolves to *different* server-side lock keys per tenant. Without
    /// that, two tenants would deadlock each other for routine
    /// per-resource locks.
    #[test]
    fn advisory_lock_tenant_namespacing_keys_differ_per_tenant() {
        use crate::harness_tenant::enter_tenant;
        use crate::TenantId;

        reset_postgres_state();
        let key_a = {
            let _g = enter_tenant(TenantId::new("tenant-a"));
            super::advisory::tenant_salt_for_test()
        };
        let key_b = {
            let _g = enter_tenant(TenantId::new("tenant-b"));
            super::advisory::tenant_salt_for_test()
        };
        let key_none = super::advisory::tenant_salt_for_test();
        assert_ne!(key_a, key_b, "same salt for distinct tenants");
        assert_eq!(key_none, 0, "no-tenant scope should produce zero salt");
        assert_ne!(key_a, 0);
    }
}
