# Diagnostics timing experiment

Two questions about giving a coding agent limited parser/AST information:

- **Precision** — which findings deserve to be stated as errors.
- **Timing** — when a finding should reach the model.

They are separable, and this rig separates them. Every arm shares one producer
and one set of precision tiers, so a measured difference between arms is timing
alone.

## The problem being measured

A model legitimately makes several edits that are individually incomplete but
jointly correct. Rename a function's definition and every call site is briefly
undefined; rename a call site first and it briefly points at a name that does
not exist yet. In that window an honest diagnostic is misleading, and a model
that stops to fix it is chasing a problem its next edit was going to resolve.

Filtering by novelty does not help here. The finding is genuinely new, and it is
genuinely accurate at the instant it is produced. Only timing can help.

## Precision: three tiers

1. **syntax** — the parser is authoritative. Always surfaced as an error.
2. **semantic** — surfaced as a confident error only where the resolver can see
   the whole symbol space: the language's declared ceiling allows it *and* the
   file carries no construct that defeats resolution.
3. **unresolved** — structurally incomplete resolution. Collapsed to one labeled
   low-confidence line regardless of how many findings it carried.

`lib/diagnostics.harn` holds both declarations as data. `RESOLVER_COMPLETENESS`
records how far a language's resolver can be trusted; `RESOLUTION_DEFEATERS`
records the constructs that void it for a given file. Neither is consulted by
name at a call site.

`probe.harn` is the deterministic check that the tiers discriminate:

```sh
harn playground --host host.harn --script probe.harn --task probe --llm mock:mock
```

A genuine typo must come back `semantic/ERROR`; a star-imported name, a
`setattr` target, and a Ruby runtime constant must all come back
`unresolved/LOW-CONFIDENCE`; a broken parse must come back `syntax/ERROR`.

## Timing: six arms

`lib/policy.harn` is the arm table. Arms are rig parameters, not product flags.

| Arm | Diagnostics reach the model |
| --- | --- |
| `A-push-all` | after every edit (the control, and the industry default) |
| `B-verify-time` | when the model runs the tests |
| `C-settle` | after an edit burst ends (no further edit within K tool events) |
| `D-pull` | only when the model calls `get_diagnostics` |
| `E-hybrid` | syntax after every edit, semantic at verify or on pull |
| `F-hybrid-batch` | `E`, plus a `batch_edit` tool carrying several edits |

Each arm also owns the prompt clause that teaches it. An arm that hides
diagnostics behind a tool without teaching the tool would be measuring "no
diagnostics" instead.

## Fixtures

`lib/fixtures.harn`. The two rename fixtures guarantee the mid-flight window in
either edit order, which is what makes them a controlled test rather than a coin
flip on which edit the model happens to make first.

| Fixture | What it measures |
| --- | --- |
| `rename-single-file` | mid-flight incompleteness within one file |
| `rename-cross-file` | mid-flight incompleteness across two files |
| `genuine-typo` | the help direction: a real defect the checker should catch |
| `dynamic-runtime-names` | permanent noise the checker cannot resolve |
| `clean-control` | no diagnostics arise; arms must not differ here |

`clean-control` is the rig's own falsifier. If arms differ on it, the rig has an
artifact and the race is invalid.

Two fixture rules exist because a mock sweep proved they were needed, not
because they seemed right:

- **First touch reports the whole file; later edits report only the delta.** A
  pure delta against the previous edit is what the industry ships, and it cannot
  report a defect that predates the session, because such a defect is never new.
  With a plain delta the pre-existing-typo fixture surfaced nothing in all six
  arms. The do-not-re-inject set still applies, so first touch costs at most one
  mention per finding.
- **The typo fixture's task is to ADD something.** A post-edit checker only
  speaks after an edit, so a bug the model fixes in its own first edit is never
  diagnosed. The model has to touch the file for an unrelated reason before the
  pre-existing defect can be surfaced at all.

Python runs with `-B`. Fixing `itmes` to `items` leaves the file byte length
unchanged, so CPython's size-and-mtime bytecode invalidation can miss the edit
and re-run stale `__pycache__`. That produced arm-correlated false failures
before it was found: the traceback showed corrected source while raising
`NameError: name 'itmes' is not defined`.

## Running

```sh
# Deterministic, no model. Proves the arms deliver at different times.
./run.sh --mode mock --fixtures rename-single-file --out runs/armcheck.jsonl

# Behavioural. Needs a real model.
./run.sh --mode live --llm ollama:qwen2.5-coder --trials 3 --out runs/stage1.jsonl
```

Mock mode cannot measure behaviour: a scripted model does not read what it is
shown. It proves mechanism only. Every behavioural number needs `--mode live`.

One JSON row per trial lands in the `--out` file: `passed`, `iterations`,
`input_tokens`, `output_tokens`, `tool_calls`, `elapsed_ms`, `flush_count`,
`surfaced_transient`, `surfaced_noise`, `surfaced_genuine`, `detour_calls`, and
the tool sequence.
