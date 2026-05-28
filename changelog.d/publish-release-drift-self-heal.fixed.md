- **`publish-release.yml` self-heals when tag-push run misses publish.**
  Two failures observed during the v0.8.47 cut: the tag-push run of
  `publish-release.yml` was cancelled by the shared `publish-release`
  concurrency group (the simultaneous main-push run for the same SHA
  was still queued), and the subsequent main-push runs all no-op'd
  because `Cargo.toml` already matched the latest tag. Two fixes:
  - Concurrency group now scopes by `github.ref_name` so tag-push
    and main-push runs of the same release commit can run in
    parallel without contending. Different versions (the only case
    where main-push would actually publish) are already in different
    groups; same-version contention is the only thing the old
    unscoped group prevented, which is exactly the case crates.io
    publish-skip already makes idempotent.
  - Drift detection now treats "tag exists but GitHub release has
    fewer than 5 assets" as drift, forcing the publish job to re-fire
    from the existing tag (`PUBLISH_REF` pinned to `$LATEST_TAG`).
    `release_ship.sh --finalize` skips already-published crates, so
    the recovery is a no-op for crates that landed and a real
    recovery for any that didn't.
