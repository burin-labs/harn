Prompt source maps retain the real source line and column for literal text,
raw blocks, and bare interpolations, including text after whitespace trimming.
Each span and its include ancestry carry the source revision so debuggers and
recording readers can distinguish the recorded source from later edits.
Identical rendered text from different source maps retains distinct recording
identities.
