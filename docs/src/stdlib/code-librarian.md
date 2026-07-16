# Code librarian stdlib

`import "std/code_librarian"` exposes the typed code-graph + Cypher
surface that ships behind the `hostlib_code_index_*` builtin family
(issue [#2434](https://github.com/burin-labs/harn/issues/2434), PR
[#2441](https://github.com/burin-labs/harn/pull/2441)) as a single
ergonomic API. Consumers — IDE hosts, headless TUI workflows, future
personas — call one library instead of stitching outputs from many
primitives.

The library never rebuilds the index on its own. Drive
`hostlib_code_index_rebuild({root: <path>})` first (or the lifecycle
equivalent), then talk to the librarian.

## Functions

| Function | Returns | Wraps |
|---|---|---|
| `code_librarian_query(cypher)` | `LibrarianCypherResult` | `hostlib_code_index_cypher` |
| `code_librarian_outline(path, depth = 1)` | `LibrarianOutline` | `path_to_id` + `outline_get` + `imports_for` + `importers_of` |
| `code_librarian_who_calls(symbol, max_hops = 2)` | `list<LibrarianCallSite>` | `hostlib_code_index_cypher` (canned `<-[:CALLS]-` query) |
| `code_librarian_member_surface(container, defining_path?)` | `LibrarianMemberSurface` | `hostlib_code_index_cypher` (`CONTAINS` ownership) |
| `code_librarian_external_consumers(symbol, defining_path, max_sites = 20)` | `LibrarianExternalConsumers` | `hostlib_code_index_cypher` (raw `CallSite` + definitions) |
| `code_librarian_what_imports(path)` | `list<LibrarianImport>` | `hostlib_code_index_importers_of` |
| `code_librarian_recent_changes(since_seq = 0)` | `list<LibrarianFileChange>` | `hostlib_code_index_changes_since` |
| `code_librarian_freshness(path)` | `LibrarianFreshness` | `hostlib_code_index_freshness` |
| `code_librarian_file_hash_snapshot(paths)` | `LibrarianFileHashSnapshot` | `std/verification::verification_file_hash_snapshot` |
| `code_librarian_branch_overlay(branch)` | `LibrarianOverlay` | `hostlib_code_index_branch_overlay` |

The Cypher executor that backs `code_librarian_query` and
`code_librarian_who_calls` is documented at
[`crates/harn-hostlib/src/code_index/cypher.rs`](https://github.com/burin-labs/harn/blob/main/crates/harn-hostlib/src/code_index/cypher.rs).
Supported clauses: `MATCH`, `WHERE`, `RETURN`, alias projections
(`RETURN expr AS name`), single-edge and variable-length traversal up
to depth 4.
Supported node labels are `Function`, `Type`, `Field`, `EnumCase`,
`Module`, `Import`, `CallSite`, and `Macro`; symbols expose
`access_level` when the grammar can normalize declaration visibility.

## Worked example: who calls this function?

```harn
import "std/code_librarian"

pipeline default() {
  const _ = hostlib_code_index_rebuild(
    {root: "crates/harn-hostlib/tests/fixtures/code_index_queries/corpus"},
  )

  const callers: list<LibrarianCallSite> = code_librarian_who_calls("fetchUser")
  __io_println("fetchUser has " + to_string(len(callers)) + " call sites:")
  for c in callers {
    __io_println("  " + c.path)
  }
}
```

Running this against the ground-truth corpus prints:

```text
fetchUser has 2 call sites:
  src/auth.ts
  src/router.ts
```

## Ground a real member surface

Container names are not globally qualified in the symbol graph. Pass the
resolved defining path when it is known so a same-named type elsewhere in the
workspace cannot contaminate the result:

```harn
const surface: LibrarianMemberSurface = code_librarian_member_surface(
  "StatusOr",
  "include/status.hpp",
)
if !surface.ambiguous {
  for member in surface.members {
    __io_println(member.signature)
  }
}
```

Without `defining_path`, definitions in more than one file set
`ambiguous: true`. The definitions and members remain available for inspection,
but consumers should not present the merged set as an authoritative API.

## Find conservative external consumers

`code_librarian_external_consumers` reads raw `CallSite` nodes by final name
segment. This deliberately differs from `code_librarian_who_calls`: a parser can
record `result.ok()` as a call to `ok` even when it could not resolve a typed
`CALLS` edge. The result excludes the defining file, counts distinct consumer
files, and bounds the returned sites independently:

```harn
const consumers: LibrarianExternalConsumers = code_librarian_external_consumers(
  "ok",
  "include/status.hpp",
  5,
)
if consumers.definition_found && consumers.sole_definer {
  __io_println(to_string(consumers.file_count) + " files depend on ok")
}
```

`sole_definer` is conservative. It is true only when the named symbol is
actually defined in `defining_path` and no other indexed file defines the same
name. A missing definition or a common same-named declaration produces `false`.

The full walk-through lives at
[`examples/code_librarian_explore.harn`](https://github.com/burin-labs/harn/blob/main/examples/code_librarian_explore.harn).

## Defaults and limitations

- `depth` on `code_librarian_outline` is reserved for upcoming graph-aware
  traversal. Today only the immediate outline is returned regardless of
  the passed value; the type signature stays stable so consumers don't
  have to rewrite calls when richer traversal lands.
- `max_hops` on `code_librarian_who_calls` is reserved for the upcoming
  transitive `<-[:CALLS*1..N]-` query shape. Today only direct callers
  are returned.
- `code_librarian_member_surface` treats definitions in distinct files as
  ambiguous unless the caller supplies `defining_path`. This avoids guessing
  across unrelated same-named types, but extensions or partial types split
  across files also require the caller to select an owning path.
- `code_librarian_external_consumers` is a conservative by-name call-site
  query, not full semantic reference resolution. It covers final-segment calls;
  non-call field/type uses remain outside this contract.
- `code_librarian_recent_changes` takes a monotonic version sequence
  number (`since_seq`) because Harn has no native `Duration` primitive
  yet. Pair with `hostlib_code_index_current_seq({})` to checkpoint and
  resume.
- `code_librarian_file_hash_snapshot` captures current file hashes for many
  workspace paths under one code-index sequence binding. Its `snapshot`
  field is the direct path-to-hash map accepted by
  `verification_diagnostic_classify`, and its `files` field preserves
  per-path index/readability metadata for HUDs and diagnostics.
- The library does not rebuild the index; consumers must call
  `hostlib_code_index_rebuild` before the first query (and after
  large workspace mutations) themselves.

## See also

- [Typed symbol graph + Cypher executor](https://github.com/burin-labs/harn/pull/2441) —
  the underlying primitives.
- [`hostlib_code_index_*` registration test][reg-test] — canonical list of every
  builtin the library wraps.
- [Ground-truth recall fixture][recall-fixture] — the 30 Q&A pairs the librarian
  inherits from #2434.

[reg-test]: https://github.com/burin-labs/harn/blob/main/crates/harn-hostlib/tests/registration.rs
[recall-fixture]: https://github.com/burin-labs/harn/blob/main/crates/harn-hostlib/tests/fixtures/code_index_queries/queries.json
