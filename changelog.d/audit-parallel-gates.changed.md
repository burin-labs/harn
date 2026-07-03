- **The CI "Harn conformance + audit" lane runs its gate battery in
  parallel.** `scripts/audit_gates.sh` builds the harn CLI + runs the
  conformance suite once, exports the warm `target/debug/harn` as `HARN_BIN`
  so no downstream gate re-walks cargo's build graph, then hands the ~25
  independent gates to `make -j -k`. The serial check-`*`/lint tail collapses
  from `sum(gates)` to `max(gate)` (measured ~116s → ~41s, 2.83x, on a warm
  binary) with identical verdicts, and `-k` reports every gate's result
  instead of stopping at the first failure. Mirrors the proven
  `release_gate.sh audit` fan-out.
