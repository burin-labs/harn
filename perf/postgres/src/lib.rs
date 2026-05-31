//! Sustained-load harness for the Postgres hostlib (`pg_pool` /
//! `pg_query_one`).
//!
//! Follow-up from the A.9 acceptance bar (issue #2512): drive the hostlib
//! against a primed schema at a target request rate and confirm
//! primary-key reads stay under a p99 latency ceiling. The harness is the
//! "is the hostlib production-ready" check that the smoke tests can't be:
//! it exercises the full VM dispatch → builtin → sqlx pool → row-decode
//! path under real concurrency, against a real Postgres.
//!
//! ## Shape
//!
//! Each worker is its own OS thread running a current-thread Tokio
//! runtime. This mirrors how the hostlib runs in production: pools live in
//! thread-local state ([`harn_vm`]'s `POOLS`), so each VM owns its own
//! pool and the server fans request handling across threads. A worker
//! compiles a tiny Harn closure once, opens its pool, then loops calling
//! the closure with random primary keys for a fixed window, recording
//! per-call latency.
//!
//! ## Running
//!
//! The harness is gated on a Postgres connection URL in an environment
//! variable (default `HARN_TEST_POSTGRES_URL`, matching the rest of the
//! hostlib's real-Postgres tests). Without it, [`LoadgenConfig::run`]
//! refuses to run — callers should check [`LoadgenConfig::url_available`]
//! and skip. Provisioning that database + a dedicated runner is tracked
//! separately; until then the nightly E2E job no-ops cleanly.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use harn_vm::value::VmClosure;
use harn_vm::{compile_source, register_vm_stdlib, VmValue};

/// Connection-URL env var the hostlib's real-Postgres tests already use.
pub const DEFAULT_URL_ENV: &str = "HARN_TEST_POSTGRES_URL";
/// Optional dir of `.sql` migrations to apply during setup so the harness
/// runs against the canonical cloud schema. Matches the var consumed by
/// `migrate_loads_harn_cloud_store_migrations_when_env_set`.
pub const MIGRATIONS_DIR_ENV: &str = "HARN_TEST_CLOUD_MIGRATIONS_DIR";

const PROBE_TABLE: &str = "harn_loadgen_probe";

/// Tunable parameters for a single loadgen run.
#[derive(Clone, Debug)]
pub struct LoadgenConfig {
    /// Name of the env var holding the Postgres connection URL.
    pub url_env: String,
    /// When set, applied with `pg_migrate` into the scratch schema before
    /// the probe table is created, so the harness composes with a real
    /// migration ledger.
    pub migrations_dir: Option<PathBuf>,
    /// Number of concurrent worker threads.
    pub workers: usize,
    /// Length of each worker's timed query loop.
    pub duration: Duration,
    /// Rows seeded into the probe table; primary keys are `1..=seed_rows`.
    pub seed_rows: i64,
    /// `max_connections` for each worker's pool. Workers are serial, so 1
    /// is sufficient and keeps the server-side connection count low.
    pub pool_max_connections: i64,
    /// Acceptance floor for sustained throughput (requests/second).
    pub target_rps: f64,
    /// Acceptance ceiling for p99 primary-key-read latency.
    pub p99_threshold: Duration,
    /// When false, [`LoadgenReport::meets`] is still computed but the
    /// caller is expected not to treat a miss as a hard failure (useful on
    /// shared CI hardware that can't honor the acceptance bar).
    pub enforce: bool,
}

impl Default for LoadgenConfig {
    fn default() -> Self {
        Self {
            url_env: DEFAULT_URL_ENV.to_string(),
            migrations_dir: None,
            workers: 32,
            duration: Duration::from_secs(5),
            seed_rows: 10_000,
            pool_max_connections: 1,
            target_rps: 10_000.0,
            p99_threshold: Duration::from_millis(5),
            enforce: true,
        }
    }
}

