**Streamed provider calls now record how long the first frame took.**
`provider_telemetry.client_first_frame_ms` measures the wait from request
dispatch to the first well-formed provider frame, sharing an origin with the
existing `client_wall_ms` so the two subtract: a slow call that was slow to
start can finally be told apart from one that was slow to stream, even on
providers that report no server-side timing of their own. The same value
reaches the `iteration_end` event's `iteration_info` beside `response_ms`, so
consumers that follow the event stream do not have to re-read the transcript
to attribute latency.

A call that did not stream reports the field as absent rather than zero, so
"not streamed" cannot be mistaken for "arrived instantly". The stamp is taken
only once a provider frame has actually parsed, which keeps SSE comments and
gateway keepalives arriving during prefill from reporting a near-zero latency.
