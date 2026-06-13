//! `pg_migrate(pool, opts)` — apply `.sql` files from a directory and
//! track the applied set in a configurable migration ledger table.
//!
//! Two ledger formats are supported via `ledger`:
//!
//! - `ledger: "harn"` (default) — the native Harn ledger. Each migration
//!   file is identified by its name (e.g. `0001_init.sql`) and runs in its
//!   own transaction. The runner takes a process-wide Postgres advisory
//!   lock so two concurrent callers serialize cleanly, and computes a
//!   SHA-256 of the file contents at apply time for drift detection.
//! - `ledger: "sqlx"` — byte-for-byte compatible with SQLx's
//!   `_sqlx_migrations` table. Versions are the integer prefix of each
//!   filename, checksums are SHA-384 of the raw file, and the advisory
//!   lock id matches SQLx's so `harn` and a concurrent `sqlx migrate`
//!   mutually exclude. This lets Harn apply an existing SQLx migration
//!   history idempotently without forking it.

use crate::value::VmDictExt;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use sqlx_core::connection::Connection;
use sqlx_core::executor::Executor;
use sqlx_core::row::Row;
use sqlx_core::sql_str::AssertSqlSafe;
use sqlx_postgres::{PgConnection, PgPool};

use crate::value::{VmError, VmValue};

use super::{handle_id, pool_by_id, recycle_pool_after_ddl, runtime_error, HANDLE_POOL};

/// Stable advisory-lock key distinct from anything sqlx-migrate uses so
/// running `pg_migrate` and `sqlx migrate` side-by-side during a
/// transition does not self-deadlock.
const MIGRATION_LOCK_KEY: i64 = 0x4861_726E_4D67_7201;

const DEFAULT_TABLE: &str = "harn_migrations";

/// SQLx's canonical ledger table name. The `ledger: "sqlx"` mode always
/// targets this table so a Harn-driven migration is indistinguishable
/// from a `sqlx migrate run`.
const SQLX_TABLE: &str = "_sqlx_migrations";

/// Which ledger format to read/write.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ledger {
    /// Native Harn ledger (`name TEXT PK`, SHA-256). Default.
    Harn,
    /// SQLx-compatible `_sqlx_migrations` ledger (`version BIGINT PK`,
    /// SHA-384).
    Sqlx,
}

impl Ledger {
    fn parse(opts: &crate::value::DictMap) -> Result<Self, VmError> {
        match opts.get("ledger") {
            None => Ok(Ledger::Harn),
            Some(VmValue::String(s)) => match s.as_ref() {
                "harn" => Ok(Ledger::Harn),
                "sqlx" => Ok(Ledger::Sqlx),
                other => Err(runtime_error(format!(
                    "pg_migrate: unknown ledger `{other}`; expected \"harn\" or \"sqlx\""
                ))),
            },
            Some(_) => Err(runtime_error(
                "pg_migrate: option `ledger` must be a string (\"harn\" or \"sqlx\")",
            )),
        }
    }
}

pub(super) async fn run(args: Vec<VmValue>) -> Result<VmValue, VmError> {
    let pool_handle = args.first().ok_or_else(|| {
        runtime_error("pg_migrate: pool handle is required as the first argument")
    })?;
    let opts = args
        .get(1)
        .and_then(VmValue::as_dict)
        .cloned()
        .ok_or_else(|| {
            runtime_error("pg_migrate: second argument must be an options dict {dir, ...}")
        })?;

    let pool_id = handle_id(Some(pool_handle), HANDLE_POOL, "pg_migrate")?;
    let pool = pool_by_id(&pool_id)?;
    let dir = dir_arg(&opts, "dir")?;
    let ledger = Ledger::parse(&opts)?;

    // In sqlx mode the ledger table name is fixed; an explicit `table`
    // that disagrees is a hard error so callers do not believe they wrote
    // somewhere they did not.
    let table_name = match ledger {
        Ledger::Sqlx => {
            if let Some(VmValue::String(s)) = opts.get("table") {
                if s.as_ref() != SQLX_TABLE {
                    return Err(runtime_error(format!(
                        "pg_migrate: ledger \"sqlx\" always uses table `{SQLX_TABLE}`; \
                         remove the conflicting `table: \"{s}\"`"
                    )));
                }
            }
            SQLX_TABLE.to_string()
        }
        Ledger::Harn => opts
            .get("table")
            .and_then(|v| match v {
                VmValue::String(s) => Some(s.to_string()),
                _ => None,
            })
            .unwrap_or_else(|| DEFAULT_TABLE.to_string()),
    };
    validate_table_name(&table_name)?;
    let dry_run = matches!(opts.get("dry_run"), Some(VmValue::Bool(true)));

    match ledger {
        Ledger::Harn => run_harn(pool, &dir, table_name, dry_run).await,
        Ledger::Sqlx => run_sqlx(pool, &dir, table_name, dry_run).await,
    }
}

