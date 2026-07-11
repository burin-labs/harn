`harn check` now keeps a persistent per-file result cache under the shared
Harn cache directory, so re-checking an unchanged tree replays diagnostics
in milliseconds instead of re-running the typechecker, linter, and preflight
for every file. Cache keys cover the file's content, its transitive import
closure, the effective `[check]` config, CLI overrides, and the compiler/CLI
build fingerprint; preflight's filesystem probes (templates, prompt assets,
directory targets) are recorded and revalidated on every hit so external
edits invalidate correctly. `HARN_CHECK_RESULT_CACHE=0` disables just this
cache; `HARN_BYTECODE_CACHE=0` disables it along with the bytecode cache.
