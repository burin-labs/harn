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
//! pg_partition_detach(db, "events", "events_2026_03")
//! pg_partition_prune(db, "events", "2026-01-01")
//! ```
//!
//! Introspection queries hit `information_schema` / `pg_catalog` directly
//! and bind schema/table names — no string concat. Partition helpers
//! validate identifiers but render them quoted; the caller-supplied
//! bounds are bound as parameters (Postgres' `FOR VALUES FROM (...) TO
//! (...)` does not accept binds, so we re-parse the literal via the
//! catalog's `pg_get_partkeydef` to confirm the partition exists after
//! the DDL runs).

use std::collections::BTreeMap;
use std::rc::Rc;

use sqlx_core::query::query;
use sqlx_core::row::Row;

use crate::stdlib::macros::{
    harn_builtin, BuiltinSignature, Param, TY_ANY, TY_BOOL, TY_DICT, TY_LIST,
};
use crate::value::{VmError, VmValue};

use super::{
    bind_params, ensure_handle_kind, handle_id, pool_by_id, pool_record_by_id, required_arg,
    runtime_error, HANDLE_POOL,
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
    let pool_handle = required_arg(&args, 0, "pg_introspect_tables", "pool handle")?;
    ensure_handle_kind(pool_handle, HANDLE_POOL, "pg_introspect_tables")?;
    let pool_id = handle_id(Some(pool_handle), HANDLE_POOL, "pg_introspect_tables")?;
    let options = args.get(1).and_then(VmValue::as_dict);
    let schema = options
        .and_then(|opts| opts.get("schema"))
        .map(VmValue::display)
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| "public".to_string());

    let pool = pool_by_id(&pool_id)?;
    let sql = "
        SELECT n.nspname  AS schema,
               c.relname  AS table_name,
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
    ";
    let rows = bind_params(query(sql), &[VmValue::String(Rc::from(schema))])
        .fetch_all(pool.as_ref())
        .await
        .map_err(|error| runtime_error(format!("pg_introspect_tables: {error}")))?;
    Ok(VmValue::List(Rc::new(
        rows.into_iter()
            .map(|row| {
                let mut dict = BTreeMap::new();
                dict.insert(
                    "schema".to_string(),
                    VmValue::String(Rc::from(row.get::<String, _>("schema"))),
                );
                dict.insert(
                    "table".to_string(),
                    VmValue::String(Rc::from(row.get::<String, _>("table_name"))),
                );
                dict.insert(
                    "kind".to_string(),
                    VmValue::String(Rc::from(row.get::<String, _>("kind"))),
                );
                VmValue::Dict(Rc::new(dict))
            })
            .collect(),
    )))
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
    let pool_handle = required_arg(&args, 0, "pg_introspect_columns", "pool handle")?;
    ensure_handle_kind(pool_handle, HANDLE_POOL, "pg_introspect_columns")?;
    let pool_id = handle_id(Some(pool_handle), HANDLE_POOL, "pg_introspect_columns")?;
    let (schema, table) = split_qualified(
        args.get(1).map(VmValue::display).as_deref().unwrap_or(""),
        "pg_introspect_columns",
    )?;

    let pool = pool_by_id(&pool_id)?;
    let sql = "
        SELECT column_name,
               udt_name,
               data_type,
               is_nullable = 'YES' AS nullable,
               column_default,
               ordinal_position
        FROM information_schema.columns
        WHERE table_schema = $1 AND table_name = $2
        ORDER BY ordinal_position
    ";
    let rows = bind_params(
        query(sql),
        &[
            VmValue::String(Rc::from(schema)),
            VmValue::String(Rc::from(table)),
        ],
    )
    .fetch_all(pool.as_ref())
    .await
    .map_err(|error| runtime_error(format!("pg_introspect_columns: {error}")))?;
    Ok(VmValue::List(Rc::new(
        rows.into_iter()
            .map(|row| {
                let mut dict = BTreeMap::new();
                dict.insert(
                    "column".to_string(),
                    VmValue::String(Rc::from(row.get::<String, _>("column_name"))),
                );
                dict.insert(
                    "type".to_string(),
                    VmValue::String(Rc::from(row.get::<String, _>("udt_name"))),
                );
                dict.insert(
                    "data_type".to_string(),
                    VmValue::String(Rc::from(row.get::<String, _>("data_type"))),
                );
                dict.insert("nullable".to_string(), VmValue::Bool(row.get("nullable")));
                let default = row
                    .try_get::<Option<String>, _>("column_default")
                    .ok()
                    .flatten();
                dict.insert(
                    "default".to_string(),
                    default
                        .map(|d| VmValue::String(Rc::from(d)))
                        .unwrap_or(VmValue::Nil),
                );
                VmValue::Dict(Rc::new(dict))
            })
            .collect(),
    )))
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
    let pool_handle = required_arg(&args, 0, "pg_introspect_indexes", "pool handle")?;
    ensure_handle_kind(pool_handle, HANDLE_POOL, "pg_introspect_indexes")?;
    let pool_id = handle_id(Some(pool_handle), HANDLE_POOL, "pg_introspect_indexes")?;
    let (schema, table) = split_qualified(
        args.get(1).map(VmValue::display).as_deref().unwrap_or(""),
        "pg_introspect_indexes",
    )?;

    let pool = pool_by_id(&pool_id)?;
    let sql = "
        SELECT i.relname AS index_name,
               array_agg(a.attname ORDER BY x.ord) AS columns,
               ix.indisunique AS is_unique,
               ix.indisprimary AS is_primary
        FROM pg_class t
        JOIN pg_namespace n ON t.relnamespace = n.oid
        JOIN pg_index ix ON t.oid = ix.indrelid
        JOIN pg_class i ON i.oid = ix.indexrelid
        JOIN LATERAL unnest(ix.indkey) WITH ORDINALITY AS x(attnum, ord) ON TRUE
        JOIN pg_attribute a ON a.attrelid = t.oid AND a.attnum = x.attnum
        WHERE n.nspname = $1 AND t.relname = $2
        GROUP BY i.relname, ix.indisunique, ix.indisprimary
        ORDER BY i.relname
    ";
    let rows = bind_params(
        query(sql),
        &[
            VmValue::String(Rc::from(schema)),
            VmValue::String(Rc::from(table)),
        ],
    )
    .fetch_all(pool.as_ref())
    .await
    .map_err(|error| runtime_error(format!("pg_introspect_indexes: {error}")))?;
    Ok(VmValue::List(Rc::new(
        rows.into_iter()
            .map(|row| {
                let mut dict = BTreeMap::new();
                dict.insert(
                    "index".to_string(),
                    VmValue::String(Rc::from(row.get::<String, _>("index_name"))),
                );
                let columns: Vec<String> = row.try_get("columns").unwrap_or_default();
                dict.insert(
                    "columns".to_string(),
                    VmValue::List(Rc::new(
                        columns
                            .into_iter()
                            .map(|c| VmValue::String(Rc::from(c)))
                            .collect(),
                    )),
                );
                dict.insert("unique".to_string(), VmValue::Bool(row.get("is_unique")));
                dict.insert("primary".to_string(), VmValue::Bool(row.get("is_primary")));
                VmValue::Dict(Rc::new(dict))
            })
            .collect(),
    )))
}

