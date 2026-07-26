# Bytecode cache (`.harnbc`)

Short-lived `harn` invocations spend the bulk of their wall time before
the VM executes a single instruction: read the source, lex it, parse it,
run the type checker, compile the AST to a bytecode chunk. Cold-start
for the kind of subcommand an IDE host is porting into `.harn`
(`keys list`, `status`, `diagnose`) is dominated by
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
schema_ver   : u32       = SCHEMA_VERSION
version_len  : u32
harn_version : [u8; version_len]
fp_len       : u32
codegen_fp   : [u8; fp_len]   CODEGEN_FINGERPRINT of the producing build
compiler_tag : u8        bitmask of active CompilerOptions
kind         : u8        1 = entry chunk, 2 = module artifact
source_hash  : [u8; 32]   sha256(entry source)
context_hash : [u8; 32]   sha256(sorted import graph contents)
payload      : postcard-serialized payload (Chunk or ModuleArtifact, per kind)
```

Mismatch on any of magic / schema / harn_version / codegen_fp /
compiler_tag / source_hash triggers a silent recompile and rewrite, as
does a `context_hash` mismatch wherever the loader has computed one. A
future Harn release that bumps the schema can simply increment
`SCHEMA_VERSION` in `crates/harn-vm/src/bytecode_cache.rs`; older binaries
reject the file as a header mismatch instead of attempting to decode an
incompatible payload.

`codegen_fp` is in the header rather than only inside `context_hash`
because the entry fast path (below) deliberately does not compute a
context hash, and a stale-compiler check that costs a graph walk is one
the fast path cannot make.

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
`context_hash` then guards against stale reuse when an imported file
changes but the entry stays identical.

Because the filename carries no graph and no location, a candidate found
this way may have been written by an entirely different entry that
happens to have identical bytes — two checkouts of one repository, say,
where only one has local edits. Everything that distinguishes them lives
inside the file, so nothing may be trusted before the header and the
manifest have both agreed.

`source_hash` is sha256 of the entry file's bytes.
`context_hash` is sha256 of the canonical path + content of every user
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
5. Decide whether the candidate is valid (below).
6. On a hit, deserialize the payload and execute. Parse, type-check,
   and compile are all skipped: the writer ran them.
7. On a miss, parse + type-check + compile, then atomically write
   the artifact back into the shared cache. Write failures are
   best-effort and silent unless `HARN_BYTECODE_CACHE_DEBUG=1`.

### Deciding an entry chunk is valid

Recomputing `context_hash` means re-reading and re-hashing the whole
import graph — a cold-path algorithm on the warm path. So an entry chunk
carries a [`ContextManifest`]: the entry it was walked from, plus each
reachable file's `(len, mtime_ns)` and the negative facts the graph
depends on (imports that resolved to nothing, paths that failed to read).
Re-checking that is stats only, and a chunk whose manifest re-checks
clean is served without any walk at all.

The manifest has to establish two separate things, and neither implies
the other:

- **that it describes this graph** — the recorded paths are absolute and
  re-check clean from anywhere, so without the anchor a manifest proves
  only that *some* graph is unchanged. That is what let one entry run
  another's bytecode (#5591).
- **that this build emitted the chunk** — from the header's `codegen_fp`,
  since the fast path never computes the `context_hash` the fingerprint
  otherwise hides in (#5610).

Anything a manifest cannot describe (a file that will not stat, a
manifest that was never written, an anchor that does not match) falls
back to the full walk, which recomputes the key from scratch. A manifest
can only ever save work; it can never decide a hit on its own.

When the walk does run and finds the key unchanged — a touched mtime, a
restored checkout — the artifact is rewritten with fresh observations, so
the next spawn is back on the fast path instead of walking forever.

Each `import` the VM executes at runtime follows the same protocol
for the `.harnmod` family: read source, look for an adjacent
`<lib>.harnmod`, then `$HARN_CACHE_DIR/<source_hash>.harnmod`. A hit
returns a [`ModuleArtifact`] (compiled init chunk + per-function
chunks + import list); the loader then runs the init chunk and mints
fresh closures bound to a per-process module env.

`harn precompile <path>` runs the same compile path and writes both
artifact families directly to disk: `<name>.harnbc` (entry chunk) and
`<name>.harnmod` (module artifact). Shipping both means the same file
hits the cache whether the user runs it (`harn run lib.harn`) or
imports it from another script. Pass a directory to walk it; otherwise
it compiles a single file. `--out DIR` mirrors the input layout under
`DIR`; without `--out`, artifacts land adjacent to each source. Burin
Code's release pipeline runs `harn precompile` against its bundled
`Sources/BurinCore/Resources/pipelines/` so the shipped DMG already
contains both artifact files for every script the user might run.

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

## What gets cached

Three artifact families share the same header but use distinct file
extensions so they coexist in one directory:

- **Entry chunks (`.harnbc`)** — the compiled [`Chunk`] for the script
  passed to `harn run`. The shortcut: cache hit, skip parse + typecheck
  and compile, go straight to VM.
- **Module artifacts (`.harnmod`)** — the [`ModuleArtifact`] for each
  imported user file or stdlib module. The shortcut: cache hit, skip
  parse + per-function compile of the imported module; the loader still
  has to run the module's `init` chunk and mint per-process closures.
  Module caching is what closes the cold-start gap for pipelines whose
  cost is dominated by `import`s rather than the entry source itself.
- **Stdlib modules** — same artifact format as user modules; the
  `STDLIB_MODULE_ARTIFACT_CACHE` in-memory layer remains the L1 cache
  per process, with the on-disk artifact as L2 across processes.

A single `.harn` source can therefore produce up to two cached files —
a `.harnbc` if anyone runs it as an entry, and a `.harnmod` if anyone
imports it.

## Out of scope

- JIT (LLVM/Cranelift). Interpreted bytecode plus the cache is enough
  for the cold-start gate behind the IDE-host CLI-porting effort.
- Cross-process shared cache / IPC.
- Standalone artifact loading without source. The current loader
  recomputes the key from the on-disk source, so the source has to
  exist. Shipping bytecode without source would require dropping the
  rehash and trusting the embedded hash — a follow-on if an
  IDE host's release pipeline grows that constraint.
