- Added a degenerate-variance guard to the behavioral ASR baseline: after the
  trials loop, if every trial produced an identical outcome signature it warns
  that the effective N is 1 and a bootstrap CI must not be claimed. This is
  provider-agnostic — it catches any temperature-ignoring serving surface (the
  confirmed `mlx_lm.server` 0.31.3 `mx.compile` RNG bug, a misconfigured server,
  or simply `temp=0`) at runtime, so the harness detects the quirk instead of
  hardcoding a brittle per-provider capability list.
