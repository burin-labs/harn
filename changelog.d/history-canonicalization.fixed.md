An agent session now remembers each assistant turn in the tool-call dialect it
was read in, rather than in the bytes the model happened to type.

A model learns its call syntax from its own transcript more strongly than from
its instructions. Measured against a local open-weight model, one drifted
fence reaching the persisted turns was enough: every later turn copied it, and
no prompt-side wording outbid it — rewriting the persisted turns to canonical
form converted the same site from nothing canonical to fully canonical. So the
turn a session SHOWS and the turn a session REMEMBERS have come apart. The
visible text is still the model's own words, for people and for the readers
that judge a turn; the recorded copy is rendered from the prose and the calls
the parser already separated, in the format that parsed them.

Only what was actually dispatched is remembered, so a collapsed duplicate read
or a dropped unsafe batch no longer leaves the model looking at a call with no
result. A turn with nothing to canonicalize — no dispatched calls, a native
route whose calls travel on the provider's tool channel, or a turn that
produced a user response — is recorded exactly as before.

This cures dialect drift, which propagates through self-history independently
of argument content. Placeholder arguments propagate the same way and are cut
off at their source instead: correctives no longer render argument values for
a model to copy.
