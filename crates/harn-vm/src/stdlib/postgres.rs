use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};
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
use crate::value::{VmError, VmValue};
use crate::vm::Vm;

const HANDLE_POOL: &str = "pg_pool";
const HANDLE_TX: &str = "pg_tx";
const HANDLE_MOCK: &str = "pg_mock_pool";

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone)]
struct PoolRecord {
    pool: Arc<PgPool>,
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
    static POOLS: RefCell<BTreeMap<String, PoolRecord>> = const { RefCell::new(BTreeMap::new()) };
    static TXS: RefCell<PgTxRegistry> =
        const { RefCell::new(BTreeMap::new()) };
    static MOCKS: RefCell<BTreeMap<String, MockPool>> = const { RefCell::new(BTreeMap::new()) };
}

pub(crate) fn reset_postgres_state() {
    POOLS.with(|pools| pools.borrow_mut().clear());
    TXS.with(|txs| txs.borrow_mut().clear());
    MOCKS.with(|mocks| mocks.borrow_mut().clear());
}

pub(crate) fn register_postgres_builtins(vm: &mut Vm) {
    vm.register_async_builtin("pg_pool", |args| async move {
        let source = args.first().ok_or_else(|| {
            runtime_error("pg_pool: url, env:, secret:, or {url|env|secret} is required")
        })?;
        let options = args.get(1).and_then(VmValue::as_dict).cloned();
        open_pool(source, options.as_ref(), false).await
    });

    vm.register_async_builtin("pg_connect", |args| async move {
        let source = args.first().ok_or_else(|| {
            runtime_error("pg_connect: url, env:, secret:, or {url|env|secret} is required")
        })?;
        let options = args.get(1).and_then(VmValue::as_dict).cloned();
        open_pool(source, options.as_ref(), true).await
    });

    vm.register_async_builtin("pg_close", |args| async move {
        let id = handle_id(args.first(), HANDLE_POOL, "pg_close")?;
        let pool = POOLS.with(|pools| pools.borrow_mut().remove(&id).map(|record| record.pool));
        if let Some(pool) = pool {
            pool.close().await;
            Ok(VmValue::Bool(true))
        } else {
            Ok(VmValue::Bool(false))
        }
    });

    vm.register_async_builtin("pg_query", |args| async move {
        let target = args
            .first()
            .ok_or_else(|| runtime_error("pg_query: pool or transaction handle is required"))?;
        let sql = required_string_arg(&args, 1, "pg_query", "sql")?;
        let params = params_arg(args.get(2), "pg_query")?;
        query_many(target, &sql, &params).await
    });

    vm.register_async_builtin("pg_query_one", |args| async move {
        let target = args
            .first()
            .ok_or_else(|| runtime_error("pg_query_one: pool or transaction handle is required"))?;
        let sql = required_string_arg(&args, 1, "pg_query_one", "sql")?;
        let params = params_arg(args.get(2), "pg_query_one")?;
        let rows = query_rows(target, &sql, &params).await?;
        Ok(rows.into_iter().next().unwrap_or(VmValue::Nil))
    });

    vm.register_async_builtin("pg_execute", |args| async move {
        let target = args
            .first()
            .ok_or_else(|| runtime_error("pg_execute: pool or transaction handle is required"))?;
        let sql = required_string_arg(&args, 1, "pg_execute", "sql")?;
        let params = params_arg(args.get(2), "pg_execute")?;
        execute_stmt(target, &sql, &params).await
    });

    vm.register_async_builtin("pg_transaction", |args| async move {
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
        let pool = pool_by_id(&pool_id)?;
        let tx = pool
            .begin()
            .await
            .map_err(|error| runtime_error(format!("pg_transaction: begin failed: {error}")))?;
        let tx_id = next_id("pgtx");
        let tx_cell = Rc::new(Mutex::new(Some(tx)));
        TXS.with(|txs| {
            txs.borrow_mut().insert(tx_id.clone(), Rc::clone(&tx_cell));
        });
        let tx_handle = handle_value(HANDLE_TX, &tx_id, BTreeMap::new());

        if let Some(settings) = options
            .as_ref()
            .and_then(|opts| opts.get("settings"))
            .and_then(VmValue::as_dict)
        {
            apply_transaction_settings(&tx_id, settings).await?;
        }

        let mut child_vm = crate::vm::clone_async_builtin_child_vm()
            .ok_or_else(|| runtime_error("pg_transaction: requires VM execution context"))?;
        let result = child_vm.call_closure_pub(&closure, &[tx_handle]).await;

        TXS.with(|txs| {
            txs.borrow_mut().remove(&tx_id);
        });
        let tx = tx_cell.lock().await.take().ok_or_else(|| {
            runtime_error("pg_transaction: transaction handle was already consumed")
        })?;
        match result {
            Ok(value) => {
                tx.commit().await.map_err(|error| {
                    runtime_error(format!("pg_transaction: commit failed: {error}"))
                })?;
                Ok(value)
            }
            Err(error) => {
                let _ = tx.rollback().await;
                Err(error)
            }
        }
    });

    vm.register_builtin("pg_mock_pool", |args, _out| {
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
    });

    vm.register_builtin("pg_mock_calls", |args, _out| {
        let id = handle_id(args.first(), HANDLE_MOCK, "pg_mock_calls")?;
        let calls = MOCKS.with(|mocks| {
            mocks
                .borrow()
                .get(&id)
                .map(|mock| mock.calls.clone())
                .unwrap_or_default()
        });
        Ok(VmValue::List(Rc::new(calls)))
    });
}

