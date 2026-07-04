- **Agent-loop post-turn policy stops no longer run completion verification by
  default.** Callback stop verdicts now terminate authoritatively unless they
  explicitly set `needs_verify: true`, preventing graceful-recovery ripcords
  from being converted back into repair feedback.
