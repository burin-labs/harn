- Fixed three first-run papercuts. A fresh `harn init` project now runs green:
  the basic scaffold's `lib/helpers.harn` exports `greet`/`add` with `pub` so
  the whole-module `import "lib/helpers"` binds them (previously
  `harn run main.harn` failed with `HARN-NAM-002` and `harn test` failed after
  the module-visibility change). The LLM quickref no longer documents the
  non-parsing `retry { } catch err { }`; it now shows `retry <count> { }`
  (count mandatory, returns nil on exhaustion, no `catch`). And `harn repl`
  no longer crashes with `Read error: Device not configured` when stdin is a
  pipe — it reads and evaluates piped source to EOF while keeping interactive
  behavior unchanged.
