use super::*;
use harn_vm_core::value::{intern_key, DictMap, VmDictExt};
use std::ops::Bound;

#[path = "tests/migrations.rs"]
mod migrations;

#[path = "tests/transactions.rs"]
mod transactions;

#[path = "describe_tests.rs"]
mod describe_tests;

use harn_vm::{compile_source, register_vm_stdlib, NativeBuiltinCtxExt};

fn live_postgres_url() -> String {
    let url = std::env::var("HARN_TEST_POSTGRES_URL")
        .expect("live Postgres tests require HARN_TEST_POSTGRES_URL; use make test-postgres-live");
    assert!(
        !url.trim().is_empty(),
        "HARN_TEST_POSTGRES_URL must not be empty"
    );
    url
}

fn cloud_migrations_dir() -> String {
    let dir = std::env::var("HARN_TEST_CLOUD_MIGRATIONS_DIR")
        .expect("cloud migration tests require HARN_TEST_CLOUD_MIGRATIONS_DIR");
    assert!(
        std::path::Path::new(&dir).is_dir(),
        "cloud migrations directory is missing"
    );
    dir
}

fn s(value: &str) -> VmValue {
    VmValue::string(value)
}

fn dict(pairs: &[(&str, VmValue)]) -> VmValue {
    VmValue::dict(
        pairs
            .iter()
            .map(|(key, value)| (intern_key(key), value.clone())),
    )
}

fn lazy_pool_for_test() -> Arc<PgPool> {
    let options = PgConnectOptions::from_str("postgres://postgres@localhost/postgres").unwrap();
    Arc::new(
        PgPoolOptions::new()
            .max_connections(1)
            .connect_lazy_with(options),
    )
}

/// M-4: the GUC allowlist accepts the app's own `app.*` namespace and the
/// benign timeout GUCs, and rejects privileged backend GUCs (`role`,
/// `session_authorization`, `is_superuser`, `search_path`) that could be used
/// to escape row-level security.
#[test]
fn transaction_setting_allowlist_permits_app_and_timeouts_rejects_privileged() {
    // Allowed: the application's own contract that RLS policies read.
    assert!(is_allowed_transaction_setting("app.current_tenant_id"));
    assert!(is_allowed_transaction_setting("app.bypass_rls"));
    assert!(is_allowed_transaction_setting("app.anything_else"));
    // Allowed: benign timeouts, case-insensitive (PG GUC names are).
    assert!(is_allowed_transaction_setting("statement_timeout"));
    assert!(is_allowed_transaction_setting("Statement_Timeout"));
    assert!(is_allowed_transaction_setting("lock_timeout"));
    assert!(is_allowed_transaction_setting(
        "idle_in_transaction_session_timeout"
    ));

    // Rejected: privileged GUCs that bypass RLS at the Postgres level.
    assert!(!is_allowed_transaction_setting("role"));
    assert!(!is_allowed_transaction_setting("ROLE"));
    assert!(!is_allowed_transaction_setting("session_authorization"));
    assert!(!is_allowed_transaction_setting("is_superuser"));
    assert!(!is_allowed_transaction_setting("search_path"));
    // Rejected: empty / malformed.
    assert!(!is_allowed_transaction_setting(""));
    assert!(!is_allowed_transaction_setting("app."));
    assert!(!is_allowed_transaction_setting("work_mem"));
}

/// M-2: SQLSTATE codes map to stable, schema-free categories. The mapping
/// must never echo a constraint or relation name — only the category and the
/// (stable) SQLSTATE.
#[test]
fn sqlstate_category_maps_sensitive_classes() {
    assert_eq!(sqlstate_category("23505"), Some("unique_violation"));
    assert_eq!(sqlstate_category("23503"), Some("foreign_key_violation"));
    assert_eq!(sqlstate_category("23502"), Some("not_null_violation"));
    assert_eq!(sqlstate_category("23514"), Some("check_violation"));
    // Unknown 23xxx still gets a stable family category, not the raw text.
    assert_eq!(sqlstate_category("23999"), Some("constraint_violation"));
    assert_eq!(sqlstate_category("22003"), Some("numeric_out_of_range"));
    assert_eq!(sqlstate_category("0A000"), None);
}

fn routing_record(replicas: usize, policy: ReadRoutingPolicy) -> Arc<PoolRecord> {
    Arc::new(PoolRecord {
        pool: lazy_pool_for_test(),
        replicas: (0..replicas).map(|_| lazy_pool_for_test()).collect(),
        replica_cursor: AtomicUsize::new(0),
        max_connections: 1,
        statement_cache_capacity: DEFAULT_STATEMENT_CACHE_CAPACITY,
        read_routing_policy: policy,
        circuit: Arc::new(CircuitBreakerState::disabled()),
        described_oids: Arc::new(SyncMutex::new(BTreeMap::new())),
    })
}

#[test]
fn read_routing_policy_options_parse_named_modes() {
    let pool_options =
        DictMap::from_iter([(intern_key("read_routing_policy"), s("round_robin_replica"))]);
    assert_eq!(
        read_routing_policy_from_options(Some(&pool_options)).unwrap(),
        ReadRoutingPolicy::RoundRobinReplica
    );

    let query_options = DictMap::from_iter([(intern_key("route"), s("replica"))]);
    assert_eq!(
        routing_from_options(Some(&query_options)).unwrap(),
        QueryRouting::Policy(ReadRoutingPolicy::Replica)
    );

    let read_only_options = DictMap::from_iter([(intern_key("read_only"), VmValue::Bool(true))]);
    assert_eq!(
        routing_from_options(Some(&read_only_options)).unwrap(),
        QueryRouting::ReadOnly
    );

    let bad_options = DictMap::from_iter([(intern_key("routing_policy"), s("nearby"))]);
    assert!(routing_from_options(Some(&bad_options)).is_err());
}

#[tokio::test(flavor = "current_thread")]
async fn read_routing_policy_selects_replicas_or_errors_deterministically() {
    let record = routing_record(2, ReadRoutingPolicy::RoundRobinReplica);
    let first = pool_for_routing(&record, QueryRouting::ReadOnly, "pg_query").unwrap();
    let second = pool_for_routing(&record, QueryRouting::ReadOnly, "pg_query").unwrap();
    assert!(Arc::ptr_eq(&first, &record.replicas[0]));
    assert!(Arc::ptr_eq(&second, &record.replicas[1]));

    let fallback = routing_record(0, ReadRoutingPolicy::ReplicaOrPrimary);
    let pool = pool_for_routing(&fallback, QueryRouting::ReadOnly, "pg_query").unwrap();
    assert!(Arc::ptr_eq(&pool, &fallback.pool));

    let strict = routing_record(0, ReadRoutingPolicy::RoundRobinReplica);
    assert!(pool_for_routing(&strict, QueryRouting::ReadOnly, "pg_query").is_err());
}

