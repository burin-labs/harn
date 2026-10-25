use super::*;

#[test]
#[ignore = "requires an isolated database; make test-postgres-live"]
/// Unique scratch schemas keep concurrent runs independent; down files must be ignored.
fn migrate_applies_synthetic_dir_and_is_idempotent_when_env_url_is_set() {
    live_postgres_url();
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

fn main(harness: Harness) {{
const admin = harness.postgres.pool("env:HARN_TEST_POSTGRES_URL", {{max_connections: 1}})
pg_execute(admin, "DROP SCHEMA IF EXISTS \"{schema}\" CASCADE", [])
pg_execute(admin, "CREATE SCHEMA \"{schema}\"", [])
pg_close(admin)

const db = harness.postgres.pool("env:HARN_TEST_POSTGRES_URL", {{max_connections: 1}})
pg_execute(db, "SET search_path TO \"{schema}\"", [])

const first = pg_migrate(db, {{dir: "{migration_dir}"}})
harness.stdio.println(len(first.applied))
harness.stdio.println(len(first.skipped))

const second = pg_migrate(db, {{dir: "{migration_dir}"}})
harness.stdio.println(len(second.applied))
harness.stdio.println(len(second.skipped))

const count = pg_query_one(db, "SELECT count(*)::int8 AS c FROM widgets", [])
harness.stdio.println(count.c)

pg_execute(db, "DROP SCHEMA \"{schema}\" CASCADE", [])
pg_close(db)
}}
"#,
    );
    let output = run_harn_source(&source);
    let lines: Vec<&str> = output.lines().collect();
    assert_eq!(lines, vec!["2", "0", "0", "2", "1"]);
}

#[test]
#[ignore = "requires an isolated database; make test-postgres-live"]
/// Rehash an already applied file so an edited migration cannot silently become a no-op.
fn migrate_harn_detects_checksum_drift_when_env_url_is_set() {
    live_postgres_url();
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

fn main(harness: Harness) {{
const admin = harness.postgres.pool("env:HARN_TEST_POSTGRES_URL", {{max_connections: 1}})
pg_execute(admin, "DROP SCHEMA IF EXISTS \"{schema}\" CASCADE", [])
pg_execute(admin, "CREATE SCHEMA \"{schema}\"", [])
pg_close(admin)

const db = harness.postgres.pool("env:HARN_TEST_POSTGRES_URL", {{max_connections: 1}})
pg_execute(db, "SET search_path TO \"{schema}\"", [])
const first = pg_migrate(db, {{dir: "{migration_dir}"}})
harness.stdio.println(len(first.applied))
pg_close(db)
}}
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

fn main(harness: Harness) {{
const db = harness.postgres.pool("env:HARN_TEST_POSTGRES_URL", {{max_connections: 1}})
pg_execute(db, "SET search_path TO \"{schema}\"", [])
const second = pg_migrate(db, {{dir: "{migration_dir}"}})
harness.stdio.println(len(second.applied))
pg_close(db)
}}
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
fn main(harness: Harness) {{
const admin = harness.postgres.pool("env:HARN_TEST_POSTGRES_URL", {{max_connections: 1}})
pg_execute(admin, "DROP SCHEMA IF EXISTS \"{schema}\" CASCADE", [])
pg_close(admin)
}}
"#,
    );
    run_harn_source(&cleanup);
}