// ---------------------------------------------------------------------------
// Harn ledger (default; unchanged behavior)
// ---------------------------------------------------------------------------

async fn run_harn(
    pool: Arc<PgPool>,
    dir: &Path,
    table_name: String,
    dry_run: bool,
) -> Result<VmValue, VmError> {
    let entries = discover_migrations(dir)?;

    let started = Instant::now();
    // Pin a SINGLE connection for the whole run. `pg_advisory_lock` is
    // session-scoped, so the lock, every read/write, and the unlock must run
    // on the same backend or there is no mutual exclusion (and the unlock
    // would no-op on a connection that never held the lock, leaking it). This
    // mirrors sqlx-migrate, which acquires one `PoolConnection` for
    // lock -> migrate -> unlock.
    let mut conn = pool.acquire().await.map_err(|error| {
        runtime_error(format!("pg_migrate: acquire connection failed: {error}"))
    })?;

    acquire_lock(&mut conn, MIGRATION_LOCK_KEY).await?;
    let result = async {
        ensure_migrations_table(&mut conn, &table_name).await?;
        let applied = applied_set(&mut conn, &table_name).await?;

        let mut applied_now = Vec::new();
        let mut skipped = Vec::new();
        for entry in entries.iter() {
            if let Some(existing) = applied.get(&entry.name) {
                // Already applied: re-hash the on-disk file and compare to the
                // recorded SHA-256 so drift (an edited migration) is caught.
                let checksum = sha256_file(&entry.path)?;
                if &checksum != existing {
                    return Err(runtime_error(format!(
                        "pg_migrate: checksum mismatch for migration {}; the \
                         recorded checksum differs from the file on disk",
                        entry.name
                    )));
                }
                skipped.push(entry.name.clone());
                continue;
            }
            if !dry_run {
                apply_one(&mut conn, &table_name, entry).await?;
            }
            applied_now.push(entry.name.clone());
        }
        Ok::<_, VmError>((applied_now, skipped))
    }
    .await;
    release_lock(&mut conn, MIGRATION_LOCK_KEY).await;
    // Release the pinned connection back to the pool *before* recycling so the
    // post-DDL cache clear can drain it too.
    drop(conn);
    let (applied_now, skipped) = result?;

    // Migrations ran DDL on a fresh connection; pooled connections may hold a
    // cached plan that the new schema invalidates (`0A000`). Recycle on a
    // non-dry-run that actually applied something (M-5).
    if !dry_run && !applied_now.is_empty() {
        let max = pool.options().get_max_connections();
        recycle_pool_after_ddl(pool.as_ref(), max).await;
    }

    let available: Vec<String> = entries.iter().map(|entry| entry.name.clone()).collect();
    Ok(build_response(
        applied_now,
        skipped,
        available,
        dry_run,
        started.elapsed().as_millis() as i64,
        &table_name,
    ))
}

fn dir_arg(dict: &crate::value::DictMap, key: &str) -> Result<PathBuf, VmError> {
    let value = dict.get(key).ok_or_else(|| {
        runtime_error(format!(
            "pg_migrate: option `{key}` is required and must be a path"
        ))
    })?;
    match value {
        VmValue::String(text) => Ok(PathBuf::from(text.as_ref())),
        _ => Err(runtime_error(format!(
            "pg_migrate: option `{key}` must be a string path"
        ))),
    }
}

