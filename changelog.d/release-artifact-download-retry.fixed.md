Release archive builds now retry transient GitHub artifact download failures
twice before failing closed, so a brief endpoint refusal cannot discard an
otherwise fully certified multi-platform release candidate.
