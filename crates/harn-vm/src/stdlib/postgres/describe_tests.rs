use super::*;

#[tokio::test(flavor = "current_thread")]
async fn nil_query_describes_once_and_caches_oids_when_env_url_is_set() {
    let Ok(url) = std::env::var("HARN_TEST_POSTGRES_URL") else {
        return;
    };
    reset_postgres_state();
    reset_describe_round_trips();
    let handle = open_single_conn_pool(&url).await;
    let sql = "SELECT $1::bigint AS v";
    let record = pool_record_from_handle(&handle, "test").expect("pool authority");
    assert!(!record.described_oids.lock().contains_key(sql));
    let first = query_rows(&handle, sql, &[VmValue::Nil], QueryRouting::Primary)
        .await
        .expect("first nil query");
    assert!(matches!(one_cell(first, "v"), VmValue::Nil));
    assert_eq!(describe_round_trips(), 1);
    assert!(record.described_oids.lock().contains_key(sql));

    for _ in 0..5 {
        let row = query_rows(&handle, sql, &[VmValue::Nil], QueryRouting::Primary)
            .await
            .expect("repeat nil query");
        assert!(matches!(one_cell(row, "v"), VmValue::Nil));
    }
    assert_eq!(describe_round_trips(), 1, "repeat SQL must not re-describe");

    let other = "SELECT $1::int AS v";
    let r = query_rows(&handle, other, &[VmValue::Nil], QueryRouting::Primary)
        .await
        .expect("different SQL nil query");
    assert!(matches!(one_cell(r, "v"), VmValue::Nil));
    assert_eq!(describe_round_trips(), 2, "distinct SQL must describe once");
}

#[test]
fn transactions_reuse_pool_describes_when_env_url_is_set() {
    if std::env::var("HARN_TEST_POSTGRES_URL").is_err() {
        return;
    }
    reset_postgres_state();
    reset_describe_round_trips();
    let source = r#"
import "std/postgres"

fn main(harness: Harness) {
  const db = harness.postgres.pool("env:HARN_TEST_POSTGRES_URL", {max_connections: 1})
  const known = "SELECT $1::bigint AS v"
  const learned = "SELECT $1::int AS v"
  const ambiguous = "SELECT $1 IS NULL AS v"
  assert(pg_query_one(db, known, [nil]).v == nil)
  for i in 0 to 2 exclusive {
    pg_transaction(db, { tx ->
      assert(pg_query_one(tx, known, [nil]).v == nil)
      assert(pg_query_one(tx, learned, [nil]).v == nil)
      // Execute and query share metadata, including failed-probe fallback.
      pg_execute(tx, learned, [nil])
      assert(pg_query_one(tx, ambiguous, [nil]).v)
      pg_execute(tx, ambiguous, [nil])
      assert(pg_query_one(tx, "SELECT $1::bigint AS non_null", [7]).non_null == 7)
    })
  }
  assert(pg_query_one(db, learned, [nil]).v == nil)
  assert(pg_query_one(db, ambiguous, [nil]).v)
  // A cold first statement publishes types for the next transaction.
  for i in 0 to 2 exclusive {
    pg_transaction(db, { tx ->
      pg_execute(tx, "SELECT $1::smallint", [nil])
      pg_execute(tx, "SELECT $1::smallint", [nil])
    })
  }
  // A separate pool must describe independently, even for identical SQL.
  const other = harness.postgres.pool("env:HARN_TEST_POSTGRES_URL", {max_connections: 1})
  assert(pg_query_one(other, known, [nil]).v == nil)
  pg_close(other)
  pg_close(db)
  harness.stdio.println("reused")
}
"#;
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let chunk = compile_source(source).expect("compile transaction describe reuse");
                let mut vm = Vm::new();
                register_vm_stdlib(&mut vm);
                vm.set_harness(crate::Harness::real());
                vm.execute(&chunk)
                    .await
                    .expect("execute transaction describe reuse");
                assert_eq!(vm.output().trim(), "reused");
            })
            .await;
        assert_eq!(
            describe_round_trips(),
            9,
            "only first statements share pool types; later SQL stays transaction-local"
        );
    });
}

#[test]
fn transaction_search_path_keeps_parameter_types_local_when_env_url_is_set() {
    if std::env::var("HARN_TEST_POSTGRES_URL").is_err() {
        return;
    }
    reset_postgres_state();
    let source = r#"
import "std/postgres"
fn main(harness: Harness) {
  const db = harness.postgres.pool("env:HARN_TEST_POSTGRES_URL", {max_connections: 1})
  pg_execute(db, "CREATE SCHEMA IF NOT EXISTS harn_9420_a", [])
  pg_execute(db, "CREATE SCHEMA IF NOT EXISTS harn_9420_b", [])
  pg_execute(db, "CREATE TABLE IF NOT EXISTS harn_9420_a.t (v integer)", [])
  pg_execute(db, "CREATE TABLE IF NOT EXISTS harn_9420_b.t (v uuid)", [])
  pg_execute(db, "SET search_path = harn_9420_a", [])
  const warmed = "INSERT INTO t (v) VALUES ($1) RETURNING v"
  const learned = "INSERT INTO t (v) VALUES ($1) RETURNING v /* learned locally */"
  assert(pg_query_one(db, warmed, [nil]).v == nil)
  pg_transaction(db, { tx ->
    pg_execute(tx, "SET LOCAL search_path = harn_9420_b", [])
    assert(pg_query_one(tx, warmed, [nil]).v == nil)
    assert(pg_query_one(tx, learned, [nil]).v == nil)
  })
  // Types learned under SET LOCAL must not leak back into the pool.
  pg_transaction(db, { tx ->
    assert(pg_query_one(tx, learned, [nil]).v == nil)
  })
  pg_execute(db, "DROP SCHEMA harn_9420_a CASCADE", [])
  pg_execute(db, "DROP SCHEMA harn_9420_b CASCADE", [])
  pg_close(db)
}
"#;
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        tokio::task::LocalSet::new()
            .run_until(async {
                let chunk = compile_source(source).expect("compile schema context regression");
                let mut vm = Vm::new();
                register_vm_stdlib(&mut vm);
                vm.set_harness(crate::Harness::real());
                vm.execute(&chunk)
                    .await
                    .expect("execute schema context regression");
            })
            .await;
    });
}
