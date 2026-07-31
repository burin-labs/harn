### Fixed

- Isolate Nextest runtime state per test attempt so concurrent agent tests no
  longer contend on or persist transcripts into a checkout-wide session store.
