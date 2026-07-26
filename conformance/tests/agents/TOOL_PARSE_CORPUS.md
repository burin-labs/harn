# The `tool_parse_*` conformance corpus

The behavioral corpus for the text tool-call parser, migrated from ~4900 lines
of Rust unit tests into executable conformance fixtures ahead of the harn#5142
parse re-platform. Every baseline was generated from the **observed stdout of
the built binary**, never from reasoning and never from the Rust assertions, so
the corpus pins what the parser does rather than what anyone believes it does.

The port's contract is: make these pass unchanged. A diff in any `.expected`
here is a behavior change and needs an argument, per `conformance/README.md`.

## Layout

`_tool_parse_probe.harn` is a helper module, not a test — it has no baseline
file, so the runner skips it the same way it skips `_common.harn` and the
`modules/*_lib.harn` helpers. It owns the four registries the Rust corpus used
(`sample_registry`, `corpus_registry`, `echo_registry`, `native_registry`) and
the printers every fixture reports through.

Keep the registries' **parameter types** intact when editing them. They are not
decoration: chat-template markup parses a parameter value as JSON when the
schema is non-string and keeps it verbatim when the schema is a string, so
`content: {type: "string"}` versus `ops: {type: "list"}` is the difference
between two fixtures passing and failing.

### What the common record prints, and what it does not

`report` prints, per case: call count, per-call name plus a key-sorted
`json_stringify` of the arguments, every parse error and protocol violation in
full, the stray-recovery count, `prose`, `user_response`, and `done_marker`.

Three observable facts are deliberately **excluded** from that record and live
in dedicated fixtures instead, because they are expected to move independently
of dialect behavior. Keeping them out means a port that re-cuts one of them
diffs one file rather than all twenty-six.

| Excluded fact | Fixture | Why it is separate |
|---|---|---|
| `dropped` fragment granularity | `tool_parse_drops` | `PORT_PLAN.md` says whole-unit fragments instead of line slices are fine. The invariant is that *something* is recorded, not where the cuts fall. |
| `canonical_text` | `tool_parse_canonical_replay` | Long, and orthogonal to whether a call parsed. |
| Synthetic call ids | `tool_parse_turn_unique_ids` | The counter is **process-global**. The Rust test asserts `tc_0`/`tc_1`; run under the full suite the same input yields `tc_265`/`tc_266`. Pinning literals would pin fixture execution order, so that fixture asserts the contract — consecutive within a turn, no reuse across turns — instead. |

## RECONCILIATION: where the binary disagrees with the Rust corpus

Every migrated case was diffed against what its Rust test asserts. The large
majority agree exactly. Six do not, and all six share one root cause: **the Rust
test drives an internal function, and the public entry point has a scanner layer
in front of it that changes the answer.** Each is pinned as observed, with the
delta noted in the fixture's own comment.

This table is the composition lanes' checklist. Items 1 and 2 are open questions
the port should answer deliberately; items 3–5 are dead code paths the port
should not resurrect by accident; item 6 is not a real conflict.

### 1. An unknown tool beside a good call loses its diagnostic

- **Fixture:** `tool_parse_core_diagnostics`, "unknown tool name beside a valid call"
- **Rust test:** `core_parser.rs::reports_unknown_tool_names`
- **Rust asserts:** `errors.len() == 1` naming `fictitious_tool`, **and**
  `calls.len() == 1` — both the error and the good call.
- **Binary does:** `calls=1`, **`errors=0`**. The unknown call lands in `prose`.
- **Why:** `parse_bare_calls_in_body` produces both the call and the error, but
  the stray-report funnel credits the recovered call and discards the sniffer's
  error. The same unknown name *alone* (case 1 of the same fixture) does surface
  the error; suppression happens only when another call succeeded in the text.
- **Read on intent:** the Rust behavior looks more correct. A model that emits
  one good call and one malformed one currently gets no feedback about the
  malformed one, so it has no reason to stop emitting it. **The port should
  decide this on purpose**, not inherit it — and if it keeps the current
  behavior, this fixture is where that choice is recorded.

### 2. A bare call cannot carry an angle bracket in a quoted argument

- **Fixtures:** `tool_parse_heredoc_recovery`, "thinking tags inside arguments,
  wrapped" and "thinking tags inside arguments, bare" — pinned side by side
  precisely so the contrast is visible.
- **Rust test:** `heredoc_and_messages.rs::thinking_tags_inside_arguments_round_trip_verbatim`
- **Rust asserts:** `calls.len() == 1`, with `<think>` and `</think>` preserved
  verbatim in the `title:` string argument and in the heredoc body.
- **Binary does (bare, unwrapped):** **`calls=0`** and
  `TOOL CALL PARSE ERROR: ... unterminated string literal`.
- **Binary does (wrapped in `<tool_call>`):** parses fine, both tags preserved —
  matching the Rust expectation.
- **Why:** the top-level chunker ends a non-structural run at the next `<`. In
  the unwrapped form that lands **mid-string**, on the `<think>` inside
  `title:`, and shreds the call. Inside a `<tool_call>` block the close-tag scan
  bounds the body first, so the argument bytes are safe.
