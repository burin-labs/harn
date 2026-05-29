# harn-postgres-perf

Sustained-load harness for the Postgres hostlib (`pg_pool` / `pg_query_one`).

It answers the A.9 acceptance question (issue #2512) that the smoke tests
can't: *is the hostlib production-ready?* The harness drives the full VM
dispatch → builtin → sqlx pool → row-decode path under real concurrency
against a real Postgres, and asserts primary-key reads stay under a p99
latency ceiling at a target request rate.

## How it works

Each worker is its own OS thread running a current-thread Tokio runtime.
This mirrors production: pools live in thread-local state, so each VM owns
its own pool and the server fans request handling across threads. A worker
compiles a tiny Harn probe closure once, opens its pool, then loops calling
`pg_query_one` with random primary keys for a fixed window, recording
per-call latency. Latencies are merged across workers into p50/p90/p99.

Setup applies an optional real migration set (`pg_migrate`) into a unique
scratch schema, then creates and seeds a probe table. Teardown drops the
schema, even if the run fails partway.

## Running

The harness is gated on a Postgres connection URL. Without it the binary
prints a skip notice and exits 0:

```sh
HARN_TEST_POSTGRES_URL=postgres://localhost/harn_loadgen \
  cargo run -p harn-postgres-perf --release
# or
make loadgen-postgres
```

Provisioning that database plus a dedicated runner is tracked separately;
until then the nightly E2E job is a clean no-op.

## Environment variables

| Variable | Default | Meaning |
| --- | --- | --- |
| `HARN_TEST_POSTGRES_URL` | *(required)* | Connection URL; gates the run |
| `HARN_TEST_CLOUD_MIGRATIONS_DIR` | *(unset)* | `.sql` dir applied via `pg_migrate` so the schema is the real one |
| `HARN_PG_LOADGEN_WORKERS` | `32` | Concurrent worker threads |
| `HARN_PG_LOADGEN_DURATION_MS` | `5000` | Timed-window length |
| `HARN_PG_LOADGEN_ROWS` | `10000` | Seeded probe rows (primary keys `1..=ROWS`) |
| `HARN_PG_LOADGEN_POOL_CONNS` | `1` | `max_connections` per worker pool |
| `HARN_PG_LOADGEN_TARGET_RPS` | `10000` | Throughput floor for PASS |
| `HARN_PG_LOADGEN_P99_MS` | `5` | p99 latency ceiling for PASS |
| `HARN_PG_LOADGEN_ENFORCE` | `1` | When `0`, a miss prints `WARN` and still exits 0 |

The acceptance bar (defaults: 10k req/s sustained at p99 ≤ 5 ms) assumes a
dedicated runner and a co-located Postgres. On shared hardware, relax the
thresholds or set `HARN_PG_LOADGEN_ENFORCE=0` to collect a report without
gating CI.
