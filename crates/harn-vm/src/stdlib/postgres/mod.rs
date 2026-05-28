use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use sqlx_core::column::Column;
use sqlx_core::query::{query, Query};
use sqlx_core::row::Row;
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

type PgTxCell = Rc<Mutex<Option<Transaction<'static, Postgres>>>>;
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
}

pub(crate) const MODULE_BUILTINS: &[&VmBuiltinDef] = &[
    // Core (v1 — issue #2500)
    &PG_POOL_IMPL_DEF,
    &PG_CONNECT_IMPL_DEF,
    &PG_CLOSE_IMPL_DEF,
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
    // Schema introspection + pool observability + partitions (v2)
    &introspect::PG_INTROSPECT_TABLES_IMPL_DEF,
    &introspect::PG_INTROSPECT_COLUMNS_IMPL_DEF,
    &introspect::PG_INTROSPECT_INDEXES_IMPL_DEF,
    &introspect::PG_POOL_STATS_IMPL_DEF,
    &introspect::PG_PARTITION_ATTACH_IMPL_DEF,
    &introspect::PG_PARTITION_DETACH_IMPL_DEF,
    &introspect::PG_PARTITION_PRUNE_IMPL_DEF,
];

mod advisory;
mod circuit;
mod introspect;
mod listen;
mod migrate;

#[harn_builtin(
    sig_expr = BuiltinSignature::variadic("pg_pool", &[Param::new("args", TY_ANY)], TY_DICT),
    kind = "async",
    category = "postgres"
)]
async fn pg_pool_impl(args: Vec<VmValue>) -> Result<VmValue, VmError> {
    let source = args.first().ok_or_else(|| {
        runtime_error("pg_pool: url, env:, secret:, or {url|env|secret} is required")
    })?;
    let options = args.get(1).and_then(VmValue::as_dict).cloned();
    open_pool(source, options.as_ref(), false).await
}

#[harn_builtin(
    sig_expr = BuiltinSignature::variadic("pg_connect", &[Param::new("args", TY_ANY)], TY_DICT),
    kind = "async",
    category = "postgres"
)]
async fn pg_connect_impl(args: Vec<VmValue>) -> Result<VmValue, VmError> {
    let source = args.first().ok_or_else(|| {
        runtime_error("pg_connect: url, env:, secret:, or {url|env|secret} is required")
    })?;
    let options = args.get(1).and_then(VmValue::as_dict).cloned();
    open_pool(source, options.as_ref(), true).await
}

#[harn_builtin(
    sig_expr = BuiltinSignature::variadic("pg_close", &[Param::new("args", TY_ANY)], TY_BOOL),
    kind = "async",
    category = "postgres"
)]
async fn pg_close_impl(args: Vec<VmValue>) -> Result<VmValue, VmError> {
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
    sig_expr = BuiltinSignature::variadic("pg_query", &[Param::new("args", TY_ANY)], TY_LIST),
    kind = "async",
    category = "postgres"
)]
async fn pg_query_impl(args: Vec<VmValue>) -> Result<VmValue, VmError> {
    let target = args
        .first()
        .ok_or_else(|| runtime_error("pg_query: pool or transaction handle is required"))?;
    let sql = required_string_arg(&args, 1, "pg_query", "sql")?;
    let params = params_arg(args.get(2), "pg_query")?;
    let options = args.get(3).and_then(VmValue::as_dict);
    let routing = routing_from_options(options);
    query_many(target, &sql, &params, routing).await
}

#[harn_builtin(
    sig_expr = BuiltinSignature::variadic("pg_query_one", &[Param::new("args", TY_ANY)], TY_ANY),
    kind = "async",
    category = "postgres"
)]
async fn pg_query_one_impl(args: Vec<VmValue>) -> Result<VmValue, VmError> {
    let target = args
        .first()
        .ok_or_else(|| runtime_error("pg_query_one: pool or transaction handle is required"))?;
    let sql = required_string_arg(&args, 1, "pg_query_one", "sql")?;
    let params = params_arg(args.get(2), "pg_query_one")?;
    let options = args.get(3).and_then(VmValue::as_dict);
    let routing = routing_from_options(options);
    let rows = query_rows(target, &sql, &params, routing).await?;
    Ok(rows.into_iter().next().unwrap_or(VmValue::Nil))
}

