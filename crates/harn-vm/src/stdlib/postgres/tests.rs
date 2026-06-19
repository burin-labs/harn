use super::*;

use crate::{compile_source, register_vm_stdlib, Vm};

fn s(value: &str) -> VmValue {
    VmValue::String(arcstr::ArcStr::from(value))
}

fn dict(pairs: &[(&str, VmValue)]) -> VmValue {
    VmValue::dict(
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_string(), value.clone()))
            .collect::<crate::value::DictMap>(),
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
    })
}

#[test]
fn read_routing_policy_options_parse_named_modes() {
    let pool_options = crate::value::DictMap::from_iter([(
        "read_routing_policy".to_string(),
        s("round_robin_replica"),
    )]);
    assert_eq!(
        read_routing_policy_from_options(Some(&pool_options)).unwrap(),
        ReadRoutingPolicy::RoundRobinReplica
    );

    let query_options = crate::value::DictMap::from_iter([("route".to_string(), s("replica"))]);
    assert_eq!(
        routing_from_options(Some(&query_options)).unwrap(),
        QueryRouting::Policy(ReadRoutingPolicy::Replica)
    );

    let read_only_options =
        crate::value::DictMap::from_iter([("read_only".to_string(), VmValue::Bool(true))]);
    assert_eq!(
        routing_from_options(Some(&read_only_options)).unwrap(),
        QueryRouting::ReadOnly
    );

    let bad_options =
        crate::value::DictMap::from_iter([("routing_policy".to_string(), s("nearby"))]);
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
    let fixtures = VmValue::List(std::sync::Arc::new(vec![dict(&[
        ("sql", s("select * from claims where tenant_id = $1")),
        (
            "params",
            VmValue::List(std::sync::Arc::new(vec![s("tenant-a")])),
        ),
        (
            "rows",
            VmValue::List(std::sync::Arc::new(vec![dict(&[("claim_id", s("c1"))])])),
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
    let handle = handle_value(HANDLE_MOCK, &id, crate::value::DictMap::new());
    let rows = mock_query(
        &handle,
        "select * from claims where tenant_id = $1",
        &[s("tenant-a")],
        false,
    )
    .unwrap();
    assert_eq!(
        VmValue::List(std::sync::Arc::new(rows)).display(),
        "[{claim_id: c1}]"
    );
    let calls = MOCKS.with(|mocks| mocks.borrow().values().next().unwrap().calls.clone());
    assert_eq!(calls.len(), 1);
}

#[test]
fn mock_execute_returns_rows_affected() {
    reset_postgres_state();
    let fixtures = parse_mock_fixtures(&[dict(&[
        ("sql", s("update receipts set status = $1")),
        ("rows_affected", VmValue::Int(3)),
    ])])
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
    let handle = handle_value(HANDLE_MOCK, &id, crate::value::DictMap::new());
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
async fn postgres_round_trip_when_env_url_is_set() {
    let Ok(url) = std::env::var("HARN_TEST_POSTGRES_URL") else {
        return;
    };
    reset_postgres_state();
    let mut options = crate::value::DictMap::new();
    options.insert("max_connections".to_string(), VmValue::Int(1));
    options.insert(
        "application_name".to_string(),
        s("harn-postgres-stdlib-test"),
    );
    let ctx = crate::vm::AsyncBuiltinCtx::for_test(crate::Vm::new());
    let handle = open_pool(&ctx, &s(&url), Some(&options), false)
        .await
        .unwrap();
    assert_eq!(handle.as_dict().unwrap()["max_connections"].display(), "1");
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
    let mut options = crate::value::DictMap::new();
    options.insert("max_connections".to_string(), VmValue::Int(1));
    options.insert("application_name".to_string(), s("harn-postgres-bind-test"));
    let ctx = crate::vm::AsyncBuiltinCtx::for_test(crate::Vm::new());
    open_pool(&ctx, &s(url), Some(&options), false)
        .await
        .expect("open single-connection pool")
}

/// Resolve the underlying primary `PgPool` `Arc` behind a `pg_pool` handle so
/// tests can assert two handles point at the SAME (or distinct) pool via
/// `Arc::ptr_eq`.
fn pool_ptr(handle: &VmValue) -> Arc<PgPool> {
    let id = handle_id(Some(handle), HANDLE_POOL, "test").expect("pool handle id");
    pool_by_id(&id).expect("pool record")
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
    let key_a = shared::PoolKey::new("postgres://u:p@h/db_a", &[], None, false);
    let key_b = shared::PoolKey::new("postgres://u:p@h/db_b", &[], None, false);

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
    }
}

/// End-to-end against a live DB (gated on `HARN_TEST_POSTGRES_URL`): with the
/// shared registry installed, `open_pool` for the same source across two
/// distinct `Vm`s / simulated requests returns handles backed by the SAME
/// physical pool; a different database does not share.
#[tokio::test(flavor = "current_thread")]
async fn open_pool_shares_across_requests_when_registry_installed() {
    let Ok(url) = std::env::var("HARN_TEST_POSTGRES_URL") else {
        return;
    };
    shared::install_shared_pool_registry();
    shared::clear_for_test();
    reset_postgres_state();

    let ctx = crate::vm::AsyncBuiltinCtx::for_test(crate::Vm::new());
    let o = dict(&[("max_connections", VmValue::Int(2))]);
    let opt = o.as_dict();

    // Request 1.
    let h1 = open_pool(&ctx, &s(&url), opt, false).await.unwrap();
    // Request 2: fresh handle id, but must resolve to the same pool Arc.
    let h2 = open_pool(&ctx, &s(&url), opt, false).await.unwrap();
    assert_ne!(
        h1.as_dict().unwrap()["id"].display(),
        h2.as_dict().unwrap()["id"].display(),
        "each call still gets a distinct opaque handle id"
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

    shared::clear_for_test();
}

/// CLI default: when the shared registry is NOT installed, two `open_pool`
/// calls for the same source get DISTINCT pools (byte-identical to legacy
/// behavior). Gated on a live DB. NOTE: relies on per-test process isolation
/// (nextest) so no sibling test has installed the registry in this process.
#[tokio::test(flavor = "current_thread")]
async fn open_pool_does_not_share_when_registry_absent() {
    let Ok(url) = std::env::var("HARN_TEST_POSTGRES_URL") else {
        return;
    };
    if shared::is_installed() {
        // Another test installed it in this (cargo test) process; skip rather
        // than assert a false negative.
        return;
    }
    reset_postgres_state();
    let ctx = crate::vm::AsyncBuiltinCtx::for_test(crate::Vm::new());
    let o = dict(&[("max_connections", VmValue::Int(1))]);
    let h1 = open_pool(&ctx, &s(&url), o.as_dict(), false).await.unwrap();
    let h2 = open_pool(&ctx, &s(&url), o.as_dict(), false).await.unwrap();
    assert!(
        !Arc::ptr_eq(&pool_ptr(&h1), &pool_ptr(&h2)),
        "without the shared registry, each pg_pool opens its own pool"
    );
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
__io_println(tenant)

let rolled = try {
  pg_transaction(db, { tx ->
    pg_execute(tx, "insert into harn_pg_tx_test(value) values ($1)", [2])
    throw_error("force rollback")
  })
} catch (e) {
  "rolled back"
}
__io_println(rolled)
__io_println(pg_query_one(db, "select count(*)::int8 as count from harn_pg_tx_test", []).count)
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
                let chunk = compile_source(source).expect("compile postgres transaction source");
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

/// Drives `pg_savepoint` / `pg_rollback_to_savepoint` /
/// `pg_release_savepoint` against a real Postgres so we cover the
/// transaction-state-machine path the mocks can't exercise.
#[test]
fn savepoint_rollback_preserves_outer_writes_when_env_url_is_set() {
    if std::env::var("HARN_TEST_POSTGRES_URL").is_err() {
        return;
    }
    reset_postgres_state();
    let source = r#"
import "std/postgres"

let db = pg_pool("env:HARN_TEST_POSTGRES_URL", {max_connections: 1})
pg_execute(db, "DROP TABLE IF EXISTS harn_pg_sp_test", [])
pg_execute(db, "CREATE TABLE harn_pg_sp_test (id int PRIMARY KEY, label text NOT NULL)", [])

pg_transaction(db, { tx ->
  pg_execute(tx, "INSERT INTO harn_pg_sp_test (id, label) VALUES ($1, $2)", [1, "outer"])
  pg_savepoint(tx, "before_inner")
  pg_execute(tx, "INSERT INTO harn_pg_sp_test (id, label) VALUES ($1, $2)", [2, "inner"])
  pg_rollback_to_savepoint(tx, "before_inner")
  pg_release_savepoint(tx, "before_inner")
  pg_execute(tx, "INSERT INTO harn_pg_sp_test (id, label) VALUES ($1, $2)", [3, "after_release"])
  return 0
})

let rows = pg_query(db, "SELECT id, label FROM harn_pg_sp_test ORDER BY id", [])
for row in rows {
  __io_println(to_string(row.id) + ":" + row.label)
}
pg_execute(db, "DROP TABLE harn_pg_sp_test", [])
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
                let chunk = compile_source(source).expect("compile postgres savepoint source");
                let mut vm = Vm::new();
                register_vm_stdlib(&mut vm);
                vm.execute(&chunk)
                    .await
                    .expect("execute postgres savepoint source");
                assert_eq!(vm.output().trim(), "1:outer\n3:after_release");
            })
            .await;
    });
}

/// `pg_migrate` applies a synthetic two-file directory exactly once
/// then no-ops on a second run. `.down.sql` siblings must be ignored.
/// Requires `HARN_TEST_POSTGRES_URL`; runs against a unique scratch
/// schema so concurrent invocations don't conflict.
#[test]
fn migrate_applies_synthetic_dir_and_is_idempotent_when_env_url_is_set() {
    if std::env::var("HARN_TEST_POSTGRES_URL").is_err() {
        return;
    }
    reset_postgres_state();
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path();
    std::fs::write(
        dir.join("0001_create_widgets.sql"),
        "CREATE TABLE widgets (id INT PRIMARY KEY, label TEXT NOT NULL)",
    )
    .unwrap();
    std::fs::write(
        dir.join("0002_seed_widget.sql"),
        "INSERT INTO widgets (id, label) VALUES (1, 'alpha')",
    )
    .unwrap();
    // The runner must ignore .down.sql siblings even when their up
    // counterpart would otherwise share a prefix.
    std::fs::write(
        dir.join("0001_create_widgets.down.sql"),
        "DROP TABLE widgets",
    )
    .unwrap();

    let schema = format!("harn_pg_mig_{}", uuid::Uuid::new_v4().simple());
    let migration_dir = dir.to_string_lossy().into_owned();
    let source = format!(
        r#"
import "std/postgres"

let admin = pg_pool("env:HARN_TEST_POSTGRES_URL", {{max_connections: 1}})
pg_execute(admin, "DROP SCHEMA IF EXISTS \"{schema}\" CASCADE", [])
pg_execute(admin, "CREATE SCHEMA \"{schema}\"", [])
pg_close(admin)

let db = pg_pool("env:HARN_TEST_POSTGRES_URL", {{max_connections: 1}})
pg_execute(db, "SET search_path TO \"{schema}\"", [])

let first = pg_migrate(db, {{dir: "{migration_dir}"}})
__io_println(len(first.applied))
__io_println(len(first.skipped))

let second = pg_migrate(db, {{dir: "{migration_dir}"}})
__io_println(len(second.applied))
__io_println(len(second.skipped))

let count = pg_query_one(db, "SELECT count(*)::int8 AS c FROM widgets", [])
__io_println(count.c)

pg_execute(db, "DROP SCHEMA \"{schema}\" CASCADE", [])
pg_close(db)
"#,
    );
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let chunk = compile_source(&source).expect("compile migrate source");
                let mut vm = Vm::new();
                register_vm_stdlib(&mut vm);
                vm.execute(&chunk).await.expect("execute migrate source");
                let lines: Vec<&str> = vm.output().lines().collect();
                assert_eq!(lines, vec!["2", "0", "0", "2", "1"]);
            })
            .await;
    });
}

