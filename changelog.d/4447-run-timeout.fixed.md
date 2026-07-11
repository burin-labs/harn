Add `harn run --timeout <duration>` and route async process timeout/kill paths
through Harn's token-scoped process-tree cleanup registry, so timed-out runs do
not depend on host `timeout(1)` behavior.
