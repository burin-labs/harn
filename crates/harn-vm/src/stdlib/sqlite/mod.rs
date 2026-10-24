use crate::value::VmDictExt;
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex as SyncMutex;
use rusqlite::types::{Value, ValueRef};
use rusqlite::{params_from_iter, Connection, OpenFlags};
use tokio::sync::Mutex;

use crate::llm::vm_value_to_json;
use crate::stdlib::args::Args;
use crate::stdlib::macros::{
    harn_builtin, BuiltinSignature, Param, TY_ANY, TY_BOOL, TY_DICT, TY_LIST, TY_RESOURCE,
};
use crate::stdlib::sandbox::{self, FsAccess};
use crate::stdlib::sql_common::{
    dir_arg, discover_migrations, is_resource_kind, option_bool, option_int, quote_identifier,
    render_savepoint_sql, runtime_error, sha256, typed_handle, MigrationEntry, MockDb, SavepointOp,
};
use crate::value::{VmError, VmResourceHandle, VmValue};
use crate::vm::Vm;

const HANDLE_DB: &str = "sqlite_db";
const HANDLE_TX: &str = "sqlite_tx";
const HANDLE_MOCK: &str = "sqlite_mock_db";
const DEFAULT_BUSY_TIMEOUT_MS: u64 = 5_000;
const DEFAULT_MIGRATIONS_TABLE: &str = "harn_sqlite_migrations";

struct DbRecord {
    conn: Arc<Mutex<Connection>>,
    active_tx: Arc<Mutex<bool>>,
    closed: AtomicBool,
    read_only: bool,
}

struct TxRecord {
    db: Arc<DbRecord>,
    active: AtomicBool,
}

pub(crate) fn reset_sqlite_state() {
    // SQLite state is owned by opaque VM resource values. Dropping a VM drops
    // its databases, transactions, and mocks without clearing ambient state.
}

pub(crate) fn register_sqlite_builtins(vm: &mut Vm) {
    crate::stdlib::macros::register_module_builtins(vm, module_path!());
}

#[harn_builtin(
    exposure = "runtime_internal",
    sig_expr = BuiltinSignature::variadic("sqlite_open", &[Param::new("args", TY_ANY
)], TY_RESOURCE),
    kind = "async",
    category = "sqlite"
)]
async fn sqlite_open_impl(
    _ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let source = args
        .first()
        .ok_or_else(|| runtime_error("sqlite_open: path or :memory: is required"))?;
    let path = source.display();
    if path.trim().is_empty() {
        return Err(runtime_error("sqlite_open: path or :memory: is required"));
    }
    let options = args.get(1).and_then(VmValue::as_dict);
    open_db(&path, options)
}

#[harn_builtin(
    exposure = "capability_arg:0",
    effects = ["fs.write@arg0.path"],
    sig_expr = BuiltinSignature::variadic("sqlite_close", &[Param::new("args", TY_ANY
)], TY_BOOL),
    kind = "async",
    category = "sqlite"
)]
async fn sqlite_close_impl(
    _ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let record = db_handle(args.first(), "sqlite_close")?;
    if *record.active_tx.lock().await {
        return Err(runtime_error(
            "sqlite_close: cannot close a database with an active transaction",
        ));
    }
    Ok(VmValue::Bool(!record.closed.swap(true, Ordering::SeqCst)))
}

#[harn_builtin(
    exposure = "capability_arg:0",
    effects = ["fs.read@arg0.path"],
    sig_expr = BuiltinSignature::variadic("sqlite_query", &[Param::new("args", TY_ANY
)], TY_LIST),
    kind = "async",
    category = "sqlite"
)]
async fn sqlite_query_impl(
    _ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let target = args
        .first()
        .ok_or_else(|| runtime_error("sqlite_query: database or transaction handle is required"))?;
    let call = Args::runtime("sqlite_query", &args);
    let sql = call.non_empty_string(1, "sql")?.to_string();
    let params = params_arg(&call, 2)?;
    let rows = query_rows(target, &sql, &params, "sqlite_query").await?;
    Ok(VmValue::list(rows))
}

