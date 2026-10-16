Session inbox delivery and workflow policy contracts now compile in
`harn-orchestration`, sharing the extracted interpreter and LLM contracts.
Native runtime entry points use that owner for queued messages, selective
draining, wakeups, and workflow callback values.
