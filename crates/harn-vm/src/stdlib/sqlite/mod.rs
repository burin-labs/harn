use crate::value::VmDictExt;
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use rusqlite::types::{Value, ValueRef};
use rusqlite::{params_from_iter, Connection, OpenFlags};
use tokio::sync::Mutex;

use crate::llm::vm_value_to_json;
use crate::stdlib::macros::{
    harn_builtin, BuiltinSignature, Param, VmBuiltinDef, TY_ANY, TY_BOOL, TY_DICT, TY_LIST,
};
use crate::stdlib::sandbox::{self, FsAccess};
use crate::value::{VmError, VmValue};
use crate::vm::Vm;

const HANDLE_DB: &str = "sqlite_db";
const HANDLE_TX: &str = "sqlite_tx";
const HANDLE_MOCK: &str = "sqlite_mock_db";
const DEFAULT_BUSY_TIMEOUT_MS: u64 = 5_000;
const DEFAULT_MIGRATIONS_TABLE: &str = "harn_sqlite_migrations";

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

struct DbRecord {
    conn: Arc<Mutex<Connection>>,
    active_tx: Arc<Mutex<Option<String>>>,
    read_only: bool,
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
struct MockDb {
    fixtures: Vec<MockFixture>,
    calls: Vec<VmValue>,
}

thread_local! {
    static DBS: RefCell<BTreeMap<String, Arc<DbRecord>>> = const { RefCell::new(std::collections::BTreeMap::new()) };
    static TXS: RefCell<BTreeMap<String, Arc<DbRecord>>> = const { RefCell::new(std::collections::BTreeMap::new()) };
    static MOCKS: RefCell<BTreeMap<String, MockDb>> = const { RefCell::new(std::collections::BTreeMap::new()) };
}

pub(crate) fn reset_sqlite_state() {
    DBS.with(|dbs| dbs.borrow_mut().clear());
    TXS.with(|txs| txs.borrow_mut().clear());
    MOCKS.with(|mocks| mocks.borrow_mut().clear());
}

pub(crate) fn register_sqlite_builtins(vm: &mut Vm) {
    for def in MODULE_BUILTINS {
        vm.register_builtin_def(def);
    }
}

pub(crate) const MODULE_BUILTINS: &[&VmBuiltinDef] = &[
    &SQLITE_OPEN_IMPL_DEF,
    &SQLITE_CLOSE_IMPL_DEF,
    &SQLITE_QUERY_IMPL_DEF,
    &SQLITE_QUERY_ONE_IMPL_DEF,
    &SQLITE_EXECUTE_IMPL_DEF,
    &SQLITE_TRANSACTION_IMPL_DEF,
    &SQLITE_SAVEPOINT_IMPL_DEF,
    &SQLITE_RELEASE_SAVEPOINT_IMPL_DEF,
    &SQLITE_ROLLBACK_TO_SAVEPOINT_IMPL_DEF,
    &SQLITE_MIGRATE_IMPL_DEF,
    &SQLITE_MOCK_DB_IMPL_DEF,
    &SQLITE_MOCK_CALLS_IMPL_DEF,
];

#[harn_builtin(
    sig_expr = BuiltinSignature::variadic("sqlite_open", &[Param::new("args", TY_ANY)], TY_DICT),
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
    sig_expr = BuiltinSignature::variadic("sqlite_close", &[Param::new("args", TY_ANY)], TY_BOOL),
    kind = "async",
    category = "sqlite"
)]
async fn sqlite_close_impl(
    _ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    let id = handle_id(args.first(), HANDLE_DB, "sqlite_close")?;
    let Some(record) = DBS.with(|dbs| dbs.borrow().get(&id).cloned()) else {
        return Ok(VmValue::Bool(false));
    };
    if record.active_tx.lock().await.is_some() {
        return Err(runtime_error(
            "sqlite_close: cannot close a database with an active transaction",
        ));
    }
    let removed = DBS.with(|dbs| dbs.borrow_mut().remove(&id));
    Ok(VmValue::Bool(removed.is_some()))
}

#[harn_builtin(
    sig_expr = BuiltinSignature::variadic("sqlite_query", &[Param::new("args", TY_ANY)], TY_LIST),
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
    let sql = required_string_arg(&args, 1, "sqlite_query", "sql")?;
    let params = params_arg(args.get(2), "sqlite_query")?;
    let rows = query_rows(target, &sql, &params, "sqlite_query").await?;
    Ok(VmValue::List(Arc::new(rows)))
}

