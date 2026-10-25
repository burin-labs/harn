use std::collections::BTreeMap;
use std::ops::Bound;
use std::sync::Arc;

use parking_lot::Mutex as SyncMutex;
use sqlx_core::column::Column;
use sqlx_core::executor::Executor;
use sqlx_core::query::{query, Query};
use sqlx_core::row::Row;
use sqlx_core::sql_str::AssertSqlSafe;
use sqlx_core::type_info::TypeInfo;
use sqlx_core::value::ValueRef;
use sqlx_postgres::{PgArguments, PgPool, PgRow, PgTypeInfo, Postgres};

#[cfg(test)]
use super::parameter_cache::{bump_describe_round_trips, bump_describe_savepoint_round_trips};
use crate::sql_common::runtime_error;
use harn_vm_core::value::json::vm_value_to_json;
use harn_vm_core::value::{DictMap, VmError, VmValue};

fn non_finite_float_error() -> VmError {
    runtime_error(
        "pg bind: non-finite float (NaN/Infinity) cannot be bound to a Postgres parameter",
    )
}

/// Reject a non-finite `VmValue::Float` anywhere inside a value that will be
/// bound. Direct `float8` binds of NaN/Infinity are legal for the column type
/// but corrupt any downstream JSON, and a non-finite float that falls through
/// to the jsonb (`vm_value_to_json`) path serializes to a JSON `null`
/// (serde_json's representation of non-finite f64), silently dropping the
/// value. We reject both up front with a clear error instead.
pub(super) fn reject_non_finite_floats(value: &VmValue) -> Result<(), VmError> {
    match value {
        VmValue::Float(f) if !f.is_finite() => Err(non_finite_float_error()),
        VmValue::List(list) => list.iter().try_for_each(reject_non_finite_floats),
        VmValue::Dict(dict) => dict.values().try_for_each(reject_non_finite_floats),
        VmValue::StructInstance(_) => value
            .struct_fields_map()
            .unwrap_or_default()
            .values()
            .try_for_each(reject_non_finite_floats),
        _ => Ok(()),
    }
}

/// A NULL declaring the current server-inferred parameter OID. Dynamic `nil`
/// has no Rust type; using `None::<String>` would declare text even for integer
/// or UUID columns. `encode` writes no payload, and `produces` supplies the
/// inferred OID to the nonpersistent query built by `bind_params_described`.
struct TypedNull(PgTypeInfo);

impl sqlx_core::types::Type<Postgres> for TypedNull {
    fn type_info() -> PgTypeInfo {
        // Fallback only; `produces()` overrides this for the actual bind.
        // `Void` (OID 2278) is the most neutral built-in. This is never the
        // declared type in practice because `produces()` always returns `Some`.
        PgTypeInfo::with_oid(sqlx_postgres::types::Oid(2278))
    }

    fn compatible(_ty: &PgTypeInfo) -> bool {
        true
    }
}

impl sqlx_core::encode::Encode<'_, Postgres> for TypedNull {
    fn encode_by_ref(
        &self,
        _buf: &mut <Postgres as sqlx_core::database::Database>::ArgumentBuffer,
    ) -> Result<sqlx_core::encode::IsNull, sqlx_core::error::BoxDynError> {
        // A NULL writes no payload bytes; the buffer machinery records the
        // `-1` length when it sees `IsNull::Yes`.
        Ok(sqlx_core::encode::IsNull::Yes)
    }

    fn produces(&self) -> Option<PgTypeInfo> {
        // This is the whole point: declare the parameter with the
        // server-described OID rather than the `Type::type_info()` fallback.
        Some(self.0.clone())
    }
}

/// True when any bound param is a dynamic `nil`. Only these queries need the
/// extra describe round-trip; all-non-null queries take the fast path
/// (`bind_params`) unchanged.
pub(crate) fn params_have_nil(params: &[VmValue]) -> bool {
    params.iter().any(|p| matches!(p, VmValue::Nil))
}

