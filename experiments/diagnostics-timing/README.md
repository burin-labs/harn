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

The rig does not decide tier 2 versus tier 3 itself. `ast.undefined_names`
reports it on `resolution`, because the two things that decide it — the
language's ceiling and the constructs present in the file — are both parser
facts. The rig started with its own substring tables, and they were removed once
the builtin owned the question; keeping them would have been a second copy of
the policy, free to drift from the first. `lib/diagnostics.harn` now reads
`resolution.complete` and names `resolution.defeaters` in the demoted line so
the model can judge for itself.

Because the rig reads a field that exists only in this worktree, `run.sh` uses
the locally built harn rather than whatever is on `PATH`.

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
`surfaced_transient`, `surfaced_noise`, `surfaced_genuine`, `detour_calls`, the
tool sequence, and a `provenance` record (resolved endpoint, model, tool
channel, LLM timeout, rig SHA).

## Analysis

```sh
harn run analyze.harn -- runs/stage1.jsonl --control A-push-all
harn run analyze.harn -- runs/stage1.jsonl --json
```

`analyze.harn` turns the trial rows into a decision table. Two of its rules are
structural rather than left to whoever reads the output.

**A tool-emission stall is its own outcome class.** A trial that stops of its
own accord without ever emitting a mutating call did not fail to resolve a
diagnostic; it never attempted the task, so no timing policy had anything to act
on. Those trials are counted separately and excluded from the pass rate and the
paired intervals, the same way an infrastructure skip is not a failure. Letting
them depress the precision number would charge the post-edit diagnostics path
for a defect in the tool-call emission stream, which is a different subsystem
and which every arm inherits equally.

**Every fixture is reported on its own line, and constants are named.** A
fixture whose outcome is identical on every arm discriminates nothing. Rolling
it into an aggregate makes the aggregate look better resolved than the evidence
supports, so the per-fixture table says `constant` and the aggregate is read
beside it, never instead of it.

**The rig's own falsifier is asserted, not left to the reader.** `clean-control`
produces no diagnostics, so no arm has anything to deliver and the arms must be
indistinguishable on every exposure counter. `analyze.harn` checks that
directly, prints `HELD`, `VIOLATED`, or `ABSENT`, and exits non-zero on a
violation, because a run where the arms differed with nothing to differ on
measured the rig rather than the policy. `ABSENT` is a distinct state from
`HELD` on purpose: a run that never exercised the fixture was not falsified
either way, and "we did not look" must never read as "we looked and found
nothing".

Turns to green are reported over resolved trials only and always beside the pass
rate. An arm that converges fast by giving up is not a winner, and separating
the two numbers is what makes that visible.

## Measurement scope: tool channel

Live results here measure the **json (fenced) tool channel**, resolved from the
catalog. They are not a general result across tool formats: on the llama.cpp
qwen3.6 route the catalog declares `native_tools = false` with
`tool_mode_parity = "text_only"` (receipted 2026-07-19 sweep), and forcing
`native` ships a request with zero tool schemas — the loop cannot run at all,
so native is unmeasurable on this serving stack rather than merely worse. Each
row's `provenance` records the requested, effective, and catalog channel so a
drifted run is visible in the data.
