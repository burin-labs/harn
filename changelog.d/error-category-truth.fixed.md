- **The documented error categories now match the ones the runtime emits.**
  `error_category()` listed 10 of 18 and `llm_call_safe()` listed 15, so a
  script switching on either list could receive a value it was told could not
  occur — `transient_network`, among others. Both now point at one canonical
  table, and a test keeps it in step with the runtime.