- **Read on intent:** the binary's bare-form behavior is a **real defect**, and
  the unit test was hiding it — `parse_bare_calls_in_body` never sees the
  chunker boundary that causes it. Worth fixing during the port, since the
  Harn-side composition owns unit delimiting; if it is fixed, this fixture's
  bare case is the one that flips.

### 3–5. Native-JSON name repairs are unreachable through the public surface

- **Fixture:** `tool_parse_native_json_aliases`, cases "harmony channel suffix on
  the name", "constrain marker with a command argument", "constrain marker with
  a read intent"
- **Rust tests:** `heredoc_and_messages.rs::native_json_fallback_strips_harmony_channel_suffix`,
  `..._infers_run_from_harmony_marker_wrapper`,
  `..._infers_look_from_harmony_marker_wrapper_read_intent`
- **Rust asserts:** one call each, resolving to `run`, `run`, and `look`.
- **Binary does:** **`calls=0`** for all three. Prose shows the name with a hole
  in it, e.g. `{"name":"run\n\ncommentary",...}`.
- **Why:** these inputs carry a harmony marker *inside the JSON `name` value*.
  The tagged scanner consumes `<|channel|>` / `<|constrain|>` as framing before
  the native-JSON lane ever sees the string, so the lane's own channel-suffix
  strip and argument-shape inference never run.
- **Read on intent:** the repairs are **dead code on this path**. Note the
  contrast: the JSON lane handles the same input correctly —
  `tool_parse_fenced_grammar`, "harmony channel suffix strips from the name",
  resolves `run<|channel|>commentary` to `run` under `tool_format: "json"`. The
  two lanes genuinely disagree. The port should either route text input so the
  repairs are reachable, or delete them as unreachable — but it should not
  port them as-is and assume they work.

### 6. Raw `[[CALL]]` wire form with a registry present

- **Fixture:** `tool_parse_reserved`, "raw wire form, registry present" and "raw
  wire form, no registry"
- **Rust test:** `reserved_token.rs::wire_form_and_canonical_form_do_not_silently_lose_calls`
- **Rust asserts:** `calls.len() == 0` on the raw wire form.
- **Binary does:** `calls=0` with no registry (**matches**); `calls=1` when a
  registry is passed.
- **Why:** not a true disagreement — the Rust test passes `None` for tools. With
  a registry the stray sniffer finds and executes the inner bare call, and the
  `[[CALL]]` delimiters survive into visible prose.
- **Read on intent:** both are current, correct behavior. Both variants are
  pinned so the port cannot change either without noticing.

## Not translated

These Rust tests were deliberately left behind. They are **not** covered by this
corpus, so the composition lanes must not treat a green `tool_parse_*` run as
permission to delete them.

| Rust test(s) | Reason | Substitute here |
|---|---|---|
| `native_tools.rs` (13 tests) | Native tool channel, not a text dialect. `PORT_PLAN.md` keeps these in Rust permanently. | none needed |
| `validate_tool_args_*` (5), `collect_tool_schemas` | Schema validation, downstream of parsing; not reachable through the parse result. | none |
| `normalize_tool_args_*` (12, across `heredoc_and_messages.rs` and `corpus_conformance.rs`) | The dispatch chokepoint, not the parser. | The *split* is pinned: `tool_parse_markup`, "heredoc nested inside a markup parameter stays verbatim", shows the parser keeping `<<EOF ... EOF` intact. That is what stops a port from unwrapping it early and stealing the chokepoint's job. |
| `build_assistant_tool_message`, `build_assistant_response_message` (5) | Provider wire-format message construction, not parsing. | none |
| `read_file_offset_and_limit` | `handle_tool_locally`; unrelated to parsing. | none |
| `wire_to_canonical`-dependent tests in `reserved_token.rs` (3) | Transport-layer remap, not exposed through any public Harn surface. The two large `testdata/qwen36_*` fixtures are reachable only through it. | `tool_parse_reserved` covers the malformed `[[CALL]` opener the remap deliberately does *not* catch — the branch the parser actually owns. |
| `streaming_and_non_streaming_remap_parse_identically` | Asserts two `wire_to_canonical` outputs are equal; no parser behavior of its own. | none |
| `tc19_zig_body_round_trips_byte_exact` | 30 KB `include_str!` payload; inlining it would bloat the baseline for no added signal. | The adversarial round-trip properties it guards are covered by 7 inline cases in `tool_parse_fenced_verbatim`: interior tag line, all promised literals, empty body, CRLF, a nested ```` ```tool ```` separator, two bodies matched by opener, and batching coexistence. |
| `multiblock_turn_lands_every_call_when_paren_omitted` | 215-line `include_str!` payload containing both `"""` and `${`, neither representable inside a Harn triple-quoted string. | **Reconstructed, not byte-copied** — `tool_parse_core_implicit_paren`, "multiblock turn lands every call when the paren is omitted", rebuilds the shape (3 heredoc edits closing with a bare `}` plus a `run`, expecting 4 calls). Flagged because it is the one case whose input is not the original bytes. |

## Regenerating a baseline

```sh
harn run conformance/tests/agents/tool_parse_<name>.harn > \
  conformance/tests/agents/tool_parse_<name>.expected
```

Generate from a binary built at the semantics you intend to pin, and say which
one in the PR. Do not hand-edit an `.expected`: the whole value of this corpus
is that every byte in it came out of a real parse.
