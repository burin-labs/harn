The mock provider now finishes a generated-prose run in whichever spelling of
the completion sentinel the prompt taught, instead of only the tagged
`<done>` block.

The tagged text grammar teaches the block and the other tool grammars teach
the bare sentinel. Keyed on the block alone, the simulator agreed with exactly
one grammar: when a route moved to the bare spelling, its runs silently
stopped completing and ran out their iteration budget instead — no fixture
changed, only the prompt they were answering. A prompt that never asks for a
sentinel still gets none, so runs that mean to exhaust their budget still do.