/// Harn-ledger drift detection (C-2): apply a migration, then edit the
/// file body on disk and re-run. The runner must re-hash the on-disk file,
/// see it no longer matches the recorded SHA-256, and error naming the
/// migration — never silently skip an edited (already-applied) file.
#[test]
fn migrate_harn_detects_checksum_drift_when_env_url_is_set() {
    if std::env::var("HARN_TEST_POSTGRES_URL").is_err() {
        return;
    }
    reset_postgres_state();
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path();
    let migration_path = dir.join("0001_create_widgets.sql");
    std::fs::write(
        &migration_path,
        "CREATE TABLE widgets (id INT PRIMARY KEY, label TEXT NOT NULL)",
    )
    .unwrap();

    let schema = format!("harn_pg_drift_{}", uuid::Uuid::new_v4().simple());
    let migration_dir = dir.to_string_lossy().into_owned();

    // First run: apply the migration cleanly into a fresh schema. The
    // schema persists in the shared DB for the second run below.
    let apply_source = format!(
        r#"
import "std/postgres"

let admin = pg_pool("env:HARN_TEST_POSTGRES_URL", {{max_connections: 1}})
pg_execute(admin, "DROP SCHEMA IF EXISTS \"{schema}\" CASCADE", [])
pg_execute(admin, "CREATE SCHEMA \"{schema}\"", [])
pg_close(admin)

let db = pg_pool("env:HARN_TEST_POSTGRES_URL", {{max_connections: 1}})
pg_execute(db, "SET search_path TO \"{schema}\"", [])
let first = pg_migrate(db, {{dir: "{migration_dir}"}})
__io_println(len(first.applied))
pg_close(db)
"#,
    );
    let out = run_harn_source(&apply_source);
    assert_eq!(out.trim(), "1", "first run should apply exactly one file");

    // Edit the migration body on disk *after* it was recorded. A clean
    // re-run would normally skip an already-applied file; here the changed
    // body must trip the checksum check.
    std::fs::write(
        &migration_path,
        "CREATE TABLE widgets (id INT PRIMARY KEY, label TEXT NOT NULL, extra INT)",
    )
    .unwrap();

    let rerun_source = format!(
        r#"
import "std/postgres"

let db = pg_pool("env:HARN_TEST_POSTGRES_URL", {{max_connections: 1}})
pg_execute(db, "SET search_path TO \"{schema}\"", [])
let second = pg_migrate(db, {{dir: "{migration_dir}"}})
__io_println(len(second.applied))
pg_close(db)
"#,
    );
    let err = run_harn_source_expect_err(&rerun_source);
    assert!(
        err.contains("checksum mismatch") && err.contains("0001_create_widgets.sql"),
        "expected harn checksum-mismatch error naming the migration, got: {err}"
    );

    // Clean up the scratch schema.
    let cleanup = format!(
        r#"
import "std/postgres"
let admin = pg_pool("env:HARN_TEST_POSTGRES_URL", {{max_connections: 1}})
pg_execute(admin, "DROP SCHEMA IF EXISTS \"{schema}\" CASCADE", [])
pg_close(admin)
"#,
    );
    run_harn_source(&cleanup);
}

