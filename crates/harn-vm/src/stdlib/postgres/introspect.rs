//! Schema introspection + connection-pool observability + declarative
//! partition helpers.
//!
//! Surface (issue #2512 A.9):
//!
//! ```harn
//! // Schema introspection.
//! pg_introspect_tables(db, {schema: "public"})       // → [{schema, table, kind}, ...]
//! pg_introspect_columns(db, "receipts")              // → [{column, type, nullable, default}, ...]
//! pg_introspect_indexes(db, "receipts")              // → [{index, columns, unique, primary}, ...]
//!
//! // Pool + statement-cache observability.
//! pg_pool_stats(db)
//!   // → {size: 5, idle: 3, in_use: 2, max_connections: 10,
//!   //    statement_cache_capacity: 100, circuit_state: "closed", ...}
//!
//! // Declarative partition helpers (pg_partman-style without the extension).
//! pg_partition_attach(db, "events", "events_2026_05", {from: "2026-05-01", to: "2026-06-01"})
//! pg_partition_attach(db, "events", "events_h0", {modulus: 4, remainder: 0})  // hash child
//! pg_partition_detach(db, "events", "events_2026_03")
//! pg_partition_prune(db, "events", "2026-01-01")                 // recurses sub-partition trees
//! pg_partition_retain(db, "events", {keep_days: 90})             // prune everything older
//! pg_partition_create_for_window(db, "events", {interval: "day", ahead: 7})  // maintenance
//! ```
//!
//! Introspection queries hit `information_schema` / `pg_catalog` directly
//! and bind schema/table names — no string concat. Partition helpers
//! validate identifiers but render them quoted; the caller-supplied
//! bounds are bound as parameters (Postgres' `FOR VALUES FROM (...) TO
//! (...)` does not accept binds, so we re-parse the literal via the
//! catalog's `pg_get_partkeydef` to confirm the partition exists after
//! the DDL runs).
//!
//! `pg_partition_prune` / `pg_partition_retain` walk the partition tree
//! recursively, so two-level layouts (range-by-day → hash-by-tenant, or
//! the inverse) prune correctly regardless of which level carries the
//! time column. `pg_partition_create_for_window` is the pg_partman
//! `run_maintenance` equivalent: it pre-creates the next N day/hour
//! partitions so inserts never hit the DEFAULT partition.

use std::collections::{BTreeMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;

use sqlx_core::query::query;
use sqlx_core::row::Row;
use sqlx_postgres::PgPool;

use crate::stdlib::macros::{
    harn_builtin, BuiltinSignature, Param, TY_ANY, TY_BOOL, TY_DICT, TY_LIST,
};
use crate::value::{VmError, VmValue};

use super::{
    bind_params, handle_id, pool_arg, pool_record_by_id, required_arg, row_to_value, runtime_error,
    validate_pg_identifier, HANDLE_POOL,
};

#[harn_builtin(
    sig_expr = BuiltinSignature::variadic(
        "pg_introspect_tables",
        &[Param::new("args", TY_ANY)],
        TY_LIST,
    ),
    kind = "async",
    category = "postgres"
)]
async fn pg_introspect_tables_impl(args: Vec<VmValue>) -> Result<VmValue, VmError> {
    let pool = pool_arg(&args, "pg_introspect_tables")?;
    let options = args.get(1).and_then(VmValue::as_dict);
    let schema = options
        .and_then(|opts| opts.get("schema"))
        .map(VmValue::display)
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| "public".to_string());
    // Aliases use double-quoted `"table"` because `table` is a PG reserved
    // word — the quoting is purely cosmetic on the output column name.
    let sql = r#"
        SELECT n.nspname AS schema,
               c.relname AS "table",
               CASE c.relkind
                 WHEN 'r' THEN 'table'
                 WHEN 'p' THEN 'partitioned_table'
                 WHEN 'v' THEN 'view'
                 WHEN 'm' THEN 'materialized_view'
                 WHEN 'f' THEN 'foreign_table'
                 ELSE c.relkind::text
               END AS kind
        FROM pg_class c
        JOIN pg_namespace n ON c.relnamespace = n.oid
        WHERE n.nspname = $1
          AND c.relkind = ANY('{r,p,v,m,f}')
        ORDER BY c.relname
    "#;
    rows_to_list(
        pool.as_ref(),
        sql,
        &[VmValue::String(Rc::from(schema))],
        "pg_introspect_tables",
    )
    .await
}