#[harn_builtin(
    sig_expr = BuiltinSignature::variadic("pg_pool_stats", &[Param::new("args", TY_ANY)], TY_DICT),
    kind = "async",
    category = "postgres"
)]
async fn pg_pool_stats_impl(args: Vec<VmValue>) -> Result<VmValue, VmError> {
    let pool_handle = required_arg(&args, 0, "pg_pool_stats", "pool handle")?;
    ensure_handle_kind(pool_handle, HANDLE_POOL, "pg_pool_stats")?;
    let pool_id = handle_id(Some(pool_handle), HANDLE_POOL, "pg_pool_stats")?;
    let record = pool_record_by_id(&pool_id)?;
    let pool = &record.pool;
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
    let pool_handle = required_arg(&args, 0, "pg_partition_attach", "pool handle")?;
    ensure_handle_kind(pool_handle, HANDLE_POOL, "pg_partition_attach")?;
    let pool_id = handle_id(Some(pool_handle), HANDLE_POOL, "pg_partition_attach")?;
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
    let pool = pool_by_id(&pool_id)?;
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
    let pool_handle = required_arg(&args, 0, "pg_partition_detach", "pool handle")?;
    ensure_handle_kind(pool_handle, HANDLE_POOL, "pg_partition_detach")?;
    let pool_id = handle_id(Some(pool_handle), HANDLE_POOL, "pg_partition_detach")?;
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

    let pool = pool_by_id(&pool_id)?;
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
    let pool_handle = required_arg(&args, 0, "pg_partition_prune", "pool handle")?;
    ensure_handle_kind(pool_handle, HANDLE_POOL, "pg_partition_prune")?;
    let pool_id = handle_id(Some(pool_handle), HANDLE_POOL, "pg_partition_prune")?;
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

    let pool = pool_by_id(&pool_id)?;
    let candidates_sql = "
        SELECT n.nspname AS schema,
               c.relname AS partition,
               pg_get_expr(c.relpartbound, c.oid) AS bound
        FROM pg_inherits inh
        JOIN pg_class c ON c.oid = inh.inhrelid
        JOIN pg_namespace n ON c.relnamespace = n.oid
        WHERE inh.inhparent = ($1::regclass)::oid
    ";
    let rows = bind_params(
        query(candidates_sql),
        &[VmValue::String(Rc::from(parent.qualified.clone()))],
    )
    .fetch_all(pool.as_ref())
    .await
    .map_err(|error| runtime_error(format!("pg_partition_prune: {error}")))?;

    let mut pruned = Vec::new();
    for row in rows {
        let schema: String = row.get("schema");
        let part_name: String = row.get("partition");
        let bound: String = row.get("bound");
        if !partition_bound_strictly_before(&bound, &before_literal) {
            continue;
        }
        let quoted = format!("\"{schema}\".\"{part_name}\"");
        if !dry_run {
            let drop_sql = format!("DROP TABLE {quoted}");
            sqlx_core::raw_sql::raw_sql(&drop_sql)
                .execute(pool.as_ref())
                .await
                .map_err(|error| runtime_error(format!("pg_partition_prune: {error}")))?;
        }
        pruned.push(VmValue::String(Rc::from(format!("{schema}.{part_name}"))));
    }
    Ok(VmValue::List(Rc::new(pruned)))
}

