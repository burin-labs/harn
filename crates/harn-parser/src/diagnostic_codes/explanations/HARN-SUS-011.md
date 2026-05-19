# HARN-SUS-011 — replay resume input hash diverges from journaled suspension

The replay runtime computed a different resume-input fingerprint than the
one captured in the original run's [`ResumptionReceipt`]. Lifecycle replay
must be deterministic: the second run must feed the suspended worker the
exact same payload it received the first time, because the worker's
post-resume trajectory was hashed into the journaled state.

Common causes:

- A pipeline that used to depend on wall-clock time or another
  non-deterministic input now produces a different resume payload on
  replay. Capture the payload itself (or use `mock_time(...)`) so the
  replayed runtime can reproduce the original value.
- The journal entry was edited after the original run completed. The
  signed timestamp on the receipt detects this — verify the receipt's
  signature with `verify_lifecycle_receipt_signature(...)` before
  blaming the runtime.
- The settlement agent (drain phase) changed its mind and produced a
  different resume input. Record a fresh `ResumptionReceipt` in the
  current run instead of replaying the stale one.

If the new payload is intentionally different (eg. you are running a
deliberate counterfactual, not a determinism replay), open the trace as
a fresh recording rather than feeding it back through the replay oracle.