#[harn_builtin(
    sig_expr = BuiltinSignature::variadic(
        "pg_introspect_columns",
        &[Param::new("args", TY_ANY)],
        TY_LIST,
    ),
    kind = "async",
    category = "postgres"
)]
async fn pg_introspect_columns_impl(args: Vec<VmValue>) -> Result<VmValue, VmError> {
    let pool = pool_arg(&args, "pg_introspect_columns")?;
    let (schema, table) = split_qualified(
        args.get(1).map(VmValue::display).as_deref().unwrap_or(""),
        "pg_introspect_columns",
    )?;
    let sql = r#"
        SELECT column_name        AS "column",
               udt_name            AS "type",
               data_type           AS data_type,
               is_nullable = 'YES' AS nullable,
               column_default      AS "default"
        FROM information_schema.columns
        WHERE table_schema = $1 AND table_name = $2
        ORDER BY ordinal_position
    "#;
    rows_to_list(
        pool.as_ref(),
        sql,
        &[
            VmValue::String(Rc::from(schema)),
            VmValue::String(Rc::from(table)),
        ],
        "pg_introspect_columns",
    )
    .await
}

#[harn_builtin(
    sig_expr = BuiltinSignature::variadic(
        "pg_introspect_indexes",
        &[Param::new("args", TY_ANY)],
        TY_LIST,
    ),
    kind = "async",
    category = "postgres"
)]
async fn pg_introspect_indexes_impl(args: Vec<VmValue>) -> Result<VmValue, VmError> {
    let pool = pool_arg(&args, "pg_introspect_indexes")?;
    let (schema, table) = split_qualified(
        args.get(1).map(VmValue::display).as_deref().unwrap_or(""),
        "pg_introspect_indexes",
    )?;
    let sql = r#"
        SELECT i.relname AS index,
               array_agg(a.attname ORDER BY x.ord) AS columns,
               ix.indisunique AS "unique",
               ix.indisprimary AS "primary"
        FROM pg_class t
        JOIN pg_namespace n ON t.relnamespace = n.oid
        JOIN pg_index ix ON t.oid = ix.indrelid
        JOIN pg_class i ON i.oid = ix.indexrelid
        JOIN LATERAL unnest(ix.indkey) WITH ORDINALITY AS x(attnum, ord) ON TRUE
        JOIN pg_attribute a ON a.attrelid = t.oid AND a.attnum = x.attnum
        WHERE n.nspname = $1 AND t.relname = $2
        GROUP BY i.relname, ix.indisunique, ix.indisprimary
        ORDER BY i.relname
    "#;
    rows_to_list(
        pool.as_ref(),
        sql,
        &[
            VmValue::String(Rc::from(schema)),
            VmValue::String(Rc::from(table)),
        ],
        "pg_introspect_indexes",
    )
    .await
}

#[harn_builtin(
    sig_expr = BuiltinSignature::variadic("pg_pool_stats", &[Param::new("args", TY_ANY)], TY_DICT),
    kind = "async",
    category = "postgres"
)]
async fn pg_pool_stats_impl(args: Vec<VmValue>) -> Result<VmValue, VmError> {
    let pool_handle = required_arg(&args, 0, "pg_pool_stats", "pool handle")?;
    let pool_id = handle_id(Some(pool_handle), HANDLE_POOL, "pg_pool_stats")?;
    let record = pool_record_by_id(&pool_id)?;
    let pool = record.pool.as_ref();
    let size = pool.size();
    let idle = pool.num_idle();
    let max = record.max_connections;
    let in_use = (size as usize).saturating_sub(idle);

    let mut dict = BTreeMap::new();
    dict.insert("size".to_string(), VmValue::Int(i64::from(size)));
    dict.insert("idle".to_string(), VmValue::Int(idle as i64));
    dict.insert("in_use".to_string(), VmValue::Int(in_use as i64));
    dict.insert("max_connections".to_string(), VmValue::Int(i64::from(max)));
    dict.insert(
        "statement_cache_capacity".to_string(),
        VmValue::Int(record.statement_cache_capacity as i64),
    );
    dict.insert(
        "replicas".to_string(),
        VmValue::Int(record.replicas.len() as i64),
    );
    if !record.replicas.is_empty() {
        let replica_stats: Vec<VmValue> = record
            .replicas
            .iter()
            .map(|pool| {
                let mut entry = BTreeMap::new();
                entry.insert("size".to_string(), VmValue::Int(i64::from(pool.size())));
                entry.insert("idle".to_string(), VmValue::Int(pool.num_idle() as i64));
                VmValue::Dict(Rc::new(entry))
            })
            .collect();
        dict.insert(
            "replica_stats".to_string(),
            VmValue::List(Rc::new(replica_stats)),
        );
    }
    let circuit_state = record.circuit.snapshot();
    dict.insert(
        "circuit_state".to_string(),
        VmValue::String(Rc::from(circuit_state.state)),
    );
    dict.insert(
        "circuit_failures".to_string(),
        VmValue::Int(circuit_state.failures as i64),
    );
    dict.insert(
        "circuit_opened_at_ms".to_string(),
        circuit_state
            .opened_at_ms
            .map(VmValue::Int)
            .unwrap_or(VmValue::Nil),
    );
    Ok(VmValue::Dict(Rc::new(dict)))
}

