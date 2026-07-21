- Back the harn-cli `lock_harn_state` test helper with a `tokio::sync::Mutex`
  and expose an async `lock_harn_state_async` acquire alongside the blocking
  one, matching the sibling `cwd`/`run_event_sink` test locks. Async tests now
  hold the state guard across `.await` legitimately, so every scattered
  `#[allow(clippy::await_holding_lock)]` this lock (and the already-retired
  cross-process CLI lock) forced is removed.