#[harn_builtin(
    exposure = "capability_arg:0",
    effects = ["fs.read@arg0.path"],
    sig_expr = BuiltinSignature::variadic("sqlite_query_one", &[Param::new("args", TY_ANY
)], TY_ANY),
    kind = "async",
    category = "sqlite"
)]
async fn sqlite_query_one_impl(
    _ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let target = args.first().ok_or_else(|| {
        runtime_error("sqlite_query_one: database or transaction handle is required")
    })?;
    let call = Args::runtime("sqlite_query_one", &args);
    let sql = call.non_empty_string(1, "sql")?.to_string();
    let params = params_arg(&call, 2)?;
    let rows = query_rows(target, &sql, &params, "sqlite_query_one").await?;
    Ok(rows.into_iter().next().unwrap_or(VmValue::Nil))
}

#[harn_builtin(
    exposure = "capability_arg:0",
    effects = ["fs.write@arg0.path"],
    sig_expr = BuiltinSignature::variadic("sqlite_execute", &[Param::new("args", TY_ANY
)], TY_DICT),
    kind = "async",
    category = "sqlite"
)]
async fn sqlite_execute_impl(
    _ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let target = args.first().ok_or_else(|| {
        runtime_error("sqlite_execute: database or transaction handle is required")
    })?;
    let call = Args::runtime("sqlite_execute", &args);
    let sql = call.non_empty_string(1, "sql")?.to_string();
    let params = params_arg(&call, 2)?;
    execute_stmt(target, &sql, &params, "sqlite_execute").await
}

#[harn_builtin(
    exposure = "capability_arg:0",
    effects = ["fs.write@arg0.path"],
    sig_expr = BuiltinSignature::variadic("sqlite_transaction", &[Param::new("args", TY_ANY
)], TY_ANY),
    kind = "async",
    category = "sqlite"
)]
async fn sqlite_transaction_impl(
    ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    if is_resource_kind(args.first(), HANDLE_MOCK) {
        let target = args.first().expect("checked above");
        let closure = Args::runtime("sqlite_transaction", &args)
            .closure(1, "body")?
            .clone();
        let mut child_vm = ctx.child_vm();
        let result = child_vm.call_closure_pub(&closure, &[target.clone()]).await;
        ctx.forward_output(&child_vm.take_output());
        return result;
    }

    let record = db_handle(args.first(), "sqlite_transaction")?;
    let closure = Args::runtime("sqlite_transaction", &args)
        .closure(1, "body")?
        .clone();
    ensure_db_open(&record, "sqlite_transaction")?;
    if record.read_only {
        return Err(runtime_error(
            "sqlite_transaction: cannot start a transaction on a read-only database",
        ));
    }

    let tx_record = Arc::new(TxRecord {
        db: Arc::clone(&record),
        active: AtomicBool::new(true),
    });
    {
        let mut active = record.active_tx.lock().await;
        if *active {
            return Err(runtime_error(
                "sqlite_transaction: database already has an active transaction",
            ));
        }
        *active = true;
    }

    let begin = option_string(args.get(2).and_then(VmValue::as_dict), "mode")
        .map(|mode| begin_sql(&mode))
        .transpose()?
        .unwrap_or("BEGIN IMMEDIATE");
    if let Err(error) = execute_batch_on_record(&record, begin, "sqlite_transaction").await {
        clear_active_tx(&record).await;
        return Err(error);
    }

    let tx_handle = VmValue::resource(VmResourceHandle::from_arc(
        HANDLE_TX,
        Arc::clone(&tx_record),
    ));
    let mut child_vm = ctx.child_vm();
    let result = child_vm.call_closure_pub(&closure, &[tx_handle]).await;
    ctx.forward_output(&child_vm.take_output());
    tx_record.active.store(false, Ordering::SeqCst);

    let finish = match result {
        Ok(value) => {
            let committed = execute_batch_on_record(&record, "COMMIT", "sqlite_transaction").await;
            clear_active_tx(&record).await;
            committed.map(|_| value)
        }
        Err(error) => {
            let _ = execute_batch_on_record(&record, "ROLLBACK", "sqlite_transaction").await;
            clear_active_tx(&record).await;
            Err(error)
        }
    };
    finish
}