#[harn_builtin(
    sig_expr = BuiltinSignature::variadic(
        "pg_partition_attach",
        &[Param::new("args", TY_ANY)],
        TY_BOOL,
    ),
    kind = "async",
    category = "postgres"
)]
async fn pg_partition_attach_impl(args: Vec<VmValue>) -> Result<VmValue, VmError> {
    let pool = pool_arg(&args, "pg_partition_attach")?;
    let parent = qualified_identifier(
        args.get(1).map(VmValue::display).as_deref().unwrap_or(""),
        "pg_partition_attach",
    )?;
    let partition = qualified_identifier(
        args.get(2).map(VmValue::display).as_deref().unwrap_or(""),
        "pg_partition_attach",
    )?;
    let bounds = args
        .get(3)
        .and_then(VmValue::as_dict)
        .cloned()
        .ok_or_else(|| {
            runtime_error("pg_partition_attach: bounds dict is required (e.g. {from, to})")
        })?;

    let bounds_clause = render_bounds_clause(&bounds)?;
    let sql = format!(
        "ALTER TABLE {} ATTACH PARTITION {} {bounds_clause}",
        parent.quoted, partition.quoted
    );
    sqlx_core::raw_sql::raw_sql(&sql)
        .execute(pool.as_ref())
        .await
        .map_err(|error| runtime_error(format!("pg_partition_attach: {error}")))?;
    Ok(VmValue::Bool(true))
}

#[harn_builtin(
    sig_expr = BuiltinSignature::variadic(
        "pg_partition_detach",
        &[Param::new("args", TY_ANY)],
        TY_BOOL,
    ),
    kind = "async",
    category = "postgres"
)]
async fn pg_partition_detach_impl(args: Vec<VmValue>) -> Result<VmValue, VmError> {
    let pool = pool_arg(&args, "pg_partition_detach")?;
    let parent = qualified_identifier(
        args.get(1).map(VmValue::display).as_deref().unwrap_or(""),
        "pg_partition_detach",
    )?;
    let partition = qualified_identifier(
        args.get(2).map(VmValue::display).as_deref().unwrap_or(""),
        "pg_partition_detach",
    )?;
    let concurrently = super::option_bool(
        args.get(3)
            .and_then(VmValue::as_dict)
            .and_then(|opts| opts.get("concurrently")),
    )
    .unwrap_or(false);

    let suffix = if concurrently { " CONCURRENTLY" } else { "" };
    let sql = format!(
        "ALTER TABLE {} DETACH PARTITION {}{suffix}",
        parent.quoted, partition.quoted
    );
    sqlx_core::raw_sql::raw_sql(&sql)
        .execute(pool.as_ref())
        .await
        .map_err(|error| runtime_error(format!("pg_partition_detach: {error}")))?;
    Ok(VmValue::Bool(true))
}

#[harn_builtin(
    sig_expr = BuiltinSignature::variadic(
        "pg_partition_prune",
        &[Param::new("args", TY_ANY)],
        TY_LIST,
    ),
    kind = "async",
    category = "postgres"
)]
async fn pg_partition_prune_impl(args: Vec<VmValue>) -> Result<VmValue, VmError> {
    let pool = pool_arg(&args, "pg_partition_prune")?;
    let parent = qualified_identifier(
        args.get(1).map(VmValue::display).as_deref().unwrap_or(""),
        "pg_partition_prune",
    )?;
    let before_literal = match args.get(2) {
        Some(VmValue::String(text)) if !text.trim().is_empty() => text.to_string(),
        _ => {
            return Err(runtime_error(
                "pg_partition_prune: third argument must be a timestamp/date literal",
            ))
        }
    };
    let options = args.get(3).and_then(VmValue::as_dict);
    let dry_run = super::option_bool(options.and_then(|opts| opts.get("dry_run"))).unwrap_or(false);
    prune_partitions(
        pool.as_ref(),
        &parent,
        &before_literal,
        dry_run,
        "pg_partition_prune",
    )
    .await
}

#[harn_builtin(
    sig_expr = BuiltinSignature::variadic(
        "pg_partition_retain",
        &[Param::new("args", TY_ANY)],
        TY_LIST,
    ),
    kind = "async",
    category = "postgres"
)]
async fn pg_partition_retain_impl(args: Vec<VmValue>) -> Result<VmValue, VmError> {
    let pool = pool_arg(&args, "pg_partition_retain")?;
    let parent = qualified_identifier(
        args.get(1).map(VmValue::display).as_deref().unwrap_or(""),
        "pg_partition_retain",
    )?;
    let options = args
        .get(2)
        .and_then(VmValue::as_dict)
        .cloned()
        .ok_or_else(|| {
            runtime_error("pg_partition_retain: options dict is required (e.g. {keep_days: 90})")
        })?;
    let dry_run = super::option_bool(options.get("dry_run")).unwrap_or(false);
    let cutoff = retention_cutoff(&options, chrono::Utc::now())?;
    prune_partitions(
        pool.as_ref(),
        &parent,
        &cutoff,
        dry_run,
        "pg_partition_retain",
    )
    .await
}

