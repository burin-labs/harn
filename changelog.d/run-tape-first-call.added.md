Run-tape branches can start live inference at the first model call, allowing
first-call prompt edits. The branch still verifies pre-inference operations,
requires a recorded baseline call, and reports an incomplete run if inference
is never reached.
