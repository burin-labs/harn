- **Runtime errors now name the source file, not just the line.** Error
  enrichment appended a bare `(line N)` drawn from the innermost stack frame,
  which is ambiguous across 100+ stdlib `.harn` files and forced a manual hunt
  to locate a crash. When the frame carries a source path the suffix is now
  `(<file>:N)` (e.g. `(stall.harn:497)`); the bare `(line N)` form is kept
  as the fallback when no path is known.
- **Failure-evidence snippets no longer amputate the tail.** `agent/stall`'s
  diagnostic snippet and `agent/sitrep`'s message truncation both did a
  head-only clip, silently dropping the end of the text — which for tool and
  command output is usually where the decisive error lives (failing assertion,
  last compiler error). Both now preserve a generous head **and** tail with an
  explicit `…[N chars elided]…` marker, so the model never loses the part of a
  tool-call error that pinpoints the fix.