#[derive(Clone)]
struct MigrationEntry {
    name: String,
    path: PathBuf,
}

fn discover_migrations(dir: &Path) -> Result<Vec<MigrationEntry>, VmError> {
    if !dir.exists() {
        return Err(runtime_error(format!(
            "pg_migrate: directory does not exist: {}",
            dir.display()
        )));
    }
    let read_dir = std::fs::read_dir(dir).map_err(|error| {
        runtime_error(format!(
            "pg_migrate: could not read directory {}: {error}",
            dir.display()
        ))
    })?;
    let mut entries: Vec<MigrationEntry> = read_dir
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.ends_with(".sql") && !name.ends_with(".down.sql") {
                Some(MigrationEntry { name, path })
            } else {
                None
            }
        })
        .collect();
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(entries)
}

async fn ensure_migrations_table(conn: &mut PgConnection, table: &str) -> Result<(), VmError> {
    let sql = format!(
        "CREATE TABLE IF NOT EXISTS \"{table}\" (\
             name TEXT PRIMARY KEY,\
             applied_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),\
             checksum BYTEA NOT NULL\
         )"
    );
    conn.execute(AssertSqlSafe(sql))
        .await
        .map_err(|error| runtime_error(format!("pg_migrate: ensure table failed: {error}")))?;
    Ok(())
}

/// Read the full ledger as `name -> stored SHA-256 checksum` so already-applied
/// migrations can be re-hashed and checked for drift.
async fn applied_set(
    conn: &mut PgConnection,
    table: &str,
) -> Result<BTreeMap<String, Vec<u8>>, VmError> {
    let sql = format!("SELECT name, checksum FROM \"{table}\"");
    let rows = sqlx_core::query::query::<sqlx_postgres::Postgres>(AssertSqlSafe(sql))
        .fetch_all(conn)
        .await
        .map_err(|error| runtime_error(format!("pg_migrate: select applied failed: {error}")))?;
    Ok(rows
        .iter()
        .map(|row| (row.get::<String, _>(0), row.get::<Vec<u8>, _>(1)))
        .collect::<BTreeMap<_, _>>())
}

async fn apply_one(
    conn: &mut PgConnection,
    table: &str,
    entry: &MigrationEntry,
) -> Result<(), VmError> {
    let sql = std::fs::read_to_string(&entry.path).map_err(|error| {
        runtime_error(format!(
            "pg_migrate: could not read {}: {error}",
            entry.path.display()
        ))
    })?;
    let checksum = sha256(&sql);

    // Per-migration transaction started from the pinned connection (so it runs
    // on the same backend that holds the advisory lock).
    let mut tx = conn
        .begin()
        .await
        .map_err(|error| runtime_error(format!("pg_migrate: begin failed: {error}")))?;

    (&mut *tx)
        .execute(AssertSqlSafe(sql))
        .await
        .map_err(|error| runtime_error(format!("pg_migrate: applying {}: {error}", entry.name)))?;

    let insert = format!("INSERT INTO \"{table}\" (name, checksum) VALUES ($1, $2)");
    sqlx_core::query::query::<sqlx_postgres::Postgres>(AssertSqlSafe(insert))
        .bind(entry.name.clone())
        .bind(checksum)
        .execute(&mut *tx)
        .await
        .map_err(|error| {
            runtime_error(format!("pg_migrate: record {} failed: {error}", entry.name))
        })?;

    tx.commit().await.map_err(|error| {
        runtime_error(format!("pg_migrate: commit {} failed: {error}", entry.name))
    })
}

/// Take the session-scoped advisory lock on the PINNED connection. Holding it
/// on the pool would be meaningless: the next pool checkout is a different
/// backend that never acquired the lock.
async fn acquire_lock(conn: &mut PgConnection, key: i64) -> Result<(), VmError> {
    sqlx_core::query::query::<sqlx_postgres::Postgres>("SELECT pg_advisory_lock($1)")
        .bind(key)
        .execute(conn)
        .await
        .map_err(|error| runtime_error(format!("pg_migrate: advisory lock failed: {error}")))?;
    Ok(())
}

