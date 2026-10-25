use super::*;

#[test]
#[ignore = "requires an isolated database; make test-postgres-live"]
fn harn_transaction_commits_rolls_back_and_applies_settings_when_env_url_is_set() {
    live_postgres_url();
    reset_postgres_state();
    let source = r#"
import "std/postgres"

fn main(harness: Harness) {
const db = harness.postgres.pool("env:HARN_TEST_POSTGRES_URL", {max_connections: 1})
pg_execute(db, "create temporary table if not exists harn_pg_tx_test(value int) on commit preserve rows", [])
pg_execute(db, "truncate table harn_pg_tx_test", [])

const tenant = pg_transaction(
  db,
  { tx ->
    pg_execute(tx, "insert into harn_pg_tx_test(value) values ($1)", [1])
    return pg_query_one(tx, "select current_setting('app.current_tenant_id', true) as tenant", []).tenant
  },
  {settings: {"app.current_tenant_id": "tenant-a"}},
)
harness.stdio.println(tenant)

const rolled = try {
  pg_transaction(db, { tx ->
    pg_execute(tx, "insert into harn_pg_tx_test(value) values ($1)", [2])
    throw_error("force rollback")
  })
} catch (e) {
  "rolled back"
}
harness.stdio.println(rolled)
harness.stdio.println(pg_query_one(db, "select count(*)::int8 as count from harn_pg_tx_test", []).count)
pg_close(db)
}
"#;
    assert_eq!(run_harn_source(source).trim(), "tenant-a\nrolled back\n1");
}

#[test]
#[ignore = "requires an isolated database; make test-postgres-live"]
/// Exercise savepoints on a real connection so transaction state cannot be mocked away.
fn savepoint_rollback_preserves_outer_writes_when_env_url_is_set() {
    live_postgres_url();
    reset_postgres_state();
    let source = r#"
import "std/postgres"

fn main(harness: Harness) {
const db = harness.postgres.pool("env:HARN_TEST_POSTGRES_URL", {max_connections: 1})
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

const rows = pg_query(db, "SELECT id, label FROM harn_pg_sp_test ORDER BY id", [])
for row in rows {
  harness.stdio.println(to_string(row.id) + ":" + row.label)
}
pg_execute(db, "DROP TABLE harn_pg_sp_test", [])
pg_close(db)
}
"#;
    assert_eq!(run_harn_source(source).trim(), "1:outer\n3:after_release");
}

#[test]
#[ignore = "requires an isolated database; make test-postgres-live"]
/// Privileged GUCs must fail before SQL runs, while application RLS settings remain usable.
fn transaction_settings_reject_privileged_gucs_when_env_url_is_set() {
    live_postgres_url();
    reset_postgres_state();

    // `role` is rejected before any SQL runs.
    let reject_role = r#"
import "std/postgres"
fn main(harness: Harness) {
const db = harness.postgres.pool("env:HARN_TEST_POSTGRES_URL", {max_connections: 1})
const r = pg_transaction(db, { tx -> return 1 }, {settings: {"role": "postgres"}})
pg_close(db)
}
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
fn main(harness: Harness) {
const db = harness.postgres.pool("env:HARN_TEST_POSTGRES_URL", {max_connections: 1})
const r = pg_transaction(db, { tx -> return 1 }, {settings: {"app.current_tenant_id": nil}})
pg_close(db)
}
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
fn main(harness: Harness) {
const db = harness.postgres.pool("env:HARN_TEST_POSTGRES_URL", {max_connections: 1})
const tenant = pg_transaction(db, { tx ->
  return pg_query_one(tx, "SELECT current_setting('app.current_tenant_id', true) AS t", []).t
}, {settings: {"app.current_tenant_id": "tenant-xyz", "app.bypass_rls": "on", "statement_timeout": "5000"}})
harness.stdio.println(tenant)
pg_close(db)
}
"#;
    let out = run_harn_source(allow_legit);
    assert_eq!(out.trim(), "tenant-xyz", "legit settings must apply: {out}");
}

#[test]
#[ignore = "requires an isolated database; make test-postgres-live"]
/// SQLSTATE 23505 must expose the stable category rather than the constraint name.
fn constraint_violation_surfaces_stable_category_when_env_url_is_set() {
    live_postgres_url();
    reset_postgres_state();
    let schema = format!("harn_pg_m2_{}", uuid::Uuid::new_v4().simple());
    let source = format!(
        r#"
import "std/postgres"
fn main(harness: Harness) {{
const db = harness.postgres.pool("env:HARN_TEST_POSTGRES_URL", {{max_connections: 1}})
pg_execute(db, "DROP SCHEMA IF EXISTS \"{schema}\" CASCADE", [])
pg_execute(db, "CREATE SCHEMA \"{schema}\"", [])
pg_execute(db, "SET search_path TO \"{schema}\"", [])
pg_execute(db, "CREATE TABLE accounts (id int4 PRIMARY KEY, email text UNIQUE)", [])
pg_execute(db, "INSERT INTO accounts (id, email) VALUES (1, 'a@b.com')", [])
pg_execute(db, "INSERT INTO accounts (id, email) VALUES ($1, $2)", [2, "a@b.com"])
pg_close(db)
}}
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
fn main(harness: Harness) {{
const db = harness.postgres.pool("env:HARN_TEST_POSTGRES_URL", {{max_connections: 1}})
pg_execute(db, "DROP SCHEMA IF EXISTS \"{schema}\" CASCADE", [])
pg_close(db)
}}
"#,
    );
    run_harn_source(&cleanup);
}

#[test]
#[ignore = "requires an isolated database; make test-postgres-live"]
/// Describe on the transaction connection and preserve NULL alongside non-null binds.
fn nil_in_transaction_when_env_url_is_set() {
    live_postgres_url();
    reset_postgres_state();
    let source = r#"
import "std/postgres"

fn main(harness: Harness) {
const db = harness.postgres.pool("env:HARN_TEST_POSTGRES_URL", {max_connections: 1})
pg_execute(db, "DROP TABLE IF EXISTS harn_pg_tx_nil", [])
pg_execute(db, "CREATE TABLE harn_pg_tx_nil (id int PRIMARY KEY, a int, b text)", [])

pg_transaction(db, { tx ->
  pg_execute(tx, "INSERT INTO harn_pg_tx_nil (id, a, b) VALUES ($1, $2, $3)", [1, nil, "x"])
  pg_execute(tx, "INSERT INTO harn_pg_tx_nil (id, a, b) VALUES ($1, $2, $3)", [2, 7, nil])
  return 0
})

const r1 = pg_query_one(db, "SELECT (a IS NULL) AS a_null, b FROM harn_pg_tx_nil WHERE id = 1", [])
harness.stdio.println(to_string(r1.a_null) + ":" + r1.b)
const r2 = pg_query_one(db, "SELECT a, (b IS NULL) AS b_null FROM harn_pg_tx_nil WHERE id = 2", [])
harness.stdio.println(to_string(r2.a) + ":" + to_string(r2.b_null))
pg_execute(db, "DROP TABLE harn_pg_tx_nil", [])
pg_close(db)
}
"#;
    assert_eq!(run_harn_source(source).trim(), "true:x\n7:true");
}
