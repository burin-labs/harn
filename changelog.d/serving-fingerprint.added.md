- **Provider telemetry now records the backend build that served each call.**
  OpenAI-compatible responses carry a `system_fingerprint` identifying the
  server build or configuration behind a route (llama.cpp reports its build
  string there), and it is now captured as
  `provider_telemetry.serving_fingerprint` on both the streaming and
  non-streaming paths. It is a build discriminator, not host identity: two
  different values prove two different server builds, while two equal values
  only mean the servers agreed on a build. That narrows — without closing —
  the ambiguity when several hosts serve byte-identical artifacts on the same
  local URL and `serving_base_url` cannot tell them apart. Absence means the
  provider reported nothing, never "the same build as before".
