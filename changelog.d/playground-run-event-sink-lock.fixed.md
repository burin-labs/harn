Fixed a flaky `harn-cli` test failure under parallel `cargo test`. The
`playground` pipeline tests ran through the process-global `run_events` sink
while holding only `env_lock`, not the `run_event_sink_lock` that every
sink-touching test is required to hold, so a concurrent `--json` run test could
capture their stdout — e.g. an `llm_call_safe` error envelope bleeding into a
neighbor's NDJSON buffer. The tests now take both locks in a single documented
order. Test-only; no runtime behavior change. (CI itself was unaffected because
the nextest runner process-isolates each test.)