impl LoadgenConfig {
    /// Build a config from defaults, layering `HARN_PG_LOADGEN_*` overrides
    /// and the shared migrations-dir env var on top.
    pub fn from_env() -> Self {
        let mut cfg = LoadgenConfig::default();
        if let Some(v) = env_parse("HARN_PG_LOADGEN_WORKERS") {
            cfg.workers = v;
        }
        if let Some(v) = env_parse::<u64>("HARN_PG_LOADGEN_DURATION_MS") {
            cfg.duration = Duration::from_millis(v);
        }
        if let Some(v) = env_parse("HARN_PG_LOADGEN_ROWS") {
            cfg.seed_rows = v;
        }
        if let Some(v) = env_parse("HARN_PG_LOADGEN_POOL_CONNS") {
            cfg.pool_max_connections = v;
        }
        if let Some(v) = env_parse("HARN_PG_LOADGEN_TARGET_RPS") {
            cfg.target_rps = v;
        }
        if let Some(v) = env_parse::<u64>("HARN_PG_LOADGEN_P99_MS") {
            cfg.p99_threshold = Duration::from_millis(v);
        }
        if let Some(v) = env_flag("HARN_PG_LOADGEN_ENFORCE") {
            cfg.enforce = v;
        }
        cfg.migrations_dir = std::env::var(MIGRATIONS_DIR_ENV)
            .ok()
            .map(PathBuf::from)
            .filter(|dir| dir.exists());
        cfg
    }

    /// Whether the gating connection URL is present in the environment.
    pub fn url_available(&self) -> bool {
        std::env::var(&self.url_env).is_ok()
    }

    /// Drive the full loadgen: set up a scratch schema, fan out the
    /// workers, then tear the schema down. Returns the measured report.
    pub fn run(&self) -> Result<LoadgenReport, String> {
        if !self.url_available() {
            return Err(format!("{} is not set", self.url_env));
        }
        if self.workers == 0 {
            return Err("workers must be > 0".to_string());
        }
        if self.seed_rows <= 0 {
            return Err("seed_rows must be > 0".to_string());
        }

        let schema = scratch_schema_name();
        let setup = run_script(&self.setup_script(&schema));
        // Always attempt teardown, even if setup failed partway, so a
        // botched run doesn't leak a scratch schema.
        if let Err(error) = setup {
            let _ = run_script(&self.teardown_script(&schema));
            return Err(format!("setup failed: {error}"));
        }

        let result = self.run_workers(&schema);
        let _ = run_script(&self.teardown_script(&schema));
        result
    }

    fn run_workers(&self, schema: &str) -> Result<LoadgenReport, String> {
        let max_id = self.seed_rows as u64;
        let duration = self.duration;

        // Each worker runs an independent `duration`-length window, so
        // aggregate throughput is total ops over that window — no shared
        // start barrier needed (and none that could deadlock if a worker
        // fails to prime). Priming is fast relative to the window, so the
        // windows overlap for all but a sliver at the edges.
        let handles: Vec<_> = (0..self.workers)
            .map(|index| {
                let script = self.worker_script(schema);
                std::thread::Builder::new()
                    .name(format!("pg-loadgen-{index}"))
                    .spawn(move || worker_thread(&script, index as u64, max_id, duration))
                    .expect("spawn loadgen worker thread")
            })
            .collect();

        let mut latencies_us: Vec<u64> = Vec::new();
        let mut errors: u64 = 0;
        let mut first_error: Option<String> = None;
        for handle in handles {
            match handle.join().expect("join loadgen worker thread") {
                Ok(stats) => {
                    latencies_us.extend(stats.latencies_us);
                    errors += stats.errors;
                    first_error = first_error.or(stats.first_error);
                }
                Err(error) => {
                    first_error = first_error.or(Some(error));
                    errors += 1;
                }
            }
        }

        if latencies_us.is_empty() {
            return Err(format!(
                "no successful queries (errors={errors}{})",
                first_error
                    .map(|e| format!(", first: {e}"))
                    .unwrap_or_default()
            ));
        }

        latencies_us.sort_unstable();
        let total_ops = latencies_us.len() as u64;
        Ok(LoadgenReport {
            workers: self.workers,
            total_ops,
            errors,
            first_error,
            window: duration,
            p50: percentile(&latencies_us, 0.50),
            p90: percentile(&latencies_us, 0.90),
            p99: percentile(&latencies_us, 0.99),
            max: Duration::from_micros(*latencies_us.last().unwrap()),
            achieved_rps: total_ops as f64 / duration.as_secs_f64(),
        })
    }

