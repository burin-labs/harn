- **`edit_safe_text_patch`: polish + correctness pass.** Follow-up to
  the #2509 / #2542 landing fixes several issues caught in adversarial
  review:
  - **H2**: `create_parents: false` is now honored on the direct-disk
    path — previously `atomic_write`'s unconditional `create_dir_all`
    silently created the parent. The disk path now pre-checks the
    parent and returns a structured error with the right remediation
    hint when the directory is missing.
  - **H3**: latent precedence bug in the `hunk_conflict` error message
    fixed — `+ outcome?.error_code ?? "no_match"` parses as
    `(... + outcome?.error_code) ?? "no_match"` so the fallback never
    fires. Parenthesized + hoisted to a `let hunk_error_code` so the
    same value flows into both the top-level `failed_hunk_error_code`
    and the per-error `hunk_error_code`.
  - **M1**: new `AgentEvent::SafeTextPatchResult` carrying
    `{session_id, path, result, hunks_count, bytes_written,
    failed_hunk_index?}` fires from every terminal return path
    (applied / no_op / stale_base / hunk_conflict). Hosts subscribe to
    stream-aggregate stale-base / hunk-conflict rates and average
    hunks-per-patch without polling. The ACP adapter translates the
    event into a `progress` extension with
    `_meta.harn.kind = "safe_text_patch_result"`. New
    `hostlib_fs_emit_safe_text_patch_result` builtin routes the event
    from the Harn wrapper; silently no-ops outside a session.
  - **M3**: dropped a redundant SHA-256 pass on the commit path —
    `__edit_sha256(working)` was computed even though the hostlib
    commit echoes the same digest. Now only computed on the dry-run
    and hunk-conflict paths where the commit isn't called.
  - **M5/M6**: dropped redundant `result.changed` (it always equalled
    `result == "applied"`). Aligned `dry_run` semantics with
    `edit_apply_node` — `applied: true` now means "matcher succeeded"
    regardless of whether bytes were written, and a new top-level
    `result.dry_run` boolean disambiguates.
  - **L1/L2**: small DRY win — new `hash_label(&[u8]) -> String`
    helper collapses 4 copies of the `format!("sha256:{}", hex::...)`
    pattern.
  - **L3**: schemas tightened — `expected_hash` / `current_hash` /
    `before_sha256` / `after_sha256` now carry the
    `^sha256:[0-9a-f]{64}$` regex pattern via a shared `$defs/sha256Label`
    schema reference. `expected_hash` is now required in the response
    (was nullable but always emitted).
  - **L5**: dropped dead `failed_hunk_message` field — the error list
    already carries the same string under `errors[0].hunk_message`.
  - **L6/L7**: docs gain a bounded `stale_base` retry loop example
    and a dry-run → apply workflow example mirroring how
    `edit_apply_node` documents the same flag.
  - **Tests**: added integration coverage for non-UTF8 read
    rejection, ~1.5 MB content roundtrip, `create_parents: false`
    rejection, and the new agent-event wiring.

  H1 (sandbox-gating of the un-gated `fs/*` and `ast/*` edit
  primitives) is filed as #2548 — cross-cutting concern with sibling
  primitives out of scope here.