#[harn_builtin(
    exposure = "capability_arg:0",
    effects = ["fs.write@arg0.path"],
    sig_expr = BuiltinSignature::variadic("sqlite_savepoint", &[Param::new("args", TY_ANY
)], TY_BOOL),
    kind = "async",
    category = "sqlite"
)]
async fn sqlite_savepoint_impl(
    _ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    savepoint_op(&args, "sqlite_savepoint", SavepointOp::Create).await
}

#[harn_builtin(
    exposure = "capability_arg:0",
    effects = ["fs.write@arg0.path"],
    sig_expr = BuiltinSignature::variadic("sqlite_release_savepoint", &[Param::new("args", TY_ANY
)], TY_BOOL),
    kind = "async",
    category = "sqlite"
)]
async fn sqlite_release_savepoint_impl(
    _ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    savepoint_op(&args, "sqlite_release_savepoint", SavepointOp::Release).await
}

#[harn_builtin(
    exposure = "capability_arg:0",
    effects = ["fs.write@arg0.path"],
    sig_expr = BuiltinSignature::variadic("sqlite_rollback_to_savepoint", &[Param::new("args", TY_ANY
)], TY_BOOL),
    kind = "async",
    category = "sqlite"
)]
async fn sqlite_rollback_to_savepoint_impl(
    _ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    savepoint_op(
        &args,
        "sqlite_rollback_to_savepoint",
        SavepointOp::RollbackTo,
    )
    .await
}

#[harn_builtin(
    exposure = "capability_arg:0",
    effects = ["fs.write@arg0.path"],
    sig_expr = BuiltinSignature::variadic("sqlite_migrate", &[Param::new("args", TY_ANY
)], TY_DICT),
    kind = "async",
    category = "sqlite"
)]
async fn sqlite_migrate_impl(
    _ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    migrate(args).await
}

#[harn_builtin(
    exposure = "harness.testing.sqlite_mock_db",
    effects = ["state.mutate@dynamic"],
    sig_expr = BuiltinSignature::variadic("sqlite_mock_db", &[Param::new("args", TY_ANY
)], TY_RESOURCE),
    category = "sqlite"
)]
fn sqlite_mock_db_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    crate::stdlib::sql_common::mock_db_resource(args, "sqlite_mock_db", HANDLE_MOCK)
}

#[harn_builtin(
    exposure = "capability_arg:0",
    effects = ["state.read@arg0"],
    sig_expr = BuiltinSignature::variadic("sqlite_mock_calls", &[Param::new("args", TY_ANY
)], TY_LIST),
    category = "sqlite"
)]
fn sqlite_mock_calls_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let mock = mock_handle(args.first(), "sqlite_mock_calls")?;
    let calls = mock.lock().calls.clone();
    Ok(VmValue::list(calls))
}

