Session spend-limit requests now return an acknowledgement, and reminders accepted before the first turn reach that turn.
Replay planning refuses recordings whose prompts ended without normal completion.
Recorded and simulated model calls no longer reserve provider spend before replay is checked.
Replay completion checks reject explicit agent failures even when the protocol reports `end_turn`.