#[harn_builtin(
    sig_expr = BuiltinSignature::variadic("sqlite_query_one", &[Param::new("args", TY_ANY)], TY_ANY),
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
    let sql = required_string_arg(&args, 1, "sqlite_query_one", "sql")?;
    let params = params_arg(args.get(2), "sqlite_query_one")?;
    let rows = query_rows(target, &sql, &params, "sqlite_query_one").await?;
    Ok(rows.into_iter().next().unwrap_or(VmValue::Nil))
}

#[harn_builtin(
    sig_expr = BuiltinSignature::variadic("sqlite_execute", &[Param::new("args", TY_ANY)], TY_DICT),
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
    let sql = required_string_arg(&args, 1, "sqlite_execute", "sql")?;
    let params = params_arg(args.get(2), "sqlite_execute")?;
    execute_stmt(target, &sql, &params, "sqlite_execute").await
}

#[harn_builtin(
    sig_expr = BuiltinSignature::variadic("sqlite_transaction", &[Param::new("args", TY_ANY)], TY_ANY),
    kind = "async",
    category = "sqlite"
)]
async fn sqlite_transaction_impl(
    ctx: crate::vm::AsyncBuiltinCtx,
    args: Vec<VmValue>,
) -> Result<VmValue, VmError> {
    if handle_kind(args.first()).as_deref() == Some(HANDLE_MOCK) {
        let target = args.first().expect("checked above");
        let closure = closure_arg(args.get(1), "sqlite_transaction")?;
        let mut child_vm = ctx.child_vm();
        let result = child_vm.call_closure_pub(&closure, &[target.clone()]).await;
        ctx.forward_output(&child_vm.take_output());
        return result;
    }

    let db_id = handle_id(args.first(), HANDLE_DB, "sqlite_transaction")?;
    let closure = closure_arg(args.get(1), "sqlite_transaction")?;
    let record = db_by_id(&db_id)?;
    if record.read_only {
        return Err(runtime_error(
            "sqlite_transaction: cannot start a transaction on a read-only database",
        ));
    }

    let tx_id = next_id("sqlitetx");
    {
        let mut active = record.active_tx.lock().await;
        if active.is_some() {
            return Err(runtime_error(
                "sqlite_transaction: database already has an active transaction",
            ));
        }
        *active = Some(tx_id.clone());
    }

    let begin = option_string(args.get(2).and_then(VmValue::as_dict), "mode")
        .map(|mode| begin_sql(&mode))
        .transpose()?
        .unwrap_or("BEGIN IMMEDIATE");
    if let Err(error) = execute_batch_on_record(&record, begin, "sqlite_transaction").await {
        clear_active_tx(&record).await;
        return Err(error);
    }

    register_tx(&tx_id, Arc::clone(&record));
    let tx_handle = handle_value(HANDLE_TX, &tx_id, crate::value::DictMap::new());
    let mut child_vm = ctx.child_vm();
    let result = child_vm.call_closure_pub(&closure, &[tx_handle]).await;
    ctx.forward_output(&child_vm.take_output());
    unregister_tx(&tx_id);

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
    sig_expr = BuiltinSignature::variadic("sqlite_savepoint", &[Param::new("args", TY_ANY)], TY_BOOL),
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
    sig_expr = BuiltinSignature::variadic("sqlite_release_savepoint", &[Param::new("args", TY_ANY)], TY_BOOL),
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
    sig_expr = BuiltinSignature::variadic("sqlite_rollback_to_savepoint", &[Param::new("args", TY_ANY)], TY_BOOL),
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
    sig_expr = BuiltinSignature::variadic("sqlite_migrate", &[Param::new("args", TY_ANY)], TY_DICT),
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
    sig_expr = BuiltinSignature::variadic("sqlite_mock_db", &[Param::new("args", TY_ANY)], TY_DICT),
    category = "sqlite"
)]
fn sqlite_mock_db_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let fixtures = match args.first() {
        Some(VmValue::List(items)) => parse_mock_fixtures(items)?,
        Some(VmValue::Dict(_)) => parse_mock_fixtures(std::slice::from_ref(&args[0]))?,
        None | Some(VmValue::Nil) => Vec::new(),
        _ => {
            return Err(runtime_error(
                "sqlite_mock_db: fixtures must be a list of dicts",
            ))
        }
    };
    let id = next_id("sqlitemock");
    MOCKS.with(|mocks| {
        mocks.borrow_mut().insert(
            id.clone(),
            MockDb {
                fixtures,
                calls: Vec::new(),
            },
        );
    });
    Ok(handle_value(HANDLE_MOCK, &id, crate::value::DictMap::new()))
}

