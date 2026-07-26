# Porting tool-call dialect composition to Harn (harn#5142)

Working spec for the agents doing the port. Written to be followed without
asking the author questions.

## The cut line

Rust keeps the **structural token stream**: one linear scan that delimits
top-level units. Harn owns **what a unit means** — which tags and markers are
tool calls, the recovery ladders, argument-extraction policy, violation
classification, and the composition of passes.

Rust does everything proportional to **bytes**. Harn does everything
proportional to **candidates**.

Delimiting still needs vocabulary (you cannot find the end of a `<tool_call>`
block without knowing the tag). So the vocabulary is **passed in as data** from
Harn — `std/llm/dialects` — and Rust must consume those lists, never restate
them. If a dialect change requires editing Rust, the cut line has been
violated.

## Measurements (read this before designing anything)

All numbers from the **debug** binary, so the profile cancels out and the
ratio is meaningful. Payloads are synthetic mixed prose/fences/calls.
Reproduce with the probes in `.harn-runs/` (uncommitted, see "Probes" below).

| What | Payload | Cost |
|---|---|---|
| Current Rust parser, end to end (`agent_parse_tool_calls`) | 79.7 KB, 250 calls + 750 drops | **35–41 ms/parse** |
| Harn walk over 1000 hand-built unit dicts, with `regex_captures` per call unit | 63.8 KB synthetic | **46–52 ms/walk** |
| Harn walk, dispatch only, no regex | 1000 units | **~17 µs/unit** (330–460 ms / 20 000 visits) |

Three conclusions, each of which changes what you should build:

1. **The contract is viable but the margin is thin.** The Harn layer alone
   costs ~1.2–1.5x the *entire* current Rust parse. Add the Rust scan on top
   and the total lands near the 2x budget. Treat per-unit cost as a scarce
   resource; do not add a second pass over the units.

2. **`regex_captures` per unit is the single largest avoidable cost** — it is
   most of the gap between the 17 µs/unit floor and the 46 µs/unit full walk.
   **Therefore the scanner must pre-extract the call head.** A `block` unit
   carries `head_name` and `head_sep` (see below) so the common case is a dict
   lookup, not a regex. Reserve regex for genuinely irregular dialects (DSML
   parameters, `<parameter=...>` markup), which are rare per response.

3. **List accumulation is NOT the bottleneck.** Counter to the obvious guess,
   a walk that accumulates with `.appending(...)` measured *faster* than one
   that only counts. Do not contort the composition to avoid building lists.

### Footgun that invalidated a first measurement

`xs.push(y)` in Harn is **pure** — it returns a new list and does not mutate
`xs`. A loop doing `parts.push(...)` and then reading `parts` silently gets an
empty list, and the probe reported a 0-byte payload that "parsed" in 2 ms.
Use `xs = xs.appending(y)`. Assume any accumulation you did not verify with a
length assertion is wrong.

## The scan primitive

```text
__host_tool_scan_units(text: string, spec: dict) -> { source: string, units: list }
```

`source` is the post-thinking-strip text. Every unit offset indexes `source`,
never the caller's original string — thinking-tag stripping rewrites the input
before scanning, which is why the current `DroppedFragment` carries text
instead of an offset.

### `spec`

| Field | Type | Meaning |
|---|---|---|
| `call_tags` | `list<string>` | Tags whose body may carry a call. Heredoc-aware close scan. From `TAGGED_DIALECT` where canonical is `tool_call`. |
| `block_tags` | `list<string>` | Plain `<tag>body</tag>` blocks (narration / answer / done). Cheap `find` close, no heredoc scan. |
| `wrapper_tags` | `list<string>` | Contentless wrappers to emit and swallow (`function_calls`). |
| `markup_openers` | `list<{opener, close}>` | Chat-template function markup. |
| `reserved_openers` | `list<string>` | Wire openers standing in for a call tag (`[[CALL]`). |
| `harmony` | `dict` | `header_markers`, `standalone_markers`, `frame_prefix`, `frame_suffix`, `message_marker`, `tool_call_header_prefix`, `corrupted_openers`. |
| `known_tools` | `list<string>` | Registry names + `IMPLICIT_TOOL_NAMES`. Needed for delimiting only — an angle-wrapped `<name(...)>` is a unit only when `name` is known, otherwise it is an unknown tag. |
| `strip_thinking` | `bool` | Default true. |

Every value comes from `std/llm/dialects`. Rust must not default any of them
to a hardcoded list; an absent field is an error, not a fallback.

### Unit kinds

Common fields on every unit: `kind`, `start`, `end`.