/// `pg_migrate` against the canonical `harn-cloud-store/migrations/`
/// directory. Opt-in via `HARN_TEST_CLOUD_MIGRATIONS_DIR`; verifies
/// that the runner consumes the full ledger without errors.
#[test]
fn migrate_loads_harn_cloud_store_migrations_when_env_set() {
    if std::env::var("HARN_TEST_POSTGRES_URL").is_err() {
        return;
    }
    let Ok(dir) = std::env::var("HARN_TEST_CLOUD_MIGRATIONS_DIR") else {
        return;
    };
    if !std::path::Path::new(&dir).exists() {
        return;
    }
    reset_postgres_state();
    let schema = format!("harn_pg_cloud_{}", uuid::Uuid::new_v4().simple());
    let source = format!(
        r#"
import "std/postgres"

let admin = pg_pool("env:HARN_TEST_POSTGRES_URL", {{max_connections: 1}})
pg_execute(admin, "DROP SCHEMA IF EXISTS \"{schema}\" CASCADE", [])
pg_execute(admin, "CREATE SCHEMA \"{schema}\"", [])
pg_close(admin)

let db = pg_pool("env:HARN_TEST_POSTGRES_URL", {{max_connections: 1}})
pg_execute(db, "SET search_path TO \"{schema}\"", [])

let result = pg_migrate(db, {{dir: "{dir}"}})
__io_println(len(result.applied))
__io_println(len(result.skipped))

let tables = pg_query(
  db,
  "SELECT table_name FROM information_schema.tables WHERE table_schema = $1",
  ["{schema}"],
)
__io_println(len(tables))

pg_execute(db, "DROP SCHEMA \"{schema}\" CASCADE", [])
pg_close(db)
"#,
    );
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let chunk = compile_source(&source).expect("compile cloud-migrate source");
                let mut vm = Vm::new();
                register_vm_stdlib(&mut vm);
                vm.execute(&chunk)
                    .await
                    .expect("execute cloud-migrate source");
                let lines: Vec<&str> = vm.output().lines().collect();
                assert_eq!(lines.len(), 3, "unexpected output: {}", vm.output());
                let applied: usize = lines[0].parse().expect("applied count");
                let tables: usize = lines[2].parse().expect("table count");
                assert!(applied > 0, "no migrations applied: {}", vm.output());
                assert!(
                    tables >= applied,
                    "fewer tables than migrations applied: tables={tables}, applied={applied}",
                );
            })
            .await;
    });
}

/// M-4 (live): `pg_transaction(settings)` rejects a privileged GUC
/// (`role`) and a nil value (M-3), and accepts the legitimate
/// `app.current_tenant_id` / `app.bypass_rls` / timeout settings. This is
/// the RLS-escape guard exercised end-to-end through the VM.
#[test]
fn transaction_settings_reject_privileged_gucs_when_env_url_is_set() {
    if std::env::var("HARN_TEST_POSTGRES_URL").is_err() {
        return;
    }
    reset_postgres_state();

    // `role` is rejected before any SQL runs.
    let reject_role = r#"
import "std/postgres"
let db = pg_pool("env:HARN_TEST_POSTGRES_URL", {max_connections: 1})
let r = pg_transaction(db, { tx -> return 1 }, {settings: {"role": "postgres"}})
pg_close(db)
"#;
    let err = run_harn_source_expect_err(reject_role);
    assert!(
        err.contains("not permitted") && err.contains("role"),
        "expected `role` to be rejected, got: {err}"
    );

    // A nil value is rejected (M-3) rather than set as the text "nil".
    reset_postgres_state();
    let reject_nil = r#"
import "std/postgres"
let db = pg_pool("env:HARN_TEST_POSTGRES_URL", {max_connections: 1})
let r = pg_transaction(db, { tx -> return 1 }, {settings: {"app.current_tenant_id": nil}})
pg_close(db)
"#;
    let err = run_harn_source_expect_err(reject_nil);
    assert!(
        err.contains("nil value"),
        "expected nil setting to be rejected, got: {err}"
    );

    // The legitimate settings pass and take effect inside the transaction.
    reset_postgres_state();
    let allow_legit = r#"
import "std/postgres"
let db = pg_pool("env:HARN_TEST_POSTGRES_URL", {max_connections: 1})
let tenant = pg_transaction(db, { tx ->
  return pg_query_one(tx, "SELECT current_setting('app.current_tenant_id', true) AS t", []).t
}, {settings: {"app.current_tenant_id": "tenant-xyz", "app.bypass_rls": "on", "statement_timeout": "5000"}})
__io_println(tenant)
pg_close(db)
"#;
    let out = run_harn_source(allow_legit);
    assert_eq!(out.trim(), "tenant-xyz", "legit settings must apply: {out}");
}

/// M-2 (live): a unique-constraint violation surfaces a *stable category*
/// (`unique_violation` + SQLSTATE 23505), never the raw constraint name.
#[test]
fn constraint_violation_surfaces_stable_category_when_env_url_is_set() {
    if std::env::var("HARN_TEST_POSTGRES_URL").is_err() {
        return;
    }
    reset_postgres_state();
    let schema = format!("harn_pg_m2_{}", uuid::Uuid::new_v4().simple());
    let source = format!(
        r#"
import "std/postgres"
let db = pg_pool("env:HARN_TEST_POSTGRES_URL", {{max_connections: 1}})
pg_execute(db, "DROP SCHEMA IF EXISTS \"{schema}\" CASCADE", [])
pg_execute(db, "CREATE SCHEMA \"{schema}\"", [])
pg_execute(db, "SET search_path TO \"{schema}\"", [])
pg_execute(db, "CREATE TABLE accounts (id int4 PRIMARY KEY, email text UNIQUE)", [])
pg_execute(db, "INSERT INTO accounts (id, email) VALUES (1, 'a@b.com')", [])
pg_execute(db, "INSERT INTO accounts (id, email) VALUES ($1, $2)", [2, "a@b.com"])
pg_close(db)
"#,
    );
    let err = run_harn_source_expect_err(&source);
    assert!(
        err.contains("unique_violation") && err.contains("23505"),
        "expected stable unique_violation category, got: {err}"
    );
    // The raw constraint name must NOT leak to the caller.
    assert!(
        !err.contains("accounts_email_key"),
        "raw constraint name leaked to caller: {err}"
    );

    // Clean up.
    let cleanup = format!(
        r#"
import "std/postgres"
let db = pg_pool("env:HARN_TEST_POSTGRES_URL", {{max_connections: 1}})
pg_execute(db, "DROP SCHEMA IF EXISTS \"{schema}\" CASCADE", [])
pg_close(db)
"#,
    );
    run_harn_source(&cleanup);
}

/// H-2 (live): an in-range `Int` binds and round-trips correctly through an
/// `int4` column, and an out-of-range value surfaces a clear, stable
/// `numeric_out_of_range` (SQLSTATE 22003) diagnostic rather than a raw or
/// confusing message.
#[test]
fn int_bind_into_int4_column_when_env_url_is_set() {
    if std::env::var("HARN_TEST_POSTGRES_URL").is_err() {
        return;
    }
    reset_postgres_state();
    let schema = format!("harn_pg_h2_{}", uuid::Uuid::new_v4().simple());
    let ok_source = format!(
        r#"
import "std/postgres"
let db = pg_pool("env:HARN_TEST_POSTGRES_URL", {{max_connections: 1}})
pg_execute(db, "DROP SCHEMA IF EXISTS \"{schema}\" CASCADE", [])
pg_execute(db, "CREATE SCHEMA \"{schema}\"", [])
pg_execute(db, "SET search_path TO \"{schema}\"", [])
pg_execute(db, "CREATE TABLE narrow (a int4, b int2)", [])
pg_execute(db, "INSERT INTO narrow (a, b) VALUES ($1, $2)", [2000000000, 30000])
let row = pg_query_one(db, "SELECT a, b FROM narrow WHERE a = $1", [2000000000])
__io_println(row.a)
__io_println(row.b)
pg_close(db)
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
let db = pg_pool("env:HARN_TEST_POSTGRES_URL", {{max_connections: 1}})
pg_execute(db, "SET search_path TO \"{schema}\"", [])
pg_execute(db, "INSERT INTO narrow (a) VALUES ($1)", [5000000000])
pg_close(db)
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
let db = pg_pool("env:HARN_TEST_POSTGRES_URL", {{max_connections: 1}})
pg_execute(db, "DROP SCHEMA IF EXISTS \"{schema}\" CASCADE", [])
pg_close(db)
"#,
    );
    run_harn_source(&cleanup);
}