#[harn_builtin(
    sig_expr = BuiltinSignature::variadic("sqlite_mock_calls", &[Param::new("args", TY_ANY)], TY_LIST),
    category = "sqlite"
)]
fn sqlite_mock_calls_impl(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let id = handle_id(args.first(), HANDLE_MOCK, "sqlite_mock_calls")?;
    let calls = MOCKS.with(|mocks| {
        mocks
            .borrow()
            .get(&id)
            .map(|mock| mock.calls.clone())
            .unwrap_or_default()
    });
    Ok(VmValue::List(Arc::new(calls)))
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
    let id = next_id("sqlitedb");
    let mut meta = crate::value::DictMap::new();
    meta.insert(
        crate::value::intern_key("read_only"),
        VmValue::Bool(read_only),
    );
    meta.insert(crate::value::intern_key("memory"), VmValue::Bool(is_memory));
    if let Some(path) = &stored_path {
        meta.put_str("path", path.to_string_lossy());
    }
    let record = Arc::new(DbRecord {
        conn: Arc::new(Mutex::new(conn)),
        active_tx: Arc::new(Mutex::new(None)),
        read_only,
    });
    DBS.with(|dbs| {
        dbs.borrow_mut().insert(id.clone(), record);
    });
    Ok(handle_value(HANDLE_DB, &id, meta))
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
    if handle_kind(Some(target)).as_deref() == Some(HANDLE_MOCK) {
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
    if handle_kind(Some(target)).as_deref() == Some(HANDLE_MOCK) {
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
    match handle_kind(Some(target)).as_deref() {
        Some(HANDLE_TX) => {
            let id = handle_id(Some(target), HANDLE_TX, builtin)?;
            let record = tx_by_id(&id)?;
            let active = record.active_tx.lock().await;
            if active.as_deref() != Some(id.as_str()) {
                return Err(runtime_error(format!("{builtin}: transaction is closed")));
            }
            drop(active);
            Ok(record)
        }
        Some(HANDLE_DB) => {
            let id = handle_id(Some(target), HANDLE_DB, builtin)?;
            let record = db_by_id(&id)?;
            let active = record.active_tx.lock().await;
            if active.is_some() {
                return Err(runtime_error(format!(
                    "{builtin}: database has an active transaction; use the transaction handle"
                )));
            }
            drop(active);
            Ok(record)
        }
        _ => Err(runtime_error(format!(
            "{builtin}: expected sqlite_db or sqlite_tx handle"
        ))),
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
    validate_identifier(&name, builtin, "savepoint name")?;
    let sql = render_savepoint_sql(op, &name);
    if handle_kind(Some(target)).as_deref() == Some(HANDLE_MOCK) {
        let _ = mock_query(target, &sql, &[], true)?;
        return Ok(VmValue::Bool(true));
    }
    let record = record_for_target(target, builtin).await?;
    execute_batch_on_record(&record, &sql, builtin).await?;
    Ok(VmValue::Bool(true))
}

fn render_savepoint_sql(op: SavepointOp, name: &str) -> String {
    let quoted = quote_identifier(name);
    match op {
        SavepointOp::Create => format!("SAVEPOINT {quoted}"),
        SavepointOp::Release => format!("RELEASE SAVEPOINT {quoted}"),
        SavepointOp::RollbackTo => format!("ROLLBACK TO SAVEPOINT {quoted}"),
    }
}

async fn migrate(args: Vec<VmValue>) -> Result<VmValue, VmError> {
    let target = args
        .first()
        .ok_or_else(|| runtime_error("sqlite_migrate: database handle is required"))?;
    let opts = args
        .get(1)
        .and_then(VmValue::as_dict)
        .ok_or_else(|| runtime_error("sqlite_migrate: second argument must be {dir, ...}"))?;
    let dir = dir_arg(opts, "dir")?;
    sandbox::enforce_fs_path("sqlite_migrate", &dir, FsAccess::Read)?;
    let table =
        option_string(Some(opts), "table").unwrap_or_else(|| DEFAULT_MIGRATIONS_TABLE.to_string());
    validate_identifier(&table, "sqlite_migrate", "table")?;
    let dry_run = option_bool(opts.get("dry_run")).unwrap_or(false);

    let id = handle_id(Some(target), HANDLE_DB, "sqlite_migrate")?;
    let record = db_by_id(&id)?;
    if record.read_only && !dry_run {
        return Err(runtime_error(
            "sqlite_migrate: cannot apply migrations on a read-only database",
        ));
    }
    let active = record.active_tx.lock().await;
    if active.is_some() {
        return Err(runtime_error(
            "sqlite_migrate: database has an active transaction",
        ));
    }
    drop(active);

    let entries = discover_migrations(&dir)?;
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
    response.insert(
        crate::value::intern_key("applied"),
        string_list(applied_now),
    );
    response.insert(crate::value::intern_key("skipped"), string_list(skipped));
    response.insert(
        crate::value::intern_key("available"),
        string_list(entries.iter().map(|entry| entry.name.clone()).collect()),
    );
    response.insert(crate::value::intern_key("dry_run"), VmValue::Bool(dry_run));
    response.put_str("table", table);
    Ok(VmValue::dict(response))
}

#[derive(Clone)]
struct MigrationEntry {
    name: String,
    path: PathBuf,
}

fn discover_migrations(dir: &Path) -> Result<Vec<MigrationEntry>, VmError> {
    if !dir.exists() {
        return Err(runtime_error(format!(
            "sqlite_migrate: directory does not exist: {}",
            dir.display()
        )));
    }
    let read_dir = std::fs::read_dir(dir).map_err(|error| {
        runtime_error(format!(
            "sqlite_migrate: could not read directory {}: {error}",
            dir.display()
        ))
    })?;
    let mut entries = read_dir
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.ends_with(".sql") && !name.ends_with(".down.sql") {
                Some(MigrationEntry { name, path })
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(entries)
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

fn sha256(text: &str) -> Vec<u8> {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    hasher.finalize().to_vec()
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
                VmValue::String(arcstr::ArcStr::from(text.to_string()))
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

fn parse_mock_fixtures(items: &[VmValue]) -> Result<Vec<MockFixture>, VmError> {
    items
        .iter()
        .map(|item| {
            let dict = item
                .as_dict()
                .ok_or_else(|| runtime_error("sqlite_mock_db: each fixture must be a dict"))?;
            let sql = dict
                .get("sql")
                .map(VmValue::display)
                .filter(|sql| !sql.trim().is_empty())
                .ok_or_else(|| runtime_error("sqlite_mock_db: fixture.sql is required"))?;
            let params = dict.get("params").map(vm_value_to_json);
            let rows = match dict.get("rows") {
                Some(VmValue::List(rows)) => (**rows).clone(),
                None | Some(VmValue::Nil) => Vec::new(),
                Some(_) => {
                    return Err(runtime_error("sqlite_mock_db: fixture.rows must be a list"))
                }
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
    let id = handle_id(Some(target), HANDLE_MOCK, "sqlite_mock")?;
    let params_json = serde_json::Value::Array(params.iter().map(vm_value_to_json).collect());
    MOCKS.with(|mocks| {
        let mut mocks = mocks.borrow_mut();
        let mock = mocks
            .get_mut(&id)
            .ok_or_else(|| runtime_error(format!("sqlite_mock: unknown mock database `{id}`")))?;
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
            .ok_or_else(|| runtime_error(format!("sqlite_mock: no fixture matched `{sql}`")))?;
        if let Some(error) = &fixture.error {
            return Err(runtime_error(format!("sqlite_mock: {error}")));
        }
        if execute {
            Ok(vec![execute_result_value(fixture.rows_affected)])
        } else {
            Ok(fixture.rows.clone())
        }
    })
}

fn execute_result_value(rows_affected: u64) -> VmValue {
    VmValue::dict(crate::value::DictMap::from_iter([(
        crate::value::intern_key("rows_affected"),
        VmValue::Int(rows_affected as i64),
    )]))
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
    *record.active_tx.lock().await = None;
}

fn db_by_id(id: &str) -> Result<Arc<DbRecord>, VmError> {
    DBS.with(|dbs| {
        dbs.borrow()
            .get(id)
            .cloned()
            .ok_or_else(|| runtime_error(format!("sqlite_open: unknown or closed database `{id}`")))
    })
}

fn tx_by_id(id: &str) -> Result<Arc<DbRecord>, VmError> {
    TXS.with(|txs| {
        txs.borrow()
            .get(id)
            .cloned()
            .ok_or_else(|| runtime_error(format!("sqlite_transaction: unknown transaction `{id}`")))
    })
}

fn register_tx(id: &str, record: Arc<DbRecord>) {
    TXS.with(|txs| {
        txs.borrow_mut().insert(id.to_string(), record);
    });
}

fn unregister_tx(id: &str) {
    TXS.with(|txs| {
        txs.borrow_mut().remove(id);
    });
}

fn handle_value(kind: &str, id: &str, mut extra: crate::value::DictMap) -> VmValue {
    extra.put_str("_type", kind);
    extra.put_str("id", id);
    VmValue::dict(extra)
}

fn handle_kind(value: Option<&VmValue>) -> Option<String> {
    value
        .and_then(VmValue::as_dict)
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

fn closure_arg(
    value: Option<&VmValue>,
    builtin: &'static str,
) -> Result<Arc<crate::value::VmClosure>, VmError> {
    match value {
        Some(VmValue::Closure(closure)) => Ok(closure.clone()),
        _ => Err(runtime_error(format!(
            "{builtin}: second argument must be a closure"
        ))),
    }
}

fn required_string_arg(
    args: &[VmValue],
    index: usize,
    builtin: &'static str,
    label: &'static str,
) -> Result<String, VmError> {
    let value = args.get(index).map(VmValue::display).unwrap_or_default();
    if value.trim().is_empty() {
        return Err(runtime_error(format!("{builtin}: {label} is required")));
    }
    Ok(value)
}

fn params_arg(value: Option<&VmValue>, builtin: &'static str) -> Result<Vec<VmValue>, VmError> {
    match value {
        None | Some(VmValue::Nil) => Ok(Vec::new()),
        Some(VmValue::List(items)) => Ok((**items).clone()),
        Some(_) => Err(runtime_error(format!(
            "{builtin}: params must be a list when provided"
        ))),
    }
}

fn dir_arg(dict: &crate::value::DictMap, key: &str) -> Result<PathBuf, VmError> {
    let value = dict.get(key).ok_or_else(|| {
        runtime_error(format!(
            "sqlite_migrate: option `{key}` is required and must be a path"
        ))
    })?;
    match value {
        VmValue::String(text) => Ok(PathBuf::from(text.as_str())),
        _ => Err(runtime_error(format!(
            "sqlite_migrate: option `{key}` must be a string path"
        ))),
    }
}

fn option_bool(value: Option<&VmValue>) -> Option<bool> {
    match value? {
        VmValue::Bool(value) => Some(*value),
        _ => None,
    }
}

fn option_int(options: Option<&crate::value::DictMap>, key: &str) -> Option<i64> {
    options
        .and_then(|opts| opts.get(key))
        .and_then(|value| match value {
            VmValue::Int(value) => Some(*value),
            VmValue::Float(value)
                if value.is_finite() && *value >= i64::MIN as f64 && *value <= i64::MAX as f64 =>
            {
                Some(*value as i64)
            }
            _ => None,
        })
}

fn option_string(options: Option<&crate::value::DictMap>, key: &str) -> Option<String> {
    options
        .and_then(|opts| opts.get(key))
        .map(VmValue::display)
        .filter(|value| !value.trim().is_empty())
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

fn quote_identifier(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

fn string_list(values: Vec<String>) -> VmValue {
    VmValue::List(Arc::new(
        values
            .into_iter()
            .map(|value| VmValue::String(arcstr::ArcStr::from(value)))
            .collect(),
    ))
}

fn next_id(prefix: &str) -> String {
    format!("{prefix}-{}", NEXT_ID.fetch_add(1, Ordering::Relaxed))
}

fn runtime_error(message: impl Into<String>) -> VmError {
    VmError::Runtime(message.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(value: &str) -> VmValue {
        VmValue::String(arcstr::ArcStr::from(value))
    }

    fn dict(pairs: &[(&str, VmValue)]) -> VmValue {
        VmValue::dict(
            pairs
                .iter()
                .map(|(key, value)| (crate::value::intern_key(key), value.clone()))
                .collect::<crate::value::DictMap>(),
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
    fn mock_db_matches_parameterized_query_and_records_calls() {
        reset_sqlite_state();
        let fixtures = parse_mock_fixtures(&[dict(&[
            ("sql", s("select id from events where topic = ?")),
            ("params", VmValue::List(Arc::new(vec![s("agent_events")]))),
            (
                "rows",
                VmValue::List(Arc::new(vec![dict(&[("id", VmValue::Int(1))])])),
            ),
        ])])
        .unwrap();
        let id = next_id("sqlitemock");
        MOCKS.with(|mocks| {
            mocks.borrow_mut().insert(
                id.clone(),
                MockDb {
                    fixtures,
                    calls: Vec::new(),
                },
            );
        });
        let handle = handle_value(HANDLE_MOCK, &id, crate::value::DictMap::new());
        let rows = mock_query(
            &handle,
            "select id from events where topic = ?",
            &[s("agent_events")],
            false,
        )
        .unwrap();
        assert_eq!(rows[0].display(), "{id: 1}");
        let calls = MOCKS.with(|mocks| mocks.borrow().values().next().unwrap().calls.clone());
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