async fn open_pool(
    source: &VmValue,
    options: Option<&BTreeMap<String, VmValue>>,
    single_connection: bool,
) -> Result<VmValue, VmError> {
    let url = resolve_connection_url(source).await?;
    let mut connect_options = PgConnectOptions::from_str(&url).map_err(|error| {
        runtime_error(format!("pg_pool: invalid Postgres URL/options: {error}"))
    })?;
    if let Some(application_name) = option_string(options, "application_name") {
        connect_options = connect_options.application_name(&application_name);
    }
    if let Some(ssl_mode) =
        option_string(options, "ssl_mode").or_else(|| option_string(options, "tls_mode"))
    {
        connect_options = connect_options.ssl_mode(parse_ssl_mode(&ssl_mode)?);
    }
    if let Some(capacity) = option_int(options, "statement_cache_capacity") {
        connect_options = connect_options.statement_cache_capacity(capacity.max(0) as usize);
    }

    let max_connections = if single_connection {
        1
    } else {
        option_int(options, "max_connections")
            .unwrap_or(5)
            .clamp(1, i64::from(u32::MAX)) as u32
    };
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

    let pool = pool_options
        .connect_with(connect_options)
        .await
        .map_err(|error| runtime_error(format!("pg_pool: connect failed: {error}")))?;
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
    if let Some(application_name) = option_string(options, "application_name") {
        meta.insert(
            "application_name".to_string(),
            VmValue::String(Rc::from(application_name)),
        );
    }
    POOLS.with(|pools| {
        pools.borrow_mut().insert(
            id.clone(),
            PoolRecord {
                pool: Arc::new(pool),
            },
        );
    });
    Ok(handle_value(HANDLE_POOL, &id, meta))
}

async fn query_many(target: &VmValue, sql: &str, params: &[VmValue]) -> Result<VmValue, VmError> {
    let rows = query_rows(target, sql, params).await?;
    Ok(VmValue::List(Rc::new(rows)))
}

async fn query_rows(
    target: &VmValue,
    sql: &str,
    params: &[VmValue],
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

    let pool = pool_from_handle(target, "pg_query")?;
    let query = bind_params(query(sql), params);
    let rows = query
        .fetch_all(pool.as_ref())
        .await
        .map_err(|error| runtime_error(format!("pg_query: {error}")))?;
    rows.into_iter().map(row_to_value).collect()
}

