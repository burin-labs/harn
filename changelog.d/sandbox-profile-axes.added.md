- Added the `workspace_paths` sandbox profile: Harn's workspace-root path
  enforcement without OS confinement of subprocesses. It fills the gap for
  trusted callers — a test runner isolating cases, a build driver invoking a
  toolchain — that want their own writes confined but must shell out freely.
  It is not containment for foreign code, since a subprocess it spawns is
  unconfined.
- `SandboxProfile` now exposes the two questions a profile answers as named
  predicates, `enforces_path_scope()` and `confines_processes()`, replacing
  ad-hoc variant matching that re-derived the distinction at each call site.
  As a result, a permission error from a subprocess under the `wasi` profile
  is no longer misreported as an OS sandbox denial: testbench mode intercepts
  subprocesses before the host spawn path, so nothing there was ever confined.
