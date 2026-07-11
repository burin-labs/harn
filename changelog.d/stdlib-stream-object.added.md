- Added partial typed-object streaming to `std/json/stream` (Vercel `streamObject` / Instructor `Partial[T]`).
  The incremental validator now exposes `partial()` — the best-effort partially-filled value from the bytes seen
  so far, closing the open string/containers at the frontier and dropping any half-typed trailing member — and a
  new `stream_object(source, schema?, opts?)` generator streams those partial objects as a `Stream<T>` that grows
  monotonically and ends with the fully parsed object, so progressive extraction no longer requires buffering to
  completion.
