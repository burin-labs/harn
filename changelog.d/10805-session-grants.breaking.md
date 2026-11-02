Run-tape branches refuse tool dispatches that create or use session-scoped
dynamic permission grants before replaying their effects. Serving those calls
without their grant cache could ask for approval again in the live tail.
Live permission decisions and replay of once-only grants are unchanged.

## Migration

Record a new full tape with format 8 before branching. Older formats remain
inspectable but lack evidence that the session permission checks ran.