#[harn_builtin(
    sig_expr = BuiltinSignature::variadic("pg_execute", &[Param::new("args", TY_ANY)], TY_DICT),
    kind = "async",
    category = "postgres"
)]
async fn pg_execute_impl(args: Vec<VmValue>) -> Result<VmValue, VmError> {
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
async fn pg_transaction_impl(args: Vec<VmValue>) -> Result<VmValue, VmError> {
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
    run_managed_transaction(&pool_id, "pg_transaction", closure, move |tx_id| {
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
    pool_id: &str,
    builtin: &'static str,
    closure: Rc<crate::value::VmClosure>,
    prepare: impl FnOnce(
        &str,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<(), VmError>> + '_>,
    >,
) -> Result<VmValue, VmError> {
    let pool = pool_by_id(pool_id)?;
    let tx = pool
        .begin()
        .await
        .map_err(|error| runtime_error(format!("{builtin}: begin failed: {error}")))?;
    let tx_id = next_id("pgtx");
    let tx_cell = Rc::new(Mutex::new(Some(tx)));
    register_tx(&tx_id, Rc::clone(&tx_cell));
    let tx_handle = handle_value(HANDLE_TX, &tx_id, BTreeMap::new());

    if let Err(error) = prepare(&tx_id).await {
        unregister_tx(&tx_id);
        if let Some(tx) = tx_cell.lock().await.take() {
            let _ = tx.rollback().await;
        }
        return Err(error);
    }

    let mut child_vm = crate::vm::clone_async_builtin_child_vm()
        .ok_or_else(|| runtime_error(format!("{builtin}: requires VM execution context")))?;
    let result = child_vm.call_closure_pub(&closure, &[tx_handle]).await;

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
async fn pg_savepoint_impl(args: Vec<VmValue>) -> Result<VmValue, VmError> {
    savepoint_op(&args, "pg_savepoint", SavepointOp::Create).await
}

#[harn_builtin(
    sig_expr = BuiltinSignature::variadic("pg_release_savepoint", &[Param::new("args", TY_ANY)], TY_BOOL),
    kind = "async",
    category = "postgres"
)]
async fn pg_release_savepoint_impl(args: Vec<VmValue>) -> Result<VmValue, VmError> {
    savepoint_op(&args, "pg_release_savepoint", SavepointOp::Release).await
}

#[harn_builtin(
    sig_expr = BuiltinSignature::variadic("pg_rollback_to_savepoint", &[Param::new("args", TY_ANY)], TY_BOOL),
    kind = "async",
    category = "postgres"
)]
async fn pg_rollback_to_savepoint_impl(args: Vec<VmValue>) -> Result<VmValue, VmError> {
    savepoint_op(&args, "pg_rollback_to_savepoint", SavepointOp::RollbackTo).await
}

#[harn_builtin(
    sig_expr = BuiltinSignature::variadic("pg_migrate", &[Param::new("args", TY_ANY)], TY_DICT),
    kind = "async",
    category = "postgres"
)]
async fn pg_migrate_impl(args: Vec<VmValue>) -> Result<VmValue, VmError> {
    migrate::run(&args).await
}

#[harn_builtin(
    sig_expr = BuiltinSignature::variadic("pg_mock_pool", &[Param::new("args", TY_ANY)], TY_DICT),
    category = "postgres"
)]
fn pg_mock_pool_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let fixtures = match args.first() {
        Some(VmValue::List(items)) => parse_mock_fixtures(items)?,
        Some(VmValue::Dict(_)) => parse_mock_fixtures(&Rc::new(vec![args[0].clone()]))?,
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
    Ok(VmValue::List(Rc::new(calls)))
}