| Kind | Extra fields | Emitted when |
|---|---|---|
| `text` | `text`, `fenced: false` | A run containing no structural opener. Heredoc bodies stay *inside* the run. |
| `fenced_line` | `text` | Cursor is inside a markdown fence, or not line-leading and not adjacent to a consumed block. One line. |
| `block` | `tag`, `body`, `closed: true`, `head_name?`, `head_sep?` | A resolved `<tag>…</tag>`. |
| `unclosed_block` | `tag`, `body` | Open tag with no close. `body` is the remainder; consumes to EOF. |
| `stray_close` | `tag` | Orphan `</tag>`. |
| `wrapper` | `tag` | A contentless wrapper tag. |
| `markup` | `opener`, `text` | Top-level function markup through its close tag, or EOF. |
| `angle_call` | `name`, `arguments`, `end` | `<name(…)>` where `name ∈ known_tools`. Rust parses the expression because the extent depends on it. |
| `harmony_line` | `text` | A `<\|message\|>tool_call to=…` header line. |
| `harmony_skip` | — | A frame marker / corrupted wrapper consumed structurally. |
| `tag` | `raw`, `name` | Unknown or unclosed tag fragment, to end-of-line or `>`. |
| `reserved_block` | *(same as `block`/`unclosed_block`)* | See below. |

**`head_name` / `head_sep` (the perf-critical fields).** For any unit carrying
a body, Rust reports the leading call head if there is one: `head_name` is the
identifier, `head_sep` is `"("`, `"{"`, or absent. This is what lets Harn
branch without a regex. Rust does *not* decide whether the name is a real tool
— that is Harn's policy call against the registry.

**Reserved openers collapse.** A `[[CALL]` stub is normalized in Rust to the
canonical opener and emitted as an ordinary `block` or `unclosed_block` with
`tag: "tool_call"` and `reserved: true`. This is what `reserved.rs` already
does internally, so Harn needs no separate branch — only the `reserved` flag,
for diagnostics.

## The drop invariant — build it into the structure

**Every path that consumes text without producing a call must record a
dropped fragment.** Make the record a *precondition of advancing the cursor*,
not a thing each branch remembers.

Concretely: the composition has one `consume(unit, outcome)` funnel. It is the
only thing that advances the cursor, and it refuses to advance unless
`outcome` either credits a call or carries drop text. No branch gets to move
the cursor on its own.

This is not theoretical tidiness. Eleven branches in the Rust scanner had the
drop record and one did not — the sniff-error branch — and the symptom was a
detector three layers away going quiet on a real dropped dialect. Fixed in
`35389641b`. Per-branch discipline is what produced that bug.

End-to-end pin: `conformance/tests/agents/agent_unparsed_tool_syntax_shape.harn`.

Drop *granularity* may change (whole-unit fragments instead of line slices are
fine — `std/llm/tool_shape` rejoins all fragments in document order and treats
reasons as metadata, not run boundaries). The invariant is that nothing is
consumed unrecorded.

## The other invariant — the stray sniffer EXECUTES

`report_stray` runs the bare-call parser over stray *and fenced* text and, when
a call names a registered tool, **dispatches it** and bumps
`recovered_from_stray_count`. Verified: fenced `<tool>look({…})</tool>`, fenced
bare `look({…})`, and fenced `{"name": "look", …}` each parse to one executed
call today.

Markdown fencing does **not** make a call inert. The fence check happens
upstream at the scan position and classifies the fragment as
`FencedNarration`; `report_stray` then sniffs it and executes anyway. The only
genuinely silent path is fenced/stray content the sniffer *cannot* execute —
unknown tool names and name-in-tag dialects.

Do not "fix" this while porting. A port that treats the fenced branch as
prose-only will silently stop dispatching real calls.

## Branch mapping

Source: `tagged/mod.rs::parse_text_tool_calls_with_tools`, in scan order.

