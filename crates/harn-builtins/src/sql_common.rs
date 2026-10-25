//! Plumbing shared by the SQLite and Postgres builtins: mock databases,
//! typed resource handles, savepoint SQL, migration discovery, and option
//! readers. Callers pass their builtin prefix (`sqlite_*` / `pg_*`) so the
//! user-visible error messages stay backend-specific.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::Mutex as SyncMutex;

use harn_vm_core::value::json::vm_value_to_json;
use harn_vm_core::value::{VmError, VmResourceHandle, VmValue};

#[derive(Clone)]
pub(crate) struct MockFixture {
    pub(crate) sql: String,
    pub(crate) params: Option<serde_json::Value>,
    pub(crate) rows: Vec<VmValue>,
    pub(crate) rows_affected: u64,
    pub(crate) error: Option<String>,
}

/// Fixture-backed stand-in for a database handle. Records every dispatched
/// statement in `calls` so tests can assert on the SQL a script sent.
#[derive(Default, Clone)]
pub(crate) struct MockDb {
    pub(crate) fixtures: Vec<MockFixture>,
    pub(crate) calls: Vec<VmValue>,
}

/// Body of the `<backend>_mock_*` constructor builtin: accepts one fixture
/// dict, a list of them, or nothing, and wraps them in a `label` resource.
pub(crate) fn mock_db_resource(
    args: &[VmValue],
    builtin: &str,
    label: &'static str,
) -> Result<VmValue, VmError> {
    let fixtures = match args.first() {
        Some(VmValue::List(items)) => parse_mock_fixtures(items, builtin)?,
        Some(VmValue::Dict(_)) => parse_mock_fixtures(std::slice::from_ref(&args[0]), builtin)?,
        None | Some(VmValue::Nil) => Vec::new(),
        _ => {
            return Err(runtime_error(format!(
                "{builtin}: fixtures must be a list of dicts"
            )))
        }
    };
    Ok(VmValue::resource(VmResourceHandle::new(
        label,
        SyncMutex::new(MockDb {
            fixtures,
            calls: Vec::new(),
        }),
    )))
}

pub(crate) fn parse_mock_fixtures(
    items: &[VmValue],
    builtin: &str,
) -> Result<Vec<MockFixture>, VmError> {
    items
        .iter()
        .map(|item| {
            let dict = item
                .as_dict()
                .ok_or_else(|| runtime_error(format!("{builtin}: each fixture must be a dict")))?;
            let sql = dict
                .get("sql")
                .map(VmValue::display)
                .filter(|sql| !sql.trim().is_empty())
                .ok_or_else(|| runtime_error(format!("{builtin}: fixture.sql is required")))?;
            let params = dict.get("params").map(vm_value_to_json);
            let rows = match dict.get("rows") {
                Some(VmValue::List(rows)) => (**rows).clone(),
                None | Some(VmValue::Nil) => Vec::new(),
                Some(_) => {
                    return Err(runtime_error(format!(
                        "{builtin}: fixture.rows must be a list"
                    )))
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

/// Record a call against `mock` and answer it from the first matching
/// fixture. Executes return the single row built by `execute_row`.
pub(crate) fn mock_query(
    mock: &SyncMutex<MockDb>,
    prefix: &str,
    sql: &str,
    params: &[VmValue],
    execute: bool,
    execute_row: impl FnOnce(u64) -> VmValue,
) -> Result<Vec<VmValue>, VmError> {
    let params_json = serde_json::Value::Array(params.iter().map(vm_value_to_json).collect());
    let mut mock = mock.lock();
    let call = harn_vm_core::schema::json_to_vm_value(&serde_json::json!({
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
        .ok_or_else(|| runtime_error(format!("{prefix}: no fixture matched `{sql}`")))?;
    if let Some(error) = &fixture.error {
        return Err(runtime_error(format!("{prefix}: {error}")));
    }
    if execute {
        Ok(vec![execute_row(fixture.rows_affected)])
    } else {
        Ok(fixture.rows.clone())
    }
}

pub(crate) fn resource_handle(
    value: Option<&VmValue>,
    expected: &str,
    builtin: &str,
) -> Result<Arc<VmResourceHandle>, VmError> {
    match value {
        Some(VmValue::Resource(handle)) if handle.label() == expected => Ok(Arc::clone(handle)),
        _ => Err(runtime_error(format!(
            "{builtin}: expected {expected} handle"
        ))),
    }
}

/// Resolve a `label` resource and downcast it to its record type. `authority`
/// names the record in the error, e.g. `"sqlite database"`.
pub(crate) fn typed_handle<T: std::any::Any + Send + Sync>(
    value: Option<&VmValue>,
    label: &str,
    builtin: &str,
    authority: &str,
) -> Result<Arc<T>, VmError> {
    resource_handle(value, label, builtin)?
        .downcast::<T>()
        .ok_or_else(|| runtime_error(format!("{builtin}: invalid {authority} authority")))
}

pub(crate) fn is_resource_kind(value: Option<&VmValue>, expected: &str) -> bool {
    matches!(value, Some(VmValue::Resource(handle)) if handle.label() == expected)
}

#[derive(Clone, Copy)]
pub(crate) enum SavepointOp {
    Create,
    Release,
    RollbackTo,
}

pub(crate) fn render_savepoint_sql(op: SavepointOp, name: &str) -> String {
    let quoted = quote_identifier(name);
    match op {
        SavepointOp::Create => format!("SAVEPOINT {quoted}"),
        SavepointOp::Release => format!("RELEASE SAVEPOINT {quoted}"),
        SavepointOp::RollbackTo => format!("ROLLBACK TO SAVEPOINT {quoted}"),
    }
}

pub(crate) fn quote_identifier(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// Required string-path option `key` of a `<builtin>({dir, ...})` call.
pub(crate) fn dir_arg(
    dict: &harn_vm_core::value::DictMap,
    key: &str,
    builtin: &str,
) -> Result<PathBuf, VmError> {
    let value = dict.get(key).ok_or_else(|| {
        runtime_error(format!(
            "{builtin}: option `{key}` is required and must be a path"
        ))
    })?;
    match value {
        VmValue::String(text) => Ok(PathBuf::from(text.as_str())),
        _ => Err(runtime_error(format!(
            "{builtin}: option `{key}` must be a string path"
        ))),
    }
}

#[derive(Clone)]
pub(crate) struct MigrationEntry {
    pub(crate) name: String,
    pub(crate) path: PathBuf,
}

/// Up-migrations (`*.sql`, excluding `*.down.sql`) in `dir`, sorted by name.
pub(crate) fn discover_migrations(
    dir: &Path,
    builtin: &str,
) -> Result<Vec<MigrationEntry>, VmError> {
    if !dir.exists() {
        return Err(runtime_error(format!(
            "{builtin}: directory does not exist: {}",
            dir.display()
        )));
    }
    let read_dir = std::fs::read_dir(dir).map_err(|error| {
        runtime_error(format!(
            "{builtin}: could not read directory {}: {error}",
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

pub(crate) fn sha256(text: &str) -> Vec<u8> {
    use sha2::{Digest, Sha256};
    Sha256::digest(text.as_bytes()).to_vec()
}

pub(crate) fn option_bool(value: Option<&VmValue>) -> Option<bool> {
    match value? {
        VmValue::Bool(value) => Some(*value),
        _ => None,
    }
}

/// An integer option; finite in-range floats are truncated.
pub(crate) fn option_int(options: Option<&harn_vm_core::value::DictMap>, key: &str) -> Option<i64> {
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

pub(crate) fn runtime_error(message: impl Into<String>) -> VmError {
    VmError::Runtime(message.into())
}
