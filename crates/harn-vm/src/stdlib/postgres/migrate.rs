//! `pg_migrate(pool, opts)` — apply `.sql` files from a directory and
//! track the applied set in a configurable migration ledger table.
//!
//! Each migration file is identified by its name (e.g. `0001_init.sql`)
//! and runs in its own transaction. The runner takes a process-wide
//! Postgres advisory lock so two concurrent callers serialize cleanly,
//! and computes a SHA-256 of the file contents at apply time for drift
//! detection.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Instant;

use sqlx_core::row::Row;
use sqlx_postgres::PgPool;

use crate::value::{VmError, VmValue};

use super::{handle_id, pool_by_id, runtime_error, HANDLE_POOL};

/// Stable advisory-lock key distinct from anything sqlx-migrate uses so
/// running `pg_migrate` and `sqlx migrate` side-by-side during a
/// transition does not self-deadlock.
const MIGRATION_LOCK_KEY: i64 = 0x4861_726E_4D67_7201;

const DEFAULT_TABLE: &str = "harn_migrations";

pub(super) async fn run(args: &[VmValue]) -> Result<VmValue, VmError> {
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
    let table_name = opts
        .get("table")
        .and_then(|v| match v {
            VmValue::String(s) => Some(s.to_string()),
            _ => None,
        })
        .unwrap_or_else(|| DEFAULT_TABLE.to_string());
    validate_table_name(&table_name)?;
    let dry_run = matches!(opts.get("dry_run"), Some(VmValue::Bool(true)));

    let entries = discover_migrations(&dir)?;

    let started = Instant::now();
    acquire_lock(&pool).await?;
    let result = async {
        ensure_migrations_table(&pool, &table_name).await?;
        let applied = applied_set(&pool, &table_name).await?;

        let mut applied_now = Vec::new();
        let mut skipped = Vec::new();
        for entry in &entries {
            if applied.contains(&entry.name) {
                skipped.push(entry.name.clone());
                continue;
            }
            if !dry_run {
                apply_one(&pool, &table_name, entry).await?;
            }
            applied_now.push(entry.name.clone());
        }
        Ok::<_, VmError>((applied_now, skipped))
    }
    .await;
    release_lock(&pool).await;
    let (applied_now, skipped) = result?;

    let mut response = BTreeMap::new();
    response.insert(
        "applied".to_string(),
        VmValue::List(Rc::new(
            applied_now
                .into_iter()
                .map(|name| VmValue::String(Rc::from(name)))
                .collect(),
        )),
    );
    response.insert(
        "skipped".to_string(),
        VmValue::List(Rc::new(
            skipped
                .into_iter()
                .map(|name| VmValue::String(Rc::from(name)))
                .collect(),
        )),
    );
    response.insert(
        "available".to_string(),
        VmValue::List(Rc::new(
            entries
                .iter()
                .map(|entry| VmValue::String(Rc::from(entry.name.clone())))
                .collect(),
        )),
    );
    response.insert("dry_run".to_string(), VmValue::Bool(dry_run));
    response.insert(
        "duration_ms".to_string(),
        VmValue::Int(started.elapsed().as_millis() as i64),
    );
    response.insert(
        "table".to_string(),
        VmValue::String(Rc::from(table_name.as_str())),
    );
    Ok(VmValue::Dict(Rc::new(response)))
}

fn dir_arg(dict: &BTreeMap<String, VmValue>, key: &str) -> Result<PathBuf, VmError> {
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

async fn ensure_migrations_table(pool: &PgPool, table: &str) -> Result<(), VmError> {
    let sql = format!(
        "CREATE TABLE IF NOT EXISTS \"{table}\" (\
             name TEXT PRIMARY KEY,\
             applied_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),\
             checksum BYTEA NOT NULL\
         )"
    );
    sqlx_core::raw_sql::raw_sql(&sql)
        .execute(pool)
        .await
        .map_err(|error| runtime_error(format!("pg_migrate: ensure table failed: {error}")))?;
    Ok(())
}

async fn applied_set(pool: &PgPool, table: &str) -> Result<BTreeSet<String>, VmError> {
    let sql = format!("SELECT name FROM \"{table}\"");
    let rows = sqlx_core::query::query::<sqlx_postgres::Postgres>(&sql)
        .fetch_all(pool)
        .await
        .map_err(|error| runtime_error(format!("pg_migrate: select applied failed: {error}")))?;
    Ok(rows
        .iter()
        .map(|row| row.get::<String, _>(0))
        .collect::<BTreeSet<_>>())
}

async fn apply_one(pool: &PgPool, table: &str, entry: &MigrationEntry) -> Result<(), VmError> {
    let sql = std::fs::read_to_string(&entry.path).map_err(|error| {
        runtime_error(format!(
            "pg_migrate: could not read {}: {error}",
            entry.path.display()
        ))
    })?;
    let checksum = sha256(&sql);

    let mut tx = pool
        .begin()
        .await
        .map_err(|error| runtime_error(format!("pg_migrate: begin failed: {error}")))?;

    sqlx_core::raw_sql::raw_sql(&sql)
        .execute(&mut *tx)
        .await
        .map_err(|error| runtime_error(format!("pg_migrate: applying {}: {error}", entry.name)))?;

    let insert = format!("INSERT INTO \"{table}\" (name, checksum) VALUES ($1, $2)");
    sqlx_core::query::query::<sqlx_postgres::Postgres>(&insert)
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

async fn acquire_lock(pool: &PgPool) -> Result<(), VmError> {
    sqlx_core::query::query::<sqlx_postgres::Postgres>("SELECT pg_advisory_lock($1)")
        .bind(MIGRATION_LOCK_KEY)
        .execute(pool)
        .await
        .map_err(|error| runtime_error(format!("pg_migrate: advisory lock failed: {error}")))?;
    Ok(())
}

async fn release_lock(pool: &PgPool) {
    let _ = sqlx_core::query::query::<sqlx_postgres::Postgres>("SELECT pg_advisory_unlock($1)")
        .bind(MIGRATION_LOCK_KEY)
        .execute(pool)
        .await;
}

fn sha256(text: &str) -> Vec<u8> {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    hasher.finalize().to_vec()
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
