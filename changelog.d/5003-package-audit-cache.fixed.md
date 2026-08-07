- Warm and persist both Linux merge-gate rust-caches (`workspace-tests` and
  `package-audit`) on post-merge refresh with Swatinem env fingerprints that
  match their consumer lanes (no job-level `CARGO_BUILD_JOBS`), so exact-SHA
  proof reuse cannot leave merge_group compiling cold (#5003).