fn open_db(path: &str, options: Option<&crate::value::DictMap>) -> Result<VmValue, VmError> {
    let create = option_bool(options.and_then(|opts| opts.get("create"))).unwrap_or(false);
    let read_only = option_bool(options.and_then(|opts| opts.get("read_only"))).unwrap_or(false);
    let is_memory = path == ":memory:" || path.trim().eq_ignore_ascii_case("memory");

    let (conn, stored_path) = if is_memory {
        if read_only {
            return Err(runtime_error(
                "sqlite_open: read_only is not valid for in-memory databases",
            ));
        }
        (
            Connection::open_in_memory()
                .map_err(|error| runtime_error(format!("sqlite_open: {error}")))?,
            None,
        )
    } else {
        let path = PathBuf::from(path);
        let access = if read_only {
            FsAccess::Read
        } else {
            FsAccess::Write
        };
        sandbox::enforce_fs_path("sqlite_open", &path, access)?;
        if create {
            if let Some(parent) = path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
            {
                std::fs::create_dir_all(parent).map_err(|error| {
                    runtime_error(format!(
                        "sqlite_open: could not create parent {}: {error}",
                        parent.display()
                    ))
                })?;
            }
        } else if !path.exists() {
            return Err(runtime_error(format!(
                "sqlite_open: database does not exist: {}",
                path.display()
            )));
        }

        let flags = if read_only {
            OpenFlags::SQLITE_OPEN_READ_ONLY
        } else if create {
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE
        } else {
            OpenFlags::SQLITE_OPEN_READ_WRITE
        };
        let conn = Connection::open_with_flags(&path, flags)
            .map_err(|error| runtime_error(format!("sqlite_open: {error}")))?;
        (conn, Some(path))
    };

    configure_connection(&conn, options, read_only)?;
    let _ = stored_path;
    let record = DbRecord {
        conn: Arc::new(Mutex::new(conn)),
        active_tx: Arc::new(Mutex::new(false)),
        closed: AtomicBool::new(false),
        read_only,
    };
    Ok(VmValue::resource(VmResourceHandle::new(HANDLE_DB, record)))
}

fn configure_connection(
    conn: &Connection,
    options: Option<&crate::value::DictMap>,
    read_only: bool,
) -> Result<(), VmError> {
    let busy_timeout_ms = option_int(options, "busy_timeout_ms")
        .unwrap_or(DEFAULT_BUSY_TIMEOUT_MS as i64)
        .clamp(0, i64::from(u32::MAX)) as u64;
    conn.busy_timeout(Duration::from_millis(busy_timeout_ms))
        .map_err(|error| runtime_error(format!("sqlite_open: busy_timeout failed: {error}")))?;

    let foreign_keys =
        option_bool(options.and_then(|opts| opts.get("foreign_keys"))).unwrap_or(true);
    conn.pragma_update(
        None,
        "foreign_keys",
        if foreign_keys { "ON" } else { "OFF" },
    )
    .map_err(|error| runtime_error(format!("sqlite_open: foreign_keys pragma failed: {error}")))?;

    if !read_only {
        if let Some(journal_mode) = option_string(options, "journal_mode") {
            let mode = journal_mode.trim().to_ascii_uppercase();
            match mode.as_str() {
                "DELETE" | "TRUNCATE" | "PERSIST" | "MEMORY" | "WAL" | "OFF" => {
                    conn.pragma_update(None, "journal_mode", mode)
                        .map_err(|error| {
                            runtime_error(format!(
                                "sqlite_open: journal_mode pragma failed: {error}"
                            ))
                        })?;
                }
                other => {
                    return Err(runtime_error(format!(
                        "sqlite_open: unsupported journal_mode `{other}`"
                    )))
                }
            }
        }
    }
    Ok(())
}

async fn query_rows(
    target: &VmValue,
    sql: &str,
    params: &[VmValue],
    builtin: &'static str,
) -> Result<Vec<VmValue>, VmError> {
    if is_resource_kind(Some(target), HANDLE_MOCK) {
        return mock_query(target, sql, params, false);
    }
    let record = record_for_target(target, builtin).await?;
    let values = bind_params(params);
    let conn = record.conn.lock().await;
    let mut stmt = conn
        .prepare(sql)
        .map_err(|error| runtime_error(format!("{builtin}: prepare failed: {error}")))?;
    let names = stmt
        .column_names()
        .into_iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    let mut rows = stmt
        .query(params_from_iter(values.iter()))
        .map_err(|error| runtime_error(format!("{builtin}: {error}")))?;
    let mut out = Vec::new();
    while let Some(row) = rows
        .next()
        .map_err(|error| runtime_error(format!("{builtin}: row step failed: {error}")))?
    {
        out.push(row_to_value(row, &names, builtin)?);
    }
    Ok(out)
}