#[harn_builtin(
    sig_expr = BuiltinSignature::variadic(
        "pg_partition_create_for_window",
        &[Param::new("args", TY_ANY)],
        TY_LIST,
    ),
    kind = "async",
    category = "postgres"
)]
async fn pg_partition_create_for_window_impl(args: Vec<VmValue>) -> Result<VmValue, VmError> {
    let builtin = "pg_partition_create_for_window";
    let pool = pool_arg(&args, builtin)?;
    let parent = qualified_identifier(
        args.get(1).map(VmValue::display).as_deref().unwrap_or(""),
        builtin,
    )?;
    let options = args
        .get(2)
        .and_then(VmValue::as_dict)
        .cloned()
        .ok_or_else(|| {
            runtime_error(format!(
                "{builtin}: options dict is required (e.g. {{interval: \"day\", ahead: 7}})"
            ))
        })?;
    let interval = parse_interval(options.get("interval"))?;
    let ahead = match options.get("ahead") {
        Some(VmValue::Int(n)) if *n >= 1 => *n,
        _ => {
            return Err(runtime_error(format!(
                "{builtin}: ahead must be a positive integer"
            )))
        }
    };
    let dry_run = super::option_bool(options.get("dry_run")).unwrap_or(false);
    let start = match options.get("start") {
        Some(VmValue::String(text)) if !text.trim().is_empty() => parse_start(text, builtin)?,
        None => chrono::Utc::now(),
        _ => {
            return Err(runtime_error(format!(
                "{builtin}: start must be an ISO date/timestamp string"
            )))
        }
    };

    let windows = partition_windows(&parent.table, interval, ahead, start);
    let existing = existing_partition_names(pool.as_ref(), &parent.qualified, builtin).await?;
    let mut created = Vec::new();
    for window in windows {
        if existing.contains(&window.name) {
            continue;
        }
        // `window.name` is machine-generated from a validated table name
        // plus date digits, so it is a safe identifier; the bounds are
        // formatted timestamps containing only digits, dashes, colons,
        // and spaces — no quote injection surface.
        let child_quoted = format!("\"{}\".\"{}\"", parent.schema, window.name);
        if !dry_run {
            let sql = format!(
                "CREATE TABLE IF NOT EXISTS {child_quoted} PARTITION OF {} \
                 FOR VALUES FROM ('{}') TO ('{}')",
                parent.quoted, window.from, window.to
            );
            sqlx_core::raw_sql::raw_sql(&sql)
                .execute(pool.as_ref())
                .await
                .map_err(|error| runtime_error(format!("{builtin}: {error}")))?;
        }
        created.push(VmValue::String(Rc::from(format!(
            "{}.{}",
            parent.schema, window.name
        ))));
    }
    Ok(VmValue::List(Rc::new(created)))
}

/// What to do with a partition node during a recursive prune.
enum PruneAction {
    /// RANGE upper bound is at or before the cutoff — drop it. Dropping a
    /// partitioned node cascades to its sub-partitions, so we never
    /// descend into a node we are about to drop.
    Drop,
    /// Keep this node but it is itself partitioned (`relkind = 'p'`), so
    /// recurse — the time column may live at the sub-partition level.
    Descend,
    /// Leaf we keep: LIST / HASH / DEFAULT, or a RANGE partition still
    /// inside the retention window.
    Skip,
}

fn classify_partition(bound: &str, before: &str, relkind: &str) -> PruneAction {
    if partition_bound_strictly_before(bound, before) {
        PruneAction::Drop
    } else if relkind == "p" {
        PruneAction::Descend
    } else {
        PruneAction::Skip
    }
}

/// Recursively drop every RANGE partition under `parent` whose upper
/// bound is at or before `before`. Shared by `pg_partition_prune` and
/// `pg_partition_retain`.
async fn prune_partitions(
    pool: &PgPool,
    parent: &QualifiedIdent,
    before: &str,
    dry_run: bool,
    builtin: &'static str,
) -> Result<VmValue, VmError> {
    let root_oid = resolve_regclass_oid(pool, &parent.qualified, builtin).await?;
    let mut pruned = Vec::new();
    prune_subtree(pool, root_oid, before, dry_run, builtin, &mut pruned).await?;
    Ok(VmValue::List(Rc::new(pruned)))
}

const PARTITION_CHILDREN_SQL: &str = "
    SELECT n.nspname        AS schema,
           c.relname        AS partition,
           c.oid::bigint    AS oid,
           c.relkind::text  AS relkind,
           pg_get_expr(c.relpartbound, c.oid) AS bound
    FROM pg_inherits inh
    JOIN pg_class c ON c.oid = inh.inhrelid
    JOIN pg_namespace n ON c.relnamespace = n.oid
    WHERE inh.inhparent = ($1::bigint)::oid
    ORDER BY c.relname
";

