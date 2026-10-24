- **Live inference speed.** Every streamed model call now reports its time to
  first token and output tokens per second as an `inference_speed` session
  update: calibrated estimates while it streams (reasoning and tool-call
  arguments included), then the provider's or inference server's settled
  numbers when it ends. The settled measurement is also recorded on the call's
  `provider_telemetry.inference_speed`, and `std/llm/speed` prefers it over
  the wall clock.