async fn execute_stmt(
    target: &VmValue,
    sql: &str,
    params: &[VmValue],
    builtin: &'static str,
) -> Result<VmValue, VmError> {
    if is_resource_kind(Some(target), HANDLE_MOCK) {
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
    let record = record_for_target(target, builtin).await?;
    if record.read_only {
        return Err(runtime_error(format!(
            "{builtin}: cannot execute write statements on a read-only database"
        )));
    }
    let values = bind_params(params);
    let conn = record.conn.lock().await;
    let rows = conn
        .execute(sql, params_from_iter(values.iter()))
        .map_err(|error| runtime_error(format!("{builtin}: {error}")))?;
    Ok(execute_result_value(rows as u64))
}

async fn record_for_target(
    target: &VmValue,
    builtin: &'static str,
) -> Result<Arc<DbRecord>, VmError> {
    if let Ok(tx) = tx_handle(Some(target), builtin) {
        if !tx.active.load(Ordering::SeqCst) {
            return Err(runtime_error(format!("{builtin}: transaction is closed")));
        }
        ensure_db_open(&tx.db, builtin)?;
        return Ok(Arc::clone(&tx.db));
    }
    let record = db_handle(Some(target), builtin)?;
    ensure_db_open(&record, builtin)?;
    if *record.active_tx.lock().await {
        return Err(runtime_error(format!(
            "{builtin}: database has an active transaction; use the transaction handle"
        )));
    }
    Ok(record)
}

async fn savepoint_op(
    args: &[VmValue],
    builtin: &'static str,
    op: SavepointOp,
) -> Result<VmValue, VmError> {
    let target = args
        .first()
        .ok_or_else(|| runtime_error(format!("{builtin}: transaction handle is required")))?;
    let name = Args::runtime(builtin, args)
        .non_empty_string(1, "name")?
        .to_string();
    validate_identifier(&name, builtin, "savepoint name")?;
    let sql = render_savepoint_sql(op, &name);
    if is_resource_kind(Some(target), HANDLE_MOCK) {
        let _ = mock_query(target, &sql, &[], true)?;
        return Ok(VmValue::Bool(true));
    }
    let record = record_for_target(target, builtin).await?;
    execute_batch_on_record(&record, &sql, builtin).await?;
    Ok(VmValue::Bool(true))
}

async fn migrate(args: Vec<VmValue>) -> Result<VmValue, VmError> {
    let target = args
        .first()
        .ok_or_else(|| runtime_error("sqlite_migrate: database handle is required"))?;
    let opts = args
        .get(1)
        .and_then(VmValue::as_dict)
        .ok_or_else(|| runtime_error("sqlite_migrate: second argument must be {dir, ...}"))?;
    let dir = dir_arg(opts, "dir", "sqlite_migrate")?;
    sandbox::enforce_fs_path("sqlite_migrate", &dir, FsAccess::Read)?;
    let table =
        option_string(Some(opts), "table").unwrap_or_else(|| DEFAULT_MIGRATIONS_TABLE.to_string());
    validate_identifier(&table, "sqlite_migrate", "table")?;
    let dry_run = option_bool(opts.get("dry_run")).unwrap_or(false);

    let record = db_handle(Some(target), "sqlite_migrate")?;
    ensure_db_open(&record, "sqlite_migrate")?;
    if record.read_only && !dry_run {
        return Err(runtime_error(
            "sqlite_migrate: cannot apply migrations on a read-only database",
        ));
    }
    let active = record.active_tx.lock().await;
    if *active {
        return Err(runtime_error(
            "sqlite_migrate: database has an active transaction",
        ));
    }
    drop(active);

    let entries = discover_migrations(&dir, "sqlite_migrate")?;
    let mut applied_now = Vec::new();
    let mut skipped = Vec::new();

    {
        let conn = record.conn.lock().await;
        let table_exists = migrations_table_exists(&conn, &table)?;
        if !dry_run && !table_exists {
            ensure_migrations_table(&conn, &table)?;
        }
        let applied = applied_set(&conn, &table)?;
        for entry in &entries {
            if applied.contains(&entry.name) {
                skipped.push(entry.name.clone());
                continue;
            }
            if !dry_run {
                apply_one(&conn, &table, entry)?;
            }
            applied_now.push(entry.name.clone());
        }
    }

    let mut response = crate::value::DictMap::new();
    response.put("applied", string_list(applied_now));
    response.put("skipped", string_list(skipped));
    response.put(
        "available",
        string_list(entries.iter().map(|entry| entry.name.clone()).collect()),
    );
    response.put_bool("dry_run", dry_run);
    response.put_str("table", table);
    Ok(VmValue::dict(response))
}

fn ensure_migrations_table(conn: &Connection, table: &str) -> Result<(), VmError> {
    let table = quote_identifier(table);
    conn.execute_batch(&format!(
        "CREATE TABLE IF NOT EXISTS {table} (\
             name TEXT PRIMARY KEY,\
             applied_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,\
             checksum BLOB NOT NULL\
         )"
    ))
    .map_err(|error| runtime_error(format!("sqlite_migrate: ensure table failed: {error}")))
}

fn applied_set(conn: &Connection, table: &str) -> Result<BTreeSet<String>, VmError> {
    if !migrations_table_exists(conn, table)? {
        return Ok(BTreeSet::new());
    }
    let table = quote_identifier(table);
    let mut stmt = conn
        .prepare(&format!("SELECT name FROM {table}"))
        .map_err(|error| {
            runtime_error(format!("sqlite_migrate: select applied failed: {error}"))
        })?;
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|error| {
            runtime_error(format!("sqlite_migrate: select applied failed: {error}"))
        })?;
    let mut out = BTreeSet::new();
    for row in rows {
        out.insert(row.map_err(|error| {
            runtime_error(format!("sqlite_migrate: select applied failed: {error}"))
        })?);
    }
    Ok(out)
}