| # | Rust branch | Unit kind | Harn handling |
|---|---|---|---|
| 1 | `recover_malformed_call_opener` | `block`/`unclosed_block` + `reserved` | Same ladder as a real tool-call block. |
| 2 | non-`<` run, heredoc-aware | `text` | Stray-report ladder: sniff → salvage → violation. Always records a drop. |
| 3 | `consume_harmony_tool_call_line` | `harmony_line` | Stray-report as prose. |
| 4 | corrupted close / wrapper open / frame marker | `harmony_skip` | Advance, no output. |
| 5 | not line-leading, or fenced | `fenced_line` | Stray-report with reason `fenced_narration`. **Sniffer still executes.** |
| 6 | `match_tool_call_block` | `block` (tag ∈ `call_tags`) | Narration recovery → JSON body → nested XML → function markup → bare call. |
| 7 | `unclosed_tool_call_open` | `unclosed_block` | XML-wrapped JSON → function markup → complete bare call → TRUNCATED diagnostic. |
| 8 | `assistant_prose` / `user_response` / `done` | `block` (tag ∈ `block_tags`) | Collect prose / answer / done marker. Empty `<done>` is a violation. |
| 9 | `try_parse_angle_wrapped_call` | `angle_call` | Credit the call, add a soft violation. |
| 10 | `try_parse_top_level_function_markup` | `markup` | Recover call + soft violation, or surface the parse error. |
| 11 | `stray_tool_call_close_len` | `stray_close` | Swallow silently. |
| 12 | `function_calls_wrapper_len` | `wrapper` | Swallow silently. |
| 13 | unknown/unclosed tag | `tag` | Unclosed terminal-answer recovery, else a violation naming the tag. |

Post-loop, all in Harn: clear prose when violations exist with no calls/answer;
the effectively-empty response violation; joining `user_response` parts;
`assign_turn_unique_ids`.

## The two async seams

Rust reaches the Harn composition through exactly two functions, both already
async as of `6f6b2bfd1`:

- `llm/api/result.rs::parse_visible_text_tools` — the provider's own text,
  reached via `build_llm_text_projection` (one projection per provider result).
- `llm/api/result.rs::parse_candidate_text_tools` — the derived strings the
  structured-output extractor invents, which have no projection to reuse.

Both call `stdlib::harn_entry::call_harn_export_typed(ctx, "std/llm/tool_parse",
…)`. `AsyncBuiltinCtx` is already threaded through `call.rs`. The third entry
is the agent loop's own `__host_agent_parse_tool_calls`, which becomes a thin
Harn call.

Two callers still parse synchronously and must be routed before the Rust
parser is deleted: `agent_tools::embedded_call_repair_result` (make
`deny_tool_call` async — it is already called from an async dispatch path) and
`tool_conformance::classify_tool_probe_response`. Observability's
`merged_tool_calls_for_observability` should take the projection's merged calls
rather than re-parsing.

## Fixture migration inventory

~4900 lines of Rust unit tests become conformance cases. Generate `.expected`
from **observed** output of the built binary, then diff each against the old
Rust assertion; disagreements are bugs or deliberate changes, never noise.

| Rust test file | Lines | Conformance grouping |
|---|---|---|
| `tools/tests/heredoc_and_messages.rs` | 1805 | `tool_parse_heredoc_*`, `tool_parse_native_json_*` |
| `tools/tests/validation_and_tagged.rs` | 1391 | `tool_parse_tagged_*`, `tool_parse_violations_*` |
| `tools/tests/core_parser.rs` | 797 | `tool_parse_core_*` |
| `parse/fenced_json/tests.rs` | 1040 | `tool_parse_fenced_*` |
| `tools/tests/corpus_conformance.rs` | 490 | `tool_parse_corpus_*` |
| `tools/tests/function_markup.rs` | 349 | `tool_parse_markup_*` |
| `tools/tests/reserved_token.rs` | 301 | `tool_parse_reserved_*` |
| `tools/tests/native_tools.rs` | 344 | keep in Rust (native channel, not text dialect) |
| `tools/tests/entity_decode.rs` | 140 | `tool_parse_entities_*` |
| `tools/tests/string_escape_fidelity.rs` | 211 | `tool_parse_escapes_*` |
| `tools/tests/implicit_call_paren_close.rs` | 88 | `tool_parse_core_*` |

Primitive-contract tests (does the scanner delimit correctly?) stay in Rust —
they are not dialect behavior.

## What stays in Rust permanently

`syntax.rs` and `ts_value_parser.rs` are a genuine lexer: the TS value parser,
heredoc scan/unescape, string-span skipping, `find_close_tag`,
`balanced_json_object_len`, `ident_length`, `strip_thinking_tags`,
`render_canonical_call`. Plus `TextIndex`, JSON parsing, and the scan itself.

`streaming.rs` (`StreamingToolCallDetector`) is incremental and
observability-only, and has no VM context, so it cannot read a Harn-owned
table. Its small opener set stays duplicated in Rust. Document the boundary;
do not invent a codegen path for it.

## Probes

The three measurement probes live in `.harn-runs/` and are deliberately
uncommitted (`.harn-runs/` is ignored): `unit_walk_probe.harn`,
`append_ab.harn`, `baseline_parse.harn`. Rebuild them from the numbers table
above if you need to re-measure; they are ~60 lines each. A committed bench
belongs in the repo's bench surface only once the port has something real to
measure.