async fn open_pool(
    source: &VmValue,
    options: Option<&BTreeMap<String, VmValue>>,
    single_connection: bool,
) -> Result<VmValue, VmError> {
    let primary_url = resolve_connection_url(source).await?;
    let stmt_cache_capacity = option_int(options, "statement_cache_capacity")
        .map(|n| n.max(0) as usize)
        .unwrap_or(DEFAULT_STATEMENT_CACHE_CAPACITY);
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

    let replica_urls = collect_replica_urls(options).await?;
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
    if let Some(application_name) = option_string(options, "application_name") {
        meta.insert(
            "application_name".to_string(),
            VmValue::String(Rc::from(application_name)),
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
        urls.push(resolve_connection_url(entry).await?);
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
    Ok(VmValue::List(Rc::new(rows)))
}

pub(super) async fn query_rows(
    target: &VmValue,
    sql: &str,
    params: &[VmValue],
    routing: QueryRouting,
) -> Result<Vec<VmValue>, VmError> {
    match handle_kind(target).as_deref() {
        Some(HANDLE_MOCK) => return mock_query(target, sql, params, false),
        Some(HANDLE_TX) => {
            let id = handle_id(Some(target), HANDLE_TX, "pg_query")?;
            let tx = tx_by_id(&id)?;
            let mut tx = tx.lock().await;
            let tx = tx
                .as_mut()
                .ok_or_else(|| runtime_error("pg_query: transaction is closed"))?;
            let query = bind_params(query(sql), params);
            let rows = query
                .fetch_all(&mut **tx)
                .await
                .map_err(|error| runtime_error(format!("pg_query: {error}")))?;
            return rows.into_iter().map(row_to_value).collect();
        }
        _ => {}
    }

    let record = pool_record_from_handle(target, "pg_query")?;
    let pool = pool_for_routing(&record, routing);
    let (probe, _) = enter_circuit(&record.circuit, "pg_query")?;
    let query = bind_params(query(sql), params);
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
        let result = bind_params(query(sql), params)
            .execute(&mut **tx)
            .await
            .map_err(|error| runtime_error(format!("pg_execute: {error}")))?;
        return Ok(query_result_value(result, started.elapsed()));
    }
    let record = pool_record_from_handle(target, "pg_execute")?;
    let (probe, _) = enter_circuit(&record.circuit, "pg_execute")?;
    let result = bind_params(query(sql), params)
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
    sqlx_core::raw_sql::raw_sql(&sql)
        .execute(&mut **tx)
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
            VmValue::String(Rc::from(key.as_str())),
            VmValue::String(Rc::from(value.display())),
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
    Ok(VmValue::Dict(Rc::new(map)))
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
        "NUMERIC" => VmValue::String(Rc::from(
            row.try_get::<rust_decimal::Decimal, _>(index)
                .map_err(decode_error)?
                .to_string(),
        )),
        "TEXT" | "VARCHAR" | "BPCHAR" | "NAME" => VmValue::String(Rc::from(
            row.try_get::<String, _>(index).map_err(decode_error)?,
        )),
        "UUID" => VmValue::String(Rc::from(
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
        "BYTEA" => VmValue::Bytes(Rc::new(
            row.try_get::<Vec<u8>, _>(index).map_err(decode_error)?,
        )),
        "DATE" => VmValue::String(Rc::from(
            row.try_get::<time::Date, _>(index)
                .map_err(decode_error)?
                .to_string(),
        )),
        "TIME" => VmValue::String(Rc::from(
            row.try_get::<time::Time, _>(index)
                .map_err(decode_error)?
                .to_string(),
        )),
        "TIMESTAMP" => VmValue::String(Rc::from(
            row.try_get::<time::PrimitiveDateTime, _>(index)
                .map_err(decode_error)?
                .to_string(),
        )),
        "TIMESTAMPTZ" => VmValue::String(Rc::from(
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
            decode_array::<String>(row, index, |v| VmValue::String(Rc::from(v)))?
        }
        "UUID[]" => {
            decode_array::<uuid::Uuid>(row, index, |v| VmValue::String(Rc::from(v.to_string())))?
        }
        "JSON[]" | "JSONB[]" => {
            let values: Vec<serde_json::Value> = row.try_get(index).map_err(decode_error)?;
            VmValue::List(Rc::new(
                values.iter().map(crate::stdlib::json_to_vm_value).collect(),
            ))
        }
        // HSTORE decodes as a Harn dict<string, string|nil>. sqlx surfaces
        // it as `BTreeMap<String, Option<String>>` already.
        "HSTORE" => {
            let map: sqlx_postgres::types::PgHstore = row.try_get(index).map_err(decode_error)?;
            let mut dict = BTreeMap::new();
            for (key, value) in map.0 {
                dict.insert(
                    key,
                    value
                        .map(|v| VmValue::String(Rc::from(v)))
                        .unwrap_or(VmValue::Nil),
                );
            }
            VmValue::Dict(Rc::new(dict))
        }
        // PG geometric POINT decodes as `{x, y}`. Other geometric types
        // (LINE, LSEG, BOX, PATH, POLYGON, CIRCLE) fall back to their
        // textual representation below — Harn callers can parse if
        // needed, but a structured decode for every geometric type is
        // niche enough to defer.
        "POINT" => {
            let point: sqlx_postgres::types::PgPoint = row.try_get(index).map_err(decode_error)?;
            let mut dict = BTreeMap::new();
            dict.insert("x".to_string(), VmValue::Float(point.x));
            dict.insert("y".to_string(), VmValue::Float(point.y));
            VmValue::Dict(Rc::new(dict))
        }
        _ => VmValue::String(Rc::from(row.try_get::<String, _>(index).map_err(
            |error| {
                runtime_error(format!(
                    "pg_query: unsupported column type {type_name}: {error}"
                ))
            },
        )?)),
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
    Ok(VmValue::List(Rc::new(
        values.into_iter().map(map).collect(),
    )))
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
    VmValue::Dict(Rc::new(map))
}

async fn resolve_connection_url(source: &VmValue) -> Result<String, VmError> {
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
                return secret_url(&secret.display()).await;
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
                secret_url(id).await
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

async fn secret_url(secret_id: &str) -> Result<String, VmError> {
    let mut child_vm = crate::vm::clone_async_builtin_child_vm()
        .ok_or_else(|| runtime_error("pg_pool: secret: references require VM execution context"))?;
    match child_vm
        .call_named_builtin(
            "secret_get",
            vec![VmValue::String(Rc::from(secret_id.trim().to_string()))],
        )
        .await?
    {
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
    extra.insert("_type".to_string(), VmValue::String(Rc::from(kind)));
    extra.insert("id".to_string(), VmValue::String(Rc::from(id.to_string())));
    VmValue::Dict(Rc::new(extra))
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

/// Round-robin replica picker. Returns the primary pool when no replicas
/// are configured or routing is `Primary`.
pub(super) fn pool_for_routing(record: &Arc<PoolRecord>, routing: QueryRouting) -> Arc<PgPool> {
    match routing {
        QueryRouting::Primary => Arc::clone(&record.pool),
        QueryRouting::ReadOnly => {
            if record.replicas.is_empty() {
                Arc::clone(&record.pool)
            } else {
                let idx =
                    record.replica_cursor.fetch_add(1, Ordering::Relaxed) % record.replicas.len();
                Arc::clone(&record.replicas[idx])
            }
        }
    }
}

/// Per-query routing policy.
#[derive(Clone, Copy)]
pub(super) enum QueryRouting {
    Primary,
    ReadOnly,
}

pub(super) fn routing_from_options(options: Option<&BTreeMap<String, VmValue>>) -> QueryRouting {
    if option_bool(options.and_then(|opts| opts.get("read_only"))) == Some(true) {
        QueryRouting::ReadOnly
    } else {
        QueryRouting::Primary
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
            VmValue::Float(number) => Some(*number as i64),
            _ => None,
        })
}

fn option_duration_ms(options: Option<&BTreeMap<String, VmValue>>, key: &str) -> Option<u64> {
    options
        .and_then(|opts| opts.get(key))
        .and_then(|value| match value {
            VmValue::Duration(ms) if *ms >= 0 => Some(*ms as u64),
            VmValue::Int(ms) if *ms >= 0 => Some(*ms as u64),
            VmValue::Float(ms) if *ms >= 0.0 => Some(*ms as u64),
            _ => None,
        })
}

pub(super) fn next_id(prefix: &str) -> String {
    format!("{prefix}-{}", NEXT_ID.fetch_add(1, Ordering::Relaxed))
}

pub(super) fn runtime_error(message: impl Into<String>) -> VmError {
    VmError::Runtime(message.into())
}

fn parse_mock_fixtures(items: &Rc<Vec<VmValue>>) -> Result<Vec<MockFixture>, VmError> {
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
        VmValue::String(Rc::from(value))
    }

    fn dict(pairs: &[(&str, VmValue)]) -> VmValue {
        VmValue::Dict(Rc::new(
            pairs
                .iter()
                .map(|(key, value)| ((*key).to_string(), value.clone()))
                .collect(),
        ))
    }

    #[test]
    fn mock_pool_matches_parameterized_query_and_records_calls() {
        reset_postgres_state();
        let fixtures = VmValue::List(Rc::new(vec![dict(&[
            ("sql", s("select * from claims where tenant_id = $1")),
            ("params", VmValue::List(Rc::new(vec![s("tenant-a")]))),
            (
                "rows",
                VmValue::List(Rc::new(vec![dict(&[("claim_id", s("c1"))])])),
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
        assert_eq!(VmValue::List(Rc::new(rows)).display(), "[{claim_id: c1}]");
        let calls = MOCKS.with(|mocks| mocks.borrow().values().next().unwrap().calls.clone());
        assert_eq!(calls.len(), 1);
    }

    #[test]
    fn mock_execute_returns_rows_affected() {
        reset_postgres_state();
        let fixtures = parse_mock_fixtures(&Rc::new(vec![dict(&[
            ("sql", s("update receipts set status = $1")),
            ("rows_affected", VmValue::Int(3)),
        ])]))
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
        let handle = open_pool(&s(&url), Some(&options), false).await.unwrap();
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
__io_println(stats.replicas)

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
                    //   disabled            # circuit_state
                    //   2                   # max_connections
                    //   0                   # replicas
                    //   locked              # advisory lock returned label
                    //   1                   # tables in schema
                    //   table               # kind
                    //   2                   # columns count
                    //   id:int4             # column 0 type
                    //   tags:_text          # column 1 type (PG type is _text)
                    //   1                   # one user-defined index (the explicit UNIQUE),
                    //                       # plus the PK index → expect 2 below
                    //   alpha,beta          # array decoding
                    //   0                   # empty array length
                    //   harn_v2_test:hello  # notification
                    assert_eq!(lines[0], "disabled");
                    assert_eq!(lines[1], "2");
                    assert_eq!(lines[2], "0");
                    assert_eq!(lines[3], "locked");
                    assert_eq!(lines[4], "1");
                    assert_eq!(lines[5], "table");
                    assert_eq!(lines[6], "2");
                    assert_eq!(lines[7], "id:int4");
                    assert!(
                        lines[8] == "tags:_text" || lines[8] == "tags:text[]",
                        "tags column type unexpected: {}",
                        lines[8]
                    );
                    // PK index + the explicit UNIQUE = 2 indexes
                    assert_eq!(lines[9], "2");
                    assert_eq!(lines[10], "alpha,beta");
                    assert_eq!(lines[11], "0");
                    assert_eq!(lines[12], "harn_v2_test:hello");
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