fn migrations_table_exists(conn: &Connection, table: &str) -> Result<bool, VmError> {
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
            [table],
            |row| row.get(0),
        )
        .map_err(|error| runtime_error(format!("sqlite_migrate: table lookup failed: {error}")))?;
    Ok(count > 0)
}

fn apply_one(conn: &Connection, table: &str, entry: &MigrationEntry) -> Result<(), VmError> {
    let sql = std::fs::read_to_string(&entry.path).map_err(|error| {
        runtime_error(format!(
            "sqlite_migrate: could not read {}: {error}",
            entry.path.display()
        ))
    })?;
    let checksum = sha256(&sql);
    conn.execute_batch("BEGIN IMMEDIATE")
        .map_err(|error| runtime_error(format!("sqlite_migrate: begin failed: {error}")))?;
    let result = (|| {
        conn.execute_batch(&sql).map_err(|error| {
            runtime_error(format!("sqlite_migrate: applying {}: {error}", entry.name))
        })?;
        let insert = format!(
            "INSERT INTO {} (name, checksum) VALUES (?, ?)",
            quote_identifier(table)
        );
        conn.execute(&insert, (&entry.name, &checksum))
            .map_err(|error| {
                runtime_error(format!(
                    "sqlite_migrate: record {} failed: {error}",
                    entry.name
                ))
            })?;
        Ok(())
    })();
    match result {
        Ok(()) => conn
            .execute_batch("COMMIT")
            .map_err(|error| runtime_error(format!("sqlite_migrate: commit failed: {error}"))),
        Err(error) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(error)
        }
    }
}

fn row_to_value(
    row: &rusqlite::Row<'_>,
    names: &[String],
    builtin: &'static str,
) -> Result<VmValue, VmError> {
    let mut map = crate::value::DictMap::new();
    for (index, name) in names.iter().enumerate() {
        let value = match row
            .get_ref(index)
            .map_err(|error| runtime_error(format!("{builtin}: row decode failed: {error}")))?
        {
            ValueRef::Null => VmValue::Nil,
            ValueRef::Integer(value) => VmValue::Int(value),
            ValueRef::Real(value) => VmValue::Float(value),
            ValueRef::Text(bytes) => {
                let text = std::str::from_utf8(bytes).map_err(|error| {
                    runtime_error(format!("{builtin}: text column is not UTF-8: {error}"))
                })?;
                VmValue::string(text.to_string())
            }
            ValueRef::Blob(bytes) => VmValue::Bytes(Arc::new(bytes.to_vec())),
        };
        map.insert(crate::value::intern_key(name), value);
    }
    Ok(VmValue::dict(map))
}

