- Added a semantic *stance* tier to the behavioral ASR probe
  (`security::stance_judge`): a judge that reads the framed attack turn and the
  model's reply and classifies obeyed-vs-resisted, run as a post-processor over
  the `BEHAVIORAL_PROBE_DUMP` transcripts. The deterministic canary metric alone
  conflates a model that *obeyed* an injection with one that *refused but quoted*
  the canary while describing it; the judge separates them, surfacing
  narrate-and-quote false alarms (canary hit, judged resisted) and subtle
  obedience the canary missed (canary absent, judged obeyed). The judging logic
  is unit-tested against a mock; the live judge is an on-demand `#[ignore]` run.