    fn setup_script(&self, schema: &str) -> String {
        let migrate = match &self.migrations_dir {
            Some(dir) => format!(
                "pg_migrate(admin, {{dir: {dir}}})\n",
                dir = harn_string_literal(&dir.to_string_lossy())
            ),
            None => String::new(),
        };
        format!(
            r#"import "std/postgres"
let admin = pg_pool("env:{url_env}", {{max_connections: 1}})
pg_execute(admin, "DROP SCHEMA IF EXISTS \"{schema}\" CASCADE", [])
pg_execute(admin, "CREATE SCHEMA \"{schema}\"", [])
pg_execute(admin, "SET search_path TO \"{schema}\"", [])
{migrate}pg_execute(admin, "CREATE TABLE \"{schema}\".{table} (id BIGINT PRIMARY KEY, payload JSONB NOT NULL)", [])
pg_execute(admin, "INSERT INTO \"{schema}\".{table} (id, payload) SELECT g, jsonb_build_object('n', g) FROM generate_series(1, $1) g", [{rows}])
pg_close(admin)
"#,
            url_env = self.url_env,
            table = PROBE_TABLE,
            rows = self.seed_rows,
        )
    }

    fn teardown_script(&self, schema: &str) -> String {
        format!(
            r#"import "std/postgres"
let admin = pg_pool("env:{url_env}", {{max_connections: 1}})
pg_execute(admin, "DROP SCHEMA IF EXISTS \"{schema}\" CASCADE", [])
pg_close(admin)
"#,
            url_env = self.url_env,
        )
    }

    fn worker_script(&self, schema: &str) -> String {
        worker_script(
            &format!(
                "pg_pool(\"env:{}\", {{max_connections: {}}})",
                self.url_env, self.pool_max_connections
            ),
            &probe_query(schema),
        )
    }
}

/// Result of a completed loadgen run.
#[derive(Clone, Debug)]
pub struct LoadgenReport {
    pub workers: usize,
    /// Successful primary-key reads.
    pub total_ops: u64,
    /// Reads that returned an error.
    pub errors: u64,
    pub first_error: Option<String>,
    /// Length of each worker's timed window; the throughput denominator.
    pub window: Duration,
    pub p50: Duration,
    pub p90: Duration,
    pub p99: Duration,
    pub max: Duration,
    pub achieved_rps: f64,
}

impl LoadgenReport {
    /// Check the report against the acceptance bar: zero errors, sustained
    /// throughput at or above the target, and p99 within the ceiling.
    pub fn meets(&self, config: &LoadgenConfig) -> Result<(), String> {
        let mut failures = Vec::new();
        if self.errors > 0 {
            failures.push(format!(
                "{} read error(s){}",
                self.errors,
                self.first_error
                    .as_ref()
                    .map(|e| format!(" (first: {e})"))
                    .unwrap_or_default()
            ));
        }
        if self.achieved_rps < config.target_rps {
            failures.push(format!(
                "throughput {:.0} req/s below target {:.0} req/s",
                self.achieved_rps, config.target_rps
            ));
        }
        if self.p99 > config.p99_threshold {
            failures.push(format!(
                "p99 {:.3} ms over ceiling {:.3} ms",
                ms(self.p99),
                ms(config.p99_threshold)
            ));
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(failures.join("; "))
        }
    }
}

impl std::fmt::Display for LoadgenReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(
            f,
            "postgres loadgen: {} workers, {} ops over {:.2}s ({:.0} req/s, {} errors)",
            self.workers,
            self.total_ops,
            self.window.as_secs_f64(),
            self.achieved_rps,
            self.errors,
        )?;
        write!(
            f,
            "  latency  p50={:.3}ms  p90={:.3}ms  p99={:.3}ms  max={:.3}ms",
            ms(self.p50),
            ms(self.p90),
            ms(self.p99),
            ms(self.max),
        )
    }
}

