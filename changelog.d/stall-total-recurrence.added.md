- **`std/agent/stall` verify-state hard-recurrence now also trips on TOTAL
  (non-consecutive) failure recurrence.** The `verify_state_recurrence_hard` cut
  (active only when a `progress_signal` callback is supplied) previously keyed
  only on a consecutive same-diagnostic streak, which resets whenever an
  off-signature failure interposes — missing interleaved churn (the same dead
  API / wrong error re-proposed N times with different failures churning between
  recurrences). A per-signature TOTAL count (`verify_signature_counts`) that
  never resets on a signature change now trips the same cut on total
  occurrences, so interleaved-churn stalls are caught. Maintained cheaply on the
  default path but consulted only when a `progress_signal` is supplied, so
  behavior is byte-identical without the callback.
