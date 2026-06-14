- **Lazy manifest-hook install for `harn test`.** A hook's handler closure is
  resolved (loading its module's whole import graph) on first fire against the
  firing VM, instead of eagerly during every test's setup. Pure-logic unit
  tests that never fire a hook no longer pay that cost — for a large manifest
  like burin-code this cut per-test setup from ~1s to single-digit ms (suite
  wall 840s -> 550s). Hook semantics are unchanged: closures still resolve
  against the firing VM, preserving per-test module-state isolation. Production
  callers (`harn run`, agent loops) stay eager via `install_manifest_hooks`, so
  a misconfigured handler still fails fast at startup; the lazy path is opt-in
  via `install_manifest_hooks_with_mode(.., lazy = true)`.
