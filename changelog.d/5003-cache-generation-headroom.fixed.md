- Keep the hosted Linux merge-gate caches resident by replacing stale immutable
  generations under measured save headroom instead of failing when release
  caches fill the shared pool (#5003).