fn bind_params(params: &[VmValue]) -> Vec<Value> {
    params
        .iter()
        .map(|param| match param {
            VmValue::Nil => Value::Null,
            VmValue::Bool(value) => Value::Integer(i64::from(*value)),
            VmValue::Int(value) => Value::Integer(*value),
            VmValue::Float(value) => Value::Real(*value),
            VmValue::String(value) => Value::Text(value.to_string()),
            VmValue::Bytes(value) => Value::Blob((**value).clone()),
            VmValue::Duration(ms) => Value::Integer(*ms),
            value => Value::Text(vm_value_to_json(value).to_string()),
        })
        .collect()
}

fn execute_result_value(rows_affected: u64) -> VmValue {
    VmValue::dict([("rows_affected", VmValue::Int(rows_affected as i64))])
}

async fn execute_batch_on_record(
    record: &Arc<DbRecord>,
    sql: &str,
    builtin: &'static str,
) -> Result<(), VmError> {
    let conn = record.conn.lock().await;
    conn.execute_batch(sql)
        .map_err(|error| runtime_error(format!("{builtin}: {error}")))
}

async fn clear_active_tx(record: &Arc<DbRecord>) {
    *record.active_tx.lock().await = false;
}

fn mock_query(
    target: &VmValue,
    sql: &str,
    params: &[VmValue],
    execute: bool,
) -> Result<Vec<VmValue>, VmError> {
    let mock = mock_handle(Some(target), "sqlite_mock")?;
    crate::stdlib::sql_common::mock_query(&mock, "sqlite_mock", sql, params, execute, |rows| {
        execute_result_value(rows)
    })
}

fn db_handle(value: Option<&VmValue>, builtin: &str) -> Result<Arc<DbRecord>, VmError> {
    typed_handle(value, HANDLE_DB, builtin, "sqlite database")
}

fn tx_handle(value: Option<&VmValue>, builtin: &str) -> Result<Arc<TxRecord>, VmError> {
    typed_handle(value, HANDLE_TX, builtin, "sqlite transaction")
}

fn mock_handle(value: Option<&VmValue>, builtin: &str) -> Result<Arc<SyncMutex<MockDb>>, VmError> {
    typed_handle(value, HANDLE_MOCK, builtin, "sqlite mock")
}

fn ensure_db_open(record: &DbRecord, builtin: &str) -> Result<(), VmError> {
    if record.closed.load(Ordering::SeqCst) {
        Err(runtime_error(format!("{builtin}: database is closed")))
    } else {
        Ok(())
    }
}

/// Positional `params` list, defaulting to empty.
fn params_arg(args: &Args<'_, '_>, index: usize) -> Result<Vec<VmValue>, VmError> {
    Ok(args.opt_list(index, "params")?.unwrap_or_default().to_vec())
}

/// A string option, or `None` when absent. Non-string values are ignored
/// rather than rendered: `display` would have turned a dict into its own
/// text and fed that to SQLite.
fn option_string(options: Option<&crate::value::DictMap>, key: &str) -> Option<String> {
    match options?.get(key)? {
        VmValue::String(text) if !text.trim().is_empty() => Some(text.to_string()),
        _ => None,
    }
}

fn begin_sql(mode: &str) -> Result<&'static str, VmError> {
    match mode.trim().to_ascii_lowercase().as_str() {
        "" | "immediate" => Ok("BEGIN IMMEDIATE"),
        "deferred" => Ok("BEGIN DEFERRED"),
        "exclusive" => Ok("BEGIN EXCLUSIVE"),
        other => Err(runtime_error(format!(
            "sqlite_transaction: unsupported transaction mode `{other}`"
        ))),
    }
}

