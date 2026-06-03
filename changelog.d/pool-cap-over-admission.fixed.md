- **Pool `max_concurrent` could transiently over-admit under concurrent dispatch.** A worker-pool dispatcher
  popped a queued task under the pool lock but inserted it into the active set under a *separate* lock hold,
  so a `submit` racing a finishing task's `finalize_task` could both admit into the same free slot —
  momentarily running `max_concurrent + 1` (or more) tasks. Dispatchers now reserve the slot at pop time and
  the admission check counts in-flight reservations, so the cap holds strictly. (Surfaced as a rare flake in
  `tests/pool/max_concurrent_caps_in_flight.harn`; a higher-contention reproduction over-admitted ~50% of
  runs before the fix and 0% after.)