/// M-5 (live): after `pg_migrate` runs DDL, a query whose result type the
/// DDL changed must NOT fail with `cached plan must not change result type`
/// (SQLSTATE 0A000) on a pooled connection. We warm a plan, migrate an
/// `ALTER TABLE` that changes the column type, then re-query on the same
/// pool — it must succeed because the migrate recycled the statement caches.
#[test]
fn migrate_recycles_statement_cache_after_ddl_when_env_url_is_set() {
    if std::env::var("HARN_TEST_POSTGRES_URL").is_err() {
        return;
    }
    reset_postgres_state();
    let schema = format!("harn_pg_m5_{}", uuid::Uuid::new_v4().simple());

    // Migration 1 creates the table with a text column.
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path();
    std::fs::write(dir.join("0001_init.sql"), "CREATE TABLE plan_t (v text)").unwrap();
    std::fs::write(
        dir.join("0002_seed.sql"),
        "INSERT INTO plan_t (v) VALUES ('x')",
    )
    .unwrap();
    let dir1 = dir.to_string_lossy().into_owned();

    // Migration 2 (added later) changes the column type, invalidating any
    // cached plan that selected it as text.
    let tmp2 = tempfile::tempdir().expect("tempdir2");
    let dir2p = tmp2.path();
    std::fs::write(dir2p.join("0001_init.sql"), "CREATE TABLE plan_t (v text)").unwrap();
    std::fs::write(
        dir2p.join("0002_seed.sql"),
        "INSERT INTO plan_t (v) VALUES ('x')",
    )
    .unwrap();
    std::fs::write(
        dir2p.join("0003_retype.sql"),
        "ALTER TABLE plan_t ALTER COLUMN v TYPE int4 USING 1",
    )
    .unwrap();
    let dir2 = dir2p.to_string_lossy().into_owned();

    let source = format!(
        r#"
import "std/postgres"
let admin = pg_pool("env:HARN_TEST_POSTGRES_URL", {{max_connections: 1}})
pg_execute(admin, "DROP SCHEMA IF EXISTS \"{schema}\" CASCADE", [])
pg_execute(admin, "CREATE SCHEMA \"{schema}\"", [])
pg_close(admin)

// A single-connection pool: the warmed connection, the migrate connection,
// and the post-migrate query all share ONE backend, so a stale cached plan
// would deterministically reproduce 0A000 unless the migrate recycled it.
// (max_connections: 1 also keeps the `SET search_path` session setting on the
// same connection migrate/queries reuse.)
let db = pg_pool("env:HARN_TEST_POSTGRES_URL", {{max_connections: 1}})
pg_execute(db, "SET search_path TO \"{schema}\"", [])
pg_migrate(db, {{dir: "{dir1}"}})
// Warm + cache a plan that selects v as text on the pooled connection.
let warm = pg_query_one(db, "SELECT v FROM plan_t LIMIT 1", [])
__io_println(warm.v)
// Apply the retype DDL through pg_migrate (which recycles caches).
pg_migrate(db, {{dir: "{dir2}"}})
// This reuse would hit 0A000 if the cache were not recycled.
let after = pg_query_one(db, "SELECT v FROM plan_t LIMIT 1", [])
__io_println(after.v)
pg_execute(db, "DROP SCHEMA \"{schema}\" CASCADE", [])
pg_close(db)
"#,
    );
    let out = run_harn_source(&source);
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 2, "unexpected output: {out}");
    assert_eq!(lines[0], "x", "warmed select should read text: {out}");
    assert_eq!(
        lines[1], "1",
        "post-DDL select must succeed (int4) not 0A000: {out}"
    );
}

/// C-1 (live): two concurrent `pg_migrate` calls against the same database
/// serialize on the advisory lock — they do not interleave, exactly one
/// applies each migration, and the lock is released afterward (a third run
/// can immediately acquire it and is a clean no-op).
#[test]
fn concurrent_migrate_serializes_on_advisory_lock_when_env_url_is_set() {
    if std::env::var("HARN_TEST_POSTGRES_URL").is_err() {
        return;
    }
    reset_postgres_state();
    let schema = format!("harn_pg_c1_{}", uuid::Uuid::new_v4().simple());

    // A slow migration: pg_sleep inside the migration body widens the window
    // in which the lock is held, so an unserialized second caller would
    // observe a half-applied state / double-apply.
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path();
    std::fs::write(
        dir.join("0001_slow.sql"),
        "SELECT pg_sleep(0.5); CREATE TABLE c1_widgets (id int4 PRIMARY KEY)",
    )
    .unwrap();
    let migration_dir = dir.to_string_lossy().into_owned();

    // Set the search_path inside each migrate task so both target the same
    // scratch schema. Both tasks run on one current-thread runtime via a
    // LocalSet; `tokio::join!` drives them concurrently.
    let setup = format!(
        r#"
import "std/postgres"
let admin = pg_pool("env:HARN_TEST_POSTGRES_URL", {{max_connections: 1}})
pg_execute(admin, "DROP SCHEMA IF EXISTS \"{schema}\" CASCADE", [])
pg_execute(admin, "CREATE SCHEMA \"{schema}\"", [])
pg_close(admin)
"#,
    );
    run_harn_source(&setup);

    let migrate_src = |label: &str| {
        format!(
            r#"
import "std/postgres"
let db = pg_pool("env:HARN_TEST_POSTGRES_URL", {{max_connections: 1}})
pg_execute(db, "SET search_path TO \"{schema}\"", [])
let r = pg_migrate(db, {{dir: "{migration_dir}"}})
__io_println("{label}:" + to_string(len(r.applied)))
pg_close(db)
"#,
        )
    };
    let src_a = migrate_src("a");
    let src_b = migrate_src("b");

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let (out_a, out_b) = rt.block_on(async {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let run_one = |src: String| async move {
                    let chunk = compile_source(&src).expect("compile migrate src");
                    let mut vm = Vm::new();
                    register_vm_stdlib(&mut vm);
                    vm.execute(&chunk).await.expect("execute migrate src");
                    vm.output().trim().to_string()
                };
                tokio::join!(run_one(src_a), run_one(src_b))
            })
            .await
    });

    // Exactly one caller applied the migration; the other saw it already
    // applied (0). If the lock did not serialize, both would race the
    // CREATE TABLE and one would error (duplicate table) — instead the
    // loser cleanly skips.
    let applied: Vec<i64> = [out_a.as_str(), out_b.as_str()]
        .iter()
        .map(|line| {
            line.split(':')
                .nth(1)
                .and_then(|n| n.trim().parse::<i64>().ok())
                .unwrap_or_else(|| panic!("unexpected migrate output: {line:?}"))
        })
        .collect();
    let mut sorted = applied.clone();
    sorted.sort_unstable();
    assert_eq!(
        sorted,
        vec![0, 1],
        "concurrent migrate must serialize: one applies (1), one skips (0); got {applied:?}"
    );

    // The lock was released: a third run acquires it immediately and is a
    // clean no-op.
    let third = run_harn_source(&migrate_src("c"));
    assert_eq!(
        third.trim(),
        "c:0",
        "third run after release must be a clean no-op: {third}"
    );

    let cleanup = format!(
        r#"
import "std/postgres"
let db = pg_pool("env:HARN_TEST_POSTGRES_URL", {{max_connections: 1}})
pg_execute(db, "DROP SCHEMA IF EXISTS \"{schema}\" CASCADE", [])
pg_close(db)
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

fn run_harn_source(source: &str) -> String {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let chunk = compile_source(source).expect("compile source");
                let mut vm = Vm::new();
                register_vm_stdlib(&mut vm);
                vm.execute(&chunk).await.expect("execute source");
                vm.output().to_string()
            })
            .await
    })
}

