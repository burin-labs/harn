# HARN-POL-002 — fail-fast pool has no immediate capacity

`pool.submit(...)` targeted a pool configured with
`Backpressure().fail_fast`, but all worker slots were already occupied.
Fail-fast pools do not queue work.

Catch the error if rejection is expected, raise `max_concurrent`, or use
`Backpressure().queue(...)` when callers should wait, drop, or retain a
bounded queue instead.