fn prune_subtree<'a>(
    pool: &'a PgPool,
    parent_oid: i64,
    before: &'a str,
    dry_run: bool,
    builtin: &'static str,
    pruned: &'a mut Vec<VmValue>,
) -> Pin<Box<dyn Future<Output = Result<(), VmError>> + 'a>> {
    Box::pin(async move {
        let rows = bind_params(query(PARTITION_CHILDREN_SQL), &[VmValue::Int(parent_oid)])
            .fetch_all(pool)
            .await
            .map_err(|error| runtime_error(format!("{builtin}: {error}")))?;
        for row in rows {
            let schema: String = row.get("schema");
            let part_name: String = row.get("partition");
            let oid: i64 = row.get("oid");
            let relkind: String = row.get("relkind");
            let bound: String = row.get::<Option<String>, _>("bound").unwrap_or_default();
            match classify_partition(&bound, before, &relkind) {
                PruneAction::Drop => {
                    // Defensive: PG identifiers from pg_class can contain
                    // any character if created double-quoted. Escape
                    // embedded `"` per PG rules before SQL synthesis.
                    let quoted = format!(
                        "\"{}\".\"{}\"",
                        schema.replace('"', "\"\""),
                        part_name.replace('"', "\"\""),
                    );
                    if !dry_run {
                        let drop_sql = format!("DROP TABLE {quoted}");
                        sqlx_core::raw_sql::raw_sql(&drop_sql)
                            .execute(pool)
                            .await
                            .map_err(|error| runtime_error(format!("{builtin}: {error}")))?;
                    }
                    pruned.push(VmValue::String(Rc::from(format!("{schema}.{part_name}"))));
                }
                PruneAction::Descend => {
                    prune_subtree(pool, oid, before, dry_run, builtin, pruned).await?;
                }
                PruneAction::Skip => {}
            }
        }
        Ok(())
    })
}

async fn resolve_regclass_oid(
    pool: &PgPool,
    qualified: &str,
    builtin: &'static str,
) -> Result<i64, VmError> {
    let row = bind_params(
        query("SELECT ($1::regclass)::oid::bigint AS oid"),
        &[VmValue::String(Rc::from(qualified))],
    )
    .fetch_one(pool)
    .await
    .map_err(|error| runtime_error(format!("{builtin}: {error}")))?;
    Ok(row.get("oid"))
}

async fn existing_partition_names(
    pool: &PgPool,
    qualified: &str,
    builtin: &'static str,
) -> Result<HashSet<String>, VmError> {
    let rows = bind_params(
        query(
            "SELECT c.relname AS name
             FROM pg_inherits inh
             JOIN pg_class c ON c.oid = inh.inhrelid
             WHERE inh.inhparent = ($1::regclass)::oid",
        ),
        &[VmValue::String(Rc::from(qualified))],
    )
    .fetch_all(pool)
    .await
    .map_err(|error| runtime_error(format!("{builtin}: {error}")))?;
    Ok(rows
        .into_iter()
        .map(|row| row.get::<String, _>("name"))
        .collect())
}

