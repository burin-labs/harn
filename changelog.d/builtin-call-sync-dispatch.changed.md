- **Synchronous builtin calls now dispatch on the fast (sync) interpreter
  path.** A bare builtin call such as `abs(x)` / `len(xs)` previously fell
  through `Op::CallBuiltin`'s sync handler to the async handler, which re-ran
  name resolution (a second local-slot scan, env walk, and — inside imported
  modules — the `module_functions` + `module_state` mutexes) and spun up the
  async state machine only to reach the same synchronous builtin. The sync
  handler now dispatches synchronous builtins directly once it has confirmed the
  name is not a user closure, eliminating the redundant resolution and the async
  hop; asynchronous builtins are unchanged. Resolution semantics are identical —
  a user `fn` that shadows a builtin name still wins — and the change holds one
  fewer lock per call inside imported modules, so it is friendly to the
  multi-threaded runtime. No `harn` language behavior changes.
