- **`harn-serve` session-store: polish + acceptance gap-fill on issue
  #2502.** Follow-up to the #2535 landing closes four acceptance items
  that were left as TODOs in the initial primitive:
  - **`ArchiveSink` wired into the retention sweep.** `StoreHooks` now
    carries an optional `archive_sink`, and the default
    `SessionStore::sweep_retention` ships archived sessions and
    tombstone records through it before the rows leave primary
    storage. Closed sessions that cross `min_age_before_archive_seconds`
    are emitted via `ArchiveSink::archive(session, events)` before
    soft-delete; hard-deletes fire `ArchiveSink::tombstone(...)` with
    the final chain root hash so the audit pipeline keeps a permanent
    record of the deletion. `SweepReport` gained `archived` +
    `tombstoned` counters; a new `RetentionPolicy::should_archive`
    predicate keeps the archive-trigger condition out of the
    soft-delete decision.
  - **SQL-level tag index + filter** (acceptance: "Per-event tag index
    for filtered list queries"). New `session_tags(session_id, tag)`
    table with a `(tag, session_id)` index. SQLite `list` now JOINs
    the tag table when `filter.tag` is set instead of post-filtering
    in Rust, and applies the cursor as keyset pagination on
    `(created_at_ms, id)` so paging through 10⁸ sessions doesn't load
    every prior row into memory.
  - **Incremental chain root hash on append.** The append hot path
    previously re-folded every event's record_hash on each commit
    (O(N) on every write). The chain root is now a versioned Merkle
    chain (`v2`) built by `chain_root_init` + `chain_root_fold`, so
    append only folds the new event's hash into the stored running
    root. `chain_root_hash(events)` still replays from genesis for
    verification; the equivalence is exercised by a new test that
    cross-checks `describe.chain_root_hash` against
    `verify.chain_root_hash`.
  - **Tracing instrumentation on every API call** (acceptance: "Every
    API call emits A.10 spans + metrics with `harn.session.*`
    attributes"). Every axum handler in `sessions::api` is now wrapped
    with `#[tracing::instrument]` emitting `harn.session.<verb>` spans
    carrying `harn.session.id`, `harn.session.tenant_id`,
    `harn.session.event_kind`, etc. The default `sweep_retention` adds
    its own span recording `harn.session.sweep.archived` /
    `soft_deleted` / `hard_deleted` counts; A.10 (#2513) will export
    them through its OTLP pipeline without further changes here.
  - **Fork chain bug fix.** Adversarial review found `fork` produced
    a broken chain on the SQLite backend (session_id rewritten on
    copied events but `record_hash` not recomputed → `verify` failed
    with HashMismatch) and a divergent shape on the memory backend
    (events kept parent's session_id, so reads on the child returned
    rows that looked like they belonged to the parent). Both backends
    now route copied events through a shared `re_anchor_events`
    helper that rewrites `session_id`, recomputes `prev_hash` +
    `record_hash` sequentially, and drops the parent's per-event
    signatures (which no longer attest the re-anchored canonical
    bytes). The child's chain stands alone as a verifiable session;
    lineage is preserved via `parent_session_id` on the meta.
  - **DRY**: removed the duplicate `chain_root_hash_from_hashes`
    helper in the SQLite backend (folded into the public
    `chain_root_fold` so memory + sqlite share one
    chain-construction primitive); collapsed `hooks()` from an
    inherent method into the new `SessionStore::hooks` trait method
    so the default sweep impl can read the archive sink without
    backend-specific plumbing; collapsed the four
    `format!("sha256:{}", hex::encode(...))` call sites in `signing`
    onto one `finalize_sha256` helper; switched the SQLite list
    `args` vec to `&'static str` parameter names so per-request
    String allocation drops to zero.

  New tests: tag-filter roundtrip, keyset cursor pagination, sweep
  archive + tombstone flow against a recording sink, chain-root
  incremental equivalence, and fork chain verifiability regression.
  Existing 34 tests still pass.