fn run_harn_source_expect_err(source: &str) -> String {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let chunk = compile_source(source).expect("compile source");
                let mut vm = Vm::new();
                register_vm_stdlib(&mut vm);
                let err = vm
                    .execute(&chunk)
                    .await
                    .expect_err("expected source to error");
                format!("{err:?}")
            })
            .await
    })
}

/// SQLx ledger mode applies all forward files into `_sqlx_migrations`
/// with the exact 6-column schema, 48-byte SHA-384 checksums, and
/// `success = true`.
#[test]
fn migrate_sqlx_applies_into_sqlx_migrations_table_when_env_url_is_set() {
    if std::env::var("HARN_TEST_POSTGRES_URL").is_err() {
        return;
    }
    reset_postgres_state();
    let (_tmp, dir) = sqlx_synthetic_migrations();
    let schema = format!("harn_pg_sqlx_{}", uuid::Uuid::new_v4().simple());
    let source = format!(
        r#"
import "std/postgres"

let admin = pg_pool("env:HARN_TEST_POSTGRES_URL", {{max_connections: 1}})
pg_execute(admin, "DROP SCHEMA IF EXISTS \"{schema}\" CASCADE", [])
pg_execute(admin, "CREATE SCHEMA \"{schema}\"", [])
pg_close(admin)

let db = pg_pool("env:HARN_TEST_POSTGRES_URL", {{max_connections: 1}})
pg_execute(db, "SET search_path TO \"{schema}\"", [])

let result = pg_migrate(db, {{dir: "{dir}", ledger: "sqlx"}})
__io_println(len(result.applied))
__io_println(len(result.available))
__io_println(result.table)

let cols = pg_query(db, "SELECT column_name FROM information_schema.columns WHERE table_schema=$1 AND table_name='_sqlx_migrations' ORDER BY column_name", ["{schema}"])
__io_println(len(cols))

let rows = pg_query_one(db, "SELECT count(*)::int8 AS c FROM _sqlx_migrations", [])
__io_println(rows.c)

let badlen = pg_query_one(db, "SELECT count(*)::int8 AS c FROM _sqlx_migrations WHERE octet_length(checksum) <> 48", [])
__io_println(badlen.c)

let failed = pg_query_one(db, "SELECT count(*)::int8 AS c FROM _sqlx_migrations WHERE success = false", [])
__io_println(failed.c)

let versions = pg_query(db, "SELECT version FROM _sqlx_migrations ORDER BY version", [])
__io_println(len(versions))

pg_execute(db, "DROP SCHEMA \"{schema}\" CASCADE", [])
pg_close(db)
"#,
    );
    let out = run_harn_source(&source);
    let lines: Vec<&str> = out.lines().collect();
    // applied=3, available=3, table name, 6 columns, 3 rows, 0 bad
    // checksum lengths, 0 failed, 3 versions.
    assert_eq!(
        lines,
        vec!["3", "3", "_sqlx_migrations", "6", "3", "0", "0", "3"],
        "unexpected output: {out}"
    );
}

/// SQLx ledger mode is idempotent: a second run applies 0, skips all,
/// and leaves the row count unchanged.
#[test]
fn migrate_sqlx_is_idempotent_when_env_url_is_set() {
    if std::env::var("HARN_TEST_POSTGRES_URL").is_err() {
        return;
    }
    reset_postgres_state();
    let (_tmp, dir) = sqlx_synthetic_migrations();
    let schema = format!("harn_pg_sqlxidem_{}", uuid::Uuid::new_v4().simple());
    let source = format!(
        r#"
import "std/postgres"

let admin = pg_pool("env:HARN_TEST_POSTGRES_URL", {{max_connections: 1}})
pg_execute(admin, "DROP SCHEMA IF EXISTS \"{schema}\" CASCADE", [])
pg_execute(admin, "CREATE SCHEMA \"{schema}\"", [])
pg_close(admin)

let db = pg_pool("env:HARN_TEST_POSTGRES_URL", {{max_connections: 1}})
pg_execute(db, "SET search_path TO \"{schema}\"", [])

let first = pg_migrate(db, {{dir: "{dir}", ledger: "sqlx"}})
__io_println(len(first.applied))
__io_println(len(first.skipped))

let count1 = pg_query_one(db, "SELECT count(*)::int8 AS c FROM _sqlx_migrations", [])
__io_println(count1.c)

let second = pg_migrate(db, {{dir: "{dir}", ledger: "sqlx"}})
__io_println(len(second.applied))
__io_println(len(second.skipped))

let count2 = pg_query_one(db, "SELECT count(*)::int8 AS c FROM _sqlx_migrations", [])
__io_println(count2.c)

pg_execute(db, "DROP SCHEMA \"{schema}\" CASCADE", [])
pg_close(db)
"#,
    );
    let out = run_harn_source(&source);
    let lines: Vec<&str> = out.lines().collect();
    // first: applied 3 skipped 0, count 3; second: applied 0 skipped 3,
    // count still 3.
    assert_eq!(
        lines,
        vec!["3", "0", "3", "0", "3", "3"],
        "unexpected output: {out}"
    );
}

/// No-fork against a "real" SQLx ledger: pre-seed `_sqlx_migrations`
/// with rows whose checksums are computed the SAME way SQLx does
/// (SHA-384 of the file body) — exactly what `sqlx migrate run` would
/// have written — then run `pg_migrate(ledger: "sqlx")`. It must apply
/// 0 and the checksums stay byte-identical.
#[test]
fn migrate_sqlx_no_fork_against_preseeded_ledger_when_env_url_is_set() {
    if std::env::var("HARN_TEST_POSTGRES_URL").is_err() {
        return;
    }
    reset_postgres_state();
    let (_tmp, dir) = sqlx_synthetic_migrations();

    // Compute SHA-384 the same way sqlx (and our runner) does: over the
    // file body read as a string.
    let checksum_hex = |name: &str| -> String {
        use sha2::{Digest, Sha384};
        let body =
            std::fs::read_to_string(std::path::Path::new(&dir).join(name)).expect("read file");
        let digest = Sha384::digest(body.as_bytes());
        digest.iter().map(|b| format!("{b:02x}")).collect()
    };
    let bootstrap_sum = checksum_hex("20260419170000_bootstrap.up.sql");
    let seed_sum = checksum_hex("20260423100000_seed_widget.up.sql");
    let gadgets_sum = checksum_hex("20260424000000_add_gadgets.up.sql");

    let schema = format!("harn_pg_sqlxnofork_{}", uuid::Uuid::new_v4().simple());
    let source = format!(
        r#"
import "std/postgres"

let admin = pg_pool("env:HARN_TEST_POSTGRES_URL", {{max_connections: 1}})
pg_execute(admin, "DROP SCHEMA IF EXISTS \"{schema}\" CASCADE", [])
pg_execute(admin, "CREATE SCHEMA \"{schema}\"", [])
pg_close(admin)

let db = pg_pool("env:HARN_TEST_POSTGRES_URL", {{max_connections: 1}})
pg_execute(db, "SET search_path TO \"{schema}\"", [])

// Replicate exactly what `sqlx migrate run` would have written, including
// the schema and the three rows with SHA-384 checksums, then create the
// objects those migrations would have created.
pg_execute(db, "CREATE TABLE _sqlx_migrations (version BIGINT PRIMARY KEY, description TEXT NOT NULL, installed_on TIMESTAMPTZ NOT NULL DEFAULT now(), success BOOLEAN NOT NULL, checksum BYTEA NOT NULL, execution_time BIGINT NOT NULL)", [])
pg_execute(db, "CREATE TABLE widgets (id INT PRIMARY KEY, label TEXT NOT NULL)", [])
pg_execute(db, "INSERT INTO widgets (id, label) VALUES (1, 'alpha')", [])
pg_execute(db, "CREATE TABLE gadgets (id INT PRIMARY KEY)", [])
pg_execute(db, "INSERT INTO _sqlx_migrations (version, description, success, checksum, execution_time) VALUES (20260419170000, 'bootstrap', TRUE, decode('{bootstrap_sum}', 'hex'), 1)", [])
pg_execute(db, "INSERT INTO _sqlx_migrations (version, description, success, checksum, execution_time) VALUES (20260423100000, 'seed widget', TRUE, decode('{seed_sum}', 'hex'), 1)", [])
pg_execute(db, "INSERT INTO _sqlx_migrations (version, description, success, checksum, execution_time) VALUES (20260424000000, 'add gadgets', TRUE, decode('{gadgets_sum}', 'hex'), 1)", [])

let before = pg_query_one(db, "SELECT md5(string_agg(encode(checksum,'hex'), ',' ORDER BY version)) AS h FROM _sqlx_migrations", [])

let result = pg_migrate(db, {{dir: "{dir}", ledger: "sqlx"}})
__io_println(len(result.applied))
__io_println(len(result.skipped))

let after = pg_query_one(db, "SELECT md5(string_agg(encode(checksum,'hex'), ',' ORDER BY version)) AS h FROM _sqlx_migrations", [])
if before.h == after.h {{ __io_println("checksums-identical") }} else {{ __io_println("checksums-CHANGED") }}

let count = pg_query_one(db, "SELECT count(*)::int8 AS c FROM _sqlx_migrations", [])
__io_println(count.c)

pg_execute(db, "DROP SCHEMA \"{schema}\" CASCADE", [])
pg_close(db)
"#,
    );
    let out = run_harn_source(&source);
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(
        lines,
        vec!["0", "3", "checksums-identical", "3"],
        "unexpected output: {out}"
    );
}