fn split_qualified(input: &str, builtin: &'static str) -> Result<(String, String), VmError> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(runtime_error(format!(
            "{builtin}: table name is required (use `schema.table` or `table`)"
        )));
    }
    if let Some((schema, table)) = trimmed.split_once('.') {
        validate_identifier(schema, builtin)?;
        validate_identifier(table, builtin)?;
        Ok((schema.to_string(), table.to_string()))
    } else {
        validate_identifier(trimmed, builtin)?;
        Ok(("public".to_string(), trimmed.to_string()))
    }
}

struct QualifiedIdent {
    quoted: String,
    qualified: String,
}

fn qualified_identifier(input: &str, builtin: &'static str) -> Result<QualifiedIdent, VmError> {
    let (schema, table) = split_qualified(input, builtin)?;
    Ok(QualifiedIdent {
        quoted: format!("\"{schema}\".\"{table}\""),
        qualified: format!("{schema}.{table}"),
    })
}

fn validate_identifier(name: &str, builtin: &'static str) -> Result<(), VmError> {
    if name.is_empty() || name.len() > 63 {
        return Err(runtime_error(format!(
            "{builtin}: identifier `{name}` must be 1..=63 bytes"
        )));
    }
    let first = name.chars().next().unwrap();
    if !(first.is_ascii_alphabetic() || first == '_') {
        return Err(runtime_error(format!(
            "{builtin}: identifier `{name}` must start with a letter or underscore"
        )));
    }
    for ch in name.chars() {
        if !(ch.is_ascii_alphanumeric() || ch == '_') {
            return Err(runtime_error(format!(
                "{builtin}: identifier `{name}` contains invalid character `{ch}`"
            )));
        }
    }
    Ok(())
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
    if super::option_bool(bounds.get("default")) == Some(true) {
        return Ok("DEFAULT".to_string());
    }
    Err(runtime_error(
        "pg_partition_attach: bounds must be {from,to}, {in: [...]}, or {default: true}",
    ))
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
}