#[test]
#[ignore = "requires database and cloud migrations; make test-postgres-cloud"]
fn migrate_loads_harn_cloud_store_migrations_when_env_set() {
    live_postgres_url();
    let dir = cloud_migrations_dir();
    reset_postgres_state();
    let schema = format!("harn_pg_cloud_{}", uuid::Uuid::new_v4().simple());
    let source = format!(
        r#"
import "std/postgres"

fn main(harness: Harness) {{
const admin = harness.postgres.pool("env:HARN_TEST_POSTGRES_URL", {{max_connections: 1}})
pg_execute(admin, "DROP SCHEMA IF EXISTS \"{schema}\" CASCADE", [])
pg_execute(admin, "CREATE SCHEMA \"{schema}\"", [])
pg_close(admin)

const db = harness.postgres.pool("env:HARN_TEST_POSTGRES_URL", {{max_connections: 1}})
pg_execute(db, "SET search_path TO \"{schema}\"", [])

const result = pg_migrate(db, {{dir: "{dir}"}})
harness.stdio.println(len(result.applied))
harness.stdio.println(len(result.skipped))

const tables = pg_query(
  db,
  "SELECT table_name FROM information_schema.tables WHERE table_schema = $1",
  ["{schema}"],
)
harness.stdio.println(len(tables))

pg_execute(db, "DROP SCHEMA \"{schema}\" CASCADE", [])
pg_close(db)
}}
"#,
    );
    let output = run_harn_source(&source);
    let lines: Vec<&str> = output.lines().collect();
    assert_eq!(lines.len(), 3, "unexpected output: {output}");
    let applied: usize = lines[0].parse().expect("applied count");
    let tables: usize = lines[2].parse().expect("table count");
    assert!(applied > 0, "no migrations applied: {output}");
    assert!(
        tables >= applied,
        "fewer tables than migrations applied: tables={tables}, applied={applied}",
    );
}