/// Checksum-mismatch detection: corrupt one recorded checksum then run;
/// the runner must error and name the offending version.
#[test]
fn migrate_sqlx_detects_checksum_mismatch_when_env_url_is_set() {
    if std::env::var("HARN_TEST_POSTGRES_URL").is_err() {
        return;
    }
    reset_postgres_state();
    let (_tmp, dir) = sqlx_synthetic_migrations();
    let schema = format!("harn_pg_sqlxmismatch_{}", uuid::Uuid::new_v4().simple());
    let source = format!(
        r#"
import "std/postgres"

let admin = pg_pool("env:HARN_TEST_POSTGRES_URL", {{max_connections: 1}})
pg_execute(admin, "DROP SCHEMA IF EXISTS \"{schema}\" CASCADE", [])
pg_execute(admin, "CREATE SCHEMA \"{schema}\"", [])
pg_close(admin)

let db = pg_pool("env:HARN_TEST_POSTGRES_URL", {{max_connections: 1}})
pg_execute(db, "SET search_path TO \"{schema}\"", [])

let first = pg_migrate(db, {{dir: "{dir}", ledger: "sqlx"}})
__io_println(len(first.applied))

// Corrupt the recorded checksum for the first migration.
pg_execute(db, "UPDATE _sqlx_migrations SET checksum = decode('deadbeef', 'hex') WHERE version = 20260419170000", [])

let second = pg_migrate(db, {{dir: "{dir}", ledger: "sqlx"}})
__io_println(len(second.applied))

pg_execute(db, "DROP SCHEMA \"{schema}\" CASCADE", [])
pg_close(db)
"#,
    );
    let err = run_harn_source_expect_err(&source);
    assert!(
        err.contains("checksum mismatch") && err.contains("20260419170000"),
        "expected checksum-mismatch error naming the version, got: {err}"
    );
}

/// Dirty-ledger detection: a `success = false` row blocks the run.
#[test]
fn migrate_sqlx_detects_dirty_ledger_when_env_url_is_set() {
    if std::env::var("HARN_TEST_POSTGRES_URL").is_err() {
        return;
    }
    reset_postgres_state();
    let (_tmp, dir) = sqlx_synthetic_migrations();
    let schema = format!("harn_pg_sqlxdirty_{}", uuid::Uuid::new_v4().simple());
    let source = format!(
        r#"
import "std/postgres"

let admin = pg_pool("env:HARN_TEST_POSTGRES_URL", {{max_connections: 1}})
pg_execute(admin, "DROP SCHEMA IF EXISTS \"{schema}\" CASCADE", [])
pg_execute(admin, "CREATE SCHEMA \"{schema}\"", [])
pg_close(admin)

let db = pg_pool("env:HARN_TEST_POSTGRES_URL", {{max_connections: 1}})
pg_execute(db, "SET search_path TO \"{schema}\"", [])

pg_execute(db, "CREATE TABLE _sqlx_migrations (version BIGINT PRIMARY KEY, description TEXT NOT NULL, installed_on TIMESTAMPTZ NOT NULL DEFAULT now(), success BOOLEAN NOT NULL, checksum BYTEA NOT NULL, execution_time BIGINT NOT NULL)", [])
pg_execute(db, "INSERT INTO _sqlx_migrations (version, description, success, checksum, execution_time) VALUES (20260419170000, 'bootstrap', FALSE, decode('deadbeef', 'hex'), -1)", [])

let result = pg_migrate(db, {{dir: "{dir}", ledger: "sqlx"}})
__io_println(len(result.applied))

pg_execute(db, "DROP SCHEMA \"{schema}\" CASCADE", [])
pg_close(db)
"#,
    );
    let err = run_harn_source_expect_err(&source);
    assert!(
        err.contains("dirty migration") && err.contains("20260419170000"),
        "expected dirty-ledger error naming the version, got: {err}"
    );
}

/// SQLx ledger mode against the canonical harn-cloud `migrations/`
/// directory: applies the full forward history, then a second run is a
/// no-op (every version skipped, no checksum drift). This is the
/// retire-`migrations.rs` acceptance test. Opt-in via
/// `HARN_TEST_CLOUD_MIGRATIONS_DIR`.
#[test]
fn migrate_sqlx_applies_real_cloud_dir_and_is_idempotent_when_env_set() {
    if std::env::var("HARN_TEST_POSTGRES_URL").is_err() {
        return;
    }
    let Ok(dir) = std::env::var("HARN_TEST_CLOUD_MIGRATIONS_DIR") else {
        return;
    };
    if !std::path::Path::new(&dir).exists() {
        return;
    }
    reset_postgres_state();
    let schema = format!("harn_pg_sqlxcloud_{}", uuid::Uuid::new_v4().simple());
    let source = format!(
        r#"
import "std/postgres"

let admin = pg_pool("env:HARN_TEST_POSTGRES_URL", {{max_connections: 1}})
pg_execute(admin, "DROP SCHEMA IF EXISTS \"{schema}\" CASCADE", [])
pg_execute(admin, "CREATE SCHEMA \"{schema}\"", [])
pg_close(admin)

let db = pg_pool("env:HARN_TEST_POSTGRES_URL", {{max_connections: 1}})
pg_execute(db, "SET search_path TO \"{schema}\"", [])

let first = pg_migrate(db, {{dir: "{dir}", ledger: "sqlx"}})
__io_println(len(first.applied))
__io_println(len(first.skipped))

let count1 = pg_query_one(db, "SELECT count(*)::int8 AS c FROM _sqlx_migrations", [])
__io_println(count1.c)

let badlen = pg_query_one(db, "SELECT count(*)::int8 AS c FROM _sqlx_migrations WHERE octet_length(checksum) <> 48", [])
__io_println(badlen.c)

let second = pg_migrate(db, {{dir: "{dir}", ledger: "sqlx"}})
__io_println(len(second.applied))
__io_println(len(second.skipped))

pg_execute(db, "DROP SCHEMA \"{schema}\" CASCADE", [])
pg_close(db)
"#,
    );
    let out = run_harn_source(&source);
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 6, "unexpected output: {out}");
    let applied: usize = lines[0].parse().expect("applied count");
    let skipped_first: usize = lines[1].parse().expect("skipped count");
    let count: usize = lines[2].parse().expect("row count");
    let bad_checksums: usize = lines[3].parse().expect("bad checksum count");
    let applied_second: usize = lines[4].parse().expect("second applied");
    let skipped_second: usize = lines[5].parse().expect("second skipped");
    assert!(applied > 0, "no migrations applied: {out}");
    assert_eq!(skipped_first, 0, "first run should skip nothing: {out}");
    assert_eq!(count, applied, "ledger rows != applied: {out}");
    assert_eq!(bad_checksums, 0, "all checksums must be 48 bytes: {out}");
    assert_eq!(applied_second, 0, "second run must apply nothing: {out}");
    assert_eq!(
        skipped_second, applied,
        "second run must skip everything: {out}"
    );
}

