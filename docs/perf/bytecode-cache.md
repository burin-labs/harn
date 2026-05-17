# Bytecode cache (`.harnbc`)

Short-lived `harn` invocations spend the bulk of their wall time before
the VM executes a single instruction: read the source, lex it, parse it,
run the type checker, compile the AST to a bytecode chunk. Cold-start
for the kind of subcommand burin-code is porting into `.harn`
(`burin keys list`, `burin status`, `burin diagnose`) is dominated by
that pipeline — the LLM/HTTP/IO work the script eventually performs
goes through the same builtins on every run.

The bytecode cache eliminates that fixed cost when nothing in the input
graph has changed. The runtime hashes the entry source + every
transitively-imported user file, looks for a `.harnbc` artifact whose
header matches, and on a hit goes straight from "load bytecode" to
"start VM."

## File format

Little-endian throughout. Every cache file starts with this header:

```text
magic        : [u8; 8]   = "HARNBC\0\0"
schema_ver   : u32       = 1
version_len  : u32
harn_version : [u8; version_len]
source_hash  : [u8; 32]   sha256(entry source)
import_hash  : [u8; 32]   sha256(sorted import graph contents)
payload      : bincode-serialized CachedChunk
```

Mismatch on any of magic / schema / harn_version / source_hash /
import_hash triggers a silent recompile and rewrite. A future Harn
release that bumps the schema can simply increment `SCHEMA_VERSION`
in `crates/harn-vm/src/bytecode_cache.rs`; older binaries reject the
file as a header mismatch instead of panicking inside `bincode`.

## Cache directory

Resolution order:

1. `$HARN_CACHE_DIR` (explicit override; used by tests + CI).
2. `$XDG_CACHE_HOME/harn/bytecode`.
3. `$HOME/.cache/harn/bytecode`.
4. `./.harn-cache/bytecode` (fallback for hermetic environments with no
   `$HOME`).

The directory is created lazily on the first cache write. The cache is
process-local; there is no IPC, no shared lock file, and no need for
one — atomic rename gives the runtime concurrent-safe writes without a
mutex.

Concurrent invocations of the same script race on the rename: the last
writer wins, but every reader sees a consistent file because rename is
atomic on every supported filesystem.

## Cache key

The on-disk filename is `<hex(source_hash)>.harnbc`. We key by the
content of the entry file alone so two invocations from different
`PATH`-relative locations share one cache entry; the in-file
`import_hash` then guards against stale reuse when an imported file
changes but the entry stays identical.

`source_hash` is sha256 of the entry file's bytes.
`import_hash` is sha256 of the canonical path + content of every user
file transitively reachable through `import` declarations. `std/…`
imports are excluded because the embedded `harn_version` covers them.
Unresolved imports still contribute a fixed sentinel so dropping a
matching file into place later invalidates the cache.

The import scan is a lightweight string walk, not a full lex/parse:
it strips comments and looks for `import "path"` and
`import { … } from "path"` patterns. False positives (e.g. an unrelated
string starting with `import` inside a heredoc) only churn the cache;
they never produce an incorrect bytecode load.

## Loader / writer flow

`harn run script.harn` resolves the cache like this:

1. Read the source from disk (always — needed for runtime error
   reporting via `vm.set_source_info`).
2. Compute the cache key.
3. Look for an adjacent `script.harnbc` (shipped artifacts win over the
   shared cache so release builds avoid touching `$HOME`).
4. Look for `$HARN_CACHE_DIR/<source_hash>.harnbc`.
5. On a hit, deserialize the payload and execute. Parse, type-check,
   and compile are all skipped: the writer ran them.
6. On a miss, parse + type-check + compile, then atomically write
   the artifact back into the shared cache. Write failures are
   best-effort and silent unless `HARN_BYTECODE_CACHE_DEBUG=1`.

`harn precompile <path>` runs the same compile path and writes the
artifact directly to disk. Pass a directory to walk it; otherwise it
compiles a single file. `--out DIR` mirrors the input layout under
`DIR`; without `--out`, artifacts land adjacent to each source. Burin
Code's release pipeline runs `harn precompile` against its bundled
`Sources/BurinCore/Resources/pipelines/` so the shipped DMG already
contains a `.harnbc` for every script the user might run.

## Toggles and environment

- `HARN_CACHE_DIR=<path>` — relocate the cache directory.
- `HARN_BYTECODE_CACHE=0` — disable both reads and writes (compiler
  debugging, deterministic eval reruns).
- `HARN_BYTECODE_CACHE_DEBUG=1` — surface cache write failures.

## Type-check warnings on cache hit

Cache hits skip parse + type-check, which means non-fatal type-check
warnings (e.g. deprecated-builtin notices) are not re-emitted from a
cached invocation. The warning was emitted once when the cache wrote
the artifact, and it re-emits whenever the cache busts. `harn check`
remains the canonical surface for the complete diagnostic list — use
it if you need every warning every time, or set
`HARN_BYTECODE_CACHE=0` to force a fresh compile.

## Out of scope

- JIT (LLVM/Cranelift). Interpreted bytecode plus the cache is enough
  for the cold-start gate that gates the burin-code thin-rust epic.
- Cross-process shared cache / IPC.
- Standalone artifact loading without source. The current loader
  recomputes the key from the on-disk source, so the source has to
  exist. Shipping bytecode without source would require dropping the
  rehash and trusting the embedded hash — a follow-on if the
  burin-code release pipeline grows that constraint.
- Caching of imported modules. The cache short-circuits parse +
  type-check + compile of the **entry** pipeline only; runtime
  `Op::Import` still re-parses and re-compiles every imported user
  file and stdlib module on each process invocation. For burin-code's
  short subcommands the entry pipeline is the dominant per-invocation
  cost, but for pipelines that depend on a large stdlib surface the
  remaining import cost is the next target. The `CachedChunk` /
  `CachedCompiledFunction` serde shape already supports the same
  on-disk format, so the follow-on plug-point is `execute_import` in
  `crates/harn-vm/src/vm/modules.rs`.
