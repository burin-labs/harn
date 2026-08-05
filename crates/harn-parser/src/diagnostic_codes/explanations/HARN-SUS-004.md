# HARN-SUS-004 — resume snapshot cannot be loaded or used

`resume_agent(...)` could not load the requested snapshot, or the snapshot was
not usable as a worker state. The path may be missing, stale, unreadable, or
from an incompatible worker-state format.

Use the `snapshot_path` from the latest worker summary and keep the worker
state directory available across process restarts. If the snapshot belongs to
an older incompatible runtime, spawn a replacement worker instead of resuming.