/// Confirms `duration_ms` lives on every real execute result. The
/// synthetic `Instant` path is covered by
/// `execute_result_value_includes_duration`.
#[test]
fn execute_reports_duration_ms_on_real_pool_when_env_url_is_set() {
    if std::env::var("HARN_TEST_POSTGRES_URL").is_err() {
        return;
    }
    reset_postgres_state();
    let source = r#"
import "std/postgres"

let db = pg_pool("env:HARN_TEST_POSTGRES_URL", {max_connections: 1})
let result = pg_execute(db, "SELECT pg_sleep(0.05)", [])
__io_println(result.duration_ms)
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
                let chunk = compile_source(source).expect("compile duration source");
                let mut vm = Vm::new();
                register_vm_stdlib(&mut vm);
                vm.execute(&chunk).await.expect("execute duration source");
                let duration_ms: i64 = vm
                    .output()
                    .trim()
                    .parse()
                    .unwrap_or_else(|_| panic!("expected int, got `{}`", vm.output()));
                assert!(duration_ms >= 50, "expected ≥50ms, got {duration_ms}");
            })
            .await;
    });
}

/// End-to-end smoke for the v2 surface against a real Postgres:
/// pool stats reflect live connections, advisory locks succeed inside
/// a transaction, schema introspection finds the test table, an
/// `int[]` column round-trips through the array decoder, and
/// LISTEN/NOTIFY delivers the payload back through pg_listener_recv.
#[test]
fn v2_surface_smoke_when_env_url_is_set() {
    if std::env::var("HARN_TEST_POSTGRES_URL").is_err() {
        return;
    }
    reset_postgres_state();
    let schema = format!("harn_pg_v2_{}", uuid::Uuid::new_v4().simple());
    let source = format!(
        r#"
import "std/postgres"

let db = pg_pool("env:HARN_TEST_POSTGRES_URL", {{max_connections: 2}})

// --- Pool observability --------------------------------------------------
let stats = pg_pool_stats(db)
__io_println(stats.circuit_state)
__io_println(stats.max_connections)
__io_println(stats.read_routing_policy)
__io_println(stats.replicas)

let clear_result = pg_stmt_cache_clear(db)
__io_println(clear_result.pools)
__io_println(clear_result.connections_cleared >= 1)
__io_println(clear_result.connections_skipped)

// --- Schema setup --------------------------------------------------------
pg_execute(db, "CREATE SCHEMA IF NOT EXISTS \"{schema}\"", [])
pg_execute(db, "SET search_path TO \"{schema}\"", [])
pg_execute(db, "CREATE TABLE widgets (id int4 PRIMARY KEY, tags text[] NOT NULL DEFAULT '{{}}')", [])
pg_execute(db, "CREATE UNIQUE INDEX widgets_id_uniq ON widgets (id)", [])
pg_execute(db, "INSERT INTO widgets (id, tags) VALUES (1, ARRAY['alpha','beta'])", [])
pg_execute(db, "INSERT INTO widgets (id, tags) VALUES (2, ARRAY[]::text[])", [])

// --- Advisory lock inside a transaction ----------------------------------
let locked_label = pg_transaction(db, {{ tx ->
  pg_advisory_xact_lock(tx, 0x4861_726E_5632_AABB)
  return pg_query_one(tx, "SELECT 'locked' AS label", []).label
}})
__io_println(locked_label)

// --- pg_with_advisory_lock (RAII helper, exercises run_managed_transaction) ----
let with_label = pg_with_advisory_lock(db, "release-cut", {{ tx ->
  return pg_query_one(tx, "SELECT 'raii' AS label", []).label
}})
__io_println(with_label)

// --- Schema introspection ------------------------------------------------
let tables = pg_introspect_tables(db, {{schema: "{schema}"}})
__io_println(len(tables))
__io_println(tables[0].kind)

let cols = pg_introspect_columns(db, "{schema}.widgets")
__io_println(len(cols))
__io_println(cols[0].column + ":" + cols[0].type)
__io_println(cols[1].column + ":" + cols[1].type)

let idx = pg_introspect_indexes(db, "{schema}.widgets")
__io_println(len(idx))

// --- Array decoding ------------------------------------------------------
let row = pg_query_one(db, "SELECT tags FROM widgets WHERE id = $1", [1])
__io_println(row.tags[0] + "," + row.tags[1])

let empty = pg_query_one(db, "SELECT tags FROM widgets WHERE id = $1", [2])
__io_println(len(empty.tags))

// --- LISTEN/NOTIFY round-trip --------------------------------------------
let listener = pg_listen(db, "harn_v2_test")
pg_notify(db, "harn_v2_test", "hello")
let notification = pg_listener_recv(listener, 5000)
__io_println(notification.channel + ":" + notification.payload)
pg_listener_close(listener)

// --- Teardown ------------------------------------------------------------
pg_execute(db, "DROP SCHEMA \"{schema}\" CASCADE", [])
pg_close(db)
"#,
    );
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let chunk = compile_source(&source).expect("compile v2 smoke source");
                let mut vm = Vm::new();
                register_vm_stdlib(&mut vm);
                vm.execute(&chunk).await.expect("execute v2 smoke source");
                let lines: Vec<&str> = vm.output().lines().collect();
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
            })
            .await;
    });
}

/// Advisory locks must isolate distinct tenants when
/// `tenant_namespace: true` is set: the same caller-supplied key
/// resolves to *different* server-side lock keys per tenant. Without
/// that, two tenants would deadlock each other for routine
/// per-resource locks.
#[test]
fn advisory_lock_tenant_namespacing_keys_differ_per_tenant() {
    use crate::harness_tenant::enter_tenant;
    use crate::TenantId;

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
        reject_non_finite_floats(&VmValue::List(std::sync::Arc::new(vec![
            VmValue::Float(1.0),
            VmValue::Int(2),
        ])))
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
    let list = VmValue::List(std::sync::Arc::new(vec![
        VmValue::Int(1),
        VmValue::Float(f64::NAN),
    ]));
    assert!(reject_non_finite_floats(&list).is_err());

    // Nested in a dict (jsonb path) — rejected.
    let nested = dict(&[("ratio", VmValue::Float(f64::INFINITY))]);
    assert!(reject_non_finite_floats(&nested).is_err());
}

