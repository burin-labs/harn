- **Run-event payloads are redacted at the bus boundary.** The redaction policy
  is now applied once, centrally, as every `RunEvent` is emitted, so a hook
  payload (and any future variant carrying free-form JSON) can't leak secrets by
  an emitter forgetting to scrub it first.
