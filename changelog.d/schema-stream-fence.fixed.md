- **Streaming `output_schema` validation tolerates markdown code fences.**
  The incremental JSON validator behind `schema_stream_abort` (and the
  `std/json/stream*` builtins) now strips a leading triple-backtick fence
  (with an optional language tag such as `json`) and a trailing closing fence
  around the JSON body, surviving arbitrary chunk boundaries. Local Ollama
  structured-output calls that wrap their JSON in a code fence no longer abort
  with `schema_stream_aborted`. Genuine non-JSON leads and schema violations
  inside the fence still fail as before.