/// Bind a single `VmValue` onto a `Query`. `nil_type`, when present, is the
/// server-described OID for *this* parameter slot and is used to bind a typed
/// `NULL` (see [`TypedNull`]); otherwise a `nil` falls back to the legacy
/// `None::<String>` (`text`) bind.
fn bind_one<'q>(
    query: Query<'q, Postgres, PgArguments>,
    param: &'q VmValue,
    nil_type: Option<&PgTypeInfo>,
) -> Result<Query<'q, Postgres, PgArguments>, VmError> {
    // Guard every param (including floats nested in the jsonb path) before it
    // reaches sqlx, so NaN/Infinity fail cleanly rather than corrupting a
    // float8 column or emitting invalid JSON.
    reject_non_finite_floats(param)?;
    Ok(match param {
        VmValue::Nil => match nil_type {
            Some(ty) => query.bind(TypedNull(ty.clone())),
            None => query.bind(None::<String>),
        },
        VmValue::Bool(value) => query.bind(*value),
        VmValue::Int(value) => query.bind(*value),
        VmValue::Float(value) => query.bind(*value),
        VmValue::String(value) => query.bind(value.to_string()),
        VmValue::Bytes(value) => query.bind((**value).clone()),
        VmValue::Duration(ms) => query.bind(*ms),
        // Bind exact decimals to NUMERIC/DECIMAL columns without going through
        // a lossy float — sqlx encodes `rust_decimal::Decimal` natively.
        VmValue::Decimal(value) => query.bind(**value),
        value => query.bind(sqlx_core::types::Json(vm_value_to_json(value))),
    })
}

pub(crate) fn bind_params<'q>(
    mut query: Query<'q, Postgres, PgArguments>,
    params: &'q [VmValue],
) -> Result<Query<'q, Postgres, PgArguments>, VmError> {
    for param in params {
        query = bind_one(query, param, None)?;
    }
    Ok(query)
}

/// Build a query bound to `sql`, encoding any dynamic `nil` as a [`TypedNull`]
/// carrying the server-described OID for that slot (`described`).
///
/// Non-null params are bound by sqlx in their natural type/format, so the
/// resulting `PgArguments` declares, per slot, either sqlx's natural type
/// (non-null) or the described OID (nil). The exact per-slot type list therefore
/// depends on *which* slots are `nil` at runtime — and sqlx's prepared-statement
/// cache is keyed by the SQL string **alone**, so two different nil-patterns of
/// the same SQL would collide on a single cached statement (a non-null slot
/// reusing a sibling pattern's `NULL`-declared OID → `incorrect binary data
/// format in bind parameter N`). We therefore mark the described query
/// [`persistent(false)`], so in sqlx 0.9.0 it Parses an **unnamed** statement
/// and is **never inserted** into the SQL-keyed cache (verified in
/// `sqlx_postgres::connection::executor::{prepare, get_or_prepare}` — the cache
/// insert is gated on `persistent == true`, and a non-persistent prepare uses
/// `StatementId::UNNAMED`). Each described execute thus Parses exactly this
/// self-consistent type list before the separate Bind + Execute flush,
/// eliminating both the binary-format mismatch (the OID-0 failure mode) and the
/// text-NULL failure modes, and it cannot poison sibling patterns or the
/// all-non-null fast path. A `nil` reusing an all-non-null cached statement is
/// harmless: a SQL `NULL` carries no payload bytes, so the declared OID is
/// irrelevant for it.
pub(super) fn bind_params_described<'q>(
    sql: &'q str,
    described: &[PgTypeInfo],
    params: &'q [VmValue],
) -> Result<Query<'q, Postgres, PgArguments>, VmError> {
    let mut query = query(AssertSqlSafe(sql)).persistent(false);
    for (index, param) in params.iter().enumerate() {
        query = bind_one(query, param, described.get(index))?;
    }
    Ok(query)
}

/// Look up server-inferred parameter types, describing on a miss. Cache scope
/// belongs to the caller: pool SQL must retain a stable schema context, while
/// transactions discard metadata after their first caller statement.
/// Migrations clear pool metadata when column types change.
///
/// Transaction probes use a savepoint so an unsuccessful describe cannot abort
/// the caller's transaction. Only a known first-statement pool cache hit skips
/// the probe and savepoint; later transaction statements infer afresh.
pub(super) async fn described_param_oids(
    cache: &SyncMutex<BTreeMap<String, Arc<Vec<PgTypeInfo>>>>,
    conn: &mut sqlx_postgres::PgConnection,
    sql: &str,
    builtin: &str,
    in_transaction: bool,
) -> Result<Arc<Vec<PgTypeInfo>>, VmError> {
    if let Some(cached) = cache.lock().get(sql).cloned() {
        return Ok(cached);
    }
    let oids = Arc::new(describe_param_oids_uncached(conn, sql, builtin, in_transaction).await?);
    cache.lock().insert(sql.to_string(), Arc::clone(&oids));
    Ok(oids)
}