struct WorkerStats {
    latencies_us: Vec<u64>,
    errors: u64,
    first_error: Option<String>,
}

fn worker_thread(
    script: &str,
    seed: u64,
    max_id: u64,
    duration: Duration,
) -> Result<WorkerStats, String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("build worker runtime: {error}"))?;
    let local = tokio::task::LocalSet::new();
    runtime.block_on(local.run_until(async move {
        let (mut vm, closure) = build_probe_closure(script).await?;
        let deadline = Instant::now() + duration;
        Ok(probe_loop(&mut vm, &closure, seed, max_id, deadline).await)
    }))
}

/// Compile a worker script that returns a probe closure, run its top level
/// (opening the pool), and hand back the still-live VM plus the closure so
/// the caller can invoke it repeatedly.
async fn build_probe_closure(script: &str) -> Result<(harn_vm::Vm, Arc<VmClosure>), String> {
    let chunk = compile_source(script).map_err(|error| format!("compile: {error}"))?;
    let mut vm = harn_vm::Vm::new();
    register_vm_stdlib(&mut vm);
    let value = vm
        .execute(&chunk)
        .await
        .map_err(|error| format!("open pool: {error}"))?;
    match value {
        VmValue::Closure(closure) => Ok((vm, closure)),
        other => Err(format!(
            "worker script must return a closure, got {}",
            other.type_name()
        )),
    }
}

async fn probe_loop(
    vm: &mut harn_vm::Vm,
    closure: &VmClosure,
    seed: u64,
    max_id: u64,
    deadline: Instant,
) -> WorkerStats {
    let mut rng = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15).wrapping_add(1);
    let mut latencies_us = Vec::new();
    let mut errors = 0;
    let mut first_error = None;
    while Instant::now() < deadline {
        rng = xorshift64(rng);
        let id = (rng % max_id) as i64 + 1;
        let started = Instant::now();
        match vm.call_closure_pub(closure, &[VmValue::Int(id)]).await {
            Ok(_) => latencies_us.push(started.elapsed().as_micros() as u64),
            Err(error) => {
                errors += 1;
                if first_error.is_none() {
                    first_error = Some(error.to_string());
                }
            }
        }
    }
    WorkerStats {
        latencies_us,
        errors,
        first_error,
    }
}

fn worker_script(pool_expr: &str, query: &str) -> String {
    // A top-level program returns nil; only a pipeline yields a value back
    // to `execute`. So open the pool in the pipeline body and return a
    // probe closure that captures the pool handle. The pool stays
    // registered in thread-local state for the closure's lifetime.
    format!(
        r#"import "std/postgres"
pipeline main(task) {{
  let db = {pool_expr}
  return {{ id -> pg_query_one(db, {query}, [id]) }}
}}
"#,
        query = harn_string_literal(query),
    )
}

fn probe_query(schema: &str) -> String {
    format!("select payload from \"{schema}\".{PROBE_TABLE} where id = $1")
}

/// Run a Harn script to completion on a fresh current-thread runtime,
/// discarding its value. Used for the one-shot setup/teardown phases.
fn run_script(script: &str) -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("build runtime: {error}"))?;
    let local = tokio::task::LocalSet::new();
    runtime.block_on(local.run_until(async move {
        let chunk = compile_source(script).map_err(|error| format!("compile: {error}"))?;
        let mut vm = harn_vm::Vm::new();
        register_vm_stdlib(&mut vm);
        vm.execute(&chunk)
            .await
            .map_err(|error| error.to_string())?;
        Ok(())
    }))
}

/// Nearest-rank percentile over a sorted slice of microsecond samples.
fn percentile(sorted_us: &[u64], quantile: f64) -> Duration {
    debug_assert!(!sorted_us.is_empty());
    let rank = (quantile * sorted_us.len() as f64).ceil() as usize;
    let index = rank.saturating_sub(1).min(sorted_us.len() - 1);
    Duration::from_micros(sorted_us[index])
}

