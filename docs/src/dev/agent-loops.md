# Agent Loop Runtime Notes

## Completion Judge Isolation

`done_judge` and `verify_completion_judge` run on a transcript projection. The
judge prompt includes the worker transcript, but the judge's own LLM request and
structured response are not appended to `agent_session_messages(session_id)`.
Only legitimate worker turns and explicit runtime feedback injections mutate the
worker session transcript.

This keeps replay and follow-up turns deterministic: a judge veto may inject a
feedback message because that message is intended to steer the worker, while an
accepted judge decision only emits `judge_decision` and `typed_checkpoint`
events. Tests should assert transcript equality around accepted judge calls
instead of inferring isolation from event counts.
