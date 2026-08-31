# p1 verification probes

Probes that check whether a merged fix is actually reachable on the canonical
path, rather than only unit-tested. Each one is paired with a negative control
against an older release binary: a probe that cannot fail on the old binary
proves nothing.

## `stream_validator_length_probe.harn` (harn#7324)

$0, no inference. Feeds a 300-character string into `std/json`'s partial-JSON
validator against a schema capping that field at 240, chunked so the string
closes before the document does — the exact moment the mid-stream schema
watcher could fire. A short-value direction control runs the same chunking so
an `invalid` verdict cannot be an artifact of how the chunks were split.

Use it as the instrument check before concluding anything from a live
structured-output run: it proves the length constraint is enforceable through
this component at all.

Observed: `v0.10.118` and current main both report
`Invalid{path: $.detail, "is longer than 240 characters"}` once the string
closes. Only current main adds `reason_kind: max_length`, the closed-vocabulary
field the issue asked for.

## `schema_cap_probe.harn` (harn#7324)

Live structured-output call whose schema carries `maxLength` on a string field.
The strict compat profiles strip `maxLength` before serializing
`response_format.json_schema`, so the model is never told the cap; the issue is
that the reply was then validated against the original, unstripped schema and
the connection severed mid-stream.

`output: {schema, strict: true, stream_abort: true}` is what arms the watcher.
Omitting `stream_abort` leaves the mechanism unreached and the probe green for
the wrong reason.

Provider and model are edited in place; the committed default is a local
OpenAI-compatible route. Note that a grammar-constrained local server can only
ever violate the stripped keywords, so the local route is a weak reproduction —
prefer a hosted route when reproducing the original population.
