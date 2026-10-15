Native host-service access and execution-evidence assembly now live above the
interpreter. Async builtins retain their execution-owned pool context through a
required poll interface, including when a pending operation is cancelled.