/// Perform the actual (uncached) server describe for `sql` on `conn`, returning
/// one [`PgTypeInfo`] per `$n`.
///
/// In sqlx 0.9.0, `prepare_with` is the only public way to obtain per-slot
/// parameter OIDs, and it *always* prepares persistently — it inserts the
/// *server-inferred* statement into the connection's SQL-keyed cache (see
/// `sqlx_postgres::connection::executor::get_or_prepare`, which gates the cache
/// insert on `persistent == true` and offers no per-key eviction). There is no
/// non-caching describe-only entry point: `Executor::describe` is gated behind
/// the `offline` feature (which harn does not enable) and is likewise
/// persistent. For a query whose non-null param's natural sqlx type differs from
/// the inferred column type (e.g. an `i64` against an `int4` column), that
/// inferred cache entry would poison a later execute, so we
/// [`clear_cached_statements`] once here. Because the result is cached by
/// [`described_param_oids`], this prepare+clear happens at most once per distinct
/// SQL in the pool's stable context. Later transaction statements instead
/// describe afresh because previous SQL can change name resolution.
///
/// **Best-effort, never worse than the legacy bind.** `prepare_with(sql, &[])`
/// forces Postgres to infer *every* `$n` from query structure alone, so a query
/// with a genuinely ambiguous slot (e.g. `could not determine data type of
/// parameter $3`) makes the probe itself fail — even though binding that nil as
/// a legacy `text` NULL would have worked. We therefore treat a probe failure as
/// "no per-slot OIDs available" rather than an error: we return an **empty**
/// `Vec`, which the bind path turns into the legacy `None::<String>` (text) NULL
/// for each nil — strictly ≥ the pre-describe behavior. The empty result is
/// cached by [`described_param_oids`] within the pool's stable context. Later
/// transaction statements never retain the fallback across caller execution.
///
/// Inside a caller transaction (`in_transaction`), a failed `prepare_with`
/// aborts the whole transaction (`current transaction is aborted`), which would
/// then break the caller's own statements. We guard the probe with `SAVEPOINT
/// _harn_describe_probe`: on success we `RELEASE` it and proceed normally; on
/// failure we `ROLLBACK TO` it, which discards the aborted sub-state and leaves
/// the transaction usable. Outside a transaction (autocommit pool conn) a
/// `SAVEPOINT` is itself an error, so we skip it and simply catch the prepare
/// failure. The savepoint round-trips are added **only** around a tx-path probe
/// — the all-non-null fast path and the successful-describe path are unchanged.
pub(super) async fn describe_param_oids_uncached(
    conn: &mut sqlx_postgres::PgConnection,
    sql: &str,
    builtin: &str,
    in_transaction: bool,
) -> Result<Vec<PgTypeInfo>, VmError> {
    use sqlx_core::connection::Connection as _;
    use sqlx_core::sql_str::SqlSafeStr as _;
    use sqlx_core::statement::Statement as _;

    const SAVEPOINT: &str = "_harn_describe_probe";

    #[cfg(test)]
    bump_describe_round_trips();

    // SQLx looks up SQL-keyed entries even for nonpersistent execution. A
    // previous nonnil execution may have prepared this SQL in another context;
    // the probe must reach the server's current name resolution instead.
    if conn.cached_statements_size() != 0 {
        conn.clear_cached_statements()
            .await
            .map_err(|error| runtime_error(format!("{builtin}: clear cache failed: {error}")))?;
    }

    // In a caller transaction, a failed probe aborts the whole tx. A savepoint
    // lets us roll back *only* the failed probe and keep the tx alive. Outside a
    // tx, SAVEPOINT is an error, so we skip it.
    if in_transaction {
        conn.execute(AssertSqlSafe(format!("SAVEPOINT {SAVEPOINT}")))
            .await
            .map_err(|error| runtime_error(format!("{builtin}: savepoint failed: {error}")))?;
        #[cfg(test)]
        bump_describe_savepoint_round_trips();
    }

    let prepared = conn
        .prepare_with(AssertSqlSafe(sql.to_string()).into_sql_str(), &[])
        .await;

    let stmt = match prepared {
        Ok(stmt) => stmt,
        Err(_) => {
            // The describe probe failed (e.g. an ambiguous `$n` Postgres cannot
            // infer from structure alone). Don't propagate — fall back to legacy
            // text NULLs by returning no per-slot OIDs. Inside a tx, undo the
            // aborted sub-state so the caller's transaction stays usable.
            if in_transaction {
                conn.execute(AssertSqlSafe(format!("ROLLBACK TO SAVEPOINT {SAVEPOINT}")))
                    .await
                    .map_err(|error| {
                        runtime_error(format!("{builtin}: rollback to savepoint failed: {error}"))
                    })?;
                #[cfg(test)]
                bump_describe_savepoint_round_trips();
                conn.execute(AssertSqlSafe(format!("RELEASE SAVEPOINT {SAVEPOINT}")))
                    .await
                    .map_err(|error| {
                        runtime_error(format!("{builtin}: release savepoint failed: {error}"))
                    })?;
                #[cfg(test)]
                bump_describe_savepoint_round_trips();
            }
            return Ok(Vec::new());
        }
    };

    let oids = match stmt.parameters() {
        Some(sqlx_core::Either::Left(types)) => types.to_vec(),
        // Count-only or no parameter metadata: no per-slot OIDs to apply; the
        // bind path will fall back to legacy text NULLs.
        _ => Vec::new(),
    };
    drop(stmt);

    if in_transaction {
        conn.execute(AssertSqlSafe(format!("RELEASE SAVEPOINT {SAVEPOINT}")))
            .await
            .map_err(|error| {
                runtime_error(format!("{builtin}: release savepoint failed: {error}"))
            })?;
        #[cfg(test)]
        bump_describe_savepoint_round_trips();
    }

    conn.clear_cached_statements()
        .await
        .map_err(|error| runtime_error(format!("{builtin}: clear cache failed: {error}")))?;
    Ok(oids)
}

