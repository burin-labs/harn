Harn now carries execution identities as a validated runtime type from VM
creation through flight recording, verdict issuance, and evidence snapshots.
Persisted run records retain the same JSON shape, while malformed host-provided
identities can no longer become trusted evidence receipts. Trace-span identity
is immutable runtime ownership rather than caller-editable metadata, so reused
VMs cannot mix evidence from separate executions. Public run views also replace
invalid persisted identities or flight metadata with explicit evidence gaps.
