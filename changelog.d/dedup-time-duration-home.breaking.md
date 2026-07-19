- **Duration strings now use one grammar everywhere.** Five call sites had
  forked the `<number><unit>` parser and disagreed on what the same string
  meant. Most sharply, `when_budget.timeout` was parsed by *both* the CLI
  manifest validator and the runtime `trigger_register`, which disagreed on
  overflow — the same value was rejected at validation but accepted and
  silently clamped at registration. The single grammar requires a unit suffix,
  accepts `ms`/`s`/`m`/`h`/`d`/`w` case-insensitively, and reports oversized
  values instead of clamping them. It now matches the Harn language's own
  duration literals. Behaviour changes by call site:
  - `when_budget.timeout` in a package manifest (`harn` CLI validation) and in
    `trigger_register` (runtime): a bare number such as `timeout = "500"` was
    read as 500ms and is now an error — write `"500ms"`. `d` and `w` suffixes
    are now accepted. An oversized value is now an error at
    `trigger_register`, where it previously saturated to `u64::MAX` and
    presented to the user as a hang.
  - `harn run --timeout`: `d` and `w` suffixes are now accepted. This flag
    keeps its own rule that the value must be greater than zero, since a
    zero-length run deadline is always a mistake rather than an instruction;
    that check is documented at the call site rather than folded into the
    shared grammar.
  - All duration arguments across the CLI: units are now matched
    case-insensitively (`"5M"`) and a space before the suffix is tolerated
    (`"5 m"`).

  No shipped manifest, fixture, conformance test, or documented example used a
  bare number for these fields — every one already wrote an explicit unit — so
  this affects hand-written configs only.