#[test]
#[ignore = "requires an isolated database; make test-postgres-live"]
/// Warm a plan before DDL and reuse the pool to expose stale prepared statement types.
fn migrate_recycles_statement_cache_after_ddl_when_env_url_is_set() {
    live_postgres_url();
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
fn main(harness: Harness) {{
const admin = harness.postgres.pool("env:HARN_TEST_POSTGRES_URL", {{max_connections: 1}})
pg_execute(admin, "DROP SCHEMA IF EXISTS \"{schema}\" CASCADE", [])
pg_execute(admin, "CREATE SCHEMA \"{schema}\"", [])
pg_close(admin)

// A single-connection pool: the warmed connection, the migrate connection,
// and the post-migrate query all share ONE backend, so a stale cached plan
// would deterministically reproduce 0A000 unless the migrate recycled it.
// (max_connections: 1 also keeps the `SET search_path` session setting on the
// same connection migrate/queries reuse.)
const db = harness.postgres.pool("env:HARN_TEST_POSTGRES_URL", {{max_connections: 1}})
pg_execute(db, "SET search_path TO \"{schema}\"", [])
pg_migrate(db, {{dir: "{dir1}"}})
// Warm + cache a plan that selects v as text on the pooled connection.
const warm = pg_query_one(db, "SELECT v FROM plan_t LIMIT 1", [])
harness.stdio.println(warm.v)
// Apply the retype DDL through pg_migrate (which recycles caches).
pg_migrate(db, {{dir: "{dir2}"}})
// This reuse would hit 0A000 if the cache were not recycled.
const after = pg_query_one(db, "SELECT v FROM plan_t LIMIT 1", [])
harness.stdio.println(after.v)
pg_execute(db, "DROP SCHEMA \"{schema}\" CASCADE", [])
pg_close(db)
}}
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

#[test]
#[ignore = "requires an isolated database; make test-postgres-live"]
/// A third run proves the advisory lock was released as well as acquired.
fn concurrent_migrate_serializes_on_advisory_lock_when_env_url_is_set() {
    live_postgres_url();
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
fn main(harness: Harness) {{
const admin = harness.postgres.pool("env:HARN_TEST_POSTGRES_URL", {{max_connections: 1}})
pg_execute(admin, "DROP SCHEMA IF EXISTS \"{schema}\" CASCADE", [])
pg_execute(admin, "CREATE SCHEMA \"{schema}\"", [])
pg_close(admin)
}}
"#,
    );
    run_harn_source(&setup);

    let migrate_src = |label: &str| {
        format!(
            r#"
import "std/postgres"
fn main(harness: Harness) {{
const db = harness.postgres.pool("env:HARN_TEST_POSTGRES_URL", {{max_connections: 1}})
pg_execute(db, "SET search_path TO \"{schema}\"", [])
const r = pg_migrate(db, {{dir: "{migration_dir}"}})
harness.stdio.println("{label}:" + to_string(len(r.applied)))
pg_close(db)
}}
"#,
        )
    };
    let src_a = migrate_src("a");
    let src_b = migrate_src("b");

    let rt = harn_clock::test_support::current_thread_runtime();
    let (out_a, out_b) = rt.block_on(async {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let run_one = |src: String| async move {
                    execute_harn_source(&src)
                        .await
                        .expect("execute migrate src")
                        .trim()
                        .to_string()
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
fn migrate_sqlx_applies_into_sqlx_migrations_table_when_env_url_is_set() {
    live_postgres_url();
    reset_postgres_state();
    let (_tmp, dir) = sqlx_synthetic_migrations();
    let schema = format!("harn_pg_sqlx_{}", uuid::Uuid::new_v4().simple());
    let source = format!(
        r#"
import "std/postgres"

fn main(harness: Harness) {{
const admin = harness.postgres.pool("env:HARN_TEST_POSTGRES_URL", {{max_connections: 1}})
pg_execute(admin, "DROP SCHEMA IF EXISTS \"{schema}\" CASCADE", [])
pg_execute(admin, "CREATE SCHEMA \"{schema}\"", [])
pg_close(admin)

const db = harness.postgres.pool("env:HARN_TEST_POSTGRES_URL", {{max_connections: 1}})
pg_execute(db, "SET search_path TO \"{schema}\"", [])

const result = pg_migrate(db, {{dir: "{dir}", ledger: "sqlx"}})
harness.stdio.println(len(result.applied))
harness.stdio.println(len(result.available))
harness.stdio.println(result.table)

const cols = pg_query(db, "SELECT column_name FROM information_schema.columns WHERE table_schema=$1 AND table_name='_sqlx_migrations' ORDER BY column_name", ["{schema}"])
harness.stdio.println(len(cols))

const rows = pg_query_one(db, "SELECT count(*)::int8 AS c FROM _sqlx_migrations", [])
harness.stdio.println(rows.c)

const badlen = pg_query_one(db, "SELECT count(*)::int8 AS c FROM _sqlx_migrations WHERE octet_length(checksum) <> 48", [])
harness.stdio.println(badlen.c)

const failed = pg_query_one(db, "SELECT count(*)::int8 AS c FROM _sqlx_migrations WHERE success = false", [])
harness.stdio.println(failed.c)

const versions = pg_query(db, "SELECT version FROM _sqlx_migrations ORDER BY version", [])
harness.stdio.println(len(versions))

pg_execute(db, "DROP SCHEMA \"{schema}\" CASCADE", [])
pg_close(db)
}}
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

#[test]
#[ignore = "requires an isolated database; make test-postgres-live"]
fn migrate_sqlx_is_idempotent_when_env_url_is_set() {
    live_postgres_url();
    reset_postgres_state();
    let (_tmp, dir) = sqlx_synthetic_migrations();
    let schema = format!("harn_pg_sqlxidem_{}", uuid::Uuid::new_v4().simple());
    let source = format!(
        r#"
import "std/postgres"

fn main(harness: Harness) {{
const admin = harness.postgres.pool("env:HARN_TEST_POSTGRES_URL", {{max_connections: 1}})
pg_execute(admin, "DROP SCHEMA IF EXISTS \"{schema}\" CASCADE", [])
pg_execute(admin, "CREATE SCHEMA \"{schema}\"", [])
pg_close(admin)

const db = harness.postgres.pool("env:HARN_TEST_POSTGRES_URL", {{max_connections: 1}})
pg_execute(db, "SET search_path TO \"{schema}\"", [])

const first = pg_migrate(db, {{dir: "{dir}", ledger: "sqlx"}})
harness.stdio.println(len(first.applied))
harness.stdio.println(len(first.skipped))

const count1 = pg_query_one(db, "SELECT count(*)::int8 AS c FROM _sqlx_migrations", [])
harness.stdio.println(count1.c)

const second = pg_migrate(db, {{dir: "{dir}", ledger: "sqlx"}})
harness.stdio.println(len(second.applied))
harness.stdio.println(len(second.skipped))

const count2 = pg_query_one(db, "SELECT count(*)::int8 AS c FROM _sqlx_migrations", [])
harness.stdio.println(count2.c)

pg_execute(db, "DROP SCHEMA \"{schema}\" CASCADE", [])
pg_close(db)
}}
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

#[test]
#[ignore = "requires an isolated database; make test-postgres-live"]
/// Preseed SQLx-compatible SHA-384 rows so a second ledger cannot pass as integration.
fn migrate_sqlx_no_fork_against_preseeded_ledger_when_env_url_is_set() {
    live_postgres_url();
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

fn main(harness: Harness) {{
const admin = harness.postgres.pool("env:HARN_TEST_POSTGRES_URL", {{max_connections: 1}})
pg_execute(admin, "DROP SCHEMA IF EXISTS \"{schema}\" CASCADE", [])
pg_execute(admin, "CREATE SCHEMA \"{schema}\"", [])
pg_close(admin)

const db = harness.postgres.pool("env:HARN_TEST_POSTGRES_URL", {{max_connections: 1}})
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

const before = pg_query_one(db, "SELECT md5(string_agg(encode(checksum,'hex'), ',' ORDER BY version)) AS h FROM _sqlx_migrations", [])

const result = pg_migrate(db, {{dir: "{dir}", ledger: "sqlx"}})
harness.stdio.println(len(result.applied))
harness.stdio.println(len(result.skipped))

const after = pg_query_one(db, "SELECT md5(string_agg(encode(checksum,'hex'), ',' ORDER BY version)) AS h FROM _sqlx_migrations", [])
if before.h == after.h {{ harness.stdio.println("checksums-identical") }} else {{ harness.stdio.println("checksums-CHANGED") }}

const count = pg_query_one(db, "SELECT count(*)::int8 AS c FROM _sqlx_migrations", [])
harness.stdio.println(count.c)

pg_execute(db, "DROP SCHEMA \"{schema}\" CASCADE", [])
pg_close(db)
}}
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

#[test]
#[ignore = "requires an isolated database; make test-postgres-live"]
fn migrate_sqlx_detects_checksum_mismatch_when_env_url_is_set() {
    live_postgres_url();
    reset_postgres_state();
    let (_tmp, dir) = sqlx_synthetic_migrations();
    let schema = format!("harn_pg_sqlxmismatch_{}", uuid::Uuid::new_v4().simple());
    let source = format!(
        r#"
import "std/postgres"

fn main(harness: Harness) {{
const admin = harness.postgres.pool("env:HARN_TEST_POSTGRES_URL", {{max_connections: 1}})
pg_execute(admin, "DROP SCHEMA IF EXISTS \"{schema}\" CASCADE", [])
pg_execute(admin, "CREATE SCHEMA \"{schema}\"", [])
pg_close(admin)

const db = harness.postgres.pool("env:HARN_TEST_POSTGRES_URL", {{max_connections: 1}})
pg_execute(db, "SET search_path TO \"{schema}\"", [])

const first = pg_migrate(db, {{dir: "{dir}", ledger: "sqlx"}})
harness.stdio.println(len(first.applied))

// Corrupt the recorded checksum for the first migration.
pg_execute(db, "UPDATE _sqlx_migrations SET checksum = decode('deadbeef', 'hex') WHERE version = 20260419170000", [])

const second = pg_migrate(db, {{dir: "{dir}", ledger: "sqlx"}})
harness.stdio.println(len(second.applied))

pg_execute(db, "DROP SCHEMA \"{schema}\" CASCADE", [])
pg_close(db)
}}
"#,
    );
    let err = run_harn_source_expect_err(&source);
    assert!(
        err.contains("checksum mismatch") && err.contains("20260419170000"),
        "expected checksum-mismatch error naming the version, got: {err}"
    );
}

#[test]
#[ignore = "requires an isolated database; make test-postgres-live"]
fn migrate_sqlx_detects_dirty_ledger_when_env_url_is_set() {
    live_postgres_url();
    reset_postgres_state();
    let (_tmp, dir) = sqlx_synthetic_migrations();
    let schema = format!("harn_pg_sqlxdirty_{}", uuid::Uuid::new_v4().simple());
    let source = format!(
        r#"
import "std/postgres"

fn main(harness: Harness) {{
const admin = harness.postgres.pool("env:HARN_TEST_POSTGRES_URL", {{max_connections: 1}})
pg_execute(admin, "DROP SCHEMA IF EXISTS \"{schema}\" CASCADE", [])
pg_execute(admin, "CREATE SCHEMA \"{schema}\"", [])
pg_close(admin)

const db = harness.postgres.pool("env:HARN_TEST_POSTGRES_URL", {{max_connections: 1}})
pg_execute(db, "SET search_path TO \"{schema}\"", [])

pg_execute(db, "CREATE TABLE _sqlx_migrations (version BIGINT PRIMARY KEY, description TEXT NOT NULL, installed_on TIMESTAMPTZ NOT NULL DEFAULT now(), success BOOLEAN NOT NULL, checksum BYTEA NOT NULL, execution_time BIGINT NOT NULL)", [])
pg_execute(db, "INSERT INTO _sqlx_migrations (version, description, success, checksum, execution_time) VALUES (20260419170000, 'bootstrap', FALSE, decode('deadbeef', 'hex'), -1)", [])

const result = pg_migrate(db, {{dir: "{dir}", ledger: "sqlx"}})
harness.stdio.println(len(result.applied))

pg_execute(db, "DROP SCHEMA \"{schema}\" CASCADE", [])
pg_close(db)
}}
"#,
    );
    let err = run_harn_source_expect_err(&source);
    assert!(
        err.contains("dirty migration") && err.contains("20260419170000"),
        "expected dirty-ledger error naming the version, got: {err}"
    );
}

#[test]
#[ignore = "requires database and cloud migrations; make test-postgres-cloud"]
fn migrate_sqlx_applies_real_cloud_dir_and_is_idempotent_when_env_set() {
    live_postgres_url();
    let dir = cloud_migrations_dir();
    reset_postgres_state();
    let schema = format!("harn_pg_sqlxcloud_{}", uuid::Uuid::new_v4().simple());
    let source = format!(
        r#"
import "std/postgres"

fn main(harness: Harness) {{
const admin = harness.postgres.pool("env:HARN_TEST_POSTGRES_URL", {{max_connections: 1}})
pg_execute(admin, "DROP SCHEMA IF EXISTS \"{schema}\" CASCADE", [])
pg_execute(admin, "CREATE SCHEMA \"{schema}\"", [])
pg_close(admin)

const db = harness.postgres.pool("env:HARN_TEST_POSTGRES_URL", {{max_connections: 1}})
pg_execute(db, "SET search_path TO \"{schema}\"", [])

const first = pg_migrate(db, {{dir: "{dir}", ledger: "sqlx"}})
harness.stdio.println(len(first.applied))
harness.stdio.println(len(first.skipped))

const count1 = pg_query_one(db, "SELECT count(*)::int8 AS c FROM _sqlx_migrations", [])
harness.stdio.println(count1.c)

const badlen = pg_query_one(db, "SELECT count(*)::int8 AS c FROM _sqlx_migrations WHERE octet_length(checksum) <> 48", [])
harness.stdio.println(badlen.c)

const second = pg_migrate(db, {{dir: "{dir}", ledger: "sqlx"}})
harness.stdio.println(len(second.applied))
harness.stdio.println(len(second.skipped))

pg_execute(db, "DROP SCHEMA \"{schema}\" CASCADE", [])
pg_close(db)
}}
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