#[test]
fn range_value_preserves_bounds_and_inclusivity() {
    let value = range_value(
        sqlx_postgres::types::PgRange {
            start: Bound::Included(10_i64),
            end: Bound::Excluded(20_i64),
        },
        VmValue::Int,
    );
    let dict = value.as_dict().expect("range dict");
    assert_eq!(dict.get("start").and_then(VmValue::as_int), Some(10));
    assert_eq!(dict.get("end").and_then(VmValue::as_int), Some(20));
    assert!(matches!(
        dict.get("start_inclusive"),
        Some(VmValue::Bool(true))
    ));
    assert!(matches!(
        dict.get("end_inclusive"),
        Some(VmValue::Bool(false))
    ));
}

#[test]
fn geometry_helpers_return_structured_dicts() {
    let point = point_value(1.5, 2.5);
    let point = point.as_dict().expect("point dict");
    assert!(matches!(point.get("x"), Some(VmValue::Float(1.5))));
    assert!(matches!(point.get("y"), Some(VmValue::Float(2.5))));

    let points = points_value(vec![sqlx_postgres::types::PgPoint { x: 3.0, y: 4.0 }]);
    let VmValue::List(items) = points else {
        panic!("points should be a list");
    };
    let first = items[0].as_dict().expect("nested point");
    assert!(matches!(first.get("x"), Some(VmValue::Float(3.0))));
    assert!(matches!(first.get("y"), Some(VmValue::Float(4.0))));
}

#[test]
fn mock_pool_matches_parameterized_query_and_records_calls() {
    reset_postgres_state();
    let fixtures = VmValue::list(vec![dict(&[
        ("sql", s("select * from claims where tenant_id = $1")),
        ("params", VmValue::list(vec![s("tenant-a")])),
        ("rows", VmValue::list(vec![dict(&[("claim_id", s("c1"))])])),
    ])]);
    let fixture_list = match &fixtures {
        VmValue::List(items) => items,
        _ => unreachable!(),
    };
    let handle = VmValue::resource(VmResourceHandle::new(
        HANDLE_MOCK,
        SyncMutex::new(MockDb {
            fixtures: crate::sql_common::parse_mock_fixtures(fixture_list, "pg_mock_pool").unwrap(),
            calls: Vec::new(),
        }),
    ));
    let rows = mock_query(
        &handle,
        "select * from claims where tenant_id = $1",
        &[s("tenant-a")],
        false,
    )
    .unwrap();
    assert_eq!(VmValue::list(rows).display(), "[{claim_id: c1}]");
    let calls = mock_handle(Some(&handle), "test")
        .unwrap()
        .lock()
        .calls
        .clone();
    assert_eq!(calls.len(), 1);
}

#[test]
fn postgres_authority_cannot_be_forged_with_a_dict() {
    let forged = dict(&[("_type", s(HANDLE_POOL)), ("id", s("pgpool-1"))]);
    let error = pool_record_from_handle(&forged, "pg_query")
        .err()
        .expect("script-visible dictionaries must not act as pool authority");
    assert!(error.to_string().contains("expected pg_pool handle"));
}