/// Run a nil-containing query against `pool` via the describe-then-bind path on
/// a single acquired pool connection.
///
/// The first nil-bearing execution describes and clears the driver's prepared
/// statement, then caches OIDs under the pool's stable-context contract. Later
/// pool calls skip that probe, but execution remains nonpersistent: SQLx Parses
/// and Describes before its separate Bind + Execute flush unless an existing
/// nonnil statement can be reused. Nil executions never insert prepared entries
/// that could poison another argument pattern. The all-non-null path bypasses
/// this helper. `run` shares the acquired connection across metadata lookup and
/// the actual `fetch_all`/`execute`.
pub(super) async fn run_described_query<T>(
    pool: &PgPool,
    cache: &SyncMutex<BTreeMap<String, Arc<Vec<PgTypeInfo>>>>,
    sql: &str,
    params: &[VmValue],
    builtin: &str,
    run: impl for<'a> FnOnce(
        Query<'a, Postgres, PgArguments>,
        &'a mut sqlx_postgres::PgConnection,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<T, sqlx_core::error::Error>> + Send + 'a>,
    >,
) -> Result<T, VmError> {
    let mut conn = pool
        .acquire()
        .await
        .map_err(|error| runtime_error(format!("{builtin}: {error}")))?;
    // Autocommit pool connection: not inside a caller transaction, so no
    // savepoint (it would error outside a tx) — the probe just catches its own
    // failure and falls back to legacy text NULLs.
    let oids = described_param_oids(cache, &mut conn, sql, builtin, false).await?;
    let query = bind_params_described(sql, &oids, params)?;
    run(query, &mut conn)
        .await
        .map_err(|error| map_db_error(builtin, error))
}