/// Release the session-scoped advisory lock on the same pinned connection that
/// took it. `pg_advisory_unlock` returns whether a lock was actually released;
/// since the connection is pinned this is normally `true`, so a `false` (or a
/// query error) signals something unexpected and is logged.
async fn release_lock(conn: &mut PgConnection, key: i64) {
    match sqlx_core::query::query::<sqlx_postgres::Postgres>("SELECT pg_advisory_unlock($1)")
        .bind(key)
        .fetch_optional(conn)
        .await
    {
        Ok(Some(row)) => {
            let released: bool = row.get::<bool, _>(0);
            if !released {
                tracing::warn!(
                    lock_key = key,
                    "pg_migrate: pg_advisory_unlock returned false; the advisory \
                     lock was not held by this connection"
                );
            }
        }
        Ok(None) => {
            tracing::warn!(
                lock_key = key,
                "pg_migrate: pg_advisory_unlock returned no row"
            );
        }
        Err(error) => {
            tracing::warn!(
                lock_key = key,
                %error,
                "pg_migrate: releasing advisory lock failed"
            );
        }
    }
}

fn sha256(text: &str) -> Vec<u8> {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    hasher.finalize().to_vec()
}

fn sha256_file(path: &Path) -> Result<Vec<u8>, VmError> {
    let sql = std::fs::read_to_string(path).map_err(|error| {
        runtime_error(format!(
            "pg_migrate: could not read {}: {error}",
            path.display()
        ))
    })?;
    Ok(sha256(&sql))
}

