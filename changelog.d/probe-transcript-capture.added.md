- Added an opt-in transcript dump to the behavioral ASR probe
  (`security::behavioral`): set `BEHAVIORAL_PROBE_DUMP=<path>` and every probe
  appends its full transcript (framed user turn, raw reply, canary, scored
  outcome) as JSONL. A live A/B — base vs. a LoRA-adapted model — can then be
  root-caused from the actual replies instead of aggregate counts, which is what
  distinguishes a model that *obeyed* an injection from one that merely *narrated*
  it and happened to quote the canary. Env unset is a byte-for-byte no-op, so CI
  (mock models, no env) is unchanged. The on-demand baseline doc also records
  that `mlx_lm.server` 0.31.3 ignores per-request temperature, so a local "N=5"
  read degenerates to N=1 — confirm variance before claiming a bootstrap CI on a
  local surface.
