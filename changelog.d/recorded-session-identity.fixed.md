Agent connection sessions retain their recorded identity during branch replay,
so session-dependent reads can verify before inference resumes. Replaying a
transcript change for a missing session now reports a divergence instead of
silently dropping the change.
Branches refuse fresh inference when recorded tool calls or reads remain
unconsumed, before entering the live continuation.
Tools that contain model calls are explicitly unservable until nested inference
has a replay contract; their recorded file effects are not applied.
