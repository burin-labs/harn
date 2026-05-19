# HARN-SUS-013 — lifecycle receipt signed timestamp failed verification

A [`SuspensionReceipt`], [`ResumptionReceipt`], or [`DrainDecisionReceipt`]
carries a `SignedLifecycleTimestamp` whose HMAC signature does not match
the receipt's identifying fields under the per-process signing salt.

The signature binds `(kind, at_ms, subject_id, initiator_id)`, so any of
the following will trip this diagnostic:

- The receipt was minted by a *different* `harn` process and is now
  being verified after a restart. Per-process salts are intentional —
  cross-process verification requires migrating to the longer-lived
  `provenance` chain (see `build_signed_receipt`).
- The journal entry was edited after the fact (eg. someone hand-tweaked
  `at_ms` or `initiator_id` in the on-disk record). The replay oracle
  rejects the tampered receipt before relying on it.
- The signing algorithm or key id changed across a Harn upgrade. The
  current algorithm is `hmac-sha256` with key id `local-session`.

If the original process is gone but you still need to read the
receipts, treat the on-disk payloads as advisory only — the unsigned
fields (`handle`, `reason`, `input_hash`, `action`) are still
informational, but `replay_resume_input` / `replay_drain_decision` will
refuse to memoize against an unverified receipt.
