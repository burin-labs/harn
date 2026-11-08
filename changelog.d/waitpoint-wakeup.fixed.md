- **Waitpoint wake-ups (#11209).** Pending in-process calls wake on local state changes
  and use the calling Harness's clock for timeouts. Durable waits retain checks
  for writes from other processes and recheck stored completion before expiry.
