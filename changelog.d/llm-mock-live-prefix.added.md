- LLM mock fixtures can replay a recorded prefix and then go live. A versioned
  header with `"liveAfterCalls": K` serves the first K calls from the fixture,
  then sends every later call to the configured provider through the normal
  path. A call the prefix cannot serve fails closed instead of going live
  early. Each call's `provider_telemetry.llm_mock_prefix` says whether the
  fixture or the live provider answered it. Fixtures without the header field
  behave as before.