#[test]
fn mock_execute_returns_rows_affected() {
    reset_postgres_state();
    let fixtures = crate::sql_common::parse_mock_fixtures(
        &[dict(&[
            ("sql", s("update receipts set status = $1")),
            ("rows_affected", VmValue::Int(3)),
        ])],
        "pg_mock_pool",
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
#[ignore = "requires an isolated database; make test-postgres-live"]
async fn postgres_round_trip_when_env_url_is_set() {
    let url = live_postgres_url();
    reset_postgres_state();
    let mut options = harn_vm_core::value::DictMap::new();
    options.put("max_connections", VmValue::Int(1));
    options.put("application_name", s("harn-postgres-stdlib-test"));
    let ctx = harn_vm_core::vm::AsyncBuiltinCtx::for_test(harn_vm::new_vm());
    let handle = open_pool(&ctx, &s(&url), Some(&options), false)
        .await
        .unwrap();
    assert_eq!(
        pool_record_from_handle(&handle, "test")
            .unwrap()
            .max_connections,
        1
    );
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

/// Opens a single-connection pool against the test database so every query
/// reuses the same physical connection (and prepared-statement cache).
async fn open_single_conn_pool(url: &str) -> VmValue {
    let mut options = harn_vm_core::value::DictMap::new();
    options.put("max_connections", VmValue::Int(1));
    options.put("application_name", s("harn-postgres-bind-test"));
    let ctx = harn_vm_core::vm::AsyncBuiltinCtx::for_test(harn_vm::new_vm());
    open_pool(&ctx, &s(url), Some(&options), false)
        .await
        .expect("open single-connection pool")
}

/// Resolve the underlying primary `PgPool` `Arc` behind a `pg_pool` handle so
/// tests can assert two handles point at the SAME (or distinct) pool via
/// `Arc::ptr_eq`.
fn pool_ptr(handle: &VmValue) -> Arc<PgPool> {
    pool_record_from_handle(handle, "test")
        .map(|record| Arc::clone(&record.pool))
        .expect("pool authority")
}

/// SECURITY/CORRECTNESS: with the shared registry installed, two `pg_pool`
/// calls for the SAME connection identity reuse ONE underlying pool, while a
/// call for a DIFFERENT identity (database) gets its own — and a CLI-style
/// run with the registry NOT consulted is unaffected. Uses lazy pools so it
/// needs no live database.
#[tokio::test(flavor = "current_thread")]
async fn shared_registry_shares_on_match_and_isolates_on_mismatch() {
    shared::install_shared_pool_registry();
    shared::clear_for_test();
    reset_postgres_state();

    // Route through the registry primitives (not open_pool's eager connect)
    // so the test needs no live database. This exercises exactly the
    // share/adopt/isolate logic open_pool relies on.
    //
    // Simulate two requests opening the same identity: build the record once
    // through the registry, then a second "request" must adopt it.
    let key_a = shared::PoolKey::new("postgres://u:p@h/db_a", &[], None, false).unwrap();
    let key_b = shared::PoolKey::new("postgres://u:p@h/db_b", &[], None, false).unwrap();

    let rec_a1 = Arc::new(lazy_record());
    let shared_a1 = shared::get_or_insert(key_a.clone(), Arc::clone(&rec_a1));
    // First insert wins and is the very record we passed in.
    assert!(Arc::ptr_eq(&rec_a1, &shared_a1));

    // A second request for the same identity builds its own record but must
    // ADOPT the already-registered one (its own is dropped).
    let rec_a2 = Arc::new(lazy_record());
    let shared_a2 = shared::get_or_insert(key_a.clone(), Arc::clone(&rec_a2));
    assert!(
        Arc::ptr_eq(&shared_a1, &shared_a2),
        "same identity must share one PoolRecord"
    );
    assert!(
        !Arc::ptr_eq(&rec_a2, &shared_a2),
        "the racing/second record must be dropped in favor of the canonical one"
    );

    // A lookup of the same key returns the canonical shared record.
    let got = shared::get(&key_a).expect("registered");
    assert!(Arc::ptr_eq(&got, &shared_a1));

    // A different identity (different database) gets its own record.
    let rec_b = Arc::new(lazy_record());
    let shared_b = shared::get_or_insert(key_b, Arc::clone(&rec_b));
    assert!(
        !Arc::ptr_eq(&shared_a1, &shared_b),
        "different identity must NOT share a pool"
    );

    assert_eq!(shared::len_for_test(), 2);
    shared::clear_for_test();
}

/// Build a `PoolRecord` around a lazily-connected pool — no network I/O until
/// a query runs (which these tests never do). Mirrors the shape `open_pool`
/// constructs.
fn lazy_record() -> PoolRecord {
    PoolRecord {
        pool: lazy_pool_for_test(),
        replicas: Vec::new(),
        replica_cursor: AtomicUsize::new(0),
        max_connections: 1,
        statement_cache_capacity: DEFAULT_STATEMENT_CACHE_CAPACITY,
        read_routing_policy: ReadRoutingPolicy::ReplicaOrPrimary,
        circuit: Arc::new(circuit::CircuitBreakerState::disabled()),
        described_oids: Arc::new(SyncMutex::new(BTreeMap::new())),
    }
}

/// End-to-end against a live DB (gated on `HARN_TEST_POSTGRES_URL`): with the
/// shared registry installed, `open_pool` for the same source across two
/// distinct `Vm`s / simulated requests returns handles backed by the SAME
/// physical pool; a different database does not share.
#[tokio::test(flavor = "current_thread")]
#[ignore = "requires an isolated database; make test-postgres-live"]
async fn open_pool_shares_across_requests_when_registry_installed() {
    let url = live_postgres_url();
    shared::install_shared_pool_registry();
    shared::clear_for_test();
    reset_postgres_state();

    let ctx = harn_vm_core::vm::AsyncBuiltinCtx::for_test(harn_vm::new_vm());
    let o = dict(&[("max_connections", VmValue::Int(2))]);
    let opt = o.as_dict();

    // Request 1.
    let h1 = open_pool(&ctx, &s(&url), opt, false).await.unwrap();
    // Request 2: fresh handle id, but must resolve to the same pool Arc.
    let h2 = open_pool(&ctx, &s(&url), opt, false).await.unwrap();
    assert!(
        !Arc::ptr_eq(
            &pool_handle(Some(&h1), "test").unwrap(),
            &pool_handle(Some(&h2), "test").unwrap(),
        ),
        "each call still gets distinct pool authority"
    );
    assert!(
        Arc::ptr_eq(&pool_ptr(&h1), &pool_ptr(&h2)),
        "same identity under shared registry must reuse one pool"
    );

    // Different identity (different max_connections) -> different pool.
    let o3 = dict(&[("max_connections", VmValue::Int(7))]);
    let h3 = open_pool(&ctx, &s(&url), o3.as_dict(), false)
        .await
        .unwrap();
    assert!(
        !Arc::ptr_eq(&pool_ptr(&h1), &pool_ptr(&h3)),
        "different pool shape must not share"
    );

    // The text and integer render alike but produce different pool limits.
    let text = dict(&[("max_connections", s("2"))]);
    let text_pool = open_pool(&ctx, &s(&url), text.as_dict(), false)
        .await
        .unwrap();

    assert_eq!(
        pool_record_from_handle(&h1, "test")
            .unwrap()
            .max_connections,
        2
    );
    assert_eq!(
        pool_record_from_handle(&text_pool, "test")
            .unwrap()
            .max_connections,
        5
    );
    assert!(!Arc::ptr_eq(&pool_ptr(&h1), &pool_ptr(&text_pool)));

    // Opaque options cannot produce a safe shared identity.
    let options = dict(&[("extra", VmValue::task_handle("task"))]);
    let first = open_pool(&ctx, &s(&url), options.as_dict(), false)
        .await
        .unwrap();
    let second = open_pool(&ctx, &s(&url), options.as_dict(), false)
        .await
        .unwrap();

    assert!(!Arc::ptr_eq(&pool_ptr(&first), &pool_ptr(&second)));
    shared::clear_for_test();
}

/// CLI default: when the shared registry is NOT installed, two `open_pool`
/// calls for the same source get DISTINCT pools (byte-identical to legacy
/// behavior). Gated on a live DB. NOTE: relies on per-test process isolation
/// (nextest) so no sibling test has installed the registry in this process.
#[tokio::test(flavor = "current_thread")]
#[ignore = "requires an isolated database; make test-postgres-live"]
async fn open_pool_does_not_share_when_registry_absent() {
    let url = live_postgres_url();
    assert!(
        !shared::is_installed(),
        "live Postgres fixtures require nextest per-test process isolation"
    );
    reset_postgres_state();
    let ctx = harn_vm_core::vm::AsyncBuiltinCtx::for_test(harn_vm::new_vm());
    let o = dict(&[("max_connections", VmValue::Int(1))]);
    let h1 = open_pool(&ctx, &s(&url), o.as_dict(), false).await.unwrap();
    let h2 = open_pool(&ctx, &s(&url), o.as_dict(), false).await.unwrap();
    assert!(
        !Arc::ptr_eq(&pool_ptr(&h1), &pool_ptr(&h2)),
        "without the shared registry, each pg_pool opens its own pool"
    );
}

/// H-2 (live): an in-range `Int` binds and round-trips correctly through an
/// `int4` column, and an out-of-range value surfaces a clear, stable
/// `numeric_out_of_range` (SQLSTATE 22003) diagnostic rather than a raw or
/// confusing message.
#[test]
#[ignore = "requires an isolated database; make test-postgres-live"]
fn int_bind_into_int4_column_when_env_url_is_set() {
    live_postgres_url();
    reset_postgres_state();
    let schema = format!("harn_pg_h2_{}", uuid::Uuid::new_v4().simple());
    let ok_source = format!(
        r#"
import "std/postgres"
fn main(harness: Harness) {{
const db = harness.postgres.pool("env:HARN_TEST_POSTGRES_URL", {{max_connections: 1}})
pg_execute(db, "DROP SCHEMA IF EXISTS \"{schema}\" CASCADE", [])
pg_execute(db, "CREATE SCHEMA \"{schema}\"", [])
pg_execute(db, "SET search_path TO \"{schema}\"", [])
pg_execute(db, "CREATE TABLE narrow (a int4, b int2)", [])
pg_execute(db, "INSERT INTO narrow (a, b) VALUES ($1, $2)", [2000000000, 30000])
const row = pg_query_one(db, "SELECT a, b FROM narrow WHERE a = $1", [2000000000])
harness.stdio.println(row.a)
harness.stdio.println(row.b)
pg_close(db)
}}
"#,
    );
    let out = run_harn_source(&ok_source);
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(
        lines,
        vec!["2000000000", "30000"],
        "in-range int must round-trip through int4/int2: {out}"
    );

    // Overflow into int4 yields a clear stable category.
    let overflow_source = format!(
        r#"
import "std/postgres"
fn main(harness: Harness) {{
const db = harness.postgres.pool("env:HARN_TEST_POSTGRES_URL", {{max_connections: 1}})
pg_execute(db, "SET search_path TO \"{schema}\"", [])
pg_execute(db, "INSERT INTO narrow (a) VALUES ($1)", [5000000000])
pg_close(db)
}}
"#,
    );
    let err = run_harn_source_expect_err(&overflow_source);
    assert!(
        err.contains("numeric_out_of_range") && err.contains("22003"),
        "expected numeric_out_of_range diagnostic for int4 overflow, got: {err}"
    );

    let cleanup = format!(
        r#"
import "std/postgres"
fn main(harness: Harness) {{
const db = harness.postgres.pool("env:HARN_TEST_POSTGRES_URL", {{max_connections: 1}})
pg_execute(db, "DROP SCHEMA IF EXISTS \"{schema}\" CASCADE", [])
pg_close(db)
}}
"#,
    );
    run_harn_source(&cleanup);
}

/// Build a synthetic SQLx-style migrations directory (with `.up.sql`
/// and ignorable `.down.sql` siblings) and return (tmpdir, dir-string).
/// Keep the `TempDir` alive for the duration of the test.
fn sqlx_synthetic_migrations() -> (tempfile::TempDir, String) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path();
    let files: &[(&str, &str)] = &[
        (
            "20260419170000_bootstrap.up.sql",
            "CREATE TABLE widgets (id INT PRIMARY KEY, label TEXT NOT NULL)",
        ),
        ("20260419170000_bootstrap.down.sql", "DROP TABLE widgets"),
        (
            "20260423100000_seed_widget.up.sql",
            "INSERT INTO widgets (id, label) VALUES (1, 'alpha')",
        ),
        (
            "20260423100000_seed_widget.down.sql",
            "DELETE FROM widgets WHERE id = 1",
        ),
        (
            "20260424000000_add_gadgets.up.sql",
            "CREATE TABLE gadgets (id INT PRIMARY KEY)",
        ),
        ("20260424000000_add_gadgets.down.sql", "DROP TABLE gadgets"),
    ];
    for (name, body) in files {
        std::fs::write(dir.join(name), body).unwrap();
    }
    let s = dir.to_string_lossy().into_owned();
    (tmp, s)
}

async fn execute_harn_source(source: &str) -> Result<String, VmError> {
    let chunk = compile_source(source).expect("compile source");
    let mut vm = harn_vm::new_vm();
    register_vm_stdlib(&mut vm);
    vm.set_harness(harn_vm::Harness::real());
    vm.execute(&chunk).await?;
    Ok(vm.output().to_string())
}

fn run_harn_source_result(source: &str) -> Result<String, VmError> {
    let rt = harn_clock::test_support::current_thread_runtime();
    rt.block_on(async {
        let local = tokio::task::LocalSet::new();
        local.run_until(execute_harn_source(source)).await
    })
}

fn run_harn_source(source: &str) -> String {
    run_harn_source_result(source).expect("execute source")
}

fn run_harn_source_expect_err(source: &str) -> String {
    format!(
        "{:?}",
        run_harn_source_result(source).expect_err("expected source to error")
    )
}

/// Confirms `duration_ms` lives on every real execute result. The
/// synthetic `Instant` path is covered by
/// `execute_result_value_includes_duration`.
#[test]
#[ignore = "requires an isolated database; make test-postgres-live"]
fn execute_reports_duration_ms_on_real_pool_when_env_url_is_set() {
    live_postgres_url();
    reset_postgres_state();
    let source = r#"
import "std/postgres"

fn main(harness: Harness) {
const db = harness.postgres.pool("env:HARN_TEST_POSTGRES_URL", {max_connections: 1})
const result = pg_execute(db, "SELECT pg_sleep(0.05)", [])
harness.stdio.println(result.duration_ms)
pg_close(db)
}
"#;
    let output = run_harn_source(source);
    let duration_ms: i64 = output.trim().parse().expect("integer duration");
    assert!(duration_ms >= 50, "expected ≥50ms, got {duration_ms}");
}

/// End-to-end smoke for the v2 surface against a real Postgres:
/// pool stats reflect live connections, advisory locks succeed inside
/// a transaction, schema introspection finds the test table, an
/// `int[]` column round-trips through the array decoder, and
/// LISTEN/NOTIFY delivers the payload back through pg_listener_recv.
#[test]
#[ignore = "requires an isolated database; make test-postgres-live"]
fn v2_surface_smoke_when_env_url_is_set() {
    live_postgres_url();
    reset_postgres_state();
    let schema = format!("harn_pg_v2_{}", uuid::Uuid::new_v4().simple());
    let source = format!(
        r#"
import "std/postgres"

fn main(harness: Harness) {{
const db = harness.postgres.pool("env:HARN_TEST_POSTGRES_URL", {{max_connections: 2}})

// --- Pool observability --------------------------------------------------
const stats = pg_pool_stats(db)
harness.stdio.println(stats.circuit_state)
harness.stdio.println(stats.max_connections)
harness.stdio.println(stats.read_routing_policy)
harness.stdio.println(stats.replicas)

const clear_result = pg_stmt_cache_clear(db)
harness.stdio.println(clear_result.pools)
harness.stdio.println(clear_result.connections_cleared >= 1)
harness.stdio.println(clear_result.connections_skipped)

// --- Schema setup --------------------------------------------------------
pg_execute(db, "CREATE SCHEMA IF NOT EXISTS \"{schema}\"", [])
pg_execute(db, "CREATE TABLE \"{schema}\".widgets (id int4 PRIMARY KEY, tags text[] NOT NULL DEFAULT '{{}}')", [])
pg_execute(db, "CREATE UNIQUE INDEX widgets_id_uniq ON \"{schema}\".widgets (id)", [])
pg_execute(db, "INSERT INTO \"{schema}\".widgets (id, tags) VALUES (1, ARRAY['alpha','beta'])", [])
pg_execute(db, "INSERT INTO \"{schema}\".widgets (id, tags) VALUES (2, ARRAY[]::text[])", [])

// --- Advisory lock inside a transaction ----------------------------------
const locked_label = pg_transaction(db, {{ tx ->
  pg_advisory_xact_lock(tx, 5215575661689875131)
  return pg_query_one(tx, "SELECT 'locked' AS label", []).label
}})
harness.stdio.println(locked_label)

// --- pg_with_advisory_lock (RAII helper, exercises run_managed_transaction) ----
const with_label = pg_with_advisory_lock(db, "release-cut", {{ tx ->
  return pg_query_one(tx, "SELECT 'raii' AS label", []).label
}})
harness.stdio.println(with_label)

// --- Schema introspection ------------------------------------------------
const tables = pg_introspect_tables(db, {{schema: "{schema}"}})
harness.stdio.println(len(tables))
harness.stdio.println(tables[0].kind)

const cols = pg_introspect_columns(db, "{schema}.widgets")
harness.stdio.println(len(cols))
harness.stdio.println(cols[0].column + ":" + cols[0].type)
harness.stdio.println(cols[1].column + ":" + cols[1].type)

const idx = pg_introspect_indexes(db, "{schema}.widgets")
assert(len(idx[0].columns) == 1 && idx[0].columns[0] == "id")
assert(len(idx[1].columns) == 1 && idx[1].columns[0] == "id")
harness.stdio.println(len(idx))

// --- Array decoding ------------------------------------------------------
const row = pg_query_one(db, "SELECT tags FROM \"{schema}\".widgets WHERE id = $1", [1])
harness.stdio.println(row.tags[0] + "," + row.tags[1])

const empty = pg_query_one(db, "SELECT tags FROM \"{schema}\".widgets WHERE id = $1", [2])
harness.stdio.println(len(empty.tags))

// --- LISTEN/NOTIFY round-trip --------------------------------------------
const listener = pg_listen(db, "harn_v2_test")
pg_notify(db, "harn_v2_test", "hello")
const notification = pg_listener_recv(listener, 5000)
harness.stdio.println(notification.channel + ":" + notification.payload)
pg_listener_close(listener)

// --- Teardown ------------------------------------------------------------
pg_execute(db, "DROP SCHEMA \"{schema}\" CASCADE", [])
pg_close(db)
}}
"#,
    );
    let output = run_harn_source(&source);
    let lines: Vec<&str> = output.lines().collect();
    assert_eq!(lines.len(), 18, "unexpected smoke output: {output}");
    // Expected (in order):
    //   disabled            // circuit_state
    //   2                   // max_connections
    //   replica_or_primary  // read_routing_policy
    //   0                   // replicas
    //   1                   // primary pool cache clear
    //   true                // at least one idle connection cleared
    //   0                   // no checked-out connections skipped
    //   locked              // pg_advisory_xact_lock path label
    //   raii                // pg_with_advisory_lock path label
    //   1                   // tables in schema
    //   table               // kind
    //   2                   // columns count
    //   id:int4             // column 0 type
    //   tags:_text          // column 1 type (PG type is _text)
    //   2                   // PK + explicit UNIQUE indexes
    //   alpha,beta          // array decoding
    //   0                   // empty array length
    //   harn_v2_test:hello  // notification
    assert_eq!(lines[0], "disabled");
    assert_eq!(lines[1], "2");
    assert_eq!(lines[2], "replica_or_primary");
    assert_eq!(lines[3], "0");
    assert_eq!(lines[4], "1");
    assert_eq!(lines[5], "true");
    assert_eq!(lines[6], "0");
    assert_eq!(lines[7], "locked");
    assert_eq!(lines[8], "raii");
    assert_eq!(lines[9], "1");
    assert_eq!(lines[10], "table");
    assert_eq!(lines[11], "2");
    assert_eq!(lines[12], "id:int4");
    assert!(
        lines[13] == "tags:_text" || lines[13] == "tags:text[]",
        "tags column type unexpected: {}",
        lines[13]
    );
    // PK index + the explicit UNIQUE = 2 indexes
    assert_eq!(lines[14], "2");
    assert_eq!(lines[15], "alpha,beta");
    assert_eq!(lines[16], "0");
    assert_eq!(lines[17], "harn_v2_test:hello");
}

/// Advisory locks must isolate distinct tenants when
/// `tenant_namespace: true` is set: the same caller-supplied key
/// resolves to *different* server-side lock keys per tenant. Without
/// that, two tenants would deadlock each other for routine
/// per-resource locks.
#[test]
fn advisory_lock_tenant_namespacing_keys_differ_per_tenant() {
    use harn_vm::harness_tenant::enter_tenant;
    use harn_vm::TenantId;

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

/// `reject_non_finite_floats` must catch a non-finite float wherever it
/// hides — bound directly, or nested in a list/dict that takes the jsonb
/// path — while leaving finite floats and float-free values alone. No DB
/// required: this is the pure guard that `bind_params` calls per param.
#[test]
fn non_finite_float_guard_catches_direct_and_nested() {
    // Finite scalars and collections pass.
    assert!(reject_non_finite_floats(&VmValue::Float(1.5)).is_ok());
    assert!(reject_non_finite_floats(&VmValue::Float(0.0)).is_ok());
    assert!(reject_non_finite_floats(&VmValue::Int(7)).is_ok());
    assert!(reject_non_finite_floats(&VmValue::Nil).is_ok());
    assert!(
        reject_non_finite_floats(&VmValue::list(vec![VmValue::Float(1.0), VmValue::Int(2),]))
            .is_ok()
    );
    assert!(reject_non_finite_floats(&dict(&[("amount", VmValue::Float(3.25))])).is_ok());

    // Direct non-finite binds are rejected.
    for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let err = reject_non_finite_floats(&VmValue::Float(bad))
            .expect_err("non-finite float must be rejected");
        assert!(
            err.to_string().contains("non-finite float"),
            "error should name the cause: {err}"
        );
    }

    // Nested in a list (jsonb path) — rejected.
    let list = VmValue::list(vec![VmValue::Int(1), VmValue::Float(f64::NAN)]);
    assert!(reject_non_finite_floats(&list).is_err());

    // Nested in a dict (jsonb path) — rejected.
    let nested = dict(&[("ratio", VmValue::Float(f64::INFINITY))]);
    assert!(reject_non_finite_floats(&nested).is_err());
}

/// Live regression: binding a non-finite float must fail cleanly with the
/// guard's error rather than corrupting a `float8` column or emitting
/// invalid JSON on the jsonb path. Gated on `HARN_TEST_POSTGRES_URL`.
#[tokio::test(flavor = "current_thread")]
#[ignore = "requires an isolated database; make test-postgres-live"]
async fn non_finite_float_bind_errors_cleanly_when_env_url_is_set() {
    let url = live_postgres_url();
    reset_postgres_state();
    let handle = open_single_conn_pool(&url).await;

    // 1) Direct float8 bind of each non-finite value must error.
    for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let err = query_rows(
            &handle,
            "select $1::float8 as v",
            &[VmValue::Float(bad)],
            QueryRouting::Primary,
        )
        .await
        .expect_err("non-finite float8 bind must be rejected before sqlx");
        assert!(
            err.to_string().contains("non-finite float"),
            "error should be the guard's, not a raw sqlx error: {err}"
        );
    }

    // 2) Non-finite float nested in a jsonb-bound value must also error
    //    (rather than silently serializing to JSON null).
    let err = query_rows(
        &handle,
        "select $1::jsonb as payload",
        &[dict(&[("ratio", VmValue::Float(f64::NAN))])],
        QueryRouting::Primary,
    )
    .await
    .expect_err("non-finite float in jsonb path must be rejected");
    assert!(
        err.to_string().contains("non-finite float"),
        "jsonb path should hit the same guard: {err}"
    );

    // 3) A finite float still round-trips unchanged.
    let row = query_rows(
        &handle,
        "select $1::float8 as v",
        &[VmValue::Float(1.5)],
        QueryRouting::Primary,
    )
    .await
    .expect("finite float bind must still work")
    .remove(0);
    assert!(
        matches!(row.as_dict().unwrap().get("v"), Some(VmValue::Float(f)) if *f == 1.5),
        "finite float must round-trip unchanged"
    );
}

/// Pull the lone `v` cell out of the first row of a single-column query.
fn one_cell(rows: Vec<VmValue>, key: &str) -> VmValue {
    rows.into_iter()
        .next()
        .and_then(|row| row.as_dict().and_then(|d| d.get(key).cloned()))
        .unwrap_or(VmValue::Nil)
}

/// describe-then-bind: a bare `$n` against a typed column stores SQL NULL
/// instead of failing with `column is of type integer but expression is of
/// type text` (the `None::<String>` failure mode).
#[tokio::test(flavor = "current_thread")]
#[ignore = "requires an isolated database; make test-postgres-live"]
async fn nil_into_typed_columns_stores_sql_null_when_env_url_is_set() {
    let url = live_postgres_url();
    reset_postgres_state();
    let handle = open_single_conn_pool(&url).await;

    execute_stmt(&handle, "DROP TABLE IF EXISTS harn_pg_nil_typed", &[])
        .await
        .expect("drop table");
    execute_stmt(
        &handle,
        "CREATE TABLE harn_pg_nil_typed (id int PRIMARY KEY, i integer, j jsonb, t text)",
        &[],
    )
    .await
    .expect("create table");

    // Bare `$n` into typed columns with nils — would be rejected as
    // "integer but expression is of type text" under the old text-NULL bind.
    execute_stmt(
        &handle,
        "INSERT INTO harn_pg_nil_typed (id, i, j, t) VALUES ($1, $2, $3, $4)",
        &[VmValue::Int(1), VmValue::Nil, VmValue::Nil, VmValue::Nil],
    )
    .await
    .expect("insert bare nils into typed columns");

    let rows = query_rows(
            &handle,
            "SELECT i, j, t, (i IS NULL) AS i_null, (j IS NULL) AS j_null FROM harn_pg_nil_typed WHERE id = 1",
            &[],
            QueryRouting::Primary,
        )
        .await
        .expect("read back nulls");
    let row = rows.into_iter().next().unwrap();
    let d = row.as_dict().unwrap();
    assert!(
        matches!(d.get("i"), Some(VmValue::Nil)),
        "i must be SQL NULL"
    );
    assert!(
        matches!(d.get("j"), Some(VmValue::Nil)),
        "j must be SQL NULL"
    );
    assert!(
        matches!(d.get("i_null"), Some(VmValue::Bool(true))),
        "i IS NULL must be true"
    );
    assert!(
        matches!(d.get("j_null"), Some(VmValue::Bool(true))),
        "j IS NULL must be true"
    );

    execute_stmt(&handle, "DROP TABLE harn_pg_nil_typed", &[])
        .await
        .expect("cleanup");
}

/// describe-then-bind: the cache-poisoning regression. The same SQL
/// (`SELECT $1::bigint`) is run NULL-first then non-null on the *same*
/// pooled connection. The old text-NULL bind poisoned the SQL-keyed
/// prepared-statement cache, so the second call failed with
/// `invalid byte sequence for encoding "UTF8": 0x00`.
#[tokio::test(flavor = "current_thread")]
#[ignore = "requires an isolated database; make test-postgres-live"]
async fn nil_then_non_null_same_sql_does_not_poison_cache_when_env_url_is_set() {
    let url = live_postgres_url();
    reset_postgres_state();
    let handle = open_single_conn_pool(&url).await;

    // NULL first — primes the statement cache for this SQL.
    let first = query_rows(
        &handle,
        "SELECT $1::bigint AS v",
        &[VmValue::Nil],
        QueryRouting::Primary,
    )
    .await
    .expect("null bigint bind must succeed");
    assert!(matches!(one_cell(first, "v"), VmValue::Nil));

    // Non-null int at the SAME `$1` slot on the SAME connection — must not
    // hit the poisoned-cache 0x00 error.
    let second = query_rows(
        &handle,
        "SELECT $1::bigint AS v",
        &[VmValue::Int(42)],
        QueryRouting::Primary,
    )
    .await
    .expect("non-null bigint after null must not be poisoned");
    assert!(matches!(one_cell(second, "v"), VmValue::Int(42)));

    // And NULL again still works.
    let third = query_rows(
        &handle,
        "SELECT $1::bigint AS v",
        &[VmValue::Nil],
        QueryRouting::Primary,
    )
    .await
    .expect("null bigint again");
    assert!(matches!(one_cell(third, "v"), VmValue::Nil));
}

/// describe-then-bind: mixed nil + non-null typed params in one query —
/// the exact shape the OID-0 ("let server infer everything") approach broke
/// with `incorrect binary data format in bind parameter N`. The concrete
/// sibling params keep their binary encodings while the nils declare the
/// described OID.
#[tokio::test(flavor = "current_thread")]
#[ignore = "requires an isolated database; make test-postgres-live"]
async fn mixed_nil_and_non_null_params_when_env_url_is_set() {
    let url = live_postgres_url();
    reset_postgres_state();
    let handle = open_single_conn_pool(&url).await;

    execute_stmt(&handle, "DROP TABLE IF EXISTS harn_pg_nil_mixed", &[])
        .await
        .expect("drop");
    execute_stmt(
        &handle,
        "CREATE TABLE harn_pg_nil_mixed (id int PRIMARY KEY, a int, b text, c jsonb)",
        &[],
    )
    .await
    .expect("create");

    // INSERT with [non-null id, nil a, "x" b, nil c] — mixes binary int +
    // text + typed NULLs across slots.
    execute_stmt(
        &handle,
        "INSERT INTO harn_pg_nil_mixed (id, a, b, c) VALUES ($1, $2, $3, $4)",
        &[VmValue::Int(1), VmValue::Nil, s("x"), VmValue::Nil],
    )
    .await
    .expect("mixed insert must not hit binary-format mismatch");

    execute_stmt(
        &handle,
        "INSERT INTO harn_pg_nil_mixed (id, a, b, c) VALUES ($1, $2, $3, $4)",
        &[
            VmValue::Int(2),
            VmValue::Int(7),
            VmValue::Nil,
            dict(&[("k", VmValue::Int(9))]),
        ],
    )
    .await
    .expect("second mixed insert");

    // SELECT mixing nil + non-null int in the WHERE clause — the failing
    // shape for OID-0.
    let rows = query_rows(
        &handle,
        "SELECT id FROM harn_pg_nil_mixed WHERE (a = $1 OR $1 IS NULL) AND id > $2 ORDER BY id",
        &[VmValue::Nil, VmValue::Int(0)],
        QueryRouting::Primary,
    )
    .await
    .expect("mixed nil + non-null WHERE must not hit binary-format mismatch");
    let ids: Vec<i64> = rows
        .iter()
        .filter_map(|r| {
            r.as_dict()
                .and_then(|d| d.get("id"))
                .and_then(VmValue::as_int)
        })
        .collect();
    assert_eq!(ids, vec![1, 2], "the `$1 IS NULL` branch matches all rows");

    // COALESCE with a nil + non-null fallback.
    let coalesced = query_rows(
        &handle,
        "SELECT COALESCE($1::int, $2::int) AS v",
        &[VmValue::Nil, VmValue::Int(99)],
        QueryRouting::Primary,
    )
    .await
    .expect("coalesce nil/non-null");
    assert!(matches!(one_cell(coalesced, "v"), VmValue::Int(99)));

    // CASE mixing a nil and a non-null branch.
    let cased = query_rows(
        &handle,
        "SELECT CASE WHEN $1::int IS NULL THEN $2::text ELSE 'no' END AS v",
        &[VmValue::Nil, s("was-null")],
        QueryRouting::Primary,
    )
    .await
    .expect("case nil/non-null");
    assert_eq!(one_cell(cased, "v").display(), "was-null");

    // Multi-row VALUES with mixed nil / non-null across rows and columns.
    let multi = query_rows(
            &handle,
            "SELECT n, t FROM (VALUES ($1::int, $2::text), ($3::int, $4::text)) AS v(n, t) ORDER BY n NULLS LAST",
            &[VmValue::Int(1), VmValue::Nil, VmValue::Nil, s("two")],
            QueryRouting::Primary,
        )
        .await
        .expect("multi-row VALUES mixed nil/non-null");
    assert_eq!(multi.len(), 2);

    execute_stmt(&handle, "DROP TABLE harn_pg_nil_mixed", &[])
        .await
        .expect("cleanup");
}

/// describe-then-bind: an ambiguous bare `SELECT $1` with a nil. Postgres
/// cannot infer a type for a lone unconstrained parameter, so it defaults
/// the slot to `text`; the described OID is therefore `text` and the NULL
/// round-trips as SQL NULL (documented expected behavior).
#[tokio::test(flavor = "current_thread")]
#[ignore = "requires an isolated database; make test-postgres-live"]
async fn ambiguous_bare_select_nil_when_env_url_is_set() {
    let url = live_postgres_url();
    reset_postgres_state();
    let handle = open_single_conn_pool(&url).await;

    let rows = query_rows(
        &handle,
        "SELECT $1 AS v",
        &[VmValue::Nil],
        QueryRouting::Primary,
    )
    .await
    .expect("ambiguous bare SELECT $1 with nil must succeed as SQL NULL");
    assert!(
        matches!(one_cell(rows, "v"), VmValue::Nil),
        "bare nil select returns SQL NULL"
    );
}

/// POOL path, describe-probe FAILURE → graceful text fallback. `SELECT $1 IS
/// NULL` is a slot Postgres cannot type from structure alone, so the
/// describe probe (`prepare_with(sql, &[])`) errors with `could not
/// determine data type of parameter $1`. The fix catches that, returns an
/// **empty** OID list (cached), and the bind path falls back to a legacy
/// `text` NULL — `text IS NULL` → `true`. Before the fix this query failed
/// outright even though the pre-describe-then-bind behavior worked.
#[tokio::test(flavor = "current_thread")]
#[ignore = "requires an isolated database; make test-postgres-live"]
async fn pool_describe_probe_failure_falls_back_to_text_null_when_env_url_is_set() {
    let url = live_postgres_url();
    reset_postgres_state();
    let handle = open_single_conn_pool(&url).await;

    // `SELECT $1 IS NULL` is a slot Postgres cannot type from structure
    // alone, so the describe probe errors. The probe must NOT reach the
    // caller as an error; the query succeeds
    // and the nil binds as a text NULL, so `$1 IS NULL` is `true`.
    let sql = "SELECT $1 IS NULL AS v";
    let rows = query_rows(&handle, sql, &[VmValue::Nil], QueryRouting::Primary)
        .await
        .expect(
            "ambiguous nil query must succeed via text fallback, not propagate the probe error",
        );
    assert!(
        matches!(one_cell(rows, "v"), VmValue::Bool(true)),
        "text NULL IS NULL must be true"
    );

    // The fallback cached an EMPTY OID list for this SQL (probe failed), so
    // later runs reuse the text fallback with no further probing.
    let record = pool_record_from_handle(&handle, "test").expect("pool authority");
    let cached = record
        .described_oids
        .lock()
        .get(sql)
        .cloned()
        .expect("ambiguous SQL must populate the OID cache (with an empty list)");
    assert!(
        cached.is_empty(),
        "probe failure must cache an empty OID list (got {cached:?})"
    );

    // A repeat run still succeeds (cache hit, still text fallback).
    let again = query_rows(&handle, sql, &[VmValue::Nil], QueryRouting::Primary)
        .await
        .expect("repeat ambiguous nil query still succeeds");
    assert!(matches!(one_cell(again, "v"), VmValue::Bool(true)));
}

/// TX path, describe-probe FAILURE must NOT abort the caller's transaction.
/// A failed `prepare_with` inside a tx normally taints it (`current
/// transaction is aborted`), so a naive same-connection fallback would still
/// fail. The savepoint guard rolls back ONLY the probe, leaving the tx
/// usable: the ambiguous nil query succeeds via the text fallback, AND a
/// subsequent write + commit in the SAME tx lands durably.
#[test]
#[ignore = "requires an isolated database; make test-postgres-live"]
fn tx_describe_probe_failure_keeps_tx_alive_when_env_url_is_set() {
    live_postgres_url();
    reset_postgres_state();
    let source = r#"
import "std/postgres"

fn main(harness: Harness) {
const db = harness.postgres.pool("env:HARN_TEST_POSTGRES_URL", {max_connections: 1})
pg_execute(db, "DROP TABLE IF EXISTS harn_pg_tx_probe", [])
pg_execute(db, "CREATE TABLE harn_pg_tx_probe (id int PRIMARY KEY, note text)", [])

const probed = pg_transaction(db, { tx ->
  // Ambiguous nil query: the describe probe fails. The savepoint must roll
  // back only the probe, the bind falls back to a text NULL, and the result
  // ($1 IS NULL) is true.
  const r = pg_query_one(tx, "SELECT $1 IS NULL AS v", [nil])
  // The tx must still be USABLE after the failed probe: this write must work.
  pg_execute(tx, "INSERT INTO harn_pg_tx_probe (id, note) VALUES ($1, $2)", [1, "after-probe"])
  return to_string(r.v)
})
harness.stdio.println(probed)

// The commit must have persisted the post-probe write.
const row = pg_query_one(db, "SELECT note FROM harn_pg_tx_probe WHERE id = 1", [])
harness.stdio.println(row.note)
pg_execute(db, "DROP TABLE harn_pg_tx_probe", [])
pg_close(db)
}
"#;
    assert_eq!(run_harn_source(source).trim(), "true\nafter-probe");
}

/// Perf path: an all-non-null query still works and reuses the per-connection
/// SQL-keyed statement cache (no describe round-trip). We can't observe the
/// cache directly here, but running the identical SQL many times on a
/// single-connection pool exercises the cached prepared statement and must
/// stay correct. Also asserts a nil-containing run of the SAME SQL afterward
/// still works (the describe path repairs/uses the same cache entry).
#[tokio::test(flavor = "current_thread")]
#[ignore = "requires an isolated database; make test-postgres-live"]
async fn all_non_null_uses_cache_and_interops_with_nil_when_env_url_is_set() {
    let url = live_postgres_url();
    reset_postgres_state();
    let handle = open_single_conn_pool(&url).await;

    for n in 0..5_i64 {
        let rows = query_rows(
            &handle,
            "SELECT $1::bigint AS v",
            &[VmValue::Int(n)],
            QueryRouting::Primary,
        )
        .await
        .expect("all-non-null cached query");
        assert!(matches!(one_cell(rows, "v"), VmValue::Int(v) if v == n));
    }

    // Now a nil at the same SQL (describe path) — must coexist with the
    // already-cached all-non-null statement.
    let null_row = query_rows(
        &handle,
        "SELECT $1::bigint AS v",
        &[VmValue::Nil],
        QueryRouting::Primary,
    )
    .await
    .expect("nil after cached all-non-null runs");
    assert!(matches!(one_cell(null_row, "v"), VmValue::Nil));

    // And back to non-null once more — still fine.
    let again = query_rows(
        &handle,
        "SELECT $1::bigint AS v",
        &[VmValue::Int(123)],
        QueryRouting::Primary,
    )
    .await
    .expect("non-null again after nil");
    assert!(matches!(one_cell(again, "v"), VmValue::Int(123)));
}

/// Micro-benchmark proving the performant path: after warmup, a
/// nil-containing query (OID-cache hit + normal prepared-statement cache)
/// has p99 latency within 1.2x of the SAME query bound with no nil (the
/// plain fast path) on the same pool. Gated behind `HARN_PG_NIL_BENCH=1` (in
/// addition to `HARN_TEST_POSTGRES_URL`) so it does not run in normal CI.
#[tokio::test(flavor = "current_thread")]
#[ignore = "requires isolated database and HARN_PG_NIL_BENCH=1"]
async fn nil_path_p99_within_budget_of_plain_path_when_bench_enabled() {
    let url = live_postgres_url();
    assert_eq!(std::env::var("HARN_PG_NIL_BENCH").as_deref(), Ok("1"));
    reset_postgres_state();
    let handle = open_single_conn_pool(&url).await;

    // Representative shape: mixed nil + non-null typed params, the workload
    // the describe-then-bind path exists for.
    let sql = "SELECT COALESCE($1::bigint, $2::bigint) AS v";
    let nil_params = [VmValue::Nil, VmValue::Int(7)];
    let plain_params = [VmValue::Int(1), VmValue::Int(7)];

    async fn run_once(handle: &VmValue, sql: &str, params: &[VmValue]) -> std::time::Duration {
        let start = std::time::Instant::now();
        query_rows(handle, sql, params, QueryRouting::Primary)
            .await
            .expect("bench query");
        start.elapsed()
    }

    // Warmup: prime the OID cache (nil path) and the statement cache (both
    // paths) so we measure steady state, not the one-time describe.
    for _ in 0..50 {
        let _ = run_once(&handle, sql, &nil_params).await;
        let _ = run_once(&handle, sql, &plain_params).await;
    }

    const N: usize = 2000;
    let mut nil_us: Vec<u128> = Vec::with_capacity(N);
    let mut plain_us: Vec<u128> = Vec::with_capacity(N);
    // Interleave to share network/scheduler noise evenly between the two.
    for _ in 0..N {
        nil_us.push(run_once(&handle, sql, &nil_params).await.as_micros());
        plain_us.push(run_once(&handle, sql, &plain_params).await.as_micros());
    }
    nil_us.sort_unstable();
    plain_us.sort_unstable();

    let pct = |v: &[u128], p: f64| -> u128 {
        let idx = ((v.len() as f64 - 1.0) * p).round() as usize;
        v[idx]
    };
    let (nil_p50, nil_p95, nil_p99) = (pct(&nil_us, 0.50), pct(&nil_us, 0.95), pct(&nil_us, 0.99));
    let (plain_p50, plain_p95, plain_p99) = (
        pct(&plain_us, 0.50),
        pct(&plain_us, 0.95),
        pct(&plain_us, 0.99),
    );

    println!(
            "pg nil-bench (N={N}, us):\n  nil:   p50={nil_p50} p95={nil_p95} p99={nil_p99}\n  plain: p50={plain_p50} p95={plain_p95} p99={plain_p99}\n  ratio: p50={:.3} p95={:.3} p99={:.3}",
            nil_p50 as f64 / plain_p50.max(1) as f64,
            nil_p95 as f64 / plain_p95.max(1) as f64,
            nil_p99 as f64 / plain_p99.max(1) as f64,
        );

    // The describe must have happened at most once per distinct SQL — never
    // per query — which is the whole point of the cache.
    // Budget: nil-path p99 <= 1.2x plain-path p99, with a small absolute
    // floor (200us) so sub-ms scheduler/network jitter doesn't trip a ratio
    // assertion on near-zero baselines.
    let budget = ((plain_p99 as f64 * 1.2) as u128).max(plain_p99 + 200);
    assert!(
            nil_p99 <= budget,
            "nil-path p99 ({nil_p99}us) must be within budget ({budget}us) of plain-path p99 ({plain_p99}us)"
        );
}
