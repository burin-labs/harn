- **Fixed an unbounded memory leak in the test runner and long-running agent loops (#2660).** Transcript and daemon
  event-log appends were dispatched as detached `tokio::runtime::Handle::spawn` tasks. The agent loop and
  `harn test` drive their runtime with `LocalSet::run_until`, which stops polling once the driving future resolves,
  so those detached append tasks were never run to completion — each stranded task pinned its transcript-sized
  `LogEvent` payload plus an `Arc<AnyEventLog>` clone for the lifetime of the runtime. Across a
  `harn test --parallel` worker this accumulated roughly one transcript per test (~18 MB) and OOM'd CI. The appends
  now run synchronously via a private `futures::executor::block_on`; no event-log backend yields to the tokio
  reactor on `append`, so this is leak-free and does not touch the ambient runtime. A counting-allocator probe
  measured the regression at ~18 MB/round of steady-state growth before the fix; a regression test now asserts the
  append lands in the log the instant the producing `run_until` future resolves, proving no detached task can
  strand the payload.