fn xorshift64(mut state: u64) -> u64 {
    state ^= state << 13;
    state ^= state >> 7;
    state ^= state << 17;
    state
}

fn scratch_schema_name() -> String {
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("harn_loadgen_{:x}_{:x}", std::process::id(), nanos)
}

/// Render a Rust string as a Harn double-quoted string literal, escaping
/// backslashes and quotes so embedded SQL/paths can't break out of the
/// generated source.
fn harn_string_literal(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            _ => out.push(ch),
        }
    }
    out.push('"');
    out
}

fn ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1_000.0
}

fn env_parse<T: std::str::FromStr>(key: &str) -> Option<T> {
    std::env::var(key).ok().and_then(|v| v.trim().parse().ok())
}

/// Parse a boolean-ish env var: `0`/`false`/`no`/`off` are false, any other
/// non-empty value is true. Returns `None` when the var is unset/empty so
/// the caller keeps its default.
fn env_flag(key: &str) -> Option<bool> {
    let value = std::env::var(key).ok()?;
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    Some(!matches!(
        value.to_ascii_lowercase().as_str(),
        "0" | "false" | "no" | "off"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentile_nearest_rank() {
        let samples: Vec<u64> = (1..=100).collect();
        assert_eq!(percentile(&samples, 0.50), Duration::from_micros(50));
        assert_eq!(percentile(&samples, 0.90), Duration::from_micros(90));
        assert_eq!(percentile(&samples, 0.99), Duration::from_micros(99));
        // Quantile 1.0 and a single sample stay in bounds.
        assert_eq!(percentile(&samples, 1.0), Duration::from_micros(100));
        assert_eq!(percentile(&[7], 0.99), Duration::from_micros(7));
    }

    #[test]
    fn string_literal_escapes_quotes_and_backslashes() {
        assert_eq!(harn_string_literal("a\"b\\c"), r#""a\"b\\c""#);
    }

    #[test]
    fn report_meets_flags_each_failure() {
        let config = LoadgenConfig {
            target_rps: 10_000.0,
            p99_threshold: Duration::from_millis(5),
            ..LoadgenConfig::default()
        };
        let pass = LoadgenReport {
            workers: 4,
            total_ops: 60_000,
            errors: 0,
            first_error: None,
            window: Duration::from_secs(5),
            p50: Duration::from_micros(200),
            p90: Duration::from_micros(800),
            p99: Duration::from_millis(3),
            max: Duration::from_millis(10),
            achieved_rps: 12_000.0,
        };
        assert!(pass.meets(&config).is_ok());

        let slow = LoadgenReport {
            p99: Duration::from_millis(9),
            achieved_rps: 4_000.0,
            errors: 2,
            first_error: Some("boom".to_string()),
            ..pass
        };
        let error = slow.meets(&config).unwrap_err();
        assert!(error.contains("read error"), "{error}");
        assert!(error.contains("below target"), "{error}");
        assert!(error.contains("over ceiling"), "{error}");
    }

    /// Exercises the full worker-driving path — compile a probe closure,
    /// open a (mock) pool, and loop calling `pg_query_one` — without a
    /// real Postgres. Guards the harness wiring so CI catches regressions
    /// even though the gated loadgen only runs against a provisioned DB.
    #[test]
    fn probe_loop_drives_mock_pool() {
        let query = "select payload from probe where id = $1";
        let pool_expr = format!(
            "pg_mock_pool([{{sql: {sql}, rows: [{{payload: 1}}]}}])",
            sql = harn_string_literal(query)
        );
        let script = worker_script(&pool_expr, query);

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let local = tokio::task::LocalSet::new();
        let stats = runtime.block_on(local.run_until(async move {
            let (mut vm, closure) = build_probe_closure(&script).await.expect("build closure");
            let deadline = Instant::now() + Duration::from_millis(50);
            probe_loop(&mut vm, &closure, 1, 64, deadline).await
        }));

        assert_eq!(stats.errors, 0, "unexpected error: {:?}", stats.first_error);
        assert!(
            !stats.latencies_us.is_empty(),
            "loop recorded no successful calls"
        );
    }
}