async fn execute_stmt(target: &VmValue, sql: &str, params: &[VmValue]) -> Result<VmValue, VmError> {
    if handle_kind(target).as_deref() == Some(HANDLE_MOCK) {
        let rows = mock_query(target, sql, params, true)?;
        let rows_affected = rows
            .first()
            .and_then(VmValue::as_dict)
            .and_then(|dict| dict.get("rows_affected"))
            .and_then(VmValue::as_int)
            .unwrap_or(0)
            .max(0) as u64;
        return Ok(execute_result_value(rows_affected));
    }
    let result = if handle_kind(target).as_deref() == Some(HANDLE_TX) {
        let id = handle_id(Some(target), HANDLE_TX, "pg_execute")?;
        let tx = tx_by_id(&id)?;
        let mut tx = tx.lock().await;
        let tx = tx
            .as_mut()
            .ok_or_else(|| runtime_error("pg_execute: transaction is closed"))?;
        bind_params(query(sql), params)
            .execute(&mut **tx)
            .await
            .map_err(|error| runtime_error(format!("pg_execute: {error}")))?
    } else {
        let pool = pool_from_handle(target, "pg_execute")?;
        bind_params(query(sql), params)
            .execute(pool.as_ref())
            .await
            .map_err(|error| runtime_error(format!("pg_execute: {error}")))?
    };
    Ok(query_result_value(result))
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

fn bind_params<'q>(
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
            VmValue::Duration(ms) => query.bind(*ms as i64),
            value => query.bind(sqlx_core::types::Json(vm_value_to_json(value))),
        };
    }
    query
}

fn row_to_value(row: PgRow) -> Result<VmValue, VmError> {
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

fn decode_error(error: sqlx_core::error::Error) -> VmError {
    runtime_error(format!("pg_query: row decode failed: {error}"))
}

fn query_result_value(result: PgQueryResult) -> VmValue {
    execute_result_value(result.rows_affected())
}

fn execute_result_value(rows_affected: u64) -> VmValue {
    let mut map = BTreeMap::new();
    map.insert(
        "rows_affected".to_string(),
        VmValue::Int(rows_affected as i64),
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

fn pool_from_handle(value: &VmValue, builtin: &str) -> Result<Arc<PgPool>, VmError> {
    let id = handle_id(Some(value), HANDLE_POOL, builtin)?;
    pool_by_id(&id)
}

fn pool_by_id(id: &str) -> Result<Arc<PgPool>, VmError> {
    POOLS.with(|pools| {
        pools
            .borrow()
            .get(id)
            .map(|record| Arc::clone(&record.pool))
            .ok_or_else(|| runtime_error(format!("pg_pool: unknown or closed pool `{id}`")))
    })
}

fn tx_by_id(id: &str) -> Result<PgTxCell, VmError> {
    TXS.with(|txs| {
        txs.borrow()
            .get(id)
            .cloned()
            .ok_or_else(|| runtime_error(format!("pg_transaction: unknown transaction `{id}`")))
    })
}

fn handle_value(kind: &str, id: &str, mut extra: BTreeMap<String, VmValue>) -> VmValue {
    extra.insert("_type".to_string(), VmValue::String(Rc::from(kind)));
    extra.insert("id".to_string(), VmValue::String(Rc::from(id.to_string())));
    VmValue::Dict(Rc::new(extra))
}

fn handle_kind(value: &VmValue) -> Option<String> {
    value
        .as_dict()
        .and_then(|dict| dict.get("_type"))
        .map(VmValue::display)
}

fn handle_id(value: Option<&VmValue>, expected: &str, builtin: &str) -> Result<String, VmError> {
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

fn params_arg(value: Option<&VmValue>, builtin: &str) -> Result<Vec<VmValue>, VmError> {
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

fn next_id(prefix: &str) -> String {
    format!("{prefix}-{}", NEXT_ID.fetch_add(1, Ordering::Relaxed))
}

fn runtime_error(message: impl Into<String>) -> VmError {
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
            Ok(vec![execute_result_value(fixture.rows_affected)])
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
        assert_eq!(rows[0].display(), "{rows_affected: 3}");
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
            "select $1::uuid as id, $2::jsonb as payload, $3::timestamptz as observed_at",
            &[
                s("00000000-0000-0000-0000-000000000001"),
                dict(&[("ok", VmValue::Bool(true))]),
                s("2024-01-02T03:04:05Z"),
            ],
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
println(tenant)

let rolled = try {
  pg_transaction(db, { tx ->
    pg_execute(tx, "insert into harn_pg_tx_test(value) values ($1)", [2])
    throw_error("force rollback")
  })
} catch (e) {
  "rolled back"
}
println(rolled)
println(pg_query_one(db, "select count(*)::int8 as count from harn_pg_tx_test", []).count)
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
}