/// Live regression: binding a non-finite float must fail cleanly with the
/// guard's error rather than corrupting a `float8` column or emitting
/// invalid JSON on the jsonb path. Gated on `HARN_TEST_POSTGRES_URL`.
#[tokio::test(flavor = "current_thread")]
async fn non_finite_float_bind_errors_cleanly_when_env_url_is_set() {
    let Ok(url) = std::env::var("HARN_TEST_POSTGRES_URL") else {
        return;
    };
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
async fn nil_into_typed_columns_stores_sql_null_when_env_url_is_set() {
    let Ok(url) = std::env::var("HARN_TEST_POSTGRES_URL") else {
        return;
    };
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
async fn nil_then_non_null_same_sql_does_not_poison_cache_when_env_url_is_set() {
    let Ok(url) = std::env::var("HARN_TEST_POSTGRES_URL") else {
        return;
    };
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
async fn mixed_nil_and_non_null_params_when_env_url_is_set() {
    let Ok(url) = std::env::var("HARN_TEST_POSTGRES_URL") else {
        return;
    };
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
async fn ambiguous_bare_select_nil_when_env_url_is_set() {
    let Ok(url) = std::env::var("HARN_TEST_POSTGRES_URL") else {
        return;
    };
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
async fn pool_describe_probe_failure_falls_back_to_text_null_when_env_url_is_set() {
    let Ok(url) = std::env::var("HARN_TEST_POSTGRES_URL") else {
        return;
    };
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
    let cached = DESCRIBED_OIDS
        .with(|c| c.borrow().get(sql).cloned())
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
fn tx_describe_probe_failure_keeps_tx_alive_when_env_url_is_set() {
    if std::env::var("HARN_TEST_POSTGRES_URL").is_err() {
        return;
    }
    reset_postgres_state();
    let source = r#"
import "std/postgres"

let db = pg_pool("env:HARN_TEST_POSTGRES_URL", {max_connections: 1})
pg_execute(db, "DROP TABLE IF EXISTS harn_pg_tx_probe", [])
pg_execute(db, "CREATE TABLE harn_pg_tx_probe (id int PRIMARY KEY, note text)", [])

let probed = pg_transaction(db, { tx ->
  // Ambiguous nil query: the describe probe fails. The savepoint must roll
  // back only the probe, the bind falls back to a text NULL, and the result
  // ($1 IS NULL) is true.
  let r = pg_query_one(tx, "SELECT $1 IS NULL AS v", [nil])
  // The tx must still be USABLE after the failed probe: this write must work.
  pg_execute(tx, "INSERT INTO harn_pg_tx_probe (id, note) VALUES ($1, $2)", [1, "after-probe"])
  return to_string(r.v)
})
__io_println(probed)

// The commit must have persisted the post-probe write.
let row = pg_query_one(db, "SELECT note FROM harn_pg_tx_probe WHERE id = 1", [])
__io_println(row.note)
pg_execute(db, "DROP TABLE harn_pg_tx_probe", [])
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
                let chunk = compile_source(source).expect("compile tx probe source");
                let mut vm = Vm::new();
                register_vm_stdlib(&mut vm);
                vm.execute(&chunk).await.expect("execute tx probe source");
                assert_eq!(vm.output().trim(), "true\nafter-probe");
            })
            .await;
    });
}

/// Perf path: an all-non-null query still works and reuses the per-connection
/// SQL-keyed statement cache (no describe round-trip). We can't observe the
/// cache directly here, but running the identical SQL many times on a
/// single-connection pool exercises the cached prepared statement and must
/// stay correct. Also asserts a nil-containing run of the SAME SQL afterward
/// still works (the describe path repairs/uses the same cache entry).
#[tokio::test(flavor = "current_thread")]
async fn all_non_null_uses_cache_and_interops_with_nil_when_env_url_is_set() {
    let Ok(url) = std::env::var("HARN_TEST_POSTGRES_URL") else {
        return;
    };
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

/// describe-then-bind inside a managed transaction: a `nil` bound through
/// the `HANDLE_TX` path (which describes on the tx connection rather than a
/// detached pool connection) must store SQL NULL and coexist with non-null
/// binds in the same transaction.
#[test]
fn nil_in_transaction_when_env_url_is_set() {
    if std::env::var("HARN_TEST_POSTGRES_URL").is_err() {
        return;
    }
    reset_postgres_state();
    let source = r#"
import "std/postgres"

let db = pg_pool("env:HARN_TEST_POSTGRES_URL", {max_connections: 1})
pg_execute(db, "DROP TABLE IF EXISTS harn_pg_tx_nil", [])
pg_execute(db, "CREATE TABLE harn_pg_tx_nil (id int PRIMARY KEY, a int, b text)", [])

pg_transaction(db, { tx ->
  pg_execute(tx, "INSERT INTO harn_pg_tx_nil (id, a, b) VALUES ($1, $2, $3)", [1, nil, "x"])
  pg_execute(tx, "INSERT INTO harn_pg_tx_nil (id, a, b) VALUES ($1, $2, $3)", [2, 7, nil])
  return 0
})

let r1 = pg_query_one(db, "SELECT (a IS NULL) AS a_null, b FROM harn_pg_tx_nil WHERE id = 1", [])
__io_println(to_string(r1.a_null) + ":" + r1.b)
let r2 = pg_query_one(db, "SELECT a, (b IS NULL) AS b_null FROM harn_pg_tx_nil WHERE id = 2", [])
__io_println(to_string(r2.a) + ":" + to_string(r2.b_null))
pg_execute(db, "DROP TABLE harn_pg_tx_nil", [])
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
                let chunk = compile_source(source).expect("compile tx nil source");
                let mut vm = Vm::new();
                register_vm_stdlib(&mut vm);
                vm.execute(&chunk).await.expect("execute tx nil source");
                assert_eq!(vm.output().trim(), "true:x\n7:true");
            })
            .await;
    });
}

/// Performant describe-then-bind: the server describe for a given SQL runs
/// at most **once**. The first nil-query of a SQL populates the
/// [`DESCRIBED_OIDS`] cache (one describe round-trip); every subsequent
/// nil-query of the SAME SQL is a cache hit and performs **no** further
/// describe. Asserted via the `cfg(test)` [`DESCRIBE_ROUND_TRIPS`] counter.
#[tokio::test(flavor = "current_thread")]
async fn nil_query_describes_once_and_caches_oids_when_env_url_is_set() {
    let Ok(url) = std::env::var("HARN_TEST_POSTGRES_URL") else {
        return;
    };
    reset_postgres_state();
    reset_describe_round_trips();
    let handle = open_single_conn_pool(&url).await;

    let sql = "SELECT $1::bigint AS v";

    // Cache must start empty for this SQL.
    assert!(
        DESCRIBED_OIDS.with(|c| !c.borrow().contains_key(sql)),
        "OID cache should not contain the SQL before first use"
    );

    // First nil-query: one describe round-trip, populates the cache.
    let first = query_rows(&handle, sql, &[VmValue::Nil], QueryRouting::Primary)
        .await
        .expect("first nil query");
    assert!(matches!(one_cell(first, "v"), VmValue::Nil));
    assert_eq!(
        describe_round_trips(),
        1,
        "first nil query must perform exactly one describe round-trip"
    );
    assert!(
        DESCRIBED_OIDS.with(|c| c.borrow().contains_key(sql)),
        "OID cache must be populated after first nil query"
    );

    // Subsequent nil-queries of the SAME SQL must NOT re-describe.
    for _ in 0..5 {
        let row = query_rows(&handle, sql, &[VmValue::Nil], QueryRouting::Primary)
            .await
            .expect("repeat nil query");
        assert!(matches!(one_cell(row, "v"), VmValue::Nil));
    }
    assert_eq!(
        describe_round_trips(),
        1,
        "repeat nil queries of the same SQL must hit the OID cache (no re-describe)"
    );

    // A different SQL still describes once (independent cache key).
    let other = "SELECT $1::int AS v";
    let r = query_rows(&handle, other, &[VmValue::Nil], QueryRouting::Primary)
        .await
        .expect("different SQL nil query");
    assert!(matches!(one_cell(r, "v"), VmValue::Nil));
    assert_eq!(
        describe_round_trips(),
        2,
        "a distinct SQL must add exactly one more describe round-trip"
    );
}

/// Micro-benchmark proving the performant path: after warmup, a
/// nil-containing query (OID-cache hit + normal prepared-statement cache)
/// has p99 latency within 1.2x of the SAME query bound with no nil (the
/// plain fast path) on the same pool. Gated behind `HARN_PG_NIL_BENCH=1` (in
/// addition to `HARN_TEST_POSTGRES_URL`) so it does not run in normal CI.
#[tokio::test(flavor = "current_thread")]
async fn nil_path_p99_within_budget_of_plain_path_when_bench_enabled() {
    let Ok(url) = std::env::var("HARN_TEST_POSTGRES_URL") else {
        return;
    };
    if std::env::var("HARN_PG_NIL_BENCH").as_deref() != Ok("1") {
        return;
    }
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
