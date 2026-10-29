Monitor producers can capture distinct session-owned occurrences with
`harn signals fire --event-id ID --session SESSION`, retaining repeated output
while deduplicating retries of one event. Existing condition-state deduplication
is unchanged. Captured occurrences remain in the existing durable, bounded inbox
and never fall back to another session.