pub(crate) fn row_to_value(row: PgRow) -> Result<VmValue, VmError> {
    let mut map = DictMap::new();
    for (index, column) in row.columns().iter().enumerate() {
        let name = column.name().to_string();
        let value = column_value(&row, index, column.type_info().name())?;
        map.insert(harn_vm_core::value::intern_key(&name), value);
    }
    Ok(VmValue::dict(map))
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
        // NUMERIC decodes to its canonical textual representation (not the
        // `decimal` type) for backward compatibility — existing data-access
        // code reads money columns as `::text`. Wrap the text in `decimal(...)`
        // to get an exact decimal value. (The bind path above DOES accept a
        // `decimal` and encodes it to NUMERIC natively.)
        "NUMERIC" => VmValue::string(
            row.try_get::<rust_decimal::Decimal, _>(index)
                .map_err(decode_error)?
                .to_string(),
        ),
        "TEXT" | "VARCHAR" | "BPCHAR" | "NAME" => {
            VmValue::string(row.try_get::<String, _>(index).map_err(decode_error)?)
        }
        "UUID" => VmValue::string(
            row.try_get::<uuid::Uuid, _>(index)
                .map_err(decode_error)?
                .to_string(),
        ),
        "JSON" | "JSONB" => {
            let json = row
                .try_get::<serde_json::Value, _>(index)
                .map_err(decode_error)?;
            harn_vm_core::schema::json_to_vm_value(&json)
        }
        "BYTEA" => VmValue::Bytes(std::sync::Arc::new(
            row.try_get::<Vec<u8>, _>(index).map_err(decode_error)?,
        )),
        "DATE" => VmValue::string(
            row.try_get::<time::Date, _>(index)
                .map_err(decode_error)?
                .to_string(),
        ),
        "TIME" => VmValue::string(
            row.try_get::<time::Time, _>(index)
                .map_err(decode_error)?
                .to_string(),
        ),
        "TIMESTAMP" => VmValue::string(
            row.try_get::<time::PrimitiveDateTime, _>(index)
                .map_err(decode_error)?
                .to_string(),
        ),
        "TIMESTAMPTZ" => VmValue::string(
            row.try_get::<time::OffsetDateTime, _>(index)
                .map_err(decode_error)?
                .to_string(),
        ),
        // Postgres array types. sqlx exposes these as `<ELEMENT>[]`. Common
        // element types map directly; anything else (e.g. user-defined
        // enum arrays) falls back to text below.
        "BOOL[]" => decode_array::<bool>(row, index, VmValue::Bool)?,
        "INT2[]" => decode_array::<i16>(row, index, |v| VmValue::Int(i64::from(v)))?,
        "INT4[]" => decode_array::<i32>(row, index, |v| VmValue::Int(i64::from(v)))?,
        "INT8[]" => decode_array::<i64>(row, index, VmValue::Int)?,
        "FLOAT4[]" => decode_array::<f32>(row, index, |v| VmValue::Float(f64::from(v)))?,
        "FLOAT8[]" => decode_array::<f64>(row, index, VmValue::Float)?,
        "TEXT[]" | "VARCHAR[]" => decode_array::<String>(row, index, VmValue::string)?,
        "UUID[]" => decode_array::<uuid::Uuid>(row, index, |v| VmValue::string(v.to_string()))?,
        "JSON[]" | "JSONB[]" => {
            let values: Vec<serde_json::Value> = row.try_get(index).map_err(decode_error)?;
            VmValue::list(
                values
                    .iter()
                    .map(harn_vm_core::schema::json_to_vm_value)
                    .collect(),
            )
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
            |v| VmValue::string(v.to_string()),
        ),
        "DATERANGE" => range_value(
            row.try_get::<sqlx_postgres::types::PgRange<time::Date>, _>(index)
                .map_err(decode_error)?,
            |v| VmValue::string(v.to_string()),
        ),
        "TSRANGE" => range_value(
            row.try_get::<sqlx_postgres::types::PgRange<time::PrimitiveDateTime>, _>(index)
                .map_err(decode_error)?,
            |v| VmValue::string(v.to_string()),
        ),
        "TSTZRANGE" => range_value(
            row.try_get::<sqlx_postgres::types::PgRange<time::OffsetDateTime>, _>(index)
                .map_err(decode_error)?,
            |v| VmValue::string(v.to_string()),
        ),
        // HSTORE decodes as a Harn dict<string, string|nil>. sqlx surfaces
        // it as `BTreeMap<String, Option<String>>` already.
        "HSTORE" => {
            let map: sqlx_postgres::types::PgHstore = row.try_get(index).map_err(decode_error)?;
            let mut dict = DictMap::new();
            for (key, value) in map.0 {
                dict.insert(
                    harn_vm_core::value::intern_key(&key),
                    value.map(VmValue::string).unwrap_or(VmValue::Nil),
                );
            }
            VmValue::dict(dict)
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
        _ => VmValue::string(row.try_get::<String, _>(index).map_err(|error| {
            runtime_error(format!(
                "pg_query: unsupported column type {type_name}: {error}"
            ))
        })?),
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
    Ok(VmValue::list(values.into_iter().map(map).collect()))
}

pub(super) fn range_value<T>(
    range: sqlx_postgres::types::PgRange<T>,
    map: impl Fn(T) -> VmValue,
) -> VmValue {
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

pub(super) fn points_value(points: Vec<sqlx_postgres::types::PgPoint>) -> VmValue {
    VmValue::list(
        points
            .into_iter()
            .map(|point| point_value(point.x, point.y))
            .collect(),
    )
}

pub(super) fn point_value(x: f64, y: f64) -> VmValue {
    dict_value([("x", VmValue::Float(x)), ("y", VmValue::Float(y))])
}

fn dict_value<const N: usize>(pairs: [(&'static str, VmValue); N]) -> VmValue {
    VmValue::dict(
        pairs
            .into_iter()
            .map(|(key, value)| (harn_vm_core::value::intern_key(key), value)),
    )
}

fn decode_error(error: sqlx_core::error::Error) -> VmError {
    runtime_error(format!("pg_query: row decode failed: {error}"))
}

/// Map a SQLSTATE code to a stable, schema-free category string.
///
/// Returns `Some(category)` for the error classes whose *raw* Postgres message
/// embeds sensitive schema detail (constraint names, column names, relation
/// names) that should not cross the hostlib boundary to a `.harn` caller. The
/// SQLSTATE itself is stable and safe to surface. Unknown codes return `None`,
/// and those fall back to a generic `"database error"` category (still without
/// the raw message). The five-character SQLSTATE class (first two chars) is used
/// so we cover whole families, e.g. `23xxx` integrity-constraint violations.
pub(super) fn sqlstate_category(code: &str) -> Option<&'static str> {
    match code {
        // 23xxx — integrity constraint violation (names tables/constraints).
        "23505" => Some("unique_violation"),
        "23503" => Some("foreign_key_violation"),
        "23502" => Some("not_null_violation"),
        "23514" => Some("check_violation"),
        "23P01" => Some("exclusion_violation"),
        // 22xxx — data exceptions (overflow, etc.) name columns/values.
        "22003" => Some("numeric_out_of_range"),
        "22001" => Some("string_too_long"),
        "22P02" => Some("invalid_text_representation"),
        // 42xxx — syntax / access-rule (relation/column names).
        "42501" => Some("insufficient_privilege"),
        "42P01" => Some("undefined_table"),
        "42703" => Some("undefined_column"),
        // 40xxx — transaction rollback (serialization / deadlock).
        "40001" => Some("serialization_failure"),
        "40P01" => Some("deadlock_detected"),
        _ => {
            // Cover the whole 23xxx integrity-violation family by class so a
            // newer constraint code still gets a stable, schema-free category.
            if code.starts_with("23") {
                Some("constraint_violation")
            } else {
                None
            }
        }
    }
}

/// Map a sqlx error from a user-facing `pg_query`/`pg_execute` into a stable
/// `VmError` whose message does **not** leak raw Postgres detail (constraint
/// names, schema/relation names, column names). The full original error —
/// including any constraint name — is recorded via `tracing` at the boundary so
/// operators keep the detail server-side (M-2).
///
/// For a database error we surface `{builtin}: <category> (SQLSTATE <code>)`.
/// Non-database errors (pool/IO/protocol) carry no schema detail, so their text
/// is preserved behind the `{builtin}:` prefix as before.
pub(super) fn map_db_error(builtin: &str, error: sqlx_core::error::Error) -> VmError {
    if let sqlx_core::error::Error::Database(db) = &error {
        let code = db.code().map(|c| c.into_owned());
        let category = code
            .as_deref()
            .and_then(sqlstate_category)
            .unwrap_or("database error");
        // Keep the full, detail-bearing error in server-side tracing only.
        tracing::warn!(
            target: "harn_vm::postgres",
            builtin,
            sqlstate = code.as_deref().unwrap_or("none"),
            constraint = db.constraint().unwrap_or(""),
            table = db.table().unwrap_or(""),
            error = %error,
            "postgres error (detail withheld from caller)"
        );
        return match code {
            Some(code) => runtime_error(format!("{builtin}: {category} (SQLSTATE {code})")),
            None => runtime_error(format!("{builtin}: {category}")),
        };
    }
    runtime_error(format!("{builtin}: {error}"))
}