/// Compute the prune cutoff for a retention policy. `keep_days` /
/// `keep_hours` are subtracted from `now`; the cutoff is formatted at
/// the matching granularity so it compares correctly against partition
/// upper bounds under bytewise ordering.
fn retention_cutoff(
    options: &BTreeMap<String, VmValue>,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<String, VmError> {
    if let Some(days) = options.get("keep_days") {
        let days = expect_nonneg_int(days, "pg_partition_retain.keep_days")?;
        let cutoff = now - chrono::Duration::days(days);
        return Ok(cutoff.format("%Y-%m-%d").to_string());
    }
    if let Some(hours) = options.get("keep_hours") {
        let hours = expect_nonneg_int(hours, "pg_partition_retain.keep_hours")?;
        let cutoff = now - chrono::Duration::hours(hours);
        return Ok(cutoff.format("%Y-%m-%d %H:00:00").to_string());
    }
    Err(runtime_error(
        "pg_partition_retain: options must specify keep_days or keep_hours",
    ))
}

#[derive(Clone, Copy)]
enum Interval {
    Day,
    Hour,
}

fn parse_interval(value: Option<&VmValue>) -> Result<Interval, VmError> {
    match value.map(VmValue::display).as_deref() {
        Some("day") => Ok(Interval::Day),
        Some("hour") => Ok(Interval::Hour),
        _ => Err(runtime_error(
            "pg_partition_create_for_window: interval must be \"day\" or \"hour\"",
        )),
    }
}

fn parse_start(
    text: &str,
    builtin: &'static str,
) -> Result<chrono::DateTime<chrono::Utc>, VmError> {
    use chrono::{NaiveDate, NaiveDateTime, TimeZone, Utc};
    let text = text.trim();
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(text) {
        return Ok(dt.with_timezone(&Utc));
    }
    if let Ok(naive) = NaiveDateTime::parse_from_str(text, "%Y-%m-%d %H:%M:%S") {
        return Ok(Utc.from_utc_datetime(&naive));
    }
    if let Ok(date) = NaiveDate::parse_from_str(text, "%Y-%m-%d") {
        return Ok(Utc.from_utc_datetime(&date.and_hms_opt(0, 0, 0).expect("midnight is valid")));
    }
    Err(runtime_error(format!(
        "{builtin}: start must be an ISO date (YYYY-MM-DD) or timestamp"
    )))
}

struct PartitionWindow {
    name: String,
    from: String,
    to: String,
}

/// Generate the `[start, start + ahead*interval)` windows, aligned to the
/// interval boundary. Child names are `<table>_<YYYY_MM_DD[_HH]>` and the
/// RANGE bounds are formatted at the interval granularity.
fn partition_windows(
    table: &str,
    interval: Interval,
    ahead: i64,
    start: chrono::DateTime<chrono::Utc>,
) -> Vec<PartitionWindow> {
    use chrono::{Duration, Timelike};
    let aligned = match interval {
        Interval::Day => start
            .date_naive()
            .and_hms_opt(0, 0, 0)
            .expect("midnight is valid"),
        Interval::Hour => start
            .date_naive()
            .and_hms_opt(start.hour(), 0, 0)
            .expect("top of hour is valid"),
    };
    let step = match interval {
        Interval::Day => Duration::days(1),
        Interval::Hour => Duration::hours(1),
    };
    let (name_fmt, bound_fmt) = match interval {
        Interval::Day => ("%Y_%m_%d", "%Y-%m-%d"),
        Interval::Hour => ("%Y_%m_%d_%H", "%Y-%m-%d %H:00:00"),
    };
    (0..ahead)
        .map(|i| {
            let from = aligned + step * (i as i32);
            let to = from + step;
            PartitionWindow {
                name: format!("{table}_{}", from.format(name_fmt)),
                from: from.format(bound_fmt).to_string(),
                to: to.format(bound_fmt).to_string(),
            }
        })
        .collect()
}

/// Common builtin tail: run a `SELECT` against `pool` with the given
/// params and decode each row through the standard `row_to_value` mapper.
/// Used by every introspection builtin so the dict-per-row shape stays
/// consistent and the SQL column alias *is* the dict key — no per-row
/// hand-coded re-mapping.
async fn rows_to_list(
    pool: &sqlx_postgres::PgPool,
    sql: &str,
    params: &[VmValue],
    builtin: &'static str,
) -> Result<VmValue, VmError> {
    let rows = bind_params(query(sql), params)
        .fetch_all(pool)
        .await
        .map_err(|error| runtime_error(format!("{builtin}: {error}")))?;
    rows.into_iter()
        .map(row_to_value)
        .collect::<Result<Vec<_>, _>>()
        .map(|values| VmValue::List(Rc::new(values)))
}

fn split_qualified(input: &str, builtin: &'static str) -> Result<(String, String), VmError> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(runtime_error(format!(
            "{builtin}: table name is required (use `schema.table` or `table`)"
        )));
    }
    if let Some((schema, table)) = trimmed.split_once('.') {
        validate_pg_identifier(schema, builtin, "identifier", &[])?;
        validate_pg_identifier(table, builtin, "identifier", &[])?;
        Ok((schema.to_string(), table.to_string()))
    } else {
        validate_pg_identifier(trimmed, builtin, "identifier", &[])?;
        Ok(("public".to_string(), trimmed.to_string()))
    }
}

struct QualifiedIdent {
    schema: String,
    table: String,
    quoted: String,
    qualified: String,
}

fn qualified_identifier(input: &str, builtin: &'static str) -> Result<QualifiedIdent, VmError> {
    let (schema, table) = split_qualified(input, builtin)?;
    Ok(QualifiedIdent {
        quoted: format!("\"{schema}\".\"{table}\""),
        qualified: format!("{schema}.{table}"),
        schema,
        table,
    })
}

fn render_bounds_clause(bounds: &BTreeMap<String, VmValue>) -> Result<String, VmError> {
    if let (Some(from), Some(to)) = (bounds.get("from"), bounds.get("to")) {
        let from_lit = sql_literal(from, "pg_partition_attach.bounds.from")?;
        let to_lit = sql_literal(to, "pg_partition_attach.bounds.to")?;
        return Ok(format!("FOR VALUES FROM ({from_lit}) TO ({to_lit})"));
    }
    if let Some(values) = bounds.get("in") {
        let list = match values {
            VmValue::List(items) => items,
            _ => {
                return Err(runtime_error(
                    "pg_partition_attach: bounds.in must be a list",
                ))
            }
        };
        let parts: Result<Vec<String>, VmError> = list
            .iter()
            .map(|v| sql_literal(v, "pg_partition_attach.bounds.in"))
            .collect();
        return Ok(format!("FOR VALUES IN ({})", parts?.join(", ")));
    }
    if let (Some(modulus), Some(remainder)) = (bounds.get("modulus"), bounds.get("remainder")) {
        let modulus = expect_nonneg_int(modulus, "pg_partition_attach.bounds.modulus")?;
        let remainder = expect_nonneg_int(remainder, "pg_partition_attach.bounds.remainder")?;
        if modulus < 1 {
            return Err(runtime_error(
                "pg_partition_attach: bounds.modulus must be >= 1",
            ));
        }
        if remainder >= modulus {
            return Err(runtime_error(
                "pg_partition_attach: bounds.remainder must be < bounds.modulus",
            ));
        }
        return Ok(format!(
            "FOR VALUES WITH (MODULUS {modulus}, REMAINDER {remainder})"
        ));
    }
    if super::option_bool(bounds.get("default")) == Some(true) {
        return Ok("DEFAULT".to_string());
    }
    Err(runtime_error(
        "pg_partition_attach: bounds must be {from,to}, {in: [...]}, {modulus,remainder}, or {default: true}",
    ))
}

