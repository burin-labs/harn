- **Stopped the full heavy-Rust CI graph from running on `editors/vscode/**`-only
  changes.** The authoritative "Detect changes" gate now classes the vscode
  extension under `editors/vscode/**` alongside `docs/`, `website/`, and `*.md`
  as a non-Rust surface. Routine dependabot dev-dep bumps now run a targeted
  VS Code compile-and-test job instead of the full behavior build, Rust test,
  audit, and cross-compile fan-out. The Windows cross-compile check now consumes
  the same `rust` gate, and the `CI status`
  aggregator treats its irrelevant-skip as a pass while still failing on any
  real failure, cancellation, or a skip that was actually required. merge_group
  and main-push runs keep the full backstop unchanged.