fn validate_table_name(name: &str) -> Result<(), VmError> {
    if name.is_empty() || name.len() > 63 {
        return Err(runtime_error(
            "pg_migrate: option `table` must be 1..=63 bytes",
        ));
    }
    let first = name.chars().next().unwrap();
    if !(first.is_ascii_alphabetic() || first == '_') {
        return Err(runtime_error(
            "pg_migrate: option `table` must start with a letter or underscore",
        ));
    }
    for ch in name.chars() {
        if !(ch.is_ascii_alphanumeric() || ch == '_') {
            return Err(runtime_error(format!(
                "pg_migrate: option `table` contains invalid character `{ch}`"
            )));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Shared response shape
// ---------------------------------------------------------------------------

fn build_response(
    applied_now: Vec<String>,
    skipped: Vec<String>,
    available: Vec<String>,
    dry_run: bool,
    duration_ms: i64,
    table_name: &str,
) -> VmValue {
    fn str_list(items: Vec<String>) -> VmValue {
        VmValue::List(Arc::new(
            items
                .into_iter()
                .map(|name| VmValue::String(Arc::from(name)))
                .collect(),
        ))
    }

    let mut response = BTreeMap::new();
    response.insert("applied".to_string(), str_list(applied_now));
    response.insert("skipped".to_string(), str_list(skipped));
    response.insert("available".to_string(), str_list(available));
    response.insert("dry_run".to_string(), VmValue::Bool(dry_run));
    response.insert("duration_ms".to_string(), VmValue::Int(duration_ms));
    response.put_str("table", table_name);
    VmValue::dict(response)
}

// ---------------------------------------------------------------------------
// SQLx-compatible ledger (`_sqlx_migrations`)
// ---------------------------------------------------------------------------

/// A forward SQLx migration parsed from a filename.
#[derive(Clone, Debug)]
struct SqlxMigration {
    /// Integer version prefix (`splitn(2, '_')[0]` parsed as i64).
    version: i64,
    /// Filename with the type suffix trimmed and `_` -> ` `.
    description: String,
    /// The original filename (for diagnostics / the result lists).
    name: String,
    path: PathBuf,
}

/// Discover forward (`*.up.sql` / non-`.down`) SQLx migrations, parse their
/// versions/descriptions, sort numerically by version, and dedupe duplicate
/// versions with a warn-and-skip (mirrors harn-cloud's `handled` set). An
/// un-parseable integer prefix is a hard error (SQLx behavior).
fn discover_sqlx_migrations(dir: &Path) -> Result<Vec<SqlxMigration>, VmError> {
    if !dir.exists() {
        return Err(runtime_error(format!(
            "pg_migrate: directory does not exist: {}",
            dir.display()
        )));
    }
    let read_dir = std::fs::read_dir(dir).map_err(|error| {
        runtime_error(format!(
            "pg_migrate: could not read directory {}: {error}",
            dir.display()
        ))
    })?;

    let mut parsed: Vec<SqlxMigration> = Vec::new();
    for entry in read_dir.filter_map(|entry| entry.ok()) {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();

        // Mirror sqlx's `resolve_blocking`: <VERSION>_<DESC>.<DIR>.sql.
        let parts: Vec<&str> = name.splitn(2, '_').collect();
        if parts.len() != 2 || !parts[1].ends_with(".sql") {
            // Not of the recognized format; ignore (matches sqlx).
            continue;
        }
        // Skip down migrations.
        if parts[1].ends_with(".down.sql") {
            continue;
        }

        let version: i64 = parts[0].parse().map_err(|_| {
            runtime_error(format!(
                "pg_migrate: error parsing migration filename {name:?}; \
                 expected integer version prefix (e.g. `01_foo.sql`)"
            ))
        })?;

        // Trim the type suffix, then `_` -> ` ` (matches sqlx).
        let suffix = if parts[1].ends_with(".up.sql") {
            ".up.sql"
        } else {
            ".sql"
        };
        let description = parts[1].trim_end_matches(suffix).replace('_', " ");

        parsed.push(SqlxMigration {
            version,
            description,
            name,
            path,
        });
    }

    // SQLx sorts migrations by `version` (the `Ord` impl on `Migration`
    // orders on `version` first); numeric, not lexicographic.
    parsed.sort_by(|a, b| a.version.cmp(&b.version).then(a.name.cmp(&b.name)));

    // Dedupe duplicate versions: first one wins, warn-and-skip the rest.
    let mut seen: HashSet<i64> = HashSet::new();
    let mut deduped = Vec::with_capacity(parsed.len());
    for migration in parsed {
        if !seen.insert(migration.version) {
            tracing::warn!(
                version = migration.version,
                description = %migration.description,
                file = %migration.name,
                "pg_migrate: skipping migration with duplicate version (another file \
                 already claimed this prefix); fix by renaming one of the files",
            );
            continue;
        }
        deduped.push(migration);
    }

    Ok(deduped)
}

async fn run_sqlx(
    pool: Arc<PgPool>,
    dir: &Path,
    table_name: String,
    dry_run: bool,
) -> Result<VmValue, VmError> {
    let migrations = discover_sqlx_migrations(dir)?;

    let started = Instant::now();
    // Pin one connection for lock -> migrate -> unlock, exactly like
    // sqlx-migrate. The advisory lock is session-scoped, so it only mutually
    // excludes when held on the same backend that does the work.
    let mut conn = pool.acquire().await.map_err(|error| {
        runtime_error(format!("pg_migrate: acquire connection failed: {error}"))
    })?;

    let lock_id = sqlx_lock_id(&mut conn).await?;
    acquire_lock(&mut conn, lock_id).await?;
    let result = run_sqlx_locked(&mut conn, &table_name, dry_run, &migrations).await;
    release_lock(&mut conn, lock_id).await;
    // Release the pinned connection before recycling so the post-DDL cache clear
    // can drain it too.
    drop(conn);
    let (applied_now, skipped) = result?;

    // See `run_harn`: recycle prepared-statement caches after DDL (M-5).
    if !dry_run && !applied_now.is_empty() {
        let max = pool.options().get_max_connections();
        recycle_pool_after_ddl(pool.as_ref(), max).await;
    }

    let available: Vec<String> = migrations.iter().map(|m| m.name.clone()).collect();
    Ok(build_response(
        applied_now,
        skipped,
        available,
        dry_run,
        started.elapsed().as_millis() as i64,
        &table_name,
    ))
}

async fn run_sqlx_locked(
    conn: &mut PgConnection,
    table: &str,
    dry_run: bool,
    migrations: &[SqlxMigration],
) -> Result<(Vec<String>, Vec<String>), VmError> {
    ensure_sqlx_migrations_table(conn, table).await?;

    // Refuse to proceed on a dirty ledger (a failed migration).
    if let Some(version) = sqlx_dirty_version(conn, table).await? {
        return Err(runtime_error(format!(
            "pg_migrate: dirty migration {version}; the ledger has a failed \
             migration recorded — resolve it before re-running"
        )));
    }

    // version -> checksum of everything already recorded.
    let applied = sqlx_applied(conn, table).await?;

    let mut applied_now = Vec::new();
    let mut skipped = Vec::new();
    for migration in migrations {
        if let Some(existing) = applied.get(&migration.version) {
            let checksum = sha384_file(&migration.path)?;
            if existing != &checksum {
                return Err(runtime_error(format!(
                    "pg_migrate: checksum mismatch for migration {} ({}); the \
                     recorded checksum differs from the file on disk",
                    migration.version, migration.name
                )));
            }
            skipped.push(migration.name.clone());
            continue;
        }
        if !dry_run {
            apply_sqlx_one(conn, table, migration).await?;
        }
        applied_now.push(migration.name.clone());
    }

    Ok((applied_now, skipped))
}

/// EXACT schema match for SQLx 0.9 `_sqlx_migrations`.
async fn ensure_sqlx_migrations_table(conn: &mut PgConnection, table: &str) -> Result<(), VmError> {
    let sql = format!(
        "CREATE TABLE IF NOT EXISTS \"{table}\" (\
             version BIGINT PRIMARY KEY,\
             description TEXT NOT NULL,\
             installed_on TIMESTAMPTZ NOT NULL DEFAULT now(),\
             success BOOLEAN NOT NULL,\
             checksum BYTEA NOT NULL,\
             execution_time BIGINT NOT NULL\
         )"
    );
    conn.execute(AssertSqlSafe(sql))
        .await
        .map_err(|error| runtime_error(format!("pg_migrate: ensure sqlx table failed: {error}")))?;
    Ok(())
}

async fn sqlx_dirty_version(conn: &mut PgConnection, table: &str) -> Result<Option<i64>, VmError> {
    let sql =
        format!("SELECT version FROM \"{table}\" WHERE success = false ORDER BY version LIMIT 1");
    let row = sqlx_core::query::query::<sqlx_postgres::Postgres>(AssertSqlSafe(sql))
        .fetch_optional(conn)
        .await
        .map_err(|error| runtime_error(format!("pg_migrate: dirty check failed: {error}")))?;
    Ok(row.map(|row| row.get::<i64, _>(0)))
}

async fn sqlx_applied(
    conn: &mut PgConnection,
    table: &str,
) -> Result<HashMap<i64, Vec<u8>>, VmError> {
    let sql = format!("SELECT version, checksum FROM \"{table}\" ORDER BY version");
    let rows = sqlx_core::query::query::<sqlx_postgres::Postgres>(AssertSqlSafe(sql))
        .fetch_all(conn)
        .await
        .map_err(|error| {
            runtime_error(format!("pg_migrate: select applied (sqlx) failed: {error}"))
        })?;
    Ok(rows
        .iter()
        .map(|row| (row.get::<i64, _>(0), row.get::<Vec<u8>, _>(1)))
        .collect())
}

async fn apply_sqlx_one(
    conn: &mut PgConnection,
    table: &str,
    migration: &SqlxMigration,
) -> Result<(), VmError> {
    let sql = std::fs::read_to_string(&migration.path).map_err(|error| {
        runtime_error(format!(
            "pg_migrate: could not read {}: {error}",
            migration.path.display()
        ))
    })?;
    let checksum = sha384(&sql);
    // SQLx opts out of the wrapping transaction when the file starts with
    // a `-- no-transaction` comment.
    let no_tx = sql.starts_with("-- no-transaction");

    let start = Instant::now();

    if no_tx {
        // Run the migration SQL then record it, no wrapping transaction, on
        // the pinned connection that holds the advisory lock.
        (&mut *conn)
            .execute(AssertSqlSafe(sql))
            .await
            .map_err(|error| {
                runtime_error(format!(
                    "pg_migrate: applying {} ({}): {error}",
                    migration.version, migration.name
                ))
            })?;
        sqlx_insert_row(&mut *conn, table, migration, &checksum).await?;
    } else {
        let mut tx = conn
            .begin()
            .await
            .map_err(|error| runtime_error(format!("pg_migrate: begin failed: {error}")))?;
        (&mut *tx)
            .execute(AssertSqlSafe(sql))
            .await
            .map_err(|error| {
                runtime_error(format!(
                    "pg_migrate: applying {} ({}): {error}",
                    migration.version, migration.name
                ))
            })?;
        sqlx_insert_row(&mut *tx, table, migration, &checksum).await?;
        tx.commit().await.map_err(|error| {
            runtime_error(format!(
                "pg_migrate: commit {} ({}) failed: {error}",
                migration.version, migration.name
            ))
        })?;
    }

    // Backfill `execution_time` (nanos) the way SQLx does. Best-effort.
    let elapsed_nanos = start.elapsed().as_nanos() as i64;
    let update = format!("UPDATE \"{table}\" SET execution_time = $1 WHERE version = $2");
    sqlx_core::query::query::<sqlx_postgres::Postgres>(AssertSqlSafe(update))
        .bind(elapsed_nanos)
        .bind(migration.version)
        .execute(&mut *conn)
        .await
        .map_err(|error| {
            runtime_error(format!(
                "pg_migrate: record execution_time for {} failed: {error}",
                migration.version
            ))
        })?;

    Ok(())
}

/// Insert the success row exactly as SQLx does, with `execution_time = -1`.
async fn sqlx_insert_row<'c, E>(
    executor: E,
    table: &str,
    migration: &SqlxMigration,
    checksum: &[u8],
) -> Result<(), VmError>
where
    E: Executor<'c, Database = sqlx_postgres::Postgres>,
{
    let insert = format!(
        "INSERT INTO \"{table}\" (version, description, success, checksum, execution_time) \
         VALUES ($1, $2, TRUE, $3, -1)"
    );
    sqlx_core::query::query::<sqlx_postgres::Postgres>(AssertSqlSafe(insert))
        .bind(migration.version)
        .bind(migration.description.clone())
        .bind(checksum.to_vec())
        .execute(executor)
        .await
        .map_err(|error| {
            runtime_error(format!(
                "pg_migrate: record {} ({}) failed: {error}",
                migration.version, migration.name
            ))
        })?;
    Ok(())
}

/// SQLx's per-database advisory lock id:
/// `0x3d32ad9e * crc32_iso_hdlc(current_database())`. Computing it the same
/// way means a Harn `pg_migrate(ledger: "sqlx")` and a concurrent
/// `sqlx migrate run` take the SAME lock and serialize against each other.
async fn sqlx_lock_id(conn: &mut PgConnection) -> Result<i64, VmError> {
    let row = sqlx_core::query::query::<sqlx_postgres::Postgres>("SELECT current_database()")
        .fetch_one(conn)
        .await
        .map_err(|error| {
            runtime_error(format!("pg_migrate: current_database() failed: {error}"))
        })?;
    let database_name = row.get::<String, _>(0);
    Ok(generate_sqlx_lock_id(&database_name))
}

/// Port of sqlx-postgres `generate_lock_id`:
/// `0x3d32ad9e * (CRC_32_ISO_HDLC(name) as i64)`. We compute the CRC inline
/// so we do not need the `crc` crate as a dependency.
fn generate_sqlx_lock_id(database_name: &str) -> i64 {
    0x3d32ad9e * (crc32_iso_hdlc(database_name.as_bytes()) as i64)
}

/// CRC-32/ISO-HDLC (a.k.a. the standard zlib CRC-32): reflected, polynomial
/// 0xEDB88320, init/xorout 0xFFFFFFFF. Identical to what `crc::CRC_32_ISO_HDLC`
/// and `crc32fast` compute.
fn crc32_iso_hdlc(bytes: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &byte in bytes {
        crc ^= byte as u32;
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

fn sha384(text: &str) -> Vec<u8> {
    use sha2::{Digest, Sha384};
    Sha384::digest(text.as_bytes()).to_vec()
}

fn sha384_file(path: &Path) -> Result<Vec<u8>, VmError> {
    let sql = std::fs::read_to_string(path).map_err(|error| {
        runtime_error(format!(
            "pg_migrate: could not read {}: {error}",
            path.display()
        ))
    })?;
    Ok(sha384(&sql))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// CRC-32/ISO-HDLC must match the canonical check value (the standard
    /// "123456789" test vector for CRC-32 is 0xCBF43926).
    #[test]
    fn crc32_iso_hdlc_matches_check_vector() {
        assert_eq!(crc32_iso_hdlc(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32_iso_hdlc(b""), 0x0000_0000);
    }

    /// The lock id must be computed exactly like sqlx-postgres'
    /// `generate_lock_id`: `0x3d32ad9e * (crc32 as i64)`.
    #[test]
    fn sqlx_lock_id_matches_sqlx_formula() {
        let db = "harn_cloud";
        let expected = 0x3d32ad9e_i64 * (crc32_iso_hdlc(db.as_bytes()) as i64);
        assert_eq!(generate_sqlx_lock_id(db), expected);
        // Never overflows i64 for any database name (0x3d32ad9e * u32::MAX
        // < i64::MAX), so plain multiply is safe and matches sqlx.
        assert!(generate_sqlx_lock_id("postgres") != 0);
    }

    /// SHA-384 of the file content is a 48-byte digest, distinct from the
    /// SHA-256 used by the harn ledger.
    #[test]
    fn sha384_is_48_bytes_and_differs_from_sha256() {
        let sql = "CREATE TABLE t (id INT);";
        let s384 = sha384(sql);
        let s256 = sha256(sql);
        assert_eq!(s384.len(), 48);
        assert_eq!(s256.len(), 32);
        assert_ne!(s384[..32], s256[..]);
    }

    /// Version/description parsing mirrors sqlx: integer prefix, suffix
    /// trimmed, `_` -> ` `. Numeric (not lexicographic) sort, dedupe.
    #[test]
    fn discover_sqlx_parses_versions_descriptions_and_sorts_numerically() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        std::fs::write(dir.join("20260419170000_bootstrap.up.sql"), "SELECT 1").unwrap();
        std::fs::write(dir.join("20260419170000_bootstrap.down.sql"), "SELECT 0").unwrap();
        std::fs::write(dir.join("9_early_thing.up.sql"), "SELECT 2").unwrap();
        std::fs::write(dir.join("100_later_thing.sql"), "SELECT 3").unwrap();
        // A non-migration file is ignored.
        std::fs::write(dir.join("README.md"), "ignore me").unwrap();

        let migrations = discover_sqlx_migrations(dir).expect("discover");
        let versions: Vec<i64> = migrations.iter().map(|m| m.version).collect();
        // Numeric ascending: 9 < 100 < 20260419170000 (lexicographic would
        // sort "100" before "20260419170000" before "9").
        assert_eq!(versions, vec![9, 100, 20260419170000]);
        assert_eq!(migrations[0].description, "early thing");
        assert_eq!(migrations[1].description, "later thing");
        assert_eq!(migrations[2].description, "bootstrap");
        // `.down.sql` was skipped.
        assert!(migrations.iter().all(|m| !m.name.ends_with(".down.sql")));
    }

    /// A non-integer version prefix is a hard error (matches sqlx).
    #[test]
    fn discover_sqlx_errors_on_non_integer_prefix() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        std::fs::write(dir.join("notanumber_thing.up.sql"), "SELECT 1").unwrap();
        let err = discover_sqlx_migrations(dir).unwrap_err();
        assert!(
            format!("{err:?}").contains("integer version prefix"),
            "unexpected error: {err:?}"
        );
    }

    /// Duplicate versions warn-and-skip (first wins).
    #[test]
    fn discover_sqlx_dedupes_duplicate_versions() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        std::fs::write(dir.join("5_first.up.sql"), "SELECT 1").unwrap();
        std::fs::write(dir.join("5_second.up.sql"), "SELECT 2").unwrap();
        let migrations = discover_sqlx_migrations(dir).expect("discover");
        assert_eq!(migrations.len(), 1);
        assert_eq!(migrations[0].version, 5);
        // First by (version, name) ordering wins: "5_first" < "5_second".
        assert_eq!(migrations[0].name, "5_first.up.sql");
    }
}
