An agent loop on a self-hosted provider no longer ends after a single model
call. A locally served route bills nothing, so its cost is known to be zero
rather than unknown, and it no longer consumes a USD ceiling on the first turn.

Spend accounting stops a budgeted run as soon as any completed call is unpriced,
which is the right fail-closed answer for a paid provider whose rate cannot be
resolved. A self-hosted runtime was falling into the same bucket for a different
reason: it has no catalog rate because it has no rate, and cost resolution was
additionally gated on reported token usage, which a streaming local server may
omit entirely. Any preset carrying a cost budget therefore stopped the loop
right after its first call, on every locally served route.

The registry already states which providers serve from hardware the caller owns,
by giving them a `local_runtime` table. That declaration is now the single
predicate behind "this route bills nothing", read by both the provider-level
rate lookup and per-call cost resolution, so the two agree. Reported token counts
stay unknown when the server omits them, and `usage_unknown_calls` still says so.
Only the cost is now known, because a zero rate does not need token counts.

A post-call budget stop also reports which ceiling actually stopped it. The loop
previously broke out without recording a reason, and the terminal receipt filled
the gap with `max_iterations`, so a spend guard that fired on turn one of eight
was indistinguishable in the artifacts from an ordinary exhausted iteration
budget. The guard now answers with the ceiling it tripped, and a stop that still
arrives without a reason is reported as `unattributed` rather than assigned one.