fn validate_identifier(
    name: &str,
    builtin: &'static str,
    label: &'static str,
) -> Result<(), VmError> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(runtime_error(format!(
            "{builtin}: {label} must not be empty"
        )));
    }
    if name.len() > 128 {
        return Err(runtime_error(format!(
            "{builtin}: {label} exceeds 128 bytes"
        )));
    }
    let first = name.chars().next().expect("non-empty checked above");
    if !(first.is_ascii_alphabetic() || first == '_') {
        return Err(runtime_error(format!(
            "{builtin}: {label} must start with a letter or underscore"
        )));
    }
    for ch in name.chars() {
        if !(ch.is_ascii_alphanumeric() || ch == '_' || ch == '.') {
            return Err(runtime_error(format!(
                "{builtin}: {label} `{name}` contains disallowed character `{ch}`"
            )));
        }
    }
    Ok(())
}

fn string_list(values: Vec<String>) -> VmValue {
    VmValue::strings(values)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(value: &str) -> VmValue {
        VmValue::string(value)
    }

    fn dict(pairs: &[(&str, VmValue)]) -> VmValue {
        VmValue::dict(
            pairs
                .iter()
                .map(|(key, value)| (crate::value::intern_key(key), value.clone())),
        )
    }

    #[test]
    fn savepoint_names_are_validated() {
        assert!(validate_identifier("step_one", "sqlite_savepoint", "savepoint name").is_ok());
        assert!(validate_identifier("step.one", "sqlite_savepoint", "savepoint name").is_ok());
        assert!(validate_identifier("1bad", "sqlite_savepoint", "savepoint name").is_err());
        assert!(validate_identifier("bad name", "sqlite_savepoint", "savepoint name").is_err());
        assert!(validate_identifier("bad;name", "sqlite_savepoint", "savepoint name").is_err());
    }

    #[test]
    fn savepoint_sql_quotes_identifier() {
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
    fn sqlite_authority_cannot_be_forged_with_a_dict() {
        let forged = dict(&[("_type", s(HANDLE_DB)), ("id", s("sqlitedb-1"))]);
        let error = db_handle(Some(&forged), "sqlite_query")
            .err()
            .expect("script-visible dictionaries must not act as database authority");
        assert!(error.to_string().contains("expected sqlite_db handle"));
    }

    #[test]
    fn mock_db_matches_parameterized_query_and_records_calls() {
        reset_sqlite_state();
        let fixtures = crate::stdlib::sql_common::parse_mock_fixtures(
            &[dict(&[
                ("sql", s("select id from events where topic = ?")),
                ("params", VmValue::list(vec![s("agent_events")])),
                (
                    "rows",
                    VmValue::list(vec![dict(&[("id", VmValue::Int(1))])]),
                ),
            ])],
            "sqlite_mock_db",
        )
        .unwrap();
        let handle = VmValue::resource(VmResourceHandle::new(
            HANDLE_MOCK,
            SyncMutex::new(MockDb {
                fixtures,
                calls: Vec::new(),
            }),
        ));
        let rows = mock_query(
            &handle,
            "select id from events where topic = ?",
            &[s("agent_events")],
            false,
        )
        .unwrap();
        assert_eq!(rows[0].display(), "{id: 1}");
        let calls = mock_handle(Some(&handle), "test")
            .unwrap()
            .lock()
            .calls
            .clone();
        assert_eq!(calls.len(), 1);
    }

    #[test]
    fn bind_params_preserves_sqlite_primitive_shapes() {
        let values = bind_params(&[
            VmValue::Nil,
            VmValue::Bool(true),
            VmValue::Int(42),
            VmValue::Float(1.5),
            s("text"),
            dict(&[("ok", VmValue::Bool(true))]),
        ]);
        assert_eq!(values[0], Value::Null);
        assert_eq!(values[1], Value::Integer(1));
        assert_eq!(values[2], Value::Integer(42));
        assert_eq!(values[3], Value::Real(1.5));
        assert_eq!(values[4], Value::Text("text".to_string()));
        assert_eq!(values[5], Value::Text("{\"ok\":true}".to_string()));
    }
}