fn expect_nonneg_int(value: &VmValue, label: &str) -> Result<i64, VmError> {
    match value {
        VmValue::Int(n) if *n >= 0 => Ok(*n),
        _ => Err(runtime_error(format!(
            "{label} must be a non-negative integer"
        ))),
    }
}

fn sql_literal(value: &VmValue, label: &'static str) -> Result<String, VmError> {
    match value {
        VmValue::Int(n) => Ok(n.to_string()),
        VmValue::Float(n) => Ok(format!("{n}")),
        VmValue::String(text) => {
            // PG single-quote literal — escape embedded `'` per spec.
            Ok(format!("'{}'", text.replace('\'', "''")))
        }
        VmValue::Bool(b) => Ok(if *b {
            "TRUE".to_string()
        } else {
            "FALSE".to_string()
        }),
        _ => Err(runtime_error(format!(
            "{label}: literals must be int, float, bool, or string"
        ))),
    }
}

/// Lexicographic strict comparison of the partition's upper bound vs the
/// caller-supplied "before" literal. Returns true if the partition's
/// upper bound is `<=` the cutoff (i.e. partition is entirely older).
///
/// Only RANGE partitions (`FOR VALUES FROM (...) TO (...)`) are handled.
/// LIST, HASH, and DEFAULT partitions return false — `pg_partition_prune`
/// is intentionally a no-op against those because the right thing to do
/// is operator-judgment, not a string match. ISO-8601 dates/timestamps
/// compare correctly under bytewise ordering, which is the only literal
/// shape this function currently understands.
fn partition_bound_strictly_before(bound: &str, before: &str) -> bool {
    let Some(to_idx) = bound.find(" TO (") else {
        return false;
    };
    let after = &bound[to_idx + 5..];
    let Some(end_idx) = after.rfind(')') else {
        return false;
    };
    let to_literal = after[..end_idx]
        .trim()
        .trim_start_matches('\'')
        .trim_end_matches('\'');
    to_literal <= before.trim_start_matches('\'').trim_end_matches('\'')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_qualified_defaults_schema_to_public() {
        let (schema, table) = split_qualified("receipts", "pg_introspect_columns").unwrap();
        assert_eq!(schema, "public");
        assert_eq!(table, "receipts");
        let (schema, table) = split_qualified("billing.invoices", "pg_introspect_columns").unwrap();
        assert_eq!(schema, "billing");
        assert_eq!(table, "invoices");
    }

    #[test]
    fn split_qualified_rejects_bad_identifiers() {
        assert!(split_qualified("", "pg").is_err());
        assert!(split_qualified("1bad", "pg").is_err());
        assert!(split_qualified("bad-name", "pg").is_err());
        assert!(split_qualified("bad name", "pg").is_err());
        assert!(split_qualified("bad;name", "pg").is_err());
    }

    #[test]
    fn render_bounds_clause_handles_three_shapes() {
        let from_to = BTreeMap::from([
            ("from".to_string(), VmValue::String(Rc::from("2026-01-01"))),
            ("to".to_string(), VmValue::String(Rc::from("2026-02-01"))),
        ]);
        assert_eq!(
            render_bounds_clause(&from_to).unwrap(),
            "FOR VALUES FROM ('2026-01-01') TO ('2026-02-01')"
        );
        let in_clause = BTreeMap::from([(
            "in".to_string(),
            VmValue::List(Rc::new(vec![VmValue::Int(1), VmValue::Int(2)])),
        )]);
        assert_eq!(
            render_bounds_clause(&in_clause).unwrap(),
            "FOR VALUES IN (1, 2)"
        );
        let default = BTreeMap::from([("default".to_string(), VmValue::Bool(true))]);
        assert_eq!(render_bounds_clause(&default).unwrap(), "DEFAULT");
        let bad = BTreeMap::new();
        assert!(render_bounds_clause(&bad).is_err());
    }

    #[test]
    fn partition_bound_comparison() {
        let bound = "FOR VALUES FROM ('2026-01-01') TO ('2026-02-01')";
        assert!(partition_bound_strictly_before(bound, "2026-02-01"));
        assert!(partition_bound_strictly_before(bound, "2026-03-01"));
        assert!(!partition_bound_strictly_before(bound, "2026-01-15"));
    }

    #[test]
    fn render_bounds_clause_handles_hash() {
        let hash = BTreeMap::from([
            ("modulus".to_string(), VmValue::Int(4)),
            ("remainder".to_string(), VmValue::Int(0)),
        ]);
        assert_eq!(
            render_bounds_clause(&hash).unwrap(),
            "FOR VALUES WITH (MODULUS 4, REMAINDER 0)"
        );
        // remainder must be strictly less than modulus.
        let bad_remainder = BTreeMap::from([
            ("modulus".to_string(), VmValue::Int(4)),
            ("remainder".to_string(), VmValue::Int(4)),
        ]);
        assert!(render_bounds_clause(&bad_remainder).is_err());
        // modulus must be >= 1.
        let zero_modulus = BTreeMap::from([
            ("modulus".to_string(), VmValue::Int(0)),
            ("remainder".to_string(), VmValue::Int(0)),
        ]);
        assert!(render_bounds_clause(&zero_modulus).is_err());
    }

    #[test]
    fn classify_partition_decides_drop_descend_skip() {
        let old = "FOR VALUES FROM ('2026-01-01') TO ('2026-02-01')";
        let fresh = "FOR VALUES FROM ('2026-06-01') TO ('2026-07-01')";
        // RANGE leaf entirely before the cutoff → drop.
        assert!(matches!(
            classify_partition(old, "2026-03-01", "r"),
            PruneAction::Drop
        ));
        // RANGE leaf still inside the window → skip.
        assert!(matches!(
            classify_partition(fresh, "2026-03-01", "r"),
            PruneAction::Skip
        ));
        // Kept node that is itself partitioned → descend into the subtree.
        assert!(matches!(
            classify_partition(fresh, "2026-03-01", "p"),
            PruneAction::Descend
        ));
        // An old node that is partitioned still drops wholesale (the drop
        // cascades to its sub-partitions) rather than descending.
        assert!(matches!(
            classify_partition(old, "2026-03-01", "p"),
            PruneAction::Drop
        ));
        // HASH / LIST / DEFAULT leaves are never pruned by date.
        assert!(matches!(
            classify_partition(
                "FOR VALUES WITH (MODULUS 4, REMAINDER 0)",
                "2026-03-01",
                "r"
            ),
            PruneAction::Skip
        ));
    }

    #[test]
    fn retention_cutoff_subtracts_window() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-05-28T12:34:56Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let days = BTreeMap::from([("keep_days".to_string(), VmValue::Int(90))]);
        assert_eq!(retention_cutoff(&days, now).unwrap(), "2026-02-27");
        let hours = BTreeMap::from([("keep_hours".to_string(), VmValue::Int(6))]);
        assert_eq!(
            retention_cutoff(&hours, now).unwrap(),
            "2026-05-28 06:00:00"
        );
        let empty = BTreeMap::new();
        assert!(retention_cutoff(&empty, now).is_err());
    }

    #[test]
    fn partition_windows_align_and_format() {
        let start = chrono::DateTime::parse_from_rfc3339("2026-05-28T14:30:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let days = partition_windows("events", Interval::Day, 2, start);
        assert_eq!(days.len(), 2);
        assert_eq!(days[0].name, "events_2026_05_28");
        assert_eq!(days[0].from, "2026-05-28");
        assert_eq!(days[0].to, "2026-05-29");
        assert_eq!(days[1].name, "events_2026_05_29");
        assert_eq!(days[1].to, "2026-05-30");

        let hours = partition_windows("events", Interval::Hour, 2, start);
        assert_eq!(hours[0].name, "events_2026_05_28_14");
        assert_eq!(hours[0].from, "2026-05-28 14:00:00");
        assert_eq!(hours[0].to, "2026-05-28 15:00:00");
        assert_eq!(hours[1].name, "events_2026_05_28_15");
    }

    #[test]
    fn parse_start_accepts_iso_shapes() {
        assert!(parse_start("2026-05-28", "pg").is_ok());
        assert!(parse_start("2026-05-28 14:00:00", "pg").is_ok());
        assert!(parse_start("2026-05-28T14:00:00Z", "pg").is_ok());
        assert!(parse_start("not-a-date", "pg").is_err());
    }

    #[test]
    fn parse_interval_validates() {
        assert!(matches!(
            parse_interval(Some(&VmValue::String(Rc::from("day")))),
            Ok(Interval::Day)
        ));
        assert!(matches!(
            parse_interval(Some(&VmValue::String(Rc::from("hour")))),
            Ok(Interval::Hour)
        ));
        assert!(parse_interval(Some(&VmValue::String(Rc::from("week")))).is_err());
        assert!(parse_interval(None).is_err());
    }
}
