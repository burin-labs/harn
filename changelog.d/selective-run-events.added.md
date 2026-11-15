Rust embedders can select which execution events a `RunEventSink` consumes.
Observing command preparation no longer requires capturing program stdout,
stderr, or unrelated transcript events. Existing sinks retain full capture.
